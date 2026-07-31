use std::path::{Path, PathBuf};
use std::process::Command;

/// What activating a link region on a PDF page does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkTarget {
    /// External target (`https://…`, `mailto:…`) opened with the OS opener.
    Url(String),
    /// Internal link to another page of the same document (1-based).
    Page(u32),
}

/// One link region on a PDF page. `rect` is (left, top, right, bottom) in the
/// coordinate space declared by the owning [`PageLinks`] (top-left origin).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PdfLink {
    pub rect: (u32, u32, u32, u32),
    pub target: LinkTarget,
}

/// The clickable link regions of one PDF page, in pdftohtml's page space.
/// Kept in integer page units (not fractions) so the type stays `Eq` for the
/// editor's tab-state comparisons; [`Self::link_at`] does the normalising.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct PageLinks {
    pub page_w: u32,
    pub page_h: u32,
    pub links: Vec<PdfLink>,
}

impl PageLinks {
    /// The link whose region intersects the page-fraction rect
    /// `(x0, y0, x1, y1)` (0..1 on both axes, top-left origin). The caller
    /// passes the whole area a terminal cell covers, so a click needs no
    /// pixel-perfect aim on the (thin) text-line rects poppler reports.
    /// Adjacent TOC lines produce rects that overlap each other and one
    /// coarse cell can straddle two of them, so among several hits the link
    /// covering the largest share of the cell wins - not the first one in
    /// document order.
    pub fn link_at(&self, frac: (f64, f64, f64, f64)) -> Option<&PdfLink> {
        if self.page_w == 0 || self.page_h == 0 {
            return None;
        }
        let (w, h) = (self.page_w as f64, self.page_h as f64);
        self.links
            .iter()
            .filter_map(|l| {
                let (lx0, ly0, lx1, ly1) = l.rect;
                let ox = (frac.2.min(lx1 as f64 / w) - frac.0.max(lx0 as f64 / w)).max(0.0);
                let oy = (frac.3.min(ly1 as f64 / h) - frac.1.max(ly0 as f64 / h)).max(0.0);
                (ox > 0.0 && oy > 0.0).then_some((l, ox * oy))
            })
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(l, _)| l)
    }
}

/// Extract the link regions of `page` (1-based) via `pdftohtml -xml`, which
/// reports each text run a link annotation covers as `<text top= left= …>
/// <a href=…>`. Text-anchored only: a link drawn over a bare image has no
/// text run and is not reported — the common case (hyperref/beamer URLs)
/// is always text.
pub fn page_links(pdf: &Path, page: u32) -> std::io::Result<PageLinks> {
    which("pdftohtml")
        .ok_or_else(|| std::io::Error::other("install poppler (pdftohtml) to open PDF links"))?;
    let p = page.to_string();
    // .output(), never .status(): the child must not inherit croft's TTY
    // (the pdftoppm trailer-dictionary spray, same class).
    let out = Command::new("pdftohtml")
        .args(["-xml", "-stdout", "-i", "-f", &p, "-l", &p])
        .arg(pdf)
        .output()?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(std::io::Error::other(format!(
            "pdftohtml exited with {}: {}",
            out.status,
            stderr.trim()
        )));
    }
    Ok(parse_pdf2xml_links(&String::from_utf8_lossy(&out.stdout), page).unwrap_or_default())
}

