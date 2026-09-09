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
// Bounded (#493). These run on the frame-loop open path, just before the
// render that `run_pdftoppm` already bounds, so an unbounded one parks the
// editor exactly as a hung renderer would - one call earlier. `croft view`
// makes that reachable from any pane.
//
// The budget is separate from and much smaller than the render's: reading a
// page count is a header parse, not a rasterisation, so a `pdfinfo` still
// running after this long is wedged rather than slow. Losing the count
// degrades to the single-page path; it does not fail the open.
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
    let program = which("pdfinfo")?;
    let (status, out) = run_bounded_stdout(&program, &[pdf.as_os_str()], PDF_INFO_BUDGET)?;
    // The guard the unbounded version had: stdout from a failed run is not a
    // page count, and every other subprocess in this file checks the same.
    if !status.success() {
        return None;
    }
    parse_pdfinfo_pages(&out)
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

/// The argv for the `mdls` page-count query, split out so it can be asserted
/// without spawning anything. A reordering or a misspelt attribute name is
/// otherwise invisible: `mdls` may be absent on the machine running the
/// tests, and a test that silently no-ops when its subject is missing is not
/// a test.
fn mdls_argv(pdf: &Path) -> [&std::ffi::OsStr; 4] {
    [
        std::ffi::OsStr::new("-raw"),
        std::ffi::OsStr::new("-name"),
        std::ffi::OsStr::new("kMDItemNumberOfPages"),
        pdf.as_os_str(),
    ]
}

fn page_count_via_mdls(pdf: &Path) -> Option<u32> {
    let (status, out) = run_bounded_stdout(
        std::path::Path::new("mdls"),
        &mdls_argv(pdf),
        PDF_INFO_BUDGET,
    )?;
    // mdls exits 0 and prints "(null)" for a file with no page count, and
    // exits non-zero on a missing one. Both fail the parse below today, but
    // the guard is what keeps that true.
    if !status.success() {
        return None;
    }
    out.trim().parse::<u32>().ok()
}

/// Run `program` with a deadline, returning its exit status and stdout, or
/// `None` on timeout or spawn failure (#493).
///
/// `dap/install.rs` has a `run_bounded` of the same shape. They are NOT
/// merged, and the reason is the exit status: that one discards it, which is
/// right for its caller (a version probe where any output at all is the
/// answer) and wrong here, because both page-count probes must reject stdout
/// from a failed run - `mdls` exits non-zero on a missing file and prints to
/// a stderr this deliberately drops. This one also caps the read and recovers
/// lossily from bad bytes. Folding them together would mean pushing this
/// caller's requirements onto a function that does not want them; if a third
/// caller wants THIS contract, that is the point to lift it into a shared
/// module.
///
/// The reader runs on its own thread and is never joined: a descendant the
/// child left behind keeps the pipe's write end open, and a join would then
/// wait on that descendant past the budget - on the success path too. This is
/// the same reasoning `run_pdftoppm` records for its stderr drain.
///
/// No process group, unlike `run_pdftoppm`, which kills `-pid` so a forked
/// descendant dies with it. Neither probe forks: `pdfinfo` is a single
/// poppler process, and `mdls` queries `mdworker` over Mach IPC rather than
/// spawning. A caller whose program does fork wants the group treatment
/// instead.
fn run_bounded_stdout(
    program: &Path,
    args: &[&std::ffi::OsStr],
    budget: std::time::Duration,
) -> Option<(std::process::ExitStatus, String)> {
    use std::io::Read;
    use std::process::Stdio;
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        // Null, never inherited: the child must not touch croft's TTY.
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let Some(mut stdout) = child.stdout.take() else {
        // Unreachable while stdout is piped, but returning through `?` here
        // would leave the child unreaped.
        let _ = child.kill();
        let _ = child.wait();
        return None;
    };
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        // Bytes, not a String: `read_to_string` fails outright on one bad
        // byte, losing a `Pages:` line that is itself valid. A title in
        // another encoding echoed into pdfinfo's header block does that.
        // Capped, because a program printing without end would otherwise
        // fill memory until the budget fires.
        let mut buf = Vec::new();
        let _ = stdout.by_ref().take(MAX_PROBE_STDOUT).read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    let out = rx.recv_timeout(budget).ok();
    let _ = child.kill();
    // Reap either way: on the timeout path the kill needs collecting, and on
    // the success path the child has exited but is still a zombie.
    let status = child.wait().ok()?;
    out.map(|bytes| (status, String::from_utf8_lossy(&bytes).into_owned()))
}

