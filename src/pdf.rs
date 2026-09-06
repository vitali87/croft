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
// Unbounded (#493): `pdfinfo` and `mdls` below each run with `.output()` on
// the same frame-loop open path as the render, just before it. The
// follow-up that moves the open off the frame loop covers them; until then
// the pdftoppm runner is the shape to reuse.
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

/// How long one page render may take before the renderer is killed and the
/// open reports a failure instead of parking the frame loop (#493).
const PDF_RENDER_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);

/// How long a failed render's stderr is given to finish arriving after the
/// exit was observed (#493). The settle poll reads it once it has been
/// quiet for ~60 ms of ticks (≈85 ms of wall clock with per-tick overhead),
/// so this is sized to leave that early exit real headroom rather than
/// being the exit itself; the worst case, a renderer that keeps writing,
/// pays the whole grace once, on a path that has already failed.
const STDERR_SETTLE_GRACE: std::time::Duration = std::time::Duration::from_millis(200);

/// Test-only: the next `rasterize_page` call for exactly this (path, page)
/// fails once, simulating a transient rasteriser failure (a spawn refused
/// under load, an OOM-killed child). Keyed on the full path so parallel
/// tests rendering the same page number of their own tempdir files never
/// see each other's injection.
#[cfg(test)]
pub static FAIL_RASTERIZE_ONCE_FOR_TEST: std::sync::Mutex<Option<(PathBuf, u32)>> =
    std::sync::Mutex::new(None);

/// Rasterise `page` (1-based) of `pdf` to PNG bytes at ~144 DPI. Returns
/// the encoded PNG. The caller is responsible for cleaning up no temp
/// files — every backend writes to a per-call temp path that we delete
/// before returning.
pub fn rasterize_page(pdf: &Path, page: u32, backend: PdfBackend) -> std::io::Result<Vec<u8>> {
    #[cfg(test)]
    {
        let mut fail = FAIL_RASTERIZE_ONCE_FOR_TEST.lock().unwrap();
        if fail.as_ref() == Some(&(pdf.to_path_buf(), page)) {
            *fail = None;
            return Err(std::io::Error::other("transient failure injected by test"));
        }
    }
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
    run_pdftoppm(Path::new("pdftoppm"), pdf, page, PDF_RENDER_BUDGET)
}

/// Render `page` of `pdf` with the pdftoppm at `program` (a parameter so a
/// test can hand in a script without touching PATH), within `budget`.
///
/// Bounded (#493): this runs on the frame loop, so a renderer that hangs on
/// a malformed document or a stalled filesystem must not park the editor.
/// The child is spawned in its own process group and polled, and at the
/// deadline the whole group is killed and the child reaped, as the video
/// poster's ffmpeg run is bounded; `.output()` would block for as long as
/// the child cared to run. Stderr is drained on its own thread into a
/// shared buffer that is never joined on: a descendant the child left
/// behind (a helper it forked) keeps the pipe's write end open, and a join
/// would wait on that descendant past the budget, on the success path too.
/// The timeout error carries no stderr on purpose: a hung renderer's partial
/// spray is rarely the reason it hung, and settling it would add to an
/// overrun the user is already waiting through.
fn run_pdftoppm(
    program: &Path,
    pdf: &Path,
    page: u32,
    budget: std::time::Duration,
) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    use std::process::Stdio;
    use std::sync::{Arc, Mutex};
    let dir = unique_temp_dir("croft-pdf")?;
    let guard = TempDirGuard(dir.clone());
    let prefix = dir.join("page");
    // Null and piped, never inherited: the child must not touch croft's TTY.
    // A half-written PDF (pdflatex rewriting the open file) made pdftoppm
    // spray "Syntax Error: Couldn't find trailer dictionary" over the UI.
    let mut cmd = Command::new(program);
    cmd.arg("-f")
        .arg(page.to_string())
        .arg("-l")
        .arg(page.to_string())
        .arg("-r")
        .arg("144")
        .args(["-png", "-singlefile"])
        .arg(pdf)
        .arg(&prefix)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Its own group, so the kill at the deadline reaches a descendant
        // the renderer forked and not only the renderer itself.
        cmd.process_group(0);
    }
    let mut child = cmd.spawn()?;
    let stderr_pipe = child.stderr.take();
    let stderr_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&stderr_buf);
    // Published chunk by chunk, not at end of file: the pipe only reaches
    // end of file once every holder of its write end has gone, and a
    // descendant the renderer forked can hold it long after the renderer
    // itself has exited. A single publish at the end would then arrive
    // after the exit had already been reported, and the spray this pipe
    // exists to capture would land nowhere.
    std::thread::spawn(move || {
        let Some(mut err) = stderr_pipe else {
            return;
        };
        let mut chunk = [0u8; 4096];
        loop {
            match err.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if let Ok(mut slot) = sink.lock() {
                        slot.extend_from_slice(&chunk[..n]);
                    }
                }
            }
        }
    });
    let deadline = std::time::Instant::now() + budget;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(30));
            }
            Ok(None) => {
                kill_group(child, guard);
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!(
                        "{} took longer than {:.1} s rendering page {page}; killed",
                        program.display(),
                        budget.as_secs_f64()
                    ),
                ));
            }
            Err(e) => {
                kill_group(child, guard);
                return Err(e);
            }
        }
    };
    if !status.success() {
        let text = settle_stderr(&stderr_buf, STDERR_SETTLE_GRACE);
        return Err(std::io::Error::other(format!(
            "{} exited with {status}: {text}",
            program.display()
        )));
    }
    let png_path = prefix.with_extension("png");
    std::fs::read(&png_path)
}