/// Parse pdftohtml's `-xml` output, returning the links of `page` (matched
/// against the `<page number=…>` attribute — poppler keeps original numbers
/// when a `-f`/`-l` range is used). None when that page is absent.
pub fn parse_pdf2xml_links(xml: &str, page: u32) -> Option<PageLinks> {
    let mut in_page = false;
    let mut out = PageLinks::default();
    let mut found = false;
    // pdftohtml emits one element per line, but poppler also chats onto
    // stdout (a GoTo annotation prints " link to page N" glued to the
    // start of the <page> line), so elements are located with find(), never
    // by line prefix.
    for line in xml.lines() {
        if let Some(pos) = line.find("<page ") {
            let tag = &line[pos + "<page ".len()..];
            in_page = attr_u32(tag, "number") == Some(page);
            if in_page {
                found = true;
                out.page_w = attr_u32(tag, "width").unwrap_or(0);
                out.page_h = attr_u32(tag, "height").unwrap_or(0);
            }
            continue;
        }
        if !in_page {
            continue;
        }
        if line.contains("</page>") {
            break;
        }
        let Some(pos) = line.find("<text ") else {
            continue;
        };
        let tag = &line[pos + "<text ".len()..];
        let (Some(left), Some(top), Some(w), Some(h)) = (
            attr_u32(tag, "left"),
            attr_u32(tag, "top"),
            attr_u32(tag, "width"),
            attr_u32(tag, "height"),
        ) else {
            continue;
        };
        // poppler often wraps only part of the run in the <a> (") is another "
        // inside a longer sentence), and pdf2xml carries no per-<a> geometry.
        // Each link therefore gets the proportional horizontal slice of the
        // run rect its own characters cover, so the words next to a link do
        // not become clickable with it.
        let Some(gt) = tag.find('>') else { continue };
        let content = tag[gt + 1..].split("</text>").next().unwrap_or("");
        let total = visible_chars(content);
        let mut pre = 0usize;
        let mut rest = content;
        while let Some(pos) = rest.find("<a href=\"") {
            pre += visible_chars(&rest[..pos]);
            let after = &rest[pos + "<a href=\"".len()..];
            let Some(end) = after.find('"') else { break };
            let href = xml_unescape(&after[..end]);
            let Some(tag_close) = after[end..].find('>') else {
                break;
            };
            let (inner, after_anchor) = after[end + tag_close + 1..]
                .split_once("</a>")
                .unwrap_or((&after[end + tag_close + 1..], ""));
            let inner_len = visible_chars(inner);
            if let Some(target) = classify_href(&href) {
                // Extents clamp to at least one page unit: the hit test
                // demands positive overlap area, so a zero-width or
                // zero-height run would otherwise be unclickable forever.
                let (x0, x1) = if total == 0 {
                    (left, left + w.max(1))
                } else {
                    let at = |chars: usize| {
                        left + (w as f64 * chars.min(total) as f64 / total as f64).round() as u32
                    };
                    // The forced one-unit width of an empty anchor is carved
                    // out of the run (x0 shifts left at the right edge), so
                    // no slice ever extends past the run into its neighbour.
                    let run_right = left + w.max(1);
                    let x0 = at(pre).min(run_right - 1);
                    (x0, at(pre + inner_len).clamp(x0 + 1, run_right))
                };
                out.links.push(PdfLink {
                    rect: (x0, top, x1, top + h.max(1)),
                    target,
                });
            }
            pre += inner_len;
            rest = after_anchor;
        }
    }
    found.then_some(out)
}

/// Visible character count of a pdf2xml text-run fragment: markup tags
/// contribute nothing and an escaped entity counts as the one character it
/// stands for. Only a well-formed entity (`&` + a short alphanumeric/`#`
/// name + `;`) collapses; a bare `&` counts as itself - swallowing to some
/// distant `;` (or the end) would make the counts on the two sides of an
/// anchor disagree and push a link's slice past its own run rect.
fn visible_chars(s: &str) -> usize {
    let mut n = 0;
    let mut in_tag = false;
    let mut it = s.char_indices();
    while let Some((i, c)) = it.next() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            '&' if !in_tag => {
                let rest = &s[i + 1..];
                if let Some(semi) = rest.find(';')
                    && (1..=8).contains(&semi)
                    && rest[..semi]
                        .chars()
                        .all(|c| c == '#' || c.is_ascii_alphanumeric())
                {
                    // The name is ASCII (just checked), so chars == bytes.
                    for _ in 0..=semi {
                        it.next();
                    }
                }
                n += 1;
            }
            _ if !in_tag => n += 1,
            _ => {}
        }
    }
    n
}

/// `name="value"` attribute lookup on a single tag line. Values poppler emits
/// here are plain integers.
fn attr_u32(tag: &str, name: &str) -> Option<u32> {
    let pat = format!("{name}=\"");
    let start = tag.find(&pat)? + pat.len();
    let rest = &tag[start..];
    rest[..rest.find('"')?].parse().ok()
}

/// External hrefs open through the OS - but a document-supplied URI is
/// untrusted input, so only web and mail schemes qualify (`file://` or a
/// registered custom scheme would be a one-click app launch from a click on
/// what looks like plain text). Internal page links come out as
/// `docname.html#page`. Anything else (a relative file link) is dropped.
fn classify_href(href: &str) -> Option<LinkTarget> {
    let lower = href.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:")
    {
        return Some(LinkTarget::Url(href.to_string()));
    }
    if lower.contains("://") {
        return None;
    }
    let (_, frag) = href.rsplit_once('#')?;
    frag.parse().ok().map(LinkTarget::Page)
}