/// How long one page render may take before the renderer is killed and the
/// open reports a failure instead of parking the frame loop (#493).
const PDF_RENDER_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);

/// Deadline for the page-count probes (#493). Far below `PDF_RENDER_BUDGET`
/// on purpose: `pdfinfo` and `mdls` parse a header rather than rasterise, so
/// one still running after two seconds is wedged, not slow. Overrunning costs
/// only the page count, and the viewer degrades to the single-page path.
const PDF_INFO_BUDGET: std::time::Duration = std::time::Duration::from_secs(2);

/// Cap on a page-count probe's stdout. `pdfinfo` prints a header block of a
/// few hundred bytes and `mdls -raw` prints one number, so this is orders
/// above either; it exists so a program that prints without end cannot fill
/// memory while the budget runs down.
const MAX_PROBE_STDOUT: u64 = 1 << 20;

/// How long a failed render's stderr is given to finish arriving after the
/// exit was observed (#493). The settle poll reads it once it has been
/// quiet for ~60 ms of ticks (≈85 ms of wall clock on a quiet machine, more
/// under load, since each tick's sleep overshoots),
/// so this is sized to leave that early exit real headroom rather than
/// being the exit itself; the worst case, a renderer that keeps writing,
/// pays the whole grace once, on a path that has already failed.
const STDERR_SETTLE_GRACE: std::time::Duration = std::time::Duration::from_millis(200);

/// How the settle poll ended, recorded under `cfg(test)` only (#548).
///
/// This is what diagnosed #548, and it is kept for the next occurrence. Which
/// exit the poll took is the whole diagnosis, and the old failure message
/// could not say:
///
/// - ended on the QUIET RUN holding less than expected: the window closed
///   while the writer was still going. Measured under load at 69 ms against
///   the 60 ms run, holding only the first of two writes;
/// - ended on the DEADLINE: the grace ran out with the buffer still growing,
///   which is a different bug and a different fix.
///
/// Guessing between those two is what this exists to stop - and it earned its
/// keep, since two plausible diagnoses were wrong before the numbers arrived.
#[cfg(test)]
#[derive(Debug, Clone, Copy)]
struct SettleOutcome {
    waited: std::time::Duration,
    on_quiet: bool,
    bytes: usize,
}

#[cfg(test)]
thread_local! {
    /// Per THREAD, so tests running in parallel cannot read each other's.
    static LAST_SETTLE: std::cell::Cell<Option<SettleOutcome>> =
        const { std::cell::Cell::new(None) };
}

/// One poll of the renderer's stderr buffer.
const SETTLE_TICK: std::time::Duration = std::time::Duration::from_millis(5);

/// How often the wait loop checks for the renderer's exit and drains its
/// stderr. Five milliseconds rather than the thirty it polled before: the
/// same loop now empties the pipe, so the gap between drains is what a very
/// chatty renderer would have to fill to block, and 64 KB in 5 ms is an
/// order of magnitude beyond anything a page render produces.
const EXIT_POLL_TICK: std::time::Duration = std::time::Duration::from_millis(5);