/// The renderer's stderr as collected by `grace` after its exit was
/// observed. Its last words usually land a scheduling tick after the exit,
/// so the buffer is polled every 5 ms and read once it has been quiet for a
/// run of ticks long enough to bridge a scheduling gap between two writes
/// (a shorter run settled early and lost the second write), or once the
/// grace is spent, whichever comes first: the run is strictly shorter than
/// the grace, so a renderer whose stderr was complete at exit settles
/// early and the deadline stays the independent backstop. An empty buffer
/// counts as quiet, so a silent failure settles just as early. Bytes a
/// descendant writes after that are not waited for; a reader still blocked
/// on its copy of the pipe is left to finish on its own. The buffer is
/// append-only, which is what makes an unchanged trimmed length mean no new
/// content arrived.
fn settle_stderr(
    buf: &std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    grace: std::time::Duration,
) -> String {
    let tick = std::time::Duration::from_millis(5);
    let deadline = std::time::Instant::now() + grace;
    // Twelve ticks (60 ms) bridges the gaps seen between a renderer's
    // writes; capped below the grace so the two exits stay distinct.
    let ticks_in_grace = (grace.as_millis() / tick.as_millis()) as u32;
    let quiet_run = 12.min(ticks_in_grace.saturating_sub(1)).max(1);
    let mut seen = 0usize;
    let mut quiet = 0u32;
    loop {
        let text = buf
            .lock()
            .map(|b| String::from_utf8_lossy(&b).trim().to_string())
            .unwrap_or_default();
        quiet = if text.len() == seen { quiet + 1 } else { 0 };
        if quiet >= quiet_run || std::time::Instant::now() >= deadline {
            return text;
        }
        seen = text.len();
        std::thread::sleep(tick);
    }
}