/// The five entities pdftohtml escapes in attribute values.
fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Rasteriser preference order:
/// 1. `pdftoppm` (poppler) — supports page selection, cross-platform when
///    poppler-utils is installed (`brew install poppler`, `apt install
///    poppler-utils`).
/// 2. `sips` — macOS built-in. Page 1 only; used as a fallback so the
///    feature works out of the box on every Mac without external deps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PdfBackend {
    PdftoppmCli,
    SipsCli,
}

pub fn detect_backend() -> Option<PdfBackend> {
    if which("pdftoppm").is_some() {
        return Some(PdfBackend::PdftoppmCli);
    }
    if cfg!(target_os = "macos") && which("sips").is_some() {
        return Some(PdfBackend::SipsCli);
    }
    None
}

fn which(cmd: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(cmd);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Total pages in `pdf`. Returns None when no detector is available so the
/// caller renders page 1 with no navigation rather than refusing the file.
pub fn detect_page_count(pdf: &Path) -> Option<u32> {
    if let Some(n) = page_count_via_pdfinfo(pdf) {
        return Some(n);
    }
    if cfg!(target_os = "macos")
        && let Some(n) = page_count_via_mdls(pdf)
    {
        return Some(n);
    }
    None
}

fn page_count_via_pdfinfo(pdf: &Path) -> Option<u32> {
    which("pdfinfo")?;
    let out = Command::new("pdfinfo").arg(pdf).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    parse_pdfinfo_pages(&s)
}

pub fn parse_pdfinfo_pages(out: &str) -> Option<u32> {
    for line in out.lines() {
        if let Some(rest) = line.strip_prefix("Pages:")
            && let Ok(n) = rest.trim().parse::<u32>()
        {
            return Some(n);
        }
    }
    None
}

fn page_count_via_mdls(pdf: &Path) -> Option<u32> {
    let out = Command::new("mdls")
        .args(["-raw", "-name", "kMDItemNumberOfPages"])
        .arg(pdf)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    s.trim().parse::<u32>().ok()
}

/// Rasterise `page` (1-based) of `pdf` to PNG bytes at ~144 DPI. Returns
/// the encoded PNG. The caller is responsible for cleaning up no temp
/// files — every backend writes to a per-call temp path that we delete
/// before returning.
pub fn rasterize_page(pdf: &Path, page: u32, backend: PdfBackend) -> std::io::Result<Vec<u8>> {
    match backend {
        PdfBackend::PdftoppmCli => rasterize_with_pdftoppm(pdf, page),
        PdfBackend::SipsCli => {
            if page != 1 {
                return Err(std::io::Error::other("sips backend can only render page 1"));
            }
            rasterize_with_sips(pdf)
        }
    }
}

fn rasterize_with_pdftoppm(pdf: &Path, page: u32) -> std::io::Result<Vec<u8>> {
    let dir = unique_temp_dir("croft-pdf")?;
    let _guard = TempDirGuard(dir.clone());
    let prefix = dir.join("page");
    // .output(), never .status(): the child must not inherit croft's TTY.
    // A half-written PDF (pdflatex rewriting the open file) made pdftoppm
    // spray "Syntax Error: Couldn't find trailer dictionary" over the UI.
    let out = Command::new("pdftoppm")
        .arg("-f")
        .arg(page.to_string())
        .arg("-l")
        .arg(page.to_string())
        .arg("-r")
        .arg("144")
        .args(["-png", "-singlefile"])
        .arg(pdf)
        .arg(&prefix)
        .output()?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(std::io::Error::other(format!(
            "pdftoppm exited with {}: {}",
            out.status,
            stderr.trim()
        )));
    }
    let png_path = prefix.with_extension("png");
    std::fs::read(&png_path)
}

fn rasterize_with_sips(pdf: &Path) -> std::io::Result<Vec<u8>> {
    let dir = unique_temp_dir("croft-pdf")?;
    let _guard = TempDirGuard(dir.clone());
    let out = dir.join("page.png");
    let status = Command::new("sips")
        .args(["-s", "format", "png"])
        .arg(pdf)
        .arg("--out")
        .arg(&out)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;
    if !status.success() {
        return Err(std::io::Error::other(format!("sips exited with {status}")));
    }
    std::fs::read(&out)
}