/// Put the pipe in non-blocking mode so a read returns what is there rather
/// than waiting for more (#553).
///
/// A no-op off unix, where the fd type does not exist; the caller's reads
/// then behave as they always did.
fn set_nonblocking(pipe: Option<&std::process::ChildStderr>) {
    #[cfg(unix)]
    if let Some(pipe) = pipe {
        use std::os::fd::AsRawFd;
        // SAFETY: `pipe` owns the descriptor for as long as this borrow
        // lasts, and both calls only read and set its status flags.
        unsafe {
            let fd = pipe.as_raw_fd();
            let flags = libc::fcntl(fd, libc::F_GETFL);
            if flags >= 0 {
                libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
            }
        }
    }
    #[cfg(not(unix))]
    let _ = pipe;
}

/// Append everything the pipe has RIGHT NOW, and stop.
///
/// The point of the whole change (#553): after the renderer exits, whatever
/// it wrote is sitting in the pipe, and this takes all of it without waiting
/// on a clock or a thread. `WouldBlock` means "nothing more available", not
/// "nothing more coming", which is exactly the distinction a blocking read
/// cannot make and a descendant holding the write end would otherwise hide.
fn drain_available(pipe: Option<&mut std::process::ChildStderr>, into: &mut Vec<u8>) {
    let Some(pipe) = pipe else {
        return;
    };
    use std::io::Read;
    let mut chunk = [0u8; 4096];
    loop {
        match pipe.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => into.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
}

/// How many quiet ticks mean the renderer has stopped writing: twelve, 60 ms,
/// which bridges the gaps measured between a renderer's own writes.
///
/// Pinned by `the_settle_rule_bridges_a_gap_shorter_than_the_quiet_run` with
/// no clock involved, so cutting it fails a test rather than only showing up
/// as half an error message on a loaded machine.
const QUIET_RUN_TICKS: u32 = 12;

#[cfg(test)]
fn record_settle(outcome: SettleOutcome) {
    LAST_SETTLE.with(|c| c.set(Some(outcome)));
}

/// How the settle poll on this thread last ended, for a failure message.
/// `unix` too, because the only consumer is a `unix`-gated test.
#[cfg(all(test, unix))]
fn last_settle() -> String {
    match LAST_SETTLE.with(std::cell::Cell::get) {
        Some(o) => format!(
            "settle ended on {} after {:?} holding {} byte(s)",
            if o.on_quiet {
                "the QUIET RUN"
            } else {
                "the DEADLINE"
            },
            o.waited,
            o.bytes
        ),
        None => String::from("settle did not run"),
    }
}

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
    use std::process::Stdio;
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
    let mut stderr_pipe = child.stderr.take();
    // Non-blocking, and read from ONE place: this thread (#553).
    //
    // A reader thread published chunk by chunk, which was right about the
    // problem - the pipe only reaches end of file once every holder of its
    // write end has gone, and a descendant the renderer forked can hold it
    // long after the renderer itself exited - but it made the answer depend
    // on that thread being SCHEDULED. Measured on a loaded machine, the
    // renderer's second write was still in the pipe when the poll gave up,
    // and the user got half the error.
    //
    // Non-blocking removes the question. Everything the renderer itself
    // wrote is in the pipe by the time it exits, so a drain-until-EAGAIN
    // after the exit takes all of it, with no window to lose and nothing to
    // schedule.
    set_nonblocking(stderr_pipe.as_ref());
    let mut stderr_bytes: Vec<u8> = Vec::new();
    let deadline = std::time::Instant::now() + budget;
    let status = loop {
        // Drained every tick as well, so a renderer spraying more than a
        // pipeful cannot block on a full buffer waiting for us.
        drain_available(stderr_pipe.as_mut(), &mut stderr_bytes);
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(EXIT_POLL_TICK);
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
        // Everything the renderer itself wrote is readable NOW - it exited,
        // so it has written all it will. Take that first, unconditionally,
        // before any timing question is asked (#553).
        drain_available(stderr_pipe.as_mut(), &mut stderr_bytes);
        let text = settle_late_writes(stderr_pipe.as_mut(), &mut stderr_bytes, STDERR_SETTLE_GRACE);
        return Err(std::io::Error::other(format!(
            "{} exited with {status}: {text}",
            program.display()
        )));
    }
    let png_path = prefix.with_extension("png");
    std::fs::read(&png_path)
}