/// Kill the renderer's whole process group on the budget's overrun (and on
/// a wait error); a renderer that exited on its own is not chased, whatever
/// it forked. The reap still happens, on its own thread: a renderer stalled
/// in uninterruptible I/O (a stuck filesystem is one of the hangs this
/// bound exists for) keeps SIGKILL pending until the kernel lets it go, and
/// a synchronous `wait` here would hold the frame loop for exactly as long
/// as the budget was meant to stop it being held. The scratch dir's guard
/// goes with the child, so the dir is removed only once the renderer is
/// reaped; deleting it under a child still stalled in I/O would orphan its
/// output. One such thread exists per overrun of a user-initiated render,
/// each blocked in `wait` for as long as its renderer stays stuck, which is
/// bounded by the user's page turns rather than by the frame rate. Off unix
/// only the child itself can be killed.
fn kill_group(mut child: std::process::Child, scratch: TempDirGuard) {
    #[cfg(unix)]
    {
        // SAFETY: a negative pid addresses the process group the child was
        // started in (`process_group(0)` above); the call only sends a signal.
        if let Ok(pid) = libc::pid_t::try_from(child.id()) {
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
    }
    std::thread::spawn(move || {
        // The non-unix path, and a no-op on unix after the group signal.
        let _ = child.kill();
        let _ = child.wait();
        drop(scratch);
    });
}

// Unbounded (#493): the follow-up that moves the open off the frame loop
// covers this backend too; the pdftoppm runner above is the shape to reuse.
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

    /// A wait handed to a spawned process, scaled for a loaded host as the
    /// repo's spawn waits are (CONTRIBUTING: never a fresh fixed constant).
    #[cfg(unix)]
    fn budget(base_ms: u64) -> std::time::Duration {
        crate::test_budget::spawn_budget(std::time::Duration::from_millis(base_ms))
    }

    /// A pdftoppm script that behaves as told: `sleep <secs>` to hang, or
    /// write a byte to the `<prefix>.png` it is asked for and exit 0.
    #[cfg(unix)]
    fn fake_pdftoppm(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("pdftoppm");
        std::fs::write(&script, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    /// #493: a renderer that hangs is killed at the budget and the open
    /// reports it, instead of parking the frame loop for as long as the
    /// child cares to run.
    #[cfg(unix)]
    #[test]
    fn a_pdftoppm_that_overruns_the_budget_is_killed_and_reported() {
        let tmp = tempfile::tempdir().unwrap();
        // The renderer sleeps well past the widest timeout the budget can
        // reach, so the elapsed ceiling below is met only by the kill.
        let timeout = budget(300);
        let script = fake_pdftoppm(
            tmp.path(),
            &format!("sleep {}", timeout.as_secs_f64() * 20.0),
        );
        let pdf = tmp.path().join("doc.pdf");
        std::fs::write(&pdf, b"%PDF-1.4\n").unwrap();
        let started = std::time::Instant::now();
        let err = run_pdftoppm(&script, &pdf, 1, timeout)
            .expect_err("an overrunning renderer is a failure, not a wait");
        let elapsed = started.elapsed();
        assert!(
            elapsed < timeout * 4,
            "the open returned at the budget, not when the child felt like it: {elapsed:?}"
        );
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut, "{err}");
        assert!(
            err.to_string().contains("longer than"),
            "the error names the budget: {err}"
        );
    }

    /// A renderer that exits promptly but leaves a descendant behind (a
    /// helper it forked, a stuck plugin) must not hold the call for as long
    /// as that descendant keeps the stderr pipe open: the budget bounds the
    /// whole call, not only the direct child's exit.
    #[cfg(unix)]
    #[test]
    fn a_pdftoppm_whose_descendant_lingers_does_not_hold_the_call() {
        let tmp = tempfile::tempdir().unwrap();
        let timeout = budget(500);
        // Comfortably past the cap at every load scale, so a call that
        // waited for the descendant would blow the assertion.
        let linger = timeout.as_secs_f64() * 6.0;
        let script = fake_pdftoppm(
            tmp.path(),
            &format!(r#"sleep {linger} & for last; do :; done; printf 'x' > "$last.png""#),
        );
        let pdf = tmp.path().join("doc.pdf");
        std::fs::write(&pdf, b"%PDF-1.4\n").unwrap();
        let started = std::time::Instant::now();
        let bytes = run_pdftoppm(&script, &pdf, 1, timeout)
            .expect("the page was written; a lingering descendant is not a failure");
        let elapsed = started.elapsed();
        assert_eq!(bytes, b"x");
        assert!(
            elapsed < std::time::Duration::from_secs_f64(linger / 2.0),
            "the call returned when the page was ready, not when the descendant exited: {elapsed:?}"
        );
    }

    /// A renderer that fails still has its stderr reported, through the
    /// drained pipe: the trailer-dictionary spray this module exists to keep
    /// off the TUI lands in the error instead.
    #[cfg(unix)]
    #[test]
    fn a_pdftoppm_that_fails_reports_its_stderr() {
        let tmp = tempfile::tempdir().unwrap();
        let script = fake_pdftoppm(tmp.path(), r#"echo "Syntax Error: no trailer" >&2; exit 1"#);
        let pdf = tmp.path().join("doc.pdf");
        std::fs::write(&pdf, b"%PDF-1.4\n").unwrap();
        let err = run_pdftoppm(&script, &pdf, 1, budget(5_000))
            .expect_err("a non-zero exit is a failure");
        let text = err.to_string();
        assert!(
            text.contains("exited with") && text.contains("Syntax Error: no trailer"),
            "the error carries the exit and the stderr: {text}"
        );
    }

    /// A failing renderer's stderr is reported even when a descendant it
    /// forked keeps the pipe open past its exit: the drain publishes what
    /// has arrived as it arrives, not only at end of file.
    #[cfg(unix)]
    #[test]
    fn a_failing_pdftoppm_with_a_lingering_descendant_still_reports_its_stderr() {
        let tmp = tempfile::tempdir().unwrap();
        let script = fake_pdftoppm(
            tmp.path(),
            r#"sleep 5 & echo "Syntax Error: no trailer" >&2; exit 1"#,
        );
        let pdf = tmp.path().join("doc.pdf");
        std::fs::write(&pdf, b"%PDF-1.4\n").unwrap();
        let err = run_pdftoppm(&script, &pdf, 1, budget(5_000))
            .expect_err("a non-zero exit is a failure");
        let text = err.to_string();
        assert!(
            text.contains("Syntax Error: no trailer"),
            "the stderr is reported despite the lingering descendant: {text}"
        );
    }

    /// A renderer whose stderr arrives in two bursts a scheduling gap apart,
    /// the second from a helper that writes just after the renderer exits,
    /// has both reported: the grace poll waits for a run of quiet ticks, not
    /// for the first lull.
    #[cfg(unix)]
    #[test]
    fn a_failing_pdftoppm_has_both_stderr_bursts_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let script = fake_pdftoppm(
            tmp.path(),
            // Past the 30 ms exit-poll tick, so the second burst genuinely
            // arrives after the exit is observed and the grace poll is what
            // collects it; a shorter sleep lands before the exit is even seen
            // and the test would pass with the grace poll gutted.
            r#"( sleep 0.05; echo "Syntax Error: second" >&2 ) & echo "Syntax Error: first" >&2; exit 1"#,
        );
        let pdf = tmp.path().join("doc.pdf");
        std::fs::write(&pdf, b"%PDF-1.4\n").unwrap();
        for _ in 0..5 {
            let err = run_pdftoppm(&script, &pdf, 1, budget(5_000))
                .expect_err("a non-zero exit is a failure");
            let text = err.to_string();
            assert!(
                text.contains("Syntax Error: first") && text.contains("Syntax Error: second"),
                "both bursts are reported: {text}"
            );
        }
    }

    /// The grace poll bridges a scheduling gap: a second line appended a
    /// few ticks after the first, long after a one-tick poll would have
    /// settled, is still collected. Pure threads, no spawned process, so
    /// the constants are the poll's own and not a spawn wait.
    #[test]
    fn the_grace_poll_collects_a_late_line_inside_its_quiet_run() {
        use std::sync::{Arc, Mutex};
        let buf = Arc::new(Mutex::new(b"Syntax Error: first\n".to_vec()));
        let late = Arc::clone(&buf);
        let writer = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(20));
            late.lock()
                .unwrap()
                .extend_from_slice(b"Syntax Error: second\n");
        });
        let text = settle_stderr(&buf, std::time::Duration::from_millis(500));
        writer.join().unwrap();
        assert!(
            text.contains("first") && text.contains("second"),
            "a line landing inside the quiet run is collected: {text:?}"
        );
    }

    /// A buffer that is already complete settles early: the poll must not
    /// charge every failing render the whole grace when nothing is coming.
    #[test]
    fn the_grace_poll_settles_a_complete_buffer_well_inside_the_grace() {
        use std::sync::{Arc, Mutex};
        // The grace the production call site passes, so lost headroom fails
        // here rather than in a page render; three quarters leaves the
        // ~85 ms quiet exit room to stretch under load.
        let grace = STDERR_SETTLE_GRACE;
        for seed in [&b"Syntax Error: complete\n"[..], &b""[..]] {
            let buf = Arc::new(Mutex::new(seed.to_vec()));
            let started = std::time::Instant::now();
            let _ = settle_stderr(&buf, grace);
            let took = started.elapsed();
            assert!(
                took < grace * 3 / 4,
                "a settled buffer does not pay the whole grace: {took:?} for seed {seed:?}"
            );
        }
    }

    /// A renderer that fails silently still names its exit status.
    #[cfg(unix)]
    #[test]
    fn a_silently_failing_pdftoppm_still_names_its_exit() {
        let tmp = tempfile::tempdir().unwrap();
        let script = fake_pdftoppm(tmp.path(), "exit 3");
        let pdf = tmp.path().join("doc.pdf");
        std::fs::write(&pdf, b"%PDF-1.4\n").unwrap();
        let err = run_pdftoppm(&script, &pdf, 1, budget(5_000))
            .expect_err("a non-zero exit is a failure");
        assert!(err.to_string().contains("exited with"), "{err}");
    }

    /// A hung renderer's descendants go down with it: the kill reaches the
    /// process group, so a helper the renderer forked cannot outlive the
    /// budget and keep running (or keep the pipe open) after the open failed.
    #[cfg(unix)]
    #[test]
    fn a_hung_pdftoppm_takes_its_descendants_down_with_it() {
        let tmp = tempfile::tempdir().unwrap();
        let marker = tmp.path().join("alive");
        // The forked helper outlives the renderer unless the GROUP is killed.
        // Its delay is derived from the scaled timeout, comfortably past the
        // widest value the budget can reach, so the helper is still pending
        // when the kill lands at every load scale rather than only at low
        // ones; the renderer's own sleep is longer still.
        let timeout = budget(300);
        let helper_delay = timeout.as_secs_f64() * 3.0;
        let script = fake_pdftoppm(
            tmp.path(),
            &format!(
                "(sleep {helper_delay}; touch '{}') & sleep {}",
                marker.display(),
                helper_delay * 10.0
            ),
        );
        let pdf = tmp.path().join("doc.pdf");
        std::fs::write(&pdf, b"%PDF-1.4\n").unwrap();
        run_pdftoppm(&script, &pdf, 1, timeout).expect_err("an overrun is a failure");
        std::thread::sleep(std::time::Duration::from_secs_f64(helper_delay * 2.0));
        assert!(
            !marker.exists(),
            "the forked helper survived the group kill"
        );
    }

    /// The control for the tests above: a renderer that finishes inside the
    /// budget hands back the bytes it wrote, budget or no budget.
    #[cfg(unix)]
    #[test]
    fn a_pdftoppm_that_finishes_in_time_hands_back_its_page() {
        let tmp = tempfile::tempdir().unwrap();
        // The prefix is the last argument; the renderer writes `<prefix>.png`.
        let script = fake_pdftoppm(
            tmp.path(),
            r#"for last; do :; done; printf 'x' > "$last.png""#,
        );
        let pdf = tmp.path().join("doc.pdf");
        std::fs::write(&pdf, b"%PDF-1.4\n").unwrap();
        let bytes = run_pdftoppm(&script, &pdf, 1, budget(5_000))
            .expect("a renderer that finishes is not a failure");
        assert_eq!(bytes, b"x");
    }

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