fn unique_temp_dir(stem: &str) -> std::io::Result<PathBuf> {
    // pid + a process-wide counter, never a clock reading: two renders that
    // landed in the same nanosecond bucket used to pick the same directory,
    // and the second one's `remove_dir_all` below deleted the first one's
    // output from under `pdftoppm` - a page turn that silently did nothing.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let pid = std::process::id();
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // Under croft's own cache dir, never the system temp dir: /tmp is
    // world-writable on Linux, so a same-named leftover owned by another
    // user would make the remove_dir_all below fail and the render with it.
    let base = crate::session_state::dirs_cache_croft().join("tmp");
    std::fs::create_dir_all(&base)?;
    let path = base.join(format!("{stem}-{pid}-{seq}"));
    if path.exists() {
        std::fs::remove_dir_all(&path)?;
    }
    std::fs::create_dir_all(&path)?;
    Ok(path)
}

struct TempDirGuard(PathBuf);

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two renders in flight at once must never share a scratch directory:
    /// the loser's `remove_dir_all` used to delete the winner's PNG before it
    /// was read, and the page turn silently did nothing.
    #[test]
    fn concurrent_scratch_dirs_are_distinct() {
        let dirs: Vec<_> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..16)
                .map(|_| s.spawn(|| unique_temp_dir("croft-pdf-test").unwrap()))
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let unique: std::collections::HashSet<_> = dirs.iter().collect();
        assert_eq!(unique.len(), dirs.len(), "every render needs its own dir");
        for d in dirs {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    /// pdftoppm's stderr must be captured into the returned error, never
    /// inherited: with a half-written PDF (pdflatex rewriting the open file)
    /// the child used to spray "Syntax Error: Couldn't find trailer
    /// dictionary" straight onto croft's TTY, corrupting the whole UI.
    #[test]
    fn pdftoppm_failure_captures_stderr_instead_of_writing_to_tty() {
        if which("pdftoppm").is_none() {
            eprintln!("skipping: pdftoppm not installed");
            return;
        }
        let dir = unique_temp_dir("croft-pdf-test-stderr").unwrap();
        let truncated = dir.join("half-written.pdf");
        std::fs::write(&truncated, b"%PDF-1.7\ntruncated mid-write, no xref").unwrap();
        let err = rasterize_with_pdftoppm(&truncated, 1).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Syntax Error") || msg.contains("trailer"),
            "error must carry the child's stderr, got: {msg}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Real pdftohtml -xml shape: link runs are `<a href>` inside a `<text>`
    /// element carrying the rect; external hrefs keep their scheme, internal
    /// ones are `docname.html#page`; `&amp;` needs unescaping; the requested
    /// page is selected by its `number` attribute.
    #[test]
    fn parses_links_from_pdf2xml_output() {
        let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<pdf2xml producer="poppler" version="25.04.0">
 link to page 2 <page number="1" position="absolute" top="0" left="0" height="1188" width="918">
<text top="192" left="223" width="55" height="13" font="0">Jump to</text>
<text top="192" left="283" width="65" height="13" font="1"><a href="ilink.html#2">the target</a></text>
</page>
<page number="2" position="absolute" top="0" left="0" height="1188" width="918">
<text top="192" left="511" width="58" height="13" font="3"><a href="https://a.b/c?d=1&amp;e=2">amp link</a></text>
<text top="300" left="100" width="90" height="12" font="2"><a href="mailto:x@y.z">mailto:x@y.z</a></text>
<text top="500" left="100" width="90" height="12" font="0">plain text</text>
</page>
</pdf2xml>
"##;
        let p1 = parse_pdf2xml_links(xml, 1).unwrap();
        assert_eq!((p1.page_w, p1.page_h), (918, 1188));
        assert_eq!(
            p1.links,
            vec![PdfLink {
                rect: (283, 192, 348, 205),
                target: LinkTarget::Page(2),
            }]
        );
        let p2 = parse_pdf2xml_links(xml, 2).unwrap();
        assert_eq!(
            p2.links,
            vec![
                PdfLink {
                    rect: (511, 192, 569, 205),
                    target: LinkTarget::Url(String::from("https://a.b/c?d=1&e=2")),
                },
                PdfLink {
                    rect: (100, 300, 190, 312),
                    target: LinkTarget::Url(String::from("mailto:x@y.z")),
                },
            ]
        );
        assert!(
            parse_pdf2xml_links(xml, 3).is_none(),
            "a page absent from the output has no links"
        );
    }

    /// poppler often wraps only part of a text run in the `<a>`: the link
    /// rect must cover just the linked words (proportionally by character),
    /// not the whole run, or clicking plain text next to a link opens it.
    /// Real shape from a published PDF where the link text is ") is another "
    /// (13 of the run's 28 visible characters).
    #[test]
    fn a_partial_run_link_covers_only_its_own_words() {
        let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<pdf2xml producer="poppler" version="25.04.0">
<page number="1" position="absolute" top="0" left="0" height="1188" width="918">
<text top="700" left="514" width="196" height="21" font="3"><a href="https://pycoders.com">) is another </a>popular weekly </text>
</page>
</pdf2xml>
"##;
        let p = parse_pdf2xml_links(xml, 1).unwrap();
        assert_eq!(p.links.len(), 1);
        let (x0, y0, x1, y1) = p.links[0].rect;
        assert_eq!((x0, y0, y1), (514, 700, 721));
        // 13/28 of the 196-wide run is 91: the rect ends near 605, well
        // short of the run's right edge at 710 where "popular weekly" sits.
        assert!(
            (600..=610).contains(&x1),
            "link rect must stop at the linked words, got x1={x1}"
        );
    }

    /// Adjacent TOC lines produce thin rects that genuinely overlap each
    /// other, and one coarse terminal cell can straddle two of them. The
    /// click must resolve to the link covering most of the cell, not the
    /// first overlapping one in document order.
    #[test]
    fn overlapping_toc_rects_resolve_to_the_most_covered_link() {
        let links = PageLinks {
            page_w: 1000,
            page_h: 1188,
            links: vec![
                PdfLink {
                    rect: (100, 189, 400, 212),
                    target: LinkTarget::Page(509),
                },
                PdfLink {
                    rect: (100, 208, 400, 231),
                    target: LinkTarget::Page(511),
                },
            ],
        };
        // A cell row covering page-y 203.7..237.6 (a 35-row canvas over a
        // 1188-tall page): 8 units of the first rect, 23 of the second.
        let hit = links
            .link_at((0.15, 203.7 / 1188.0, 0.30, 237.6 / 1188.0))
            .expect("the cell overlaps both rects");
        assert_eq!(hit.target, LinkTarget::Page(511));
    }

    /// A document-supplied href is untrusted input: only web and mail links
    /// may reach the OS opener. `file://` (one-click app launch on macOS)
    /// and arbitrary registered schemes must be dropped, not opened.
    #[test]
    fn only_web_and_mail_hrefs_open_externally() {
        assert_eq!(
            classify_href("file:///System/Applications/Calculator.app"),
            None
        );
        assert_eq!(classify_href("vscode://extension/whatever"), None);
        assert_eq!(classify_href("ssh://root@host"), None);
        assert_eq!(
            classify_href("https://a.b/c"),
            Some(LinkTarget::Url(String::from("https://a.b/c")))
        );
        assert_eq!(
            classify_href("HTTPS://A.B/c"),
            Some(LinkTarget::Url(String::from("HTTPS://A.B/c")))
        );
        assert_eq!(
            classify_href("mailto:x@y.z"),
            Some(LinkTarget::Url(String::from("mailto:x@y.z")))
        );
    }

    /// A degenerate run (`height="0"` or `width="0"`, which the parser must
    /// tolerate even if poppler is not known to emit it) still yields a rect
    /// the positive-area hit test can match: extents clamp to at least one
    /// page unit, or the link is silently unclickable forever.
    #[test]
    fn a_zero_extent_run_still_yields_a_clickable_rect() {
        let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<pdf2xml producer="poppler" version="25.04.0">
<page number="1" position="absolute" top="0" left="0" height="1000" width="1000">
<text top="100" left="50" width="120" height="0"><a href="https://a.b/">flat</a></text>
<text top="300" left="50" width="0" height="12"><a href="https://c.d/"></a></text>
</page>
</pdf2xml>
"##;
        let p = parse_pdf2xml_links(xml, 1).unwrap();
        assert_eq!(p.links.len(), 2);
        for l in &p.links {
            let (x0, y0, x1, y1) = l.rect;
            assert!(x1 > x0 && y1 > y0, "degenerate rect survived: {:?}", l.rect);
        }
        // And a cell over each degenerate run actually hits it.
        assert!(p.link_at((0.05, 0.099, 0.18, 0.102)).is_some());
        assert!(p.link_at((0.049, 0.30, 0.052, 0.312)).is_some());
    }

    /// An empty anchor sitting at the right edge of a run with text: the
    /// forced one-unit width must be carved out of the run, not appended
    /// past its edge where it would make the neighbouring run clickable.
    #[test]
    fn an_empty_anchor_at_the_run_edge_stays_inside_the_run() {
        let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<pdf2xml producer="poppler" version="25.04.0">
<page number="1" position="absolute" top="0" left="0" height="1000" width="1000">
<text top="100" left="200" width="120" height="12">text<a href="https://a.b/"></a></text>
</page>
</pdf2xml>
"##;
        let p = parse_pdf2xml_links(xml, 1).unwrap();
        assert_eq!(p.links.len(), 1);
        let (x0, _, x1, _) = p.links[0].rect;
        assert!(
            x0 >= 200 && x1 <= 320 && x0 < x1,
            "the forced width must stay inside the run rect (200..320), got {x0}..{x1}"
        );
    }

    /// A bare `&` (no terminating `;`) must count as a plain character, not
    /// swallow the rest of the fragment as a half-open entity: the counts on
    /// the two sides of the anchor would disagree and the link's slice could
    /// extend past its own run rect, making unrelated text clickable.
    #[test]
    fn an_unterminated_entity_cannot_push_a_link_past_its_run() {
        let xml = r##"<?xml version="1.0" encoding="UTF-8"?>
<pdf2xml producer="poppler" version="25.04.0">
<page number="1" position="absolute" top="0" left="0" height="1000" width="1000">
<text top="100" left="200" width="120" height="12">a & b<a href="https://a.b/">L</a></text>
</page>
</pdf2xml>
"##;
        let p = parse_pdf2xml_links(xml, 1).unwrap();
        assert_eq!(p.links.len(), 1);
        let (x0, _, x1, _) = p.links[0].rect;
        assert!(
            x0 >= 200 && x1 <= 320 && x0 < x1,
            "the link slice must stay inside its run rect (200..320), got {x0}..{x1}"
        );
        // Terminated entities still count as the single character they
        // stand for: "&amp; " is 2 visible chars, so the anchor's slice
        // starts past the midpoint of this 6-char run, not at its left edge.
        let ent = r##"<?xml version="1.0" encoding="UTF-8"?>
<pdf2xml producer="poppler" version="25.04.0">
<page number="1" position="absolute" top="0" left="0" height="1000" width="1000">
<text top="100" left="0" width="600" height="12">&amp; ab<a href="https://a.b/">cd</a></text>
</page>
</pdf2xml>
"##;
        let p = parse_pdf2xml_links(ent, 1).unwrap();
        assert_eq!(p.links[0].rect.0, 400, "4 of 6 chars precede the anchor");
    }

    /// The whole cell area is hit-tested, so a click one row under a thin
    /// text-line rect still lands: any overlap counts, containment is not
    /// required. Misses stay misses.
    #[test]
    fn link_at_matches_on_overlap_not_containment() {
        let links = PageLinks {
            page_w: 1000,
            page_h: 1000,
            links: vec![PdfLink {
                rect: (100, 100, 200, 113),
                target: LinkTarget::Page(2),
            }],
        };
        // A cell straddling the rect's bottom edge (y 0.110..0.130) hits.
        assert!(links.link_at((0.15, 0.110, 0.16, 0.130)).is_some());
        // A cell fully past the rect misses.
        assert!(links.link_at((0.15, 0.120, 0.16, 0.140)).is_none());
        // A cell left of the rect misses.
        assert!(links.link_at((0.05, 0.105, 0.09, 0.110)).is_none());
    }

    #[test]
    fn parses_page_count_from_pdfinfo_output() {
        let sample = "\
Title:          \nAuthor:         \nCreator:        \nProducer:       \n\
Pages:          12\nEncrypted:      no\nPage size:      612 x 792 pts\n";
        assert_eq!(parse_pdfinfo_pages(sample), Some(12));
    }

    #[test]
    fn pdfinfo_returns_none_when_pages_line_missing() {
        let sample = "Title:  Untitled\nAuthor:  Anon\n";
        assert!(parse_pdfinfo_pages(sample).is_none());
    }

    #[test]
    fn pdfinfo_handles_extra_whitespace() {
        let sample = "Pages:    7\n";
        assert_eq!(parse_pdfinfo_pages(sample), Some(7));
    }
}