/// The renderer's last words, once its own output has already been taken.
///
/// By the time this runs, `drain_available` has emptied the pipe of
/// everything the renderer wrote, so this is only about a DESCENDANT that
/// outlived it and may still write - the case the pipe cannot signal, because
/// end of file waits for every holder of the write end (#553).
///
/// So the grace here is genuinely optional generosity rather than the thing
/// standing between the user and half an error message. It drains on each
/// tick and settles once the bytes stop arriving for a run of them, or once
/// the grace is spent. An empty buffer counts as quiet, so a renderer that
/// said nothing settles immediately.
fn settle_late_writes(
    mut pipe: Option<&mut std::process::ChildStderr>,
    into: &mut Vec<u8>,
    grace: std::time::Duration,
) -> String {
    let started = std::time::Instant::now();
    let deadline = started + grace;
    let (text, on_quiet) = settle_over(QUIET_RUN_TICKS, |_| {
        drain_available(pipe.as_deref_mut(), into);
        let text = String::from_utf8_lossy(into).trim().to_string();
        if std::time::Instant::now() >= deadline {
            return Sample::Last(text);
        }
        std::thread::sleep(SETTLE_TICK);
        Sample::More(text)
    });
    #[cfg(test)]
    record_settle(SettleOutcome {
        waited: started.elapsed(),
        on_quiet,
        bytes: text.len(),
    });
    // Read only under `cfg(test)`, by the recording above.
    let _ = on_quiet;
    text
}

/// What the poll saw on one tick, and whether another tick is allowed.
enum Sample {
    /// This reading, and there is time for another.
    More(String),
    /// This reading, and the budget is spent - stop whatever it says.
    Last(String),
}

/// The settle decision, driven by TICKS rather than by a clock (#548).
///
/// Extracted so the rule can be tested without a wall clock: the caller
/// supplies the readings and says when the budget is spent, and this decides
/// when the buffer has stopped growing. Returns the text it settled on, and
/// whether it settled because the buffer went quiet (rather than because the
/// budget ran out).
///
/// The clock lived inside this loop before, which meant the ONLY way to test
/// the rule was to race a real one - and the test that did so was the flake
/// this issue is about. Worse, the wall-clock test could not pin
/// `QUIET_RUN_TICKS` at all: the run was silently clamped to a function of the
/// grace, so the constant could be cut in half and every test still passed.
///
/// TERMINATION IS THE CALLER'S: this loops until the buffer goes quiet or the
/// sampler says `Last`, so a sampler that never says `Last` while the text
/// keeps changing never returns. Production guarantees it with the deadline
/// check; the test bounds its script.
fn settle_over(quiet_run: u32, mut sample: impl FnMut(u32) -> Sample) -> (String, bool) {
    let quiet_run = quiet_run.max(1);
    let mut seen = 0usize;
    let mut quiet = 0u32;
    let mut tick = 0u32;
    loop {
        let (text, last) = match sample(tick) {
            Sample::More(t) => (t, false),
            Sample::Last(t) => (t, true),
        };
        quiet = if text.len() == seen { quiet + 1 } else { 0 };
        if quiet >= quiet_run || last {
            // `&& !last` matters on the tick where both fire: the recorded
            // outcome exists to tell a quiet exit from a deadline one, so
            // labelling the boundary as quiet mislabels precisely the case
            // the record is for.
            return (text, quiet >= quiet_run && !last);
        }
        seen = text.len();
        tick += 1;
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
        let bytes = run_pdftoppm(&script, &pdf, 1, timeout).unwrap_or_else(|e| {
            // This failed once on CI and the `expect` said only that it should
            // not have, which left nothing to tell a timeout apart from a
            // missing page (#548). The error names which.
            panic!("the page was written; a lingering descendant is not a failure: {e}")
        });
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

    /// A renderer that fails has its stderr and its exit status in the
    /// error, rather than sprayed over the TUI.
    ///
    /// It no longer asserts the SECOND of two bursts: whether a write landing
    /// after the exit arrives inside the settle window is a race this test
    /// cannot win on a loaded machine (#548). That property is asked of the
    /// rule directly in
    /// [`the_settle_rule_bridges_a_gap_shorter_than_the_quiet_run`].
    #[cfg(unix)]
    #[test]
    fn a_failing_pdftoppm_reports_the_stderr_it_sprayed() {
        // End to end: a renderer that fails has its stderr in the error
        // rather than on the user's TUI. What this canNOT assert is the
        // second burst (#548): whether a write landing after the exit arrives
        // inside the settle window is a race against scheduling, measured at
        // 69 ms under load against a 60 ms window, and asserting it here made
        // the test fail for the machine's reasons rather than croft's.
        //
        // The property itself is not dropped - it moved to
        // `the_settle_rule_bridges_a_gap_shorter_than_the_quiet_run`, which
        // asks it of the rule directly, in ticks, with no clock to lose to.
        let tmp = tempfile::tempdir().unwrap();
        let script = fake_pdftoppm(
            tmp.path(),
            r#"( sleep 0.05; echo "Syntax Error: second" >&2 ) & echo "Syntax Error: first" >&2; exit 1"#,
        );
        let pdf = tmp.path().join("doc.pdf");
        std::fs::write(&pdf, b"%PDF-1.4\n").unwrap();
        // Once, not five times: the repeat existed to shake out the flaky
        // second-burst assertion, and what is left is deterministic.
        let err = run_pdftoppm(&script, &pdf, 1, budget(5_000))
            .expect_err("a non-zero exit is a failure");
        let text = err.to_string();
        assert!(
            text.contains("Syntax Error: first"),
            "the renderer's spray reaches the error: {text}\n[#548] {}",
            last_settle()
        );
        assert!(
            text.contains("exited with"),
            "and so does the exit status: {text}"
        );
    }

    /// The settle rule bridges a gap shorter than its quiet run and does not
    /// wait for one longer, which is what `QUIET_RUN_TICKS` means.
    ///
    /// Scripted by TICK INDEX, with no threads and no clock: the readings are
    /// stated rather than raced for, so the rule is what is under test and
    /// the host's scheduling cannot decide the result.
    #[test]
    fn the_settle_rule_bridges_a_gap_shorter_than_the_quiet_run() {
        // Ticks, not a clock (#548). The wall-clock version of this raced
        // scheduling and was the flake; worse, it could not pin
        // `QUIET_RUN_TICKS` at all, because the run was silently clamped to a
        // function of the grace - the constant could be halved and every test
        // still passed.
        //
        // A scripted buffer says exactly what arrived and when, so the
        // question this asks is the real one: is the production run long
        // enough to bridge the gap it exists to bridge?
        // The gaps are ABSOLUTE on purpose. Deriving them from
        // `QUIET_RUN_TICKS` reads better and pins nothing: the fixture then
        // moves with the constant and holds for any value it takes, which a
        // re-break caught - halving the constant left this test green.
        // Written as numbers, the pair straddles the production run, so the
        // constant cannot move far in either direction without this failing.
        // Measured tolerance: this accepts a run of 10 through 14, and 12
        // sits in the middle, so it is a bound with two ticks of slack rather
        // than an exact pin.
        let script = |gap: u32| {
            move |tick: u32| {
                let text = if tick < gap {
                    String::from("Syntax Error: first")
                } else {
                    String::from("Syntax Error: first\nSyntax Error: second")
                };
                // Far past the gap, so the QUIET RUN decides and the deadline
                // is never the reason.
                if tick > gap + 60 {
                    Sample::Last(text)
                } else {
                    Sample::More(text)
                }
            }
        };

        let (text, on_quiet) = settle_over(QUIET_RUN_TICKS, script(10));
        assert!(
            text.contains("second") && on_quiet,
            "a 10-tick gap is inside the production run and must be bridged, \
             settling on the run rather than the deadline: {text:?}"
        );

        let (text, _) = settle_over(QUIET_RUN_TICKS, script(15));
        assert!(
            !text.contains("second"),
            "a 15-tick gap is outside it and must NOT be waited for - without \
             this half the pair above is satisfied by a rule that waits \
             forever: {text:?}"
        );
    }

    /// Everything the RENDERER ITSELF wrote is reported, however loaded the
    /// machine is (#553).
    ///
    /// This is the case the user meets: poppler prints a generic first line
    /// and the specific cause after it, then exits. Both are in the pipe by
    /// the time it exits, so taking them cannot depend on a clock - and under
    /// the previous design it did, which is how an error arrived with its
    /// useful half missing and nothing to say so.
    ///
    /// The writes are separated by a sleep so they land as two reads rather
    /// than one, which is what made the old drain lose the second.
    #[cfg(unix)]
    #[test]
    fn every_line_the_renderer_wrote_before_exiting_is_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let script = fake_pdftoppm(
            tmp.path(),
            r#"echo "Syntax Error: couldn't find trailer dictionary" >&2; sleep 0.05; echo "Syntax Error: Couldn't read xref table" >&2; exit 1"#,
        );
        let pdf = tmp.path().join("doc.pdf");
        std::fs::write(&pdf, b"%PDF-1.4\n").unwrap();
        for _ in 0..5 {
            let err = run_pdftoppm(&script, &pdf, 1, budget(5_000))
                .expect_err("a non-zero exit is a failure");
            let text = err.to_string();
            assert!(
                text.contains("trailer dictionary") && text.contains("xref table"),
                "both of the renderer's own lines reach the error: {text}\n[#548] {}",
                last_settle()
            );
        }
    }

    /// A stream that has stopped settles on the QUIET RUN, not at the
    /// deadline: a failing render must not pay the whole grace when nothing
    /// more is coming.
    ///
    /// In ticks rather than wall clock, so the assertion is about the rule
    /// and not about how promptly this machine schedules a sleep.
    #[test]
    fn a_stream_that_has_stopped_settles_on_the_quiet_run() {
        for seed in ["Syntax Error: complete", ""] {
            let (text, on_quiet) = settle_over(QUIET_RUN_TICKS, |tick| {
                let text = seed.to_string();
                // A budget far beyond the run, so settling early is the only
                // way to return before it.
                if tick > QUIET_RUN_TICKS * 10 {
                    Sample::Last(text)
                } else {
                    Sample::More(text)
                }
            });
            assert!(
                on_quiet,
                "a stream that stopped settles on the run, not the deadline: {text:?}"
            );
            assert_eq!(text, seed.trim(), "and returns what had arrived");
        }
    }

    /// The deadline is the bound when the stream never goes quiet: bytes
    /// arriving faster than the run can complete are cut off with what
    /// arrived, rather than followed indefinitely.
    #[test]
    fn a_stream_that_never_pauses_is_cut_off_at_the_deadline() {
        let budget = QUIET_RUN_TICKS * 3;
        let (text, on_quiet) = settle_over(QUIET_RUN_TICKS, |tick| {
            // Grows on every tick, so the quiet run can never complete.
            let text = "x".repeat(tick as usize + 1);
            if tick >= budget {
                Sample::Last(text)
            } else {
                Sample::More(text)
            }
        });
        assert!(
            !on_quiet,
            "the deadline is the exit when nothing settles: {text:?}"
        );
        assert_eq!(
            text.len() as u32,
            budget + 1,
            "and it returns everything that had arrived by then"
        );
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

    /// The `mdls` argv, asserted without spawning `mdls` (#493). This is the
    /// one part of that probe a test can reach on any machine.
    #[test]
    fn the_mdls_argv_names_the_page_count_attribute_in_order() {
        let pdf = std::path::Path::new("/tmp/doc.pdf");
        let argv = mdls_argv(pdf);
        assert_eq!(argv[0], std::ffi::OsStr::new("-raw"));
        assert_eq!(argv[1], std::ffi::OsStr::new("-name"));
        assert_eq!(argv[2], std::ffi::OsStr::new("kMDItemNumberOfPages"));
        assert_eq!(argv[3], pdf.as_os_str(), "the file is the last argument");
    }

    /// The probe budget must stay below the render's. The doc comments argue
    /// the gap at length - a header parse still running that long is wedged,
    /// not slow - and nothing else would notice someone raising it.
    #[test]
    fn the_probe_budget_stays_under_the_render_budget() {
        assert!(
            PDF_INFO_BUDGET < PDF_RENDER_BUDGET,
            "a page-count probe must not be allowed to outlive a rasterisation"
        );
    }

    /// The budget must actually fire (#493). Without a test that HANGS a
    /// child, the bound is only shown on the path where the child exits on
    /// its own - which is the path that never needed bounding.
    ///
    /// `sleep 30` against a 150ms budget: the call must return `None` in
    /// well under the sleep, and the assertion on elapsed time is what
    /// distinguishes "the deadline fired" from "the child happened to be
    /// fast". Without the elapsed bound this test would also pass if
    /// `recv_timeout` were removed entirely.
    #[cfg(unix)]
    #[test]
    fn a_hanging_probe_is_killed_at_the_budget_not_waited_on() {
        // Scaled, not a literal: CONTRIBUTING's "Waiting on a spawned
        // process in a test" names exactly this case - a deadline handed to
        // a recv_timeout - and the test twelve lines up already uses this
        // helper. A fixed 150ms is a spawn plus a channel round trip on a
        // contended runner.
        let timeout = budget(150);
        let start = std::time::Instant::now();
        let out = run_bounded_stdout(
            std::path::Path::new("sleep"),
            &[std::ffi::OsStr::new("30")],
            timeout,
        );
        let elapsed = start.elapsed();
        assert!(out.is_none(), "a child that outruns the budget yields None");
        assert!(
            elapsed < timeout.mul_f64(20.0),
            "the deadline must fire rather than wait out the child: took {elapsed:?}"
        );
    }

    /// The paired presence case, so the refusal above cannot pass vacuously:
    /// a child that finishes inside the budget still returns its stdout.
    #[cfg(unix)]
    #[test]
    fn a_quick_probe_returns_its_output() {
        // `sh -c` with a sleep, not a bare `echo`: the interesting success
        // path is a child that writes and exits while the parent is INSIDE
        // recv_timeout, which an immediate exit never reaches.
        let (status, out) = run_bounded_stdout(
            std::path::Path::new("sh"),
            &[
                std::ffi::OsStr::new("-c"),
                std::ffi::OsStr::new("sleep 0.05; echo 'Pages:          7'"),
            ],
            budget(2000),
        )
        .expect("a child that exits in time yields its stdout");
        assert!(status.success(), "a clean exit is reported as success");
        assert_eq!(parse_pdfinfo_pages(&out), Some(7));
    }

    /// The status must reach the caller, because both probes gate on it and
    /// the unbounded version they replaced did too. Stdout that parses from a
    /// FAILED run is the case that would otherwise be taken as an answer.
    #[cfg(unix)]
    #[test]
    fn a_failing_probe_reports_its_status_even_with_parseable_output() {
        let (status, out) = run_bounded_stdout(
            std::path::Path::new("sh"),
            &[
                std::ffi::OsStr::new("-c"),
                std::ffi::OsStr::new("echo 'Pages:          9'; exit 1"),
            ],
            budget(2000),
        )
        .expect("output is still returned; the status is what says to ignore it");
        assert!(
            !status.success(),
            "a non-zero exit must be visible to the caller"
        );
        assert_eq!(
            parse_pdfinfo_pages(&out),
            Some(9),
            "the output really would have parsed, which is why the guard matters"
        );
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
