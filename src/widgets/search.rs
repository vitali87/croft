use globset::{Glob, GlobSet, GlobSetBuilder};
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{BinaryDetection, MmapChoice, Searcher, SearcherBuilder, Sink, SinkMatch};
use ignore::{WalkBuilder, WalkState};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Widget},
};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

const MAX_LINE_LEN: usize = 200;

/// Per-channel lift applied to the theme background to fill an idle input row,
/// mirroring VS Code's `input.background` sitting a touch lighter than the
/// panel. On Black (#000) this lands a neutral #141414; on Croft Dark
/// (#1e222e) a lighter navy. Single source of truth for both helpers below.
const FIELD_FILL_LIFT_IDLE: u8 = 0x14;
/// The focused input row lifts further still, standing in for VS Code's
/// `focusBorder` ring — in a cell grid we cannot draw a 1px sub-cell edge, so
/// the brighter fill plus the accent left-bar carry the focus signal.
const FIELD_FILL_LIFT_FOCUS: u8 = 0x22;
/// The dim left-edge bar an unfocused field wears, so every input reads as a
/// touch target even before it gains the accent bar on focus.
const FIELD_BAR_IDLE: Color = Color::Rgb(0x2b, 0x31, 0x3b);

/// Lift each channel of `(r, g, b)` by `d`, saturating. Used to derive the
/// input fill from the active theme background.
fn lift_bg((r, g, b): (u8, u8, u8), d: u8) -> Color {
    Color::Rgb(
        r.saturating_add(d),
        g.saturating_add(d),
        b.saturating_add(d),
    )
}

/// Draw a faint hairline rule from column `x0` to `x1` (inclusive) on row `y`,
/// used to separate the stacked Search inputs into distinct groups. A terminal
/// cannot gap by less than one whole cell, so the divider turns the separator
/// row into a deliberate rule instead of dead space. It is dimmer than, and
/// spans less width than, the full results separator so the two never read as
/// the same element.
fn draw_field_divider(buf: &mut Buffer, x0: u16, x1: u16, y: u16) {
    let style = Style::default().fg(FIELD_BAR_IDLE);
    for x in x0..=x1 {
        buf.set_string(x, y, "─", style);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchHit {
    pub path: PathBuf,
    /// 1-indexed line number, the way humans / editors talk about lines.
    pub line_no: usize,
    /// The matched line, with surrounding whitespace trimmed and length capped.
    pub line_text: String,
}

/// Mode toggles that drive `search_workspace`. Mirror VS Code's three
/// search input toggles, in the same left-to-right order:
///   - `case_sensitive` (Aa)
///   - `whole_word` (ab with underline)
///   - `use_regex` (.*)
///
/// All-false matches the original case-insensitive substring behaviour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct SearchOpts {
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub use_regex: bool,
}

fn build_matcher(query: &str, opts: SearchOpts) -> Option<RegexMatcher> {
    if query.is_empty() {
        return None;
    }
    let mut b = RegexMatcherBuilder::new();
    b.case_insensitive(!opts.case_sensitive);
    b.word(opts.whole_word);
    b.fixed_strings(!opts.use_regex);
    b.build(query).ok()
}

fn build_searcher() -> Searcher {
    SearcherBuilder::new()
        .line_number(true)
        .binary_detection(BinaryDetection::quit(b'\x00'))
        .memory_map(MmapChoice::never())
        .build()
}

/// Compiled `files to include` / `files to exclude` globs, mirroring VS
/// Code's two Search-sidebar glob inputs. Both inputs take a comma-separated
/// list of globs. A bare pattern with no path separator (e.g. `*.rs`) is
/// treated as match-at-any-depth (`**/*.rs`), exactly like VS Code, so users
/// don't have to remember to prefix `**/`.
#[derive(Clone, Debug, Default)]
pub struct PathFilter {
    include: Option<GlobSet>,
    exclude: Option<GlobSet>,
}

/// Build a `GlobSet` from a comma-separated glob list, returning `None` when
/// the trimmed input is empty (meaning "no filter"). Invalid individual globs
/// are skipped silently so one typo doesn't disable the whole filter.
fn build_glob_set(raw: &str) -> Option<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    let mut any = false;
    for part in raw.split(',') {
        let pat = part.trim();
        if pat.is_empty() {
            continue;
        }
        // VS Code convention: a pattern without a path separator matches at
        // any depth, so `*.rs` finds Rust files in every subdirectory.
        let expanded = if pat.contains('/') {
            pat.to_string()
        } else {
            format!("**/{pat}")
        };
        if let Ok(g) = Glob::new(&expanded) {
            builder.add(g);
            any = true;
        }
    }
    if !any {
        return None;
    }
    builder.build().ok()
}

impl PathFilter {
    /// Compile a filter from the raw include/exclude input strings. An empty
    /// (or all-whitespace) string disables that side of the filter.
    pub fn new(include: &str, exclude: &str) -> Self {
        Self {
            include: build_glob_set(include),
            exclude: build_glob_set(exclude),
        }
    }

    /// True when no include and no exclude globs are active, so the walker can
    /// skip the per-file relative-path work entirely.
    pub fn is_empty(&self) -> bool {
        self.include.is_none() && self.exclude.is_none()
    }

    /// Decide whether `path` (under `root`) should be searched. Matching is
    /// done against the path relative to `root` (falling back to the full path
    /// when it isn't under `root`). Include wins first: if an include set is
    /// present, the path must match it; then any exclude match drops the path.
    pub fn allows(&self, root: &Path, path: &Path) -> bool {
        let rel = path.strip_prefix(root).unwrap_or(path);
        if let Some(inc) = &self.include
            && !inc.is_match(rel)
        {
            return false;
        }
        if let Some(exc) = &self.exclude
            && exc.is_match(rel)
        {
            return false;
        }
        true
    }
}

struct HitSink<'a> {
    path: PathBuf,
    batch: &'a mut Vec<SearchHit>,
    cancel: Option<Arc<AtomicBool>>,
}

impl<'a> Sink for HitSink<'a> {
    type Error = std::io::Error;
    fn matched(
        &mut self,
        _searcher: &Searcher,
        mat: &SinkMatch<'_>,
    ) -> Result<bool, std::io::Error> {
        if let Some(c) = self.cancel.as_ref()
            && c.load(Ordering::Relaxed)
        {
            return Ok(false);
        }
        let line_no = mat.line_number().unwrap_or(0) as usize;
        let line_str = std::str::from_utf8(mat.bytes()).unwrap_or("");
        let stripped = line_str.trim_end_matches(['\r', '\n']);
        let trimmed = stripped.trim_start();
        let cut = if trimmed.chars().count() > MAX_LINE_LEN {
            let mut s: String = trimmed.chars().take(MAX_LINE_LEN).collect();
            s.push('…');
            s
        } else {
            trimmed.to_string()
        };
        self.batch.push(SearchHit {
            path: self.path.clone(),
            line_no,
            line_text: cut,
        });
        Ok(true)
    }
}

/// Stream every match for `query` under `root` into `on_batch`, one batch
/// per file. Honours `.gitignore` and skips binaries via NUL detection.
/// Walks in parallel using `WalkBuilder::build_parallel` and uses
/// `grep-searcher` + `grep-regex` (the same engine that powers ripgrep).
/// Set `cancel` to abort an in-flight scan.
pub fn search_workspace_streaming<F>(
    root: &Path,
    query: &str,
    opts: SearchOpts,
    cancel: &Arc<AtomicBool>,
    on_batch: F,
) where
    F: FnMut(Vec<SearchHit>) + Send + Clone,
{
    search_workspace_streaming_skipping(root, query, opts, cancel, &HashSet::new(), on_batch);
}

/// Like `search_workspace_streaming` but skips any walked file whose
/// canonical path is in `skip`. Used for dirty-aware search: files open in
/// unsaved editors are searched from their in-memory buffer instead, so the
/// stale on-disk copy must not also contribute (duplicate / outdated) hits.
pub fn search_workspace_streaming_skipping<F>(
    root: &Path,
    query: &str,
    opts: SearchOpts,
    cancel: &Arc<AtomicBool>,
    skip: &HashSet<PathBuf>,
    on_batch: F,
) where
    F: FnMut(Vec<SearchHit>) + Send + Clone,
{
    search_workspace_streaming_filtered(
        root,
        query,
        opts,
        cancel,
        skip,
        &PathFilter::default(),
        on_batch,
    );
}

/// Like `search_workspace_streaming_skipping` but also honours a `PathFilter`
/// (VS Code's "files to include" / "files to exclude" glob inputs). A file is
/// searched only when `filter.allows(root, path)` returns true.
pub fn search_workspace_streaming_filtered<F>(
    root: &Path,
    query: &str,
    opts: SearchOpts,
    cancel: &Arc<AtomicBool>,
    skip: &HashSet<PathBuf>,
    filter: &PathFilter,
    on_batch: F,
) where
    F: FnMut(Vec<SearchHit>) + Send + Clone,
{
    let q = query.trim();
    if q.is_empty() {
        return;
    }
    let Some(matcher) = build_matcher(q, opts) else {
        return;
    };
    let walker = WalkBuilder::new(root)
        .git_ignore(true)
        .hidden(true)
        .build_parallel();
    let root = root.to_path_buf();
    walker.run(|| {
        let matcher = matcher.clone();
        let cancel = cancel.clone();
        let skip = skip.clone();
        let filter = filter.clone();
        let root = root.clone();
        let mut on_batch = on_batch.clone();
        Box::new(move |entry| {
            if cancel.load(Ordering::Relaxed) {
                return WalkState::Quit;
            }
            let entry = match entry {
                Ok(e) => e,
                Err(_) => return WalkState::Continue,
            };
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                return WalkState::Continue;
            }
            let path = entry.path();
            // Apply the include/exclude glob filter before any IO on the file.
            if !filter.is_empty() && !filter.allows(&root, path) {
                return WalkState::Continue;
            }
            // Dirty files are searched from their in-memory buffer elsewhere;
            // skip the stale disk copy. Only pay the canonicalize syscall when
            // there is actually something to skip.
            if !skip.is_empty() && path.canonicalize().is_ok_and(|canon| skip.contains(&canon)) {
                return WalkState::Continue;
            }
            let mut batch: Vec<SearchHit> = Vec::new();
            {
                let mut sink = HitSink {
                    path: path.to_path_buf(),
                    batch: &mut batch,
                    cancel: Some(cancel.clone()),
                };
                let mut searcher = build_searcher();
                let _ = searcher.search_path(&matcher, path, &mut sink);
            }
            if !batch.is_empty() {
                on_batch(batch);
            }
            WalkState::Continue
        })
    });
}

/// Aggregating wrapper around `search_workspace_streaming`. Returns every
/// match - no arbitrary cap. Honours `.gitignore`. Order is non-deterministic
/// because the walker is parallel; callers that need ordering must sort.
pub fn search_workspace(root: &Path, query: &str, opts: SearchOpts) -> Vec<SearchHit> {
    let cancel = Arc::new(AtomicBool::new(false));
    let collected: Arc<std::sync::Mutex<Vec<SearchHit>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let collected_for_cb = collected.clone();
    search_workspace_streaming(root, query, opts, &cancel, move |batch| {
        if let Ok(mut g) = collected_for_cb.lock() {
            g.extend(batch);
        }
    });
    Arc::try_unwrap(collected)
        .map(|m| m.into_inner().unwrap_or_default())
        .unwrap_or_default()
}

/// Split `line` into `(segment, is_match)` runs against `needle` honouring
/// the supplied `opts`, so highlights in result rows / the editor stay
/// consistent with what `collect_matches_in_text` would actually match.
/// Concatenating the segments reproduces the original line byte-for-byte.
/// Empty needle, no match, or invalid regex all return the whole line as
/// a single non-match segment.
pub fn split_for_highlight(line: &str, needle: &str, opts: SearchOpts) -> Vec<(String, bool)> {
    if needle.is_empty() {
        return vec![(line.to_string(), false)];
    }
    if opts.use_regex {
        return split_for_highlight_regex(line, needle, opts);
    }
    let (haystack, search_for): (String, String) = if opts.case_sensitive {
        (line.to_string(), needle.to_string())
    } else {
        (line.to_lowercase(), needle.to_lowercase())
    };
    // If lowercasing changed the byte length (rare Unicode edge case) we
    // can't safely map `haystack` indices back into `line`, so bail out
    // without highlights.
    if haystack.len() != line.len() || search_for.is_empty() {
        return vec![(line.to_string(), false)];
    }
    let mut out: Vec<(String, bool)> = Vec::new();
    let mut last = 0usize;
    let mut start = 0usize;
    while let Some(rel) = haystack[start..].find(&search_for) {
        let abs = start + rel;
        let end = abs + search_for.len();
        if opts.whole_word && !is_whole_word_match(&haystack, abs, end) {
            start = end;
            continue;
        }
        if abs > last {
            out.push((line[last..abs].to_string(), false));
        }
        out.push((line[abs..end].to_string(), true));
        last = end;
        start = end;
    }
    if last < line.len() {
        out.push((line[last..].to_string(), false));
    }
    if out.is_empty() {
        out.push((line.to_string(), false));
    }
    out
}

fn split_for_highlight_regex(line: &str, needle: &str, opts: SearchOpts) -> Vec<(String, bool)> {
    let mut pattern = String::new();
    if !opts.case_sensitive {
        pattern.push_str("(?i)");
    }
    if opts.whole_word {
        pattern.push_str("\\b(?:");
        pattern.push_str(needle);
        pattern.push_str(")\\b");
    } else {
        pattern.push_str(needle);
    }
    let re = match regex::Regex::new(&pattern) {
        Ok(r) => r,
        Err(_) => return vec![(line.to_string(), false)],
    };
    let mut out: Vec<(String, bool)> = Vec::new();
    let mut last = 0usize;
    for m in re.find_iter(line) {
        if m.start() > last {
            out.push((line[last..m.start()].to_string(), false));
        }
        // Empty matches (`.*` against an empty position) would loop
        // forever; advance past them.
        if m.end() == m.start() {
            continue;
        }
        out.push((line[m.start()..m.end()].to_string(), true));
        last = m.end();
    }
    if last < line.len() {
        out.push((line[last..].to_string(), false));
    }
    if out.is_empty() {
        out.push((line.to_string(), false));
    }
    out
}

/// One unit of streamed output from the search worker. The query and opts
/// are echoed back on every event so the receiver can drop stale events
/// when the user has typed past or flipped a toggle since the scan started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchEvent {
    /// A batch of hits, one batch per file as the parallel walker finds them.
    Hits(String, SearchOpts, Vec<SearchHit>),
    /// Sentinel emitted exactly once per submitted request when the scan
    /// finishes (or short-circuits on an empty query). UI uses this to flip
    /// the "(searching…)" caption off.
    Done(String, SearchOpts),
}

/// Work submitted to the search worker. `Query` carries the query string and
/// the toggle state at the moment of submission, so the user can flip a toggle
/// and re-fire the same string with new opts. `SetRoot` rebinds the directory
/// the worker scans, so search always targets the Explorer's current root and
/// its subdirectories after a Make-root / open-folder, never the root captured
/// when the worker was spawned.
#[derive(Clone, Debug)]
pub enum SearchRequest {
    Query(String, SearchOpts),
    SetRoot(PathBuf),
    /// Snapshot of files open in unsaved (dirty) editors as `(path, content)`
    /// where `content` is the buffer's current text. The worker searches these
    /// in-memory copies and skips their stale disk versions, so an unsaved edit
    /// (e.g. a Rename Symbol) is findable immediately. Like `SetRoot`, this is
    /// state, not a query: it updates the snapshot used by the next scan.
    SetDirtyBuffers(Vec<(PathBuf, String)>),
    /// VS Code's "files to include" / "files to exclude" glob inputs, as raw
    /// comma-separated strings. Like `SetRoot`, this is state, not a query: it
    /// updates the filter the next scan applies. Empty strings clear the
    /// filter.
    SetFilter(String, String),
}

/// How long the user has to pause typing before the search fires. Each
/// fresh keystroke restarts the timer, so a fast typist running through
/// "cla" never triggers searches for "c" or "cl" - only the trailing
/// query gets scanned. This is the standard search-as-you-type pattern
/// (VS Code, GitHub, etc.). 200 ms is in the sweet spot: short enough
/// to feel responsive, long enough that bursts of typing are silent.
const SEARCH_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(200);

/// Background worker loop. Implements trailing-edge debounce: blocks on
/// `recv()` for the first input, then loops on `recv_timeout(200 ms)` -
/// each fresh keystroke restarts the quiet window, so the search fires
/// only after the user has actually paused. The scan runs on a dedicated
/// thread so a new keystroke arriving DURING a scan can cancel it via
/// the shared `Arc<AtomicBool>` and start fresh. Each scan emits per-file
/// `Hits` batches followed by exactly one `Done`, even when canceled,
/// so the receiver always sees a balanced event stream. The thread exits
/// cleanly when the channel closes.
pub fn search_worker_loop(
    mut root: PathBuf,
    rx: std::sync::mpsc::Receiver<SearchRequest>,
    tx: std::sync::mpsc::Sender<SearchEvent>,
) {
    use std::sync::mpsc::RecvTimeoutError;
    let mut current: Option<(std::thread::JoinHandle<()>, Arc<AtomicBool>)> = None;
    // Latest snapshot of unsaved editor buffers, refreshed by SetDirtyBuffers.
    let mut dirty: Vec<(PathBuf, String)> = Vec::new();
    // Latest include/exclude glob filter, refreshed by SetFilter.
    let mut filter = PathFilter::default();
    while let Ok(mut latest) = rx.recv() {
        // A new message landed: cancel the in-flight scan (if any) so its
        // worker thread bails out before the user pauses long enough to
        // trigger the next scan.
        if let Some((handle, cancel)) = current.take() {
            cancel.store(true, Ordering::Relaxed);
            let _ = handle.join();
        }
        // State updates (root rebind, dirty-buffer snapshot) are not queries
        // and do not debounce: apply and wait for the next message.
        match latest {
            SearchRequest::SetRoot(new_root) => {
                root = new_root;
                continue;
            }
            SearchRequest::SetDirtyBuffers(buffers) => {
                dirty = buffers;
                continue;
            }
            SearchRequest::SetFilter(ref include, ref exclude) => {
                filter = PathFilter::new(include, exclude);
                continue;
            }
            SearchRequest::Query(..) => {}
        }
        // Trailing-edge debounce: each new keystroke resets the timer.
        // The search fires only after `SEARCH_DEBOUNCE` of silence. A
        // root rebind or dirty-buffer snapshot arriving mid-debounce updates
        // the state without displacing the pending query.
        loop {
            match rx.recv_timeout(SEARCH_DEBOUNCE) {
                Ok(SearchRequest::SetRoot(new_root)) => root = new_root,
                Ok(SearchRequest::SetDirtyBuffers(buffers)) => dirty = buffers,
                Ok(SearchRequest::SetFilter(include, exclude)) => {
                    filter = PathFilter::new(&include, &exclude)
                }
                Ok(newer) => latest = newer,
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
        let SearchRequest::Query(query, opts) = latest else {
            continue;
        };
        if query.trim().is_empty() {
            if tx.send(SearchEvent::Done(query, opts)).is_err() {
                return;
            }
            continue;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_for_thread = cancel.clone();
        let tx_for_thread = tx.clone();
        let root_for_thread = root.clone();
        let query_for_thread = query.clone();
        let dirty_for_thread = dirty.clone();
        let filter_for_thread = filter.clone();
        let handle = std::thread::spawn(move || {
            let q_for_emit = query_for_thread.clone();
            let tx_for_emit = tx_for_thread.clone();
            // First, search the unsaved buffers in-memory and collect the set
            // of their canonical paths so the disk walk skips the stale copies.
            // Dirty buffers must respect the include/exclude filter too, so a
            // glob that excludes a file hides its unsaved hits as well.
            let mut skip: HashSet<PathBuf> = HashSet::new();
            let mut buf_hits: Vec<SearchHit> = Vec::new();
            for (path, content) in &dirty_for_thread {
                if let Ok(canon) = path.canonicalize() {
                    skip.insert(canon);
                }
                if !filter_for_thread.is_empty()
                    && !filter_for_thread.allows(&root_for_thread, path)
                {
                    continue;
                }
                collect_matches_in_text(path, content, &query_for_thread, opts, &mut buf_hits);
            }
            if !buf_hits.is_empty() {
                let _ = tx_for_emit.send(SearchEvent::Hits(q_for_emit.clone(), opts, buf_hits));
            }
            search_workspace_streaming_filtered(
                &root_for_thread,
                &query_for_thread,
                opts,
                &cancel_for_thread,
                &skip,
                &filter_for_thread,
                move |batch| {
                    let _ = tx_for_emit.send(SearchEvent::Hits(q_for_emit.clone(), opts, batch));
                },
            );
            let _ = tx_for_thread.send(SearchEvent::Done(query_for_thread, opts));
        });
        current = Some((handle, cancel));
    }
    if let Some((handle, cancel)) = current.take() {
        cancel.store(true, Ordering::Relaxed);
        let _ = handle.join();
    }
}

/// Per-text search helper. Wraps `grep-searcher` so the matching logic is
/// identical to the file-walking engine (same regex, line numbers, trimming).
/// Honours the three `SearchOpts` toggles. Used both by tests and by the
/// dirty-aware worker to search unsaved in-memory buffers. Regex compilation
/// failure for invalid patterns is silent: returns no matches.
pub fn collect_matches_in_text(
    path: &Path,
    content: &str,
    query: &str,
    opts: SearchOpts,
    out: &mut Vec<SearchHit>,
) {
    let q = query.trim();
    if q.is_empty() {
        return;
    }
    let Some(matcher) = build_matcher(q, opts) else {
        return;
    };
    let mut sink = HitSink {
        path: path.to_path_buf(),
        batch: out,
        cancel: None,
    };
    let mut searcher = SearcherBuilder::new()
        .line_number(true)
        .binary_detection(BinaryDetection::quit(b'\x00'))
        .build();
    let _ = searcher.search_slice(&matcher, content.as_bytes(), &mut sink);
}

/// Compile the search query + `SearchOpts` into a `regex::Regex`, mirroring
/// the pattern construction used by `split_for_highlight_regex` so replace
/// matches exactly what the highlighter shows. In literal (non-regex) mode the
/// query is escaped so metacharacters are matched verbatim. Returns `None` for
/// an empty query or an uncompilable pattern.
fn build_replace_regex(query: &str, opts: SearchOpts) -> Option<regex::Regex> {
    let q = query.trim();
    if q.is_empty() {
        return None;
    }
    let body = if opts.use_regex {
        q.to_string()
    } else {
        regex::escape(q)
    };
    let mut pattern = String::new();
    if !opts.case_sensitive {
        pattern.push_str("(?i)");
    }
    if opts.whole_word {
        pattern.push_str("\\b(?:");
        pattern.push_str(&body);
        pattern.push_str(")\\b");
    } else {
        pattern.push_str(&body);
    }
    regex::Regex::new(&pattern).ok()
}

/// Replace every match of `query` (honouring `opts`) in `content` with
/// `replacement`, returning `(new_content, count)`. In regex mode the
/// replacement honours `$1` / `${name}` capture references, like VS Code; in
/// literal mode the replacement is inserted verbatim (a `$` in the replacement
/// is escaped so it is not treated as a capture reference). Returns `None` when
/// the query is empty or the pattern can't compile, so callers leave the file
/// untouched.
pub fn replace_in_text(
    content: &str,
    query: &str,
    replacement: &str,
    opts: SearchOpts,
) -> Option<(String, usize)> {
    let re = build_replace_regex(query, opts)?;
    let count = re.find_iter(content).count();
    if count == 0 {
        return Some((content.to_string(), 0));
    }
    let out = if opts.use_regex {
        re.replace_all(content, replacement).into_owned()
    } else {
        // Literal replacement: escape `$` so the substituted text is inserted
        // verbatim rather than parsed as a capture reference.
        let escaped = replacement.replace('$', "$$");
        re.replace_all(content, escaped.as_str()).into_owned()
    };
    Some((out, count))
}

/// Replace every match in the file at `path` on disk, writing the result back
/// only when at least one replacement was made. Returns the number of matches
/// replaced, or `None` when the file can't be read, the pattern can't compile,
/// or the write fails. Preserves files with no matches untouched (returns 0).
pub fn replace_in_file(
    path: &Path,
    query: &str,
    replacement: &str,
    opts: SearchOpts,
) -> Option<usize> {
    let content = std::fs::read_to_string(path).ok()?;
    let (new_content, count) = replace_in_text(&content, query, replacement, opts)?;
    if count == 0 {
        return Some(0);
    }
    std::fs::write(path, new_content).ok()?;
    Some(count)
}

fn is_whole_word_match(haystack: &str, start: usize, end: usize) -> bool {
    let prev_ok = start == 0
        || haystack[..start]
            .chars()
            .last()
            .map(|c| !is_word_char(c))
            .unwrap_or(true);
    let next_ok = end >= haystack.len()
        || haystack[end..]
            .chars()
            .next()
            .map(|c| !is_word_char(c))
            .unwrap_or(true);
    prev_ok && next_ok
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Side-panel widget shown when the active sidebar view is "Search".
pub struct SearchPanel {
    pub query: String,
    pub hits: Vec<SearchHit>,
    pub selected: usize,
    pub scroll: usize,
    pub focused: bool,
    /// True under the Black theme: overpaint the focused outer border with the
    /// orange→green brand gradient instead of the legacy solid blue. Set by the
    /// app's focus/theme sync.
    pub focus_gradient: bool,
    /// Active color theme; drives the scrollbar track/thumb colors. Set by the
    /// app's theme sync.
    pub theme: crate::theme::Theme,
    pub last_inner: Rect,
    pub last_area: Rect,
    pub root: PathBuf,
    pub opts: SearchOpts,
    /// Per-toggle absolute screen column captured by the most recent render.
    /// Used by `App::handle_mouse` to map clicks on `Aa`, `ab`, `.*` into
    /// flag flips. Zero means the row was too narrow to render that toggle.
    pub toggle_case_x: u16,
    pub toggle_word_x: u16,
    pub toggle_regex_x: u16,
    pub toggle_y: u16,
    pub paste_button_x: u16,
    pub paste_button_y: u16,
    pub paste_button_w: u16,
    pub selection: Option<(usize, usize)>,
    /// Hit-test rect captured during the last render of the result-list
    /// scrollbar. Empty when there's no overflow. Used by the mouse
    /// handler to route track clicks / drags to `scroll_to_bar_y`.
    pub last_scrollbar: Rect,
    /// Render-time pointer cell, fed from `App::pointer_cell` each frame so a
    /// hovered (but unselected) result row can lift to the hover background.
    pub hover_pointer: Option<(u16, u16)>,

    // --- Replace + files-to-include/exclude (VS Code Search sidebar) ---
    /// Replacement text. Editable only while the Replace row is expanded.
    pub replace: String,
    /// "files to include" glob list (comma-separated), shown when the details
    /// (`...`) row is expanded.
    pub include: String,
    /// "files to exclude" glob list (comma-separated), shown when details is
    /// expanded.
    pub exclude: String,
    /// Whether the Replace row is expanded, toggled by the left chevron.
    pub replace_open: bool,
    /// Whether the include/exclude detail rows are expanded, toggled by `...`.
    pub details_open: bool,
    /// Which input field currently receives typed characters. Tab cycles
    /// through the visible fields; clicking an input focuses it directly.
    pub field: SearchField,
    /// Selection range within the currently focused field's text.
    pub field_selection: Option<(usize, usize)>,

    // --- Hit-test geometry captured during render (absolute screen coords) ---
    /// The left expand chevron (toggles the Replace row).
    pub chevron_x: u16,
    pub chevron_y: u16,
    /// The replace-all action icon at the right of the Replace row.
    pub replace_all_x: u16,
    pub replace_all_y: u16,
    /// The `...` details toggle (expands include/exclude).
    pub ellipsis_x: u16,
    pub ellipsis_y: u16,
    /// The SEARCH header action icons (top-right): refresh re-runs the query,
    /// clear wipes the query + results. Single-cell hit targets.
    pub refresh_x: u16,
    pub refresh_y: u16,
    pub clear_x: u16,
    pub clear_y: u16,
    /// Content rects of each input box's editable area, for click-to-focus.
    pub query_input_rect: Rect,
    pub replace_input_rect: Rect,
    pub include_input_rect: Rect,
    pub exclude_input_rect: Rect,
    /// Row offset (from `inner.y`) where the results list starts. Recomputed
    /// each render because expanding Replace / details rows pushes results
    /// down. `results_viewport_height` and `hit_at_y` read this so scrolling
    /// and click mapping stay aligned with the rendered layout.
    pub results_start_offset: u16,
    /// Screen cell of the focused field's text caret, captured each render so
    /// the app can drive the host's blinking hardware caret there (matching
    /// the Source Control commit box). `None` when no field is focused or the
    /// panel has not laid out yet.
    caret_cell: Option<(u16, u16)>,
}

/// Which input field of the Search panel currently has the cursor. Drives
/// where typed characters land and which box draws the focus accent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SearchField {
    #[default]
    Query,
    Replace,
    Include,
    Exclude,
}

impl SearchPanel {
    pub fn new(root: PathBuf) -> Self {
        Self {
            query: String::new(),
            hits: Vec::new(),
            selected: 0,
            scroll: 0,
            focused: false,
            focus_gradient: false,
            theme: crate::theme::Theme::default(),
            last_inner: Rect::default(),
            last_area: Rect::default(),
            root,
            opts: SearchOpts::default(),
            toggle_case_x: 0,
            toggle_word_x: 0,
            toggle_regex_x: 0,
            toggle_y: 0,
            paste_button_x: 0,
            paste_button_y: 0,
            paste_button_w: 0,
            selection: None,
            last_scrollbar: Rect::default(),
            hover_pointer: None,
            replace: String::new(),
            include: String::new(),
            exclude: String::new(),
            replace_open: false,
            details_open: false,
            field: SearchField::Query,
            field_selection: None,
            chevron_x: 0,
            chevron_y: 0,
            replace_all_x: 0,
            replace_all_y: 0,
            ellipsis_x: 0,
            ellipsis_y: 0,
            refresh_x: 0,
            refresh_y: 0,
            clear_x: 0,
            clear_y: 0,
            query_input_rect: Rect::default(),
            replace_input_rect: Rect::default(),
            include_input_rect: Rect::default(),
            exclude_input_rect: Rect::default(),
            results_start_offset: 5,
            caret_cell: None,
        }
    }

    /// Screen position of the focused field's text caret, in absolute terminal
    /// coordinates, for the app to drive the host's blinking hardware caret
    /// (mirroring `SourceControlPanel::cursor_screen_pos`). `None` when the
    /// panel isn't focused or hasn't been laid out yet.
    pub fn cursor_screen_pos(&self) -> Option<(u16, u16)> {
        if !self.focused {
            return None;
        }
        self.caret_cell
    }

    pub fn paste_button_at(&self, col: u16, row: u16) -> bool {
        if self.paste_button_w == 0 || row != self.paste_button_y {
            return false;
        }
        col >= self.paste_button_x && col < self.paste_button_x + self.paste_button_w
    }

    pub fn selection_range(&self) -> Option<(usize, usize)> {
        self.selection
            .map(|(a, b)| if a <= b { (a, b) } else { (b, a) })
    }

    pub fn select_all_query(&mut self) {
        if self.query.is_empty() {
            self.selection = None;
        } else {
            self.selection = Some((0, self.query.len()));
        }
    }

    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    pub fn delete_selection(&mut self) -> bool {
        let Some((a, b)) = self.selection_range() else {
            return false;
        };
        if a == b {
            self.selection = None;
            return false;
        }
        self.query.replace_range(a..b, "");
        self.selection = None;
        true
    }

    pub fn selection_text(&self) -> String {
        match self.selection_range() {
            Some((a, b)) if a < b => self.query[a..b].to_string(),
            _ => String::new(),
        }
    }

    /// Insert `s` at the end of the query, replacing the current selection
    /// (if any) first. Newlines and carriage returns are stripped — search
    /// queries are single-line.
    pub fn insert_str_into_query(&mut self, s: &str) {
        self.delete_selection();
        for c in s.chars() {
            if c != '\n' && c != '\r' {
                self.query.push(c);
            }
        }
    }

    /// Mutable handle to the text of whichever field is focused. The Query
    /// field keeps its own `selection`; the other fields share
    /// `field_selection`, so selecting text in one input clears it elsewhere.
    fn active_text_mut(&mut self) -> &mut String {
        match self.field {
            SearchField::Query => &mut self.query,
            SearchField::Replace => &mut self.replace,
            SearchField::Include => &mut self.include,
            SearchField::Exclude => &mut self.exclude,
        }
    }

    /// Read-only view of the focused field's text.
    pub fn active_text(&self) -> &str {
        match self.field {
            SearchField::Query => &self.query,
            SearchField::Replace => &self.replace,
            SearchField::Include => &self.include,
            SearchField::Exclude => &self.exclude,
        }
    }

    /// Clear the focused field's text entirely (used by Esc).
    pub fn active_text_mut_clear(&mut self) {
        self.active_text_mut().clear();
    }

    /// Clear the selection of the focused field.
    pub fn clear_active_selection(&mut self) {
        self.set_active_selection(None);
    }

    fn active_selection(&self) -> Option<(usize, usize)> {
        let sel = match self.field {
            SearchField::Query => self.selection,
            _ => self.field_selection,
        };
        sel.map(|(a, b)| if a <= b { (a, b) } else { (b, a) })
    }

    fn set_active_selection(&mut self, sel: Option<(usize, usize)>) {
        match self.field {
            SearchField::Query => self.selection = sel,
            _ => self.field_selection = sel,
        }
    }

    /// Append `c` to the focused field, replacing any selection first.
    pub fn type_char(&mut self, c: char) {
        if c == '\n' || c == '\r' {
            return;
        }
        self.delete_active_selection();
        self.active_text_mut().push(c);
    }

    /// Backspace one char (or the selection) from the focused field. Returns
    /// true when the text changed.
    pub fn backspace(&mut self) -> bool {
        if self.delete_active_selection() {
            return true;
        }
        self.active_text_mut().pop().is_some()
    }

    /// Insert `s` into the focused field, replacing the selection and stripping
    /// newlines (all Search inputs are single-line).
    pub fn insert_str(&mut self, s: &str) {
        self.delete_active_selection();
        let text = self.active_text_mut();
        for c in s.chars() {
            if c != '\n' && c != '\r' {
                text.push(c);
            }
        }
    }

    /// Select the whole text of the focused field.
    pub fn select_all_active(&mut self) {
        let len = self.active_text().len();
        if len == 0 {
            self.set_active_selection(None);
        } else {
            self.set_active_selection(Some((0, len)));
        }
    }

    fn delete_active_selection(&mut self) -> bool {
        let Some((a, b)) = self.active_selection() else {
            return false;
        };
        if a == b {
            self.set_active_selection(None);
            return false;
        }
        self.active_text_mut().replace_range(a..b, "");
        self.set_active_selection(None);
        true
    }

    /// Text currently selected in the focused field (empty when none).
    pub fn active_selection_text(&self) -> String {
        match self.active_selection() {
            Some((a, b)) if a < b => self.active_text()[a..b].to_string(),
            _ => String::new(),
        }
    }

    /// Move keyboard focus to the next visible field, wrapping around. Only
    /// fields whose row is currently expanded participate, mirroring VS Code
    /// where Tab skips collapsed inputs.
    pub fn focus_next_field(&mut self) {
        let order = self.visible_fields();
        let pos = order.iter().position(|f| *f == self.field).unwrap_or(0);
        let next = order[(pos + 1) % order.len()];
        self.focus_field(next);
    }

    /// Set the focused field, clearing selection in the field being left.
    pub fn focus_field(&mut self, field: SearchField) {
        if self.field != field {
            self.set_active_selection(None);
            self.field = field;
        }
    }

    /// Visible (focusable) fields, in tab order, honouring which rows are
    /// expanded. Query is always present.
    fn visible_fields(&self) -> Vec<SearchField> {
        let mut v = vec![SearchField::Query];
        if self.replace_open {
            v.push(SearchField::Replace);
        }
        if self.details_open {
            v.push(SearchField::Include);
            v.push(SearchField::Exclude);
        }
        v
    }

    /// Toggle the Replace row. When collapsing while it holds focus, focus
    /// falls back to the Query field.
    pub fn toggle_replace(&mut self) {
        self.replace_open = !self.replace_open;
        if !self.replace_open && self.field == SearchField::Replace {
            self.focus_field(SearchField::Query);
        }
    }

    /// Toggle the include/exclude detail rows. Collapsing while one holds
    /// focus returns focus to the Query field.
    pub fn toggle_details(&mut self) {
        self.details_open = !self.details_open;
        if !self.details_open && matches!(self.field, SearchField::Include | SearchField::Exclude) {
            self.focus_field(SearchField::Query);
        }
    }

    /// Compile the current include/exclude inputs into a `PathFilter`.
    pub fn path_filter(&self) -> PathFilter {
        PathFilter::new(&self.include, &self.exclude)
    }

    /// True when `(col, row)` lands on the expand chevron (toggle Replace row).
    pub fn chevron_at(&self, col: u16, row: u16) -> bool {
        row == self.chevron_y && col == self.chevron_x
    }

    /// True when `(col, row)` lands on the `...` ellipsis (toggle details).
    pub fn ellipsis_at(&self, col: u16, row: u16) -> bool {
        self.ellipsis_x != 0 && row == self.ellipsis_y && col == self.ellipsis_x
    }

    /// True when `(col, row)` lands on the replace-all action icon.
    pub fn replace_all_at(&self, col: u16, row: u16) -> bool {
        self.replace_all_x != 0 && row == self.replace_all_y && col == self.replace_all_x
    }

    /// True when `(col, row)` lands on the header "Refresh" action icon.
    pub fn refresh_at(&self, col: u16, row: u16) -> bool {
        self.refresh_x != 0 && row == self.refresh_y && col == self.refresh_x
    }

    /// True when `(col, row)` lands on the header "Clear Search Results" icon.
    pub fn clear_at(&self, col: u16, row: u16) -> bool {
        self.clear_x != 0 && row == self.clear_y && col == self.clear_x
    }

    /// The hover-tooltip label for whatever actionable control sits under
    /// `(col, row)`, or `None` over plain text/fields/empty space. Reuses the
    /// same hit-tests the click handler uses, so a hint targets exactly the
    /// cells that act. Text inputs deliberately return `None`: their purpose is
    /// shown by the inline placeholder, not a tooltip.
    pub fn tooltip_at(&self, col: u16, row: u16) -> Option<&'static str> {
        if self.refresh_at(col, row) {
            return Some("Refresh");
        }
        if self.clear_at(col, row) {
            return Some("Clear Search Results");
        }
        if self.chevron_at(col, row) {
            return Some(if self.replace_open {
                "Collapse Replace"
            } else {
                "Toggle Replace"
            });
        }
        if self.ellipsis_at(col, row) {
            return Some("Toggle Search Details");
        }
        if self.replace_all_at(col, row) {
            return Some("Replace All");
        }
        if let Some(toggle) = self.toggle_at(col, row) {
            return Some(match toggle {
                SearchToggle::CaseSensitive => "Match Case",
                SearchToggle::WholeWord => "Match Whole Word",
                SearchToggle::UseRegex => "Use Regular Expression",
            });
        }
        None
    }

    /// Clear the search like VS Code's "Clear Search Results": empty the query
    /// (and the replace text typed against it) and drop every hit, resetting
    /// selection and scroll. The include/exclude glob filters are intentionally
    /// preserved — they are scoping settings, not search results.
    pub fn clear_all(&mut self) {
        self.query.clear();
        self.replace.clear();
        self.hits.clear();
        self.selected = 0;
        self.scroll = 0;
        self.selection = None;
        self.field_selection = None;
    }

    /// Map a click to the input field whose editable row it lands on, for
    /// click-to-focus. Returns `None` when the click is outside every input.
    pub fn field_at(&self, col: u16, row: u16) -> Option<SearchField> {
        let on = |r: Rect| r.width != 0 && row == r.y && col >= r.x && col < r.x + r.width;
        if on(self.query_input_rect) {
            return Some(SearchField::Query);
        }
        if on(self.replace_input_rect) {
            return Some(SearchField::Replace);
        }
        if on(self.include_input_rect) {
            return Some(SearchField::Include);
        }
        if on(self.exclude_input_rect) {
            return Some(SearchField::Exclude);
        }
        None
    }

    /// Replace every match in every distinct file among the current hits with
    /// `self.replace`, honouring the active `SearchOpts`. Returns the total
    /// number of matches replaced across all files. The caller is responsible
    /// for re-running the query afterwards so the (now stale) hit list updates.
    pub fn replace_all_on_disk(&self) -> usize {
        let needle = self.query.trim();
        if needle.is_empty() {
            return 0;
        }
        let mut seen: HashSet<PathBuf> = HashSet::new();
        let mut total = 0usize;
        for hit in &self.hits {
            if !seen.insert(hit.path.clone()) {
                continue;
            }
            if let Some(n) = replace_in_file(&hit.path, needle, &self.replace, self.opts) {
                total += n;
            }
        }
        total
    }

    /// Run the current query, store the results, and reset selection.
    pub fn run_query(&mut self) {
        self.hits = search_workspace(&self.root, &self.query, self.opts);
        self.selected = 0;
        self.scroll = 0;
    }

    /// If the cell `(col, row)` falls on one of the three toggle glyphs
    /// (each rendered as a 2-cell pair), return a mutable pointer to the
    /// corresponding flag so the caller can flip it. Returns `None`
    /// otherwise.
    pub fn toggle_at(&self, col: u16, row: u16) -> Option<SearchToggle> {
        if row != self.toggle_y {
            return None;
        }
        for (start, kind) in [
            (self.toggle_case_x, SearchToggle::CaseSensitive),
            (self.toggle_word_x, SearchToggle::WholeWord),
            (self.toggle_regex_x, SearchToggle::UseRegex),
        ] {
            if start != 0 && col >= start && col < start + 2 {
                return Some(kind);
            }
        }
        None
    }

    pub fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
        self.ensure_selected_visible();
    }

    pub fn move_down(&mut self) {
        if self.selected + 1 < self.hits.len() {
            self.selected += 1;
        }
        self.ensure_selected_visible();
    }

    /// Pull `scroll` so the current `selected` row sits inside the viewport.
    /// Only invoked from keyboard-driven moves; wheel scroll and scrollbar
    /// drag must NOT call this - the user's wheel position is sacrosanct
    /// and selection trails wherever they put it.
    fn ensure_selected_visible(&mut self) {
        let viewport = self.results_viewport_height();
        if viewport == 0 {
            return;
        }
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + viewport {
            self.scroll = self.selected + 1 - viewport;
        }
    }

    pub fn scroll_up(&mut self, rows: usize) {
        self.scroll = self.scroll.saturating_sub(rows);
    }

    pub fn scroll_down(&mut self, rows: usize) {
        let viewport = self.results_viewport_height();
        let max_scroll = self.hits.len().saturating_sub(viewport);
        self.scroll = self.scroll.saturating_add(rows).min(max_scroll);
    }

    /// Map a track-y from a scrollbar click/drag back into a scroll offset.
    pub fn scroll_to_bar_y(&mut self, y: u16) -> bool {
        let viewport = self.results_viewport_height();
        let Some(metrics) = crate::widgets::scrollbar::vertical_metrics(
            self.last_scrollbar,
            self.hits.len(),
            viewport,
            self.scroll,
        ) else {
            return false;
        };
        let new_scroll = crate::widgets::scrollbar::scroll_for_y(metrics, y);
        let max_scroll = self.hits.len().saturating_sub(viewport);
        self.scroll = new_scroll.min(max_scroll);
        true
    }

    fn results_viewport_height(&self) -> usize {
        // Results start at `results_start_offset` rows below `inner.y`. That
        // offset is recomputed each render and grows when the Replace row or
        // the include/exclude detail rows are expanded.
        let inner = self.last_inner;
        if inner.height == 0 {
            return 0;
        }
        let results_top = self.results_start_offset;
        if inner.height <= results_top {
            return 0;
        }
        (inner.height - results_top) as usize
    }

    pub fn selected_hit(&self) -> Option<&SearchHit> {
        self.hits.get(self.selected)
    }

    /// Map a click row to a hit index, if any. Hits sit below the input
    /// cluster; the exact first row depends on which optional rows (Replace,
    /// include/exclude) are expanded, captured in `results_start_offset`.
    pub fn hit_at_y(&self, y: u16) -> Option<usize> {
        let inner = self.last_inner;
        let results_start = inner.y + self.results_start_offset;
        if y < results_start || y >= inner.y + inner.height {
            return None;
        }
        let row_in_results = (y - results_start) as usize;
        let idx = self.scroll + row_in_results;
        if idx < self.hits.len() {
            Some(idx)
        } else {
            None
        }
    }
}

/// Identifies which of the three search-mode toggles a click landed on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchToggle {
    CaseSensitive,
    WholeWord,
    UseRegex,
}

/// Parameters for [`render_field`], bundled so the long argument list stays
/// readable at each call site.
struct FieldArgs<'a> {
    /// Column of the accent/idle left-edge bar; the filled content begins one
    /// cell to its right.
    bar_x: u16,
    /// The single content row this field occupies.
    y: u16,
    /// Last column of the fill (inclusive).
    fill_right: u16,
    /// Optional leading glyph (e.g. the magnifier) drawn at the fill start.
    lead_glyph: Option<&'a str>,
    /// Current text of the field.
    text: &'a str,
    /// Placeholder shown italic-grey when `text` is empty.
    placeholder: &'a str,
    /// Selection range within `text`, already normalised (a <= b).
    selection: Option<(usize, usize)>,
    /// Whether THIS field is focused (drives the accent bar, brighter fill and
    /// whether the inline cursor renders here).
    field_focused: bool,
    /// Accent color for the bar / lead glyph / cursor when focused.
    accent: Color,
    /// The active theme background, lifted to derive the idle/focused fill so
    /// the field reads as elevated on Black and on Croft Dark alike.
    bg: (u8, u8, u8),
    /// Cells kept clear at the right of the fill for inset controls (the query
    /// row's toggles). The returned hit rect also stops short of them so a
    /// toggle click is not swallowed as a click-to-focus.
    reserve_right: u16,
}

/// Render a single filled input row (VS Code's `input.background` model) and
/// return the click-to-focus hit `Rect` covering the bar + editable span (but
/// not any reserved control cells on the right). Shared by all four Search
/// inputs so they look identical: a one-cell accent/idle left bar, a subtly
/// lifted fill, an optional lead glyph, then the text/placeholder. Replaces the
/// old 3-row rounded box, which read as bulky next to VS Code / Zed's thin
/// single-line fields.
///
/// When the field is focused, `caret_out` is set to the screen cell where the
/// caret belongs (just past the text, at the field's left edge when empty) so
/// the app can drive the host's blinking hardware caret there, matching the
/// Source Control commit box. We no longer paint a static full block, which
/// never blinked and so read differently from the commit input.
fn render_field(buf: &mut Buffer, args: FieldArgs, caret_out: &mut Option<(u16, u16)>) -> Rect {
    if args.fill_right <= args.bar_x {
        return Rect {
            x: args.bar_x,
            y: args.y,
            width: 0,
            height: 1,
        };
    }
    // Left-edge bar: accent when focused (standing in for VS Code's focus
    // border, which we cannot draw as a sub-cell stroke), dim otherwise so the
    // field still reads as a target at rest.
    let bar_color = if args.field_focused {
        args.accent
    } else {
        FIELD_BAR_IDLE
    };
    buf.set_string(args.bar_x, args.y, "▏", Style::default().fg(bar_color));

    let fill_x = args.bar_x + 1;
    let lift = if args.field_focused {
        FIELD_FILL_LIFT_FOCUS
    } else {
        FIELD_FILL_LIFT_IDLE
    };
    let fill = lift_bg(args.bg, lift);
    let fill_style = Style::default().bg(fill);
    for x in fill_x..=args.fill_right {
        buf[(x, args.y)].set_symbol(" ").set_style(fill_style);
    }

    let lead_w: u16 = if args.lead_glyph.is_some() { 2 } else { 0 };
    if let Some(g) = args.lead_glyph {
        let lead_color = if args.field_focused {
            args.accent
        } else {
            Color::Rgb(0x9d, 0xa5, 0xb4)
        };
        buf.set_string(fill_x, args.y, g, Style::default().fg(lead_color).bg(fill));
    }

    // Editable span: after the lead glyph, up to the reserved control cells.
    let text_x = fill_x + lead_w;
    let text_right = args.fill_right.saturating_sub(args.reserve_right);
    let text_w = text_right.saturating_sub(text_x).saturating_add(1);

    let mut spans: Vec<Span> = Vec::with_capacity(4);
    if args.text.is_empty() {
        spans.push(Span::styled(
            args.placeholder.to_string(),
            Style::default()
                .fg(Color::Rgb(0x6c, 0x7d, 0x9c))
                .bg(fill)
                .add_modifier(Modifier::ITALIC),
        ));
    } else {
        let plain = Style::default().fg(Color::White).bg(fill);
        let selected = Style::default()
            .fg(Color::White)
            .bg(Color::Rgb(0x26, 0x4f, 0x78));
        match args.selection {
            Some((a, b)) if a < b => {
                if a > 0 {
                    spans.push(Span::styled(args.text[..a].to_string(), plain));
                }
                spans.push(Span::styled(args.text[a..b].to_string(), selected));
                if b < args.text.len() {
                    spans.push(Span::styled(args.text[b..].to_string(), plain));
                }
            }
            _ => spans.push(Span::styled(args.text.to_string(), plain)),
        }
    }
    if text_w > 0 {
        buf.set_line(text_x, args.y, &Line::from(spans), text_w);
    }

    // The caret rides just past the text (at the field's left edge when empty),
    // clamped inside the editable span. The app shows the host's blinking
    // hardware caret here only while this field is focused, so we record the
    // cell for the focused field alone. Char count matches the commit box's
    // caret model (`message_cursor`); Search has no mid-text caret.
    if args.field_focused && text_w > 0 {
        let off = (args.text.chars().count() as u16).min(text_w.saturating_sub(1));
        *caret_out = Some((text_x + off, args.y));
    }

    // Hit rect: bar + editable span, excluding reserved control cells so the
    // query toggles (checked after `field_at`) still receive their clicks.
    Rect {
        x: args.bar_x,
        y: args.y,
        width: text_right.saturating_sub(args.bar_x).saturating_add(1),
        height: 1,
    }
}

impl Widget for &mut SearchPanel {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let focus_blue = Color::Rgb(0x4e, 0x9a, 0xff);
        // Black theme: inner stroke accents (chevron, input ring, magnifier,
        // cursor) wear the muted brand teal instead of the legacy blue.
        let accent = if self.focus_gradient {
            crate::gradient::rgb_color(crate::gradient::INNER_ACCENT)
        } else {
            focus_blue
        };
        let outer_style = if self.focused {
            Style::default().fg(focus_blue)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let outer = Block::default()
            .borders(Borders::ALL)
            .border_style(outer_style);
        let inner = outer.inner(area);
        outer.render(area, buf);
        // Black theme: replace the solid focus border with the brand gradient.
        // No title sits on this border (the SEARCH header is inside), so there
        // is nothing to re-stamp.
        if self.focused && self.focus_gradient {
            crate::gradient::paint_gradient_box(buf, area);
        }
        self.last_area = area;
        self.last_inner = inner;
        // Clear last frame's caret up front so an early return (too small to
        // lay out a field) never leaves a stale hardware-caret position.
        self.caret_cell = None;
        if inner.height == 0 || inner.width == 0 {
            return;
        }

        // Row 0: "SEARCH" header (top-left) plus the VS Code SEARCH header
        // actions (refresh / clear) at the top-right.
        buf.set_string(
            inner.x,
            inner.y,
            "SEARCH",
            Style::default()
                .fg(Color::Rgb(0xb0, 0xb8, 0xc8))
                .add_modifier(Modifier::BOLD),
        );

        // Header action icons, laid out from the right edge: clear (rightmost)
        // then refresh, each a single-cell click target that lifts to the accent
        // on hover. Only drawn when the panel is wide enough to clear the label.
        self.refresh_x = 0;
        self.refresh_y = inner.y;
        self.clear_x = 0;
        self.clear_y = inner.y;
        if inner.width > 16 {
            let clear_x = inner.x + inner.width - 1;
            let refresh_x = clear_x - 3;
            for (x, glyph) in [
                (refresh_x, crate::icons::SEARCH_REFRESH),
                (clear_x, crate::icons::SEARCH_CLEAR_ALL),
            ] {
                let hot = self
                    .hover_pointer
                    .is_some_and(|(px, py)| py == inner.y && px == x);
                let color = if hot {
                    accent
                } else {
                    Color::Rgb(0x8b, 0x94, 0xa4)
                };
                buf.set_string(x, inner.y, glyph.to_string(), Style::default().fg(color));
            }
            self.refresh_x = refresh_x;
            self.clear_x = clear_x;
        }

        // Below this the panel cannot host the header + a field row, or is too
        // narrow for the chevron column + a field bar + lead glyph + content.
        if inner.height < 4 || inner.width < 8 {
            return;
        }

        // ---- Field cluster geometry ----
        // VS Code / Zed model: each input is a single filled row, never a 3-row
        // box. A chevron sits at the far left (toggles the Replace row); every
        // field is indented two cells past it so the chevron column stays clear.
        // The query row's `Aa ab .*` toggles are inset INSIDE the field at its
        // right edge (VS Code's `.controls`), and the `...` details expander
        // sits in the rightmost cell, just past the field.
        let bg = self.theme.editor_bg_rgb();
        let chevron_x = inner.x;
        let bar_x = inner.x + 2; // left edge of every field's fill bar
        let inner_right = inner.x + inner.width - 1; // last usable column
        let content_y = inner.y + 2; // one blank row below the header

        // The `...` details expander occupies the rightmost cell of the query
        // row; the query field's fill ends two cells short of it.
        let ellipsis_x = inner_right;
        let query_fill_right = ellipsis_x.saturating_sub(2);

        // Inset toggles, right-aligned inside the query fill with a 1-cell pad.
        // Each glyph is two cells; one-cell gaps separate them. `toggle_at` keys
        // off these stored columns, so the layout must match cell-for-cell.
        let regex_x = query_fill_right.saturating_sub(2); // 1-cell pad + 2-cell glyph
        let word_x = regex_x.saturating_sub(3);
        let case_x = word_x.saturating_sub(3);
        // Cells the query field reserves on its right so its text never slides
        // under the toggles: from the first toggle column to the fill edge.
        let query_reserve = query_fill_right.saturating_sub(case_x).saturating_add(1);

        // Expand chevron (far left). ▾ when Replace is open, ▸ when collapsed.
        let chevron_color = if self.focused {
            accent
        } else {
            Color::DarkGray
        };
        let chevron_glyph = if self.replace_open {
            crate::icons::CHEVRON_OPEN
        } else {
            crate::icons::CHEVRON_CLOSED
        };
        buf.set_string(
            chevron_x,
            content_y,
            chevron_glyph.to_string(),
            Style::default()
                .fg(chevron_color)
                .add_modifier(Modifier::BOLD),
        );
        self.chevron_x = chevron_x;
        self.chevron_y = content_y;

        // Caret cell of whichever field is focused. `render_field` writes it
        // only for the focused field (and never clears it for the others), so
        // it accumulates the single focused field's caret across the calls
        // below; committed to `self.caret_cell` once the cluster is laid out.
        let mut caret: Option<(u16, u16)> = None;

        // Query field: filled single row, magnifier lead, inset toggles.
        let query_focused = self.focused && self.field == SearchField::Query;
        self.query_input_rect = render_field(
            buf,
            FieldArgs {
                bar_x,
                y: content_y,
                fill_right: query_fill_right,
                lead_glyph: Some("\u{ea6d}"),
                text: &self.query,
                placeholder: "Search",
                selection: self.selection_range(),
                field_focused: query_focused,
                accent,
                bg,
                reserve_right: query_reserve,
            },
            &mut caret,
        );

        // Inset toggles `Aa ab .*`, painted on top of the query fill at its
        // right edge. Active = a muted accent chip (standing in for VS Code's
        // translucent `inputOption.activeBackground`); inactive = a dim glyph
        // sitting on the same fill so the chips read as part of the field.
        self.paste_button_x = 0;
        self.paste_button_y = content_y;
        self.paste_button_w = 0;
        let toggle_fill = lift_bg(
            bg,
            if query_focused {
                FIELD_FILL_LIFT_FOCUS
            } else {
                FIELD_FILL_LIFT_IDLE
            },
        );
        let active_style = Style::default()
            .fg(Color::White)
            .bg(crate::gradient::rgb_color(crate::gradient::POPUP_SEL_BG))
            .add_modifier(Modifier::BOLD);
        let inactive_style = Style::default()
            .fg(Color::Rgb(0x79, 0x82, 0x8f))
            .bg(toggle_fill);
        self.toggle_y = content_y;
        self.toggle_case_x = case_x;
        self.toggle_word_x = word_x;
        self.toggle_regex_x = regex_x;
        for (x, glyph, active) in [
            (case_x, "Aa", self.opts.case_sensitive),
            (word_x, "ab", self.opts.whole_word),
            (regex_x, ".*", self.opts.use_regex),
        ] {
            let style = if active { active_style } else { inactive_style };
            buf.set_string(x, content_y, glyph, style);
        }

        // `...` details expander, just past the query field. Accent when open.
        let ellipsis_style = if self.details_open {
            Style::default().fg(accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Rgb(0x79, 0x82, 0x8f))
        };
        buf.set_string(
            ellipsis_x,
            content_y,
            crate::icons::SEARCH_ELLIPSIS.to_string(),
            ellipsis_style,
        );
        self.ellipsis_x = ellipsis_x;
        self.ellipsis_y = content_y;

        // ---- Optional Replace row ----
        // `next_y` tracks the next content row as optional fields stack. Each is
        // now a single row, so the cluster is far shorter than the old boxes.
        let mut next_y = content_y + 1;
        self.replace_input_rect = Rect::default();
        self.replace_all_x = 0;
        self.replace_all_y = 0;
        if self.replace_open && next_y + 1 < inner.y + inner.height {
            // A faint hairline between the query and replace fields so they read
            // as two separate inputs instead of one continuous filled container.
            draw_field_divider(buf, bar_x, query_fill_right, next_y);
            next_y += 1;
            // The replace-all action icon sits at the far right (under the ...),
            // and the replace fill ends two cells short of it.
            let replace_all_x = inner_right;
            let replace_fill_right = replace_all_x.saturating_sub(2).max(bar_x + 1);
            let replace_focused = self.focused && self.field == SearchField::Replace;
            self.replace_input_rect = render_field(
                buf,
                FieldArgs {
                    bar_x,
                    y: next_y,
                    fill_right: replace_fill_right,
                    lead_glyph: Some(&crate::icons::SEARCH_REPLACE.to_string()),
                    text: &self.replace,
                    placeholder: "Replace",
                    selection: if self.field == SearchField::Replace {
                        self.active_selection()
                    } else {
                        None
                    },
                    field_focused: replace_focused,
                    accent,
                    bg,
                    reserve_right: 0,
                },
                &mut caret,
            );
            buf.set_string(
                replace_all_x,
                next_y,
                crate::icons::SEARCH_REPLACE_ALL.to_string(),
                Style::default().fg(if self.focused {
                    accent
                } else {
                    Color::Rgb(0x9d, 0xa5, 0xb4)
                }),
            );
            self.replace_all_x = replace_all_x;
            self.replace_all_y = next_y;
            next_y += 1;
        }

        // ---- Optional files-to-include / files-to-exclude rows ----
        // VS Code stacks an 11px caption above each thin field rather than using
        // placeholder-as-label, so each detail field is a caption row over a
        // single filled field.
        self.include_input_rect = Rect::default();
        self.exclude_input_rect = Rect::default();
        if self.details_open {
            let detail_fill_right = inner_right.saturating_sub(1).max(bar_x + 1);
            let caption_style = Style::default().fg(Color::Rgb(0x9d, 0xa5, 0xb4));

            if next_y + 2 < inner.y + inner.height {
                // Hairline before the include group, matching the query/replace
                // separation so every input reads as its own row.
                draw_field_divider(buf, bar_x, detail_fill_right, next_y);
                next_y += 1;
                buf.set_string(bar_x, next_y, "files to include", caption_style);
                next_y += 1;
                let inc_focused = self.focused && self.field == SearchField::Include;
                self.include_input_rect = render_field(
                    buf,
                    FieldArgs {
                        bar_x,
                        y: next_y,
                        fill_right: detail_fill_right,
                        lead_glyph: None,
                        text: &self.include,
                        placeholder: "e.g. src/**/*.rs",
                        selection: if self.field == SearchField::Include {
                            self.active_selection()
                        } else {
                            None
                        },
                        field_focused: inc_focused,
                        accent,
                        bg,
                        reserve_right: 0,
                    },
                    &mut caret,
                );
                next_y += 1;
            }
            if next_y + 2 < inner.y + inner.height {
                // Hairline before the exclude group, same as above.
                draw_field_divider(buf, bar_x, detail_fill_right, next_y);
                next_y += 1;
                buf.set_string(bar_x, next_y, "files to exclude", caption_style);
                next_y += 1;
                let exc_focused = self.focused && self.field == SearchField::Exclude;
                self.exclude_input_rect = render_field(
                    buf,
                    FieldArgs {
                        bar_x,
                        y: next_y,
                        fill_right: detail_fill_right,
                        lead_glyph: None,
                        text: &self.exclude,
                        placeholder: "e.g. target, dist",
                        selection: if self.field == SearchField::Exclude {
                            self.active_selection()
                        } else {
                            None
                        },
                        field_focused: exc_focused,
                        accent,
                        bg,
                        reserve_right: 0,
                    },
                    &mut caret,
                );
                next_y += 1;
            }
        }
        // Commit the focused field's caret now that the whole input cluster is
        // laid out. Always runs (the optional rows above only skip their own
        // render), so the app sees `None` whenever no field carried a caret.
        self.caret_cell = caret;

        // ---- Separator + match-count caption ----
        let separator_y = next_y;
        if separator_y < inner.y + inner.height {
            let sep_style = Style::default().fg(Color::Rgb(0x40, 0x48, 0x58));
            for x in inner.x..inner.x + inner.width {
                buf.set_string(x, separator_y, "─", sep_style);
            }
        }
        let caption_y = separator_y + 1;
        let results_start_y = caption_y + 1;
        // Record where results begin so scrolling / click mapping stay aligned.
        self.results_start_offset = results_start_y.saturating_sub(inner.y);
        if caption_y < inner.y + inner.height && !self.query.trim().is_empty() {
            let count = self.hits.len();
            let header = format!("{count} match{}", if count == 1 { "" } else { "es" });
            let caption = Line::from(Span::styled(
                header,
                Style::default()
                    .fg(Color::Rgb(0x9d, 0xa5, 0xb4))
                    .add_modifier(Modifier::ITALIC),
            ));
            buf.set_line(inner.x, caption_y, &caption, inner.width);
        }

        // Results
        self.last_scrollbar = Rect::default();
        if results_start_y >= inner.y + inner.height {
            return;
        }
        let visible = (inner.y + inner.height - results_start_y) as usize;
        if visible == 0 {
            return;
        }
        // Clamp scroll so the viewport is always full when there's enough
        // content; otherwise the user can wheel past the end and see blanks.
        // Critically, we do NOT auto-follow `selected` here: that would
        // fight the user every time a streamed `Hits` batch lands and
        // re-renders. Selection-follow lives in `move_up`/`move_down`,
        // where it belongs - keyboard nav, not wheel scroll.
        let max_scroll = self.hits.len().saturating_sub(visible);
        if self.scroll > max_scroll {
            self.scroll = max_scroll;
        }
        // Reserve the rightmost column for the scrollbar when the result
        // list overflows the viewport. `vertical_metrics` returns None when
        // there's no overflow, so the rightmost column is freed for content.
        let scrollbar_area = Rect {
            x: inner.x + inner.width.saturating_sub(1),
            y: results_start_y,
            width: 1,
            height: inner.y + inner.height - results_start_y,
        };
        let scrollbar_metrics = crate::widgets::scrollbar::vertical_metrics(
            scrollbar_area,
            self.hits.len(),
            visible,
            self.scroll,
        );
        if let Some(metrics) = scrollbar_metrics {
            self.last_scrollbar = metrics.area;
        }
        let row_width = inner
            .width
            .saturating_sub(u16::from(scrollbar_metrics.is_some()));
        let end = (self.scroll + visible).min(self.hits.len());
        for (row_idx, hit_idx) in (self.scroll..end).enumerate() {
            let y = results_start_y + row_idx as u16;
            let hit = &self.hits[hit_idx];
            // Lift the row under the pointer (unless it is already the
            // selected row, which carries its own highlight). Pre-filling the
            // bg works because `set_line` only patches the cells it writes and
            // leaves their bg untouched when a span sets no bg of its own.
            let row_rect = Rect {
                x: inner.x,
                y,
                width: row_width,
                height: 1,
            };
            if hit_idx != self.selected
                && let Some(bg) = crate::widgets::hover::row_hover_bg(
                    row_rect,
                    self.hover_pointer,
                    self.focus_gradient,
                )
            {
                let fill = Style::default().bg(bg);
                for rx in 0..row_width {
                    buf[(inner.x + rx, y)].set_symbol(" ").set_style(fill);
                }
            }
            // Show just the basename so the matched line itself has room
            // to render inside a narrow side panel — without this, deep
            // monorepo paths consumed the whole row and the highlight fell
            // off the right edge.
            let path_display = hit
                .path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| {
                    hit.path
                        .strip_prefix(&self.root)
                        .unwrap_or(hit.path.as_path())
                        .display()
                        .to_string()
                });
            // Black theme: selected result row matches the Explorer's
            // white-on-dark-teal selection; Croft Dark keeps black-on-blue.
            let header_style = if hit_idx == self.selected {
                if self.focus_gradient {
                    Style::default()
                        .fg(Color::White)
                        .bg(crate::gradient::rgb_color(crate::gradient::POPUP_SEL_BG))
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Rgb(0x4e, 0x9a, 0xff))
                        .add_modifier(Modifier::BOLD)
                }
            } else {
                Style::default().fg(Color::White)
            };
            let needle = self.query.trim();
            // High-contrast yellow background like ripgrep / VS Code's
            // editor.findMatchHighlightBackground. Different treatment when
            // the row is selected so the highlight stays readable against
            // the blue row bg instead of fighting it.
            let highlight_style = Style::default()
                .fg(Color::Black)
                .bg(Color::Rgb(0xff, 0xd7, 0x4a))
                .add_modifier(Modifier::BOLD);
            let plain_style = Style::default().fg(Color::Gray);
            let mut spans: Vec<Span> = vec![
                Span::styled(format!(" {path_display}"), header_style),
                Span::styled(
                    format!(":{}: ", hit.line_no),
                    Style::default().fg(Color::Rgb(0xeb, 0xcb, 0x8b)),
                ),
            ];
            for (chunk, is_match) in split_for_highlight(&hit.line_text, needle, self.opts) {
                spans.push(Span::styled(
                    chunk,
                    if is_match {
                        highlight_style
                    } else {
                        plain_style
                    },
                ));
            }
            let line = Line::from(spans);
            buf.set_line(inner.x, y, &line, row_width);
        }
        if let Some(metrics) = scrollbar_metrics {
            crate::widgets::scrollbar::render_vertical(buf, metrics, self.focused, self.theme);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write(p: &Path, s: &str) {
        fs::write(p, s).unwrap();
    }

    #[test]
    fn collect_matches_substring_case_insensitive() {
        let mut out = Vec::new();
        let content = "Hello World\nfoo bar\nHELLO again\nno match here";
        collect_matches_in_text(
            Path::new("a.txt"),
            content,
            "hello",
            SearchOpts::default(),
            &mut out,
        );
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].line_no, 1);
        assert_eq!(out[0].line_text, "Hello World");
        assert_eq!(out[1].line_no, 3);
        assert_eq!(out[1].line_text, "HELLO again");
    }

    #[test]
    fn collect_matches_truncates_very_long_lines() {
        let mut out = Vec::new();
        let long_line = "x".repeat(MAX_LINE_LEN + 50) + "needle";
        collect_matches_in_text(
            Path::new("a.txt"),
            &long_line,
            "needle",
            SearchOpts::default(),
            &mut out,
        );
        assert_eq!(out.len(), 1);
        assert!(out[0].line_text.ends_with('…'));
        assert!(out[0].line_text.chars().count() <= MAX_LINE_LEN + 1);
    }

    #[test]
    fn search_workspace_finds_matches_across_files() {
        let tmp = TempDir::new().unwrap();
        write(&tmp.path().join("a.txt"), "alpha\nbeta\nbananas\n");
        write(&tmp.path().join("b.rs"), "fn beta() {}\nlet bananas = 1;\n");
        let hits = search_workspace(tmp.path(), "bananas", SearchOpts::default());
        assert_eq!(hits.len(), 2);
        let names: Vec<String> = hits
            .iter()
            .map(|h| h.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"a.txt".to_string()));
        assert!(names.contains(&"b.rs".to_string()));
    }

    #[test]
    fn search_workspace_returns_empty_for_blank_query() {
        let tmp = TempDir::new().unwrap();
        write(&tmp.path().join("a.txt"), "anything");
        assert!(search_workspace(tmp.path(), "", SearchOpts::default()).is_empty());
        assert!(search_workspace(tmp.path(), "   ", SearchOpts::default()).is_empty());
    }

    #[test]
    fn search_workspace_skips_binary_or_unreadable_files() {
        let tmp = TempDir::new().unwrap();
        write(&tmp.path().join("text.txt"), "hello world");
        // A non-utf8 file: read_to_string returns Err, we silently skip.
        std::fs::write(tmp.path().join("blob.bin"), [0u8, 159, 146, 150]).unwrap();
        let hits = search_workspace(tmp.path(), "hello", SearchOpts::default());
        assert_eq!(hits.len(), 1);
        assert!(hits[0].path.ends_with("text.txt"));
    }

    #[test]
    fn search_workspace_returns_every_match_with_no_arbitrary_cap() {
        // Regression: the engine used to silently truncate at 200 matches,
        // which the user (correctly) called rubbish. Five files, each with
        // 100 lines containing the needle, must all show up.
        let tmp = TempDir::new().unwrap();
        for i in 0..5 {
            let many = (0..100).map(|_| "needle\n").collect::<String>();
            write(&tmp.path().join(format!("f{i}.txt")), &many);
        }
        let hits = search_workspace(tmp.path(), "needle", SearchOpts::default());
        assert_eq!(
            hits.len(),
            500,
            "engine must surface every match - silent truncation is unacceptable"
        );
    }

    #[test]
    fn search_panel_run_query_populates_hits_and_resets_selection() {
        let tmp = TempDir::new().unwrap();
        write(&tmp.path().join("x.txt"), "one\ntwo\nthree\n");
        let mut panel = SearchPanel::new(tmp.path().to_path_buf());
        panel.query = "two".into();
        panel.run_query();
        assert_eq!(panel.hits.len(), 1);
        assert_eq!(panel.selected, 0);
    }

    /// Layout x-offset for the typed-content cell, relative to `inner.x`:
    /// chevron (1) + gap (1) + field bar (1) + 2-cell magnifier glyph
    /// (codicon + 1 cell gap) = 5.
    const INPUT_TYPED_COL: u16 = 5;
    /// Layout y-offset for the input content row, relative to `inner.y`:
    /// header + blank + content = 2. The single filled field has no top
    /// border row, so the query sits one row higher than the old 3-row box.
    const INPUT_CONTENT_ROW: u16 = 2;

    #[test]
    fn cursor_sits_at_input_start_when_query_empty_and_focused() {
        use ratatui::buffer::Buffer;
        let tmp = TempDir::new().unwrap();
        let mut panel = SearchPanel::new(tmp.path().to_path_buf());
        panel.focused = true;
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 12,
        };
        let mut buf = Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
        let inner_x = panel.last_inner.x;
        let inner_y = panel.last_inner.y;
        // The caret (driven as the host's blinking hardware caret, like the
        // commit box) sits at the start of the input content when empty.
        assert_eq!(
            panel.cursor_screen_pos(),
            Some((inner_x + INPUT_TYPED_COL, inner_y + INPUT_CONTENT_ROW)),
            "caret must sit at the start of the input content area when the query is empty"
        );
    }

    #[test]
    fn cursor_sits_after_typed_text_when_query_non_empty_and_focused() {
        use ratatui::buffer::Buffer;
        let tmp = TempDir::new().unwrap();
        let mut panel = SearchPanel::new(tmp.path().to_path_buf());
        panel.focused = true;
        panel.query = String::from("foo");
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 12,
        };
        let mut buf = Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
        let inner_x = panel.last_inner.x;
        let inner_y = panel.last_inner.y;
        // INPUT_TYPED_COL + "foo" (3 cells) → caret sits 3 cells past the start
        // of typed content.
        assert_eq!(
            panel.cursor_screen_pos(),
            Some((inner_x + INPUT_TYPED_COL + 3, inner_y + INPUT_CONTENT_ROW)),
            "caret must sit immediately after the typed query"
        );
    }

    fn buffer_to_string(buf: &Buffer) -> String {
        let mut out = String::new();
        for y in buf.area.y..buf.area.y + buf.area.height {
            for x in buf.area.x..buf.area.x + buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn header_row_says_search_in_upper_case() {
        // Mockup-driven: an uppercase "SEARCH" header sits at the top-left
        // of the inner area, in light grey — replacing the old dark-blue
        // title bar that used to overlap the outer border.
        use ratatui::buffer::Buffer;
        let tmp = TempDir::new().unwrap();
        let mut panel = SearchPanel::new(tmp.path().to_path_buf());
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 10,
        };
        let mut buf = Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
        let inner_x = panel.last_inner.x;
        let inner_y = panel.last_inner.y;
        let mut header = String::new();
        for x in inner_x..inner_x + 8 {
            header.push_str(buf[(x, inner_y)].symbol());
        }
        assert!(
            header.starts_with("SEARCH"),
            "first inner row must start with the uppercase SEARCH header; got: {header:?}"
        );
        // Outer-border title row must NOT carry a SEARCH chip — the mockup
        // has no title bar; the header lives inside the panel.
        let mut top_border = String::new();
        for x in area.x..area.x + area.width {
            top_border.push_str(buf[(x, area.y)].symbol());
        }
        assert!(
            !top_border.contains("SEARCH"),
            "outer border row must not carry a SEARCH title chip: {top_border:?}"
        );
    }

    #[test]
    fn input_box_renders_a_thin_focused_border_three_rows_tall() {
        use ratatui::buffer::Buffer;
        let tmp = TempDir::new().unwrap();
        let mut panel = SearchPanel::new(tmp.path().to_path_buf());
        panel.focused = true;
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 12,
        };
        let mut buf = Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
        let dump = buffer_to_string(&buf);
        // The input box must render a single-line top-left corner glyph
        // somewhere in the upper half of the panel.
        assert!(
            dump.contains('┌') || dump.contains('╭'),
            "input box must have a top-left corner; got buffer:\n{dump}"
        );
        assert!(
            dump.contains('└') || dump.contains('╰'),
            "input box must have a bottom-left corner so the border reads as a 3-row box, not a single rule:\n{dump}"
        );
    }

    #[test]
    fn rendering_at_narrow_widths_does_not_panic() {
        use ratatui::buffer::Buffer;
        let tmp = TempDir::new().unwrap();
        for width in 0u16..40 {
            for height in 0u16..20 {
                let mut panel = SearchPanel::new(tmp.path().to_path_buf());
                let area = Rect {
                    x: 0,
                    y: 0,
                    width,
                    height,
                };
                if area.width == 0 || area.height == 0 {
                    continue;
                }
                let mut buf = Buffer::empty(area);
                ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
            }
        }
    }

    #[test]
    fn asterisk_keeps_breathing_room_against_the_outer_right_border() {
        // User report: the asterisk in `.*` was sitting flush against the
        // outer right border, looking crowded into the corner. The fix
        // mirrors the gap that already exists between the input box and
        // `Aa` so the toggle cluster reads as a contained group on both
        // sides.
        use ratatui::buffer::Buffer;
        let tmp = TempDir::new().unwrap();
        let mut panel = SearchPanel::new(tmp.path().to_path_buf());
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 12,
        };
        let mut buf = Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
        let inner = panel.last_inner;
        // `.*` toggle is 2 cells wide starting at toggle_regex_x.
        let regex_end = panel.toggle_regex_x + 1;
        let inner_right = inner.x + inner.width - 1;
        let right_gap = inner_right.saturating_sub(regex_end);
        assert!(
            right_gap >= 1,
            "asterisk must not sit flush against the outer right border (right_gap={right_gap})"
        );
    }

    #[test]
    fn input_row_carries_a_chevron_and_pipe_separated_toggles() {
        use ratatui::buffer::Buffer;
        let tmp = TempDir::new().unwrap();
        let mut panel = SearchPanel::new(tmp.path().to_path_buf());
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 10,
        };
        let mut buf = Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
        let dump = buffer_to_string(&buf);
        // VS Code's expand-to-replace chevron points RIGHT (▸) while the
        // Replace row is collapsed and flips DOWN (▾) once expanded. A fresh
        // panel starts collapsed, so the right chevron must render.
        assert!(
            dump.contains('▸'),
            "collapsed panel must show a right-pointing expand chevron on the left:\n{dump}"
        );
        // Now expand the Replace row: the chevron flips to a down-arrow.
        panel.toggle_replace();
        let mut buf2 = Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut panel, area, &mut buf2);
        let dump2 = buffer_to_string(&buf2);
        assert!(
            dump2.contains('▾'),
            "expanded panel must flip the chevron to a down-arrow:\n{dump2}"
        );
        // Toggles separated by pipe '│' so the trio reads "Aa │ ab │ .*".
        assert!(
            dump.contains("Aa") && dump.contains("ab") && dump.contains(".*"),
            "all three toggle glyphs must render:\n{dump}"
        );
        let pipes = dump.matches('│').count();
        assert!(
            pipes >= 2,
            "at least two pipe separators must sit between the three toggles; got {pipes} pipes in:\n{dump}"
        );
    }

    #[test]
    fn results_start_below_the_input_box_with_a_separator_in_between() {
        use ratatui::buffer::Buffer;
        let tmp = TempDir::new().unwrap();
        write(&tmp.path().join("a.txt"), "needle\n");
        let mut panel = SearchPanel::new(tmp.path().to_path_buf());
        panel.query = String::from("needle");
        panel.run_query();
        assert_eq!(panel.hits.len(), 1);
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 14,
        };
        let mut buf = Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
        let dump = buffer_to_string(&buf);
        let row_with_needle = dump
            .lines()
            .position(|l| l.contains("needle") && l.contains("a.txt"))
            .expect("result row with needle must render");
        assert!(
            row_with_needle >= 5,
            "result row must sit below the input box (expected row >= 5, got {row_with_needle}):\n{dump}"
        );
    }

    #[test]
    fn search_panel_does_not_render_paste_button_in_input_row() {
        use ratatui::buffer::Buffer;
        let tmp = TempDir::new().unwrap();
        let mut panel = SearchPanel::new(tmp.path().to_path_buf());
        panel.focused = true;
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 5,
        };
        let mut buf = Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
        assert_eq!(
            panel.paste_button_w, 0,
            "paste button should not reserve cells"
        );
        let mut collected = String::new();
        for x in area.x..area.x + area.width {
            collected.push_str(buf[(x, 1)].symbol());
        }
        assert!(
            !collected.to_lowercase().contains("paste"),
            "input row should not visibly contain a Paste button, got: {collected:?}"
        );
    }

    #[test]
    fn paste_button_at_returns_false_when_button_hidden() {
        use ratatui::buffer::Buffer;
        let tmp = TempDir::new().unwrap();
        let mut panel = SearchPanel::new(tmp.path().to_path_buf());
        panel.focused = true;
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 5,
        };
        let mut buf = Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
        assert_eq!(panel.paste_button_w, 0);
        assert!(!panel.paste_button_at(0, 1));
        assert!(!panel.paste_button_at(40, 1));
    }

    #[test]
    fn split_highlight_returns_whole_line_when_needle_empty() {
        let segs = split_for_highlight("hello world", "", SearchOpts::default());
        assert_eq!(segs, vec![(String::from("hello world"), false)]);
    }

    #[test]
    fn split_highlight_returns_whole_line_when_no_match() {
        let segs = split_for_highlight("hello world", "xyz", SearchOpts::default());
        assert_eq!(segs, vec![(String::from("hello world"), false)]);
    }

    #[test]
    fn split_highlight_marks_each_match_run_case_insensitive() {
        let segs = split_for_highlight(
            "The Quick brown fox jumps over the QUICK fence",
            "quick",
            SearchOpts::default(),
        );
        // Two runs of "quick" (mixed case), three non-match tails.
        let matches: Vec<&String> = segs
            .iter()
            .filter_map(|(s, m)| if *m { Some(s) } else { None })
            .collect();
        assert_eq!(matches.len(), 2);
        // Original-case slice must be preserved in the matched segment.
        assert_eq!(matches[0], "Quick");
        assert_eq!(matches[1], "QUICK");
        // Concatenating all segments must reproduce the original line.
        let joined: String = segs.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(joined, "The Quick brown fox jumps over the QUICK fence");
    }

    #[test]
    fn split_highlight_handles_match_at_start_and_end() {
        let segs = split_for_highlight("foo bar foo", "foo", SearchOpts::default());
        let joined: String = segs.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(joined, "foo bar foo");
        assert_eq!(
            segs.first().map(|(_, m)| *m),
            Some(true),
            "first seg is match"
        );
        assert_eq!(
            segs.last().map(|(_, m)| *m),
            Some(true),
            "last seg is match"
        );
    }

    fn collect_until_done(
        rx: &std::sync::mpsc::Receiver<SearchEvent>,
        deadline: std::time::Duration,
    ) -> (Option<String>, Vec<SearchHit>, bool) {
        let until = std::time::Instant::now() + deadline;
        let mut last_q: Option<String> = None;
        let mut hits: Vec<SearchHit> = Vec::new();
        let mut saw_done = false;
        while std::time::Instant::now() < until {
            let remaining = until.saturating_duration_since(std::time::Instant::now());
            match rx.recv_timeout(remaining.max(std::time::Duration::from_millis(50))) {
                Ok(SearchEvent::Hits(q, _, batch)) => {
                    last_q = Some(q);
                    hits.extend(batch);
                }
                Ok(SearchEvent::Done(q, _)) => {
                    last_q = Some(q);
                    saw_done = true;
                    break;
                }
                Err(_) => break,
            }
        }
        (last_q, hits, saw_done)
    }

    #[test]
    fn search_worker_loop_searches_dirty_buffer_and_skips_stale_disk_copy() {
        // Disk holds the OLD name; the unsaved editor buffer holds the NEW
        // name. Dirty-aware search must find the new name (from the buffer)
        // and must NOT find the old name (the dirty file's stale disk copy is
        // skipped). This is the VS Code / Zed behavior the reported bug lacked.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("a.txt");
        write(&path, "oldname\n");
        let (q_tx, q_rx) = std::sync::mpsc::channel::<SearchRequest>();
        let (e_tx, e_rx) = std::sync::mpsc::channel::<SearchEvent>();
        let root = tmp.path().to_path_buf();
        let join = std::thread::spawn(move || search_worker_loop(root, q_rx, e_tx));

        q_tx.send(SearchRequest::SetDirtyBuffers(vec![(
            path.clone(),
            "newname\n".into(),
        )]))
        .unwrap();

        q_tx.send(SearchRequest::Query(
            "newname".into(),
            SearchOpts::default(),
        ))
        .unwrap();
        let (_, hits, done) = collect_until_done(&e_rx, std::time::Duration::from_secs(2));
        assert!(done);
        assert_eq!(hits.len(), 1, "the unsaved buffer's new name must be found");
        assert_eq!(hits[0].line_text, "newname");

        q_tx.send(SearchRequest::Query(
            "oldname".into(),
            SearchOpts::default(),
        ))
        .unwrap();
        let (_, stale, done2) = collect_until_done(&e_rx, std::time::Duration::from_secs(2));
        assert!(done2);
        assert_eq!(
            stale.len(),
            0,
            "the stale on-disk copy of a dirty file must be skipped"
        );

        drop(q_tx);
        join.join().unwrap();
    }

    #[test]
    fn search_worker_loop_returns_hits_for_a_typed_query() {
        let tmp = TempDir::new().unwrap();
        write(&tmp.path().join("a.txt"), "one\nneedle\nthree\n");
        let (q_tx, q_rx) = std::sync::mpsc::channel::<SearchRequest>();
        let (e_tx, e_rx) = std::sync::mpsc::channel::<SearchEvent>();
        let root = tmp.path().to_path_buf();
        let join = std::thread::spawn(move || search_worker_loop(root, q_rx, e_tx));
        q_tx.send(SearchRequest::Query("needle".into(), SearchOpts::default()))
            .unwrap();
        let (q, hits, done) = collect_until_done(&e_rx, std::time::Duration::from_secs(2));
        assert_eq!(q.as_deref(), Some("needle"));
        assert_eq!(hits.len(), 1);
        assert!(done, "worker must terminate the stream with a Done event");
        drop(q_tx);
        join.join().unwrap();
    }

    #[test]
    fn search_worker_loop_streams_per_file_batches_for_a_typed_query() {
        // Three files each containing matches must surface as separate Hits
        // batches followed by exactly one Done. This pins the streaming
        // protocol so the UI can show "N matches (still searching)".
        let tmp = TempDir::new().unwrap();
        write(&tmp.path().join("a.txt"), "needle\n");
        write(&tmp.path().join("b.txt"), "needle\nneedle\n");
        write(&tmp.path().join("c.txt"), "needle\n");
        let (q_tx, q_rx) = std::sync::mpsc::channel::<SearchRequest>();
        let (e_tx, e_rx) = std::sync::mpsc::channel::<SearchEvent>();
        let root = tmp.path().to_path_buf();
        let join = std::thread::spawn(move || search_worker_loop(root, q_rx, e_tx));
        q_tx.send(SearchRequest::Query("needle".into(), SearchOpts::default()))
            .unwrap();
        let mut total = 0usize;
        let mut hits_events = 0usize;
        let mut done_events = 0usize;
        let until = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < until {
            match e_rx.recv_timeout(std::time::Duration::from_millis(500)) {
                Ok(SearchEvent::Hits(_, _, batch)) => {
                    hits_events += 1;
                    total += batch.len();
                }
                Ok(SearchEvent::Done(_, _)) => {
                    done_events += 1;
                    break;
                }
                Err(_) => break,
            }
        }
        drop(q_tx);
        join.join().unwrap();
        assert_eq!(total, 4, "every match across all files must arrive");
        assert!(
            hits_events >= 3,
            "expected per-file batches, got {hits_events}"
        );
        assert_eq!(done_events, 1, "exactly one Done sentinel per request");
    }

    #[test]
    fn search_worker_loop_coalesces_a_burst_of_keystrokes() {
        // Simulate a fast typist sending o, on, one. Worker must coalesce
        // and only run the latest, returning hits for "one" — not for the
        // intermediate prefixes.
        let tmp = TempDir::new().unwrap();
        write(&tmp.path().join("a.txt"), "alpha one beta\nzeta\n");
        let (q_tx, q_rx) = std::sync::mpsc::channel::<SearchRequest>();
        let (e_tx, e_rx) = std::sync::mpsc::channel::<SearchEvent>();
        let root = tmp.path().to_path_buf();
        let join = std::thread::spawn(move || search_worker_loop(root, q_rx, e_tx));
        q_tx.send(SearchRequest::Query("o".into(), SearchOpts::default()))
            .unwrap();
        q_tx.send(SearchRequest::Query("on".into(), SearchOpts::default()))
            .unwrap();
        q_tx.send(SearchRequest::Query("one".into(), SearchOpts::default()))
            .unwrap();
        let (q, hits, done) = collect_until_done(&e_rx, std::time::Duration::from_secs(2));
        assert_eq!(
            q.as_deref(),
            Some("one"),
            "coalesce must drop intermediate prefixes"
        );
        assert_eq!(hits.len(), 1);
        assert!(done);
        drop(q_tx);
        join.join().unwrap();
    }

    #[test]
    fn search_worker_loop_resets_debounce_on_each_keystroke() {
        // Trailing-edge debounce: keys arriving within the quiet window
        // (200 ms) restart the timer, so the user typing "one" with
        // 100 ms between each keystroke must fire EXACTLY ONE search at
        // the end - for "one" - not three searches for "o", "on", "one".
        let tmp = TempDir::new().unwrap();
        write(&tmp.path().join("a.txt"), "alpha one beta\nzeta\n");
        let (q_tx, q_rx) = std::sync::mpsc::channel::<SearchRequest>();
        let (e_tx, e_rx) = std::sync::mpsc::channel::<SearchEvent>();
        let root = tmp.path().to_path_buf();
        let join = std::thread::spawn(move || search_worker_loop(root, q_rx, e_tx));
        // Each gap is well under SEARCH_DEBOUNCE (200 ms), so each new
        // send resets the timer and no search should fire mid-burst.
        q_tx.send(SearchRequest::Query("o".into(), SearchOpts::default()))
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));
        q_tx.send(SearchRequest::Query("on".into(), SearchOpts::default()))
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));
        q_tx.send(SearchRequest::Query("one".into(), SearchOpts::default()))
            .unwrap();
        // Now stop typing and let the debounce expire + the scan finish.
        let mut dones: Vec<String> = Vec::new();
        let until = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < until {
            if let Ok(SearchEvent::Done(q, _)) =
                e_rx.recv_timeout(std::time::Duration::from_millis(200))
            {
                dones.push(q)
            }
        }
        drop(q_tx);
        join.join().unwrap();
        assert_eq!(
            dones,
            vec!["one".to_string()],
            "exactly one search should have fired - for the trailing query - not one per keystroke"
        );
    }

    #[test]
    fn search_worker_loop_cancels_in_flight_scan_when_a_new_request_arrives() {
        // User-reported "0 matches" bug: a fast typist hits "c", which
        // matches almost every line of every file. The worker used to
        // block on that giant scan and ignore "cl"/"cla" until "c"
        // finished, so the user would stare at "0 matches" while typing.
        // Workspace: 200 files × 20000 lines = 4M matches. Without
        // cancellation, the "c" scan takes many seconds. With cancellation
        // wired up, switching to a no-match query must produce Done within
        // a tight window.
        let tmp = TempDir::new().unwrap();
        for i in 0..200 {
            let many = (0..20000)
                .map(|n| format!("ccc line {n}\n"))
                .collect::<String>();
            write(&tmp.path().join(format!("f{i}.txt")), &many);
        }
        let (q_tx, q_rx) = std::sync::mpsc::channel::<SearchRequest>();
        let (e_tx, e_rx) = std::sync::mpsc::channel::<SearchEvent>();
        let root = tmp.path().to_path_buf();
        let join = std::thread::spawn(move || search_worker_loop(root, q_rx, e_tx));
        q_tx.send(SearchRequest::Query("c".into(), SearchOpts::default()))
            .unwrap();
        // Wait past the 120ms debounce so the worker is committed.
        std::thread::sleep(std::time::Duration::from_millis(200));
        let switched_at = std::time::Instant::now();
        q_tx.send(SearchRequest::Query(
            "zzznoneatall".into(),
            SearchOpts::default(),
        ))
        .unwrap();
        // Done for the new query must arrive within 1.5s of submission.
        // Without cancellation, the worker would still be plowing through
        // the 4M-match "c" scan and Done would not arrive for many seconds.
        let deadline = switched_at + std::time::Duration::from_millis(1500);
        let mut second_done = false;
        while std::time::Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            match e_rx.recv_timeout(remaining.max(std::time::Duration::from_millis(50))) {
                Ok(SearchEvent::Done(q, _)) if q == "zzznoneatall" => {
                    second_done = true;
                    break;
                }
                _ => {}
            }
        }
        drop(q_tx);
        join.join().unwrap();
        assert!(
            second_done,
            "worker must cancel the in-flight scan and emit Done for the new query within 1.5s"
        );
    }

    #[test]
    fn search_worker_loop_short_circuits_empty_queries() {
        let tmp = TempDir::new().unwrap();
        write(&tmp.path().join("a.txt"), "anything\n");
        let (q_tx, q_rx) = std::sync::mpsc::channel::<SearchRequest>();
        let (e_tx, e_rx) = std::sync::mpsc::channel::<SearchEvent>();
        let root = tmp.path().to_path_buf();
        let join = std::thread::spawn(move || search_worker_loop(root, q_rx, e_tx));
        q_tx.send(SearchRequest::Query("".into(), SearchOpts::default()))
            .unwrap();
        let (q, hits, done) = collect_until_done(&e_rx, std::time::Duration::from_secs(2));
        assert_eq!(q.as_deref(), Some(""));
        assert!(hits.is_empty());
        assert!(done, "empty query still emits a Done sentinel");
        drop(q_tx);
        join.join().unwrap();
    }

    #[test]
    fn search_worker_loop_targets_the_directory_set_by_the_latest_set_root() {
        let dir_a = TempDir::new().unwrap();
        write(&dir_a.path().join("a.txt"), "alpha only\n");
        let dir_b = TempDir::new().unwrap();
        write(&dir_b.path().join("b.txt"), "the needle is here\n");
        let (q_tx, q_rx) = std::sync::mpsc::channel::<SearchRequest>();
        let (e_tx, e_rx) = std::sync::mpsc::channel::<SearchEvent>();
        let start_root = dir_a.path().to_path_buf();
        let join = std::thread::spawn(move || search_worker_loop(start_root, q_rx, e_tx));
        q_tx.send(SearchRequest::SetRoot(dir_b.path().to_path_buf()))
            .unwrap();
        q_tx.send(SearchRequest::Query("needle".into(), SearchOpts::default()))
            .unwrap();
        let (q, hits, done) = collect_until_done(&e_rx, std::time::Duration::from_secs(2));
        assert_eq!(q.as_deref(), Some("needle"));
        assert_eq!(
            hits.len(),
            1,
            "after SetRoot, the worker must search the new directory, not the one captured at spawn"
        );
        assert!(
            hits[0].path.starts_with(dir_b.path()),
            "the hit must come from the directory the Explorer now points at"
        );
        assert!(done);
        drop(q_tx);
        join.join().unwrap();
    }

    #[test]
    fn search_opts_default_matches_legacy_case_insensitive_substring() {
        let mut out = Vec::new();
        let opts = SearchOpts::default();
        collect_matches_in_text(
            Path::new("a.txt"),
            "Hello World\nfoo bar\nHELLO again\nno match",
            "hello",
            opts,
            &mut out,
        );
        assert_eq!(out.len(), 2, "default opts mirror the original behaviour");
    }

    #[test]
    fn search_opts_case_sensitive_excludes_different_case() {
        let mut out = Vec::new();
        let opts = SearchOpts {
            case_sensitive: true,
            ..Default::default()
        };
        collect_matches_in_text(
            Path::new("a.txt"),
            "Hello World\nhello again\nHELLO loud",
            "hello",
            opts,
            &mut out,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].line_text, "hello again");
    }

    #[test]
    fn search_opts_whole_word_excludes_partial_matches() {
        let mut out = Vec::new();
        let opts = SearchOpts {
            whole_word: true,
            ..Default::default()
        };
        collect_matches_in_text(
            Path::new("a.txt"),
            "cat sat\ncategory animal\ncat\nbobcat sleeps",
            "cat",
            opts,
            &mut out,
        );
        // Only the lines where 'cat' is a standalone word should match.
        let texts: Vec<&str> = out.iter().map(|h| h.line_text.as_str()).collect();
        assert_eq!(texts, vec!["cat sat", "cat"]);
    }

    #[test]
    fn search_opts_regex_finds_pattern_class() {
        let mut out = Vec::new();
        let opts = SearchOpts {
            use_regex: true,
            ..Default::default()
        };
        collect_matches_in_text(
            Path::new("a.txt"),
            "let x = 42;\nlet y = abc;\nlet z = 13;\nno number",
            r"\d+",
            opts,
            &mut out,
        );
        let texts: Vec<&str> = out.iter().map(|h| h.line_text.as_str()).collect();
        assert_eq!(texts, vec!["let x = 42;", "let z = 13;"]);
    }

    #[test]
    fn search_opts_regex_honours_case_sensitive_flag() {
        let mut out = Vec::new();
        let opts = SearchOpts {
            use_regex: true,
            case_sensitive: true,
            ..Default::default()
        };
        collect_matches_in_text(Path::new("a.txt"), "Foo\nfoo\nFOO", r"foo", opts, &mut out);
        let texts: Vec<&str> = out.iter().map(|h| h.line_text.as_str()).collect();
        assert_eq!(texts, vec!["foo"], "regex must respect case-sensitive flag");
    }

    #[test]
    fn search_opts_invalid_regex_returns_empty_quietly() {
        let mut out = Vec::new();
        let opts = SearchOpts {
            use_regex: true,
            ..Default::default()
        };
        collect_matches_in_text(
            Path::new("a.txt"),
            "anything\nat all",
            r"[unclosed",
            opts,
            &mut out,
        );
        assert!(
            out.is_empty(),
            "invalid regex must not crash, just yield no hits"
        );
    }

    #[test]
    fn toggle_at_maps_a_click_on_each_glyph_to_the_right_kind() {
        use ratatui::buffer::Buffer;
        let tmp = TempDir::new().unwrap();
        let mut panel = SearchPanel::new(tmp.path().to_path_buf());
        panel.focused = true;
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 12,
        };
        let mut buf = Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
        let y = panel.toggle_y;
        // Each toggle is 2 cells; clicking either cell on the same row
        // should map to its kind.
        assert_eq!(
            panel.toggle_at(panel.toggle_case_x, y),
            Some(SearchToggle::CaseSensitive)
        );
        assert_eq!(
            panel.toggle_at(panel.toggle_case_x + 1, y),
            Some(SearchToggle::CaseSensitive)
        );
        assert_eq!(
            panel.toggle_at(panel.toggle_word_x, y),
            Some(SearchToggle::WholeWord)
        );
        assert_eq!(
            panel.toggle_at(panel.toggle_regex_x + 1, y),
            Some(SearchToggle::UseRegex)
        );
        // Wrong row → None.
        assert_eq!(panel.toggle_at(panel.toggle_case_x, y + 1), None);
        // Outside the toggle columns → None.
        assert_eq!(panel.toggle_at(0, y), None);
    }

    #[test]
    fn tooltip_at_labels_each_actionable_control_and_skips_inert_cells() {
        use ratatui::buffer::Buffer;
        let tmp = TempDir::new().unwrap();
        let mut panel = SearchPanel::new(tmp.path().to_path_buf());
        panel.focused = true;
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 12,
        };
        let mut buf = Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
        let y = panel.toggle_y;
        assert_eq!(panel.tooltip_at(panel.toggle_case_x, y), Some("Match Case"));
        assert_eq!(
            panel.tooltip_at(panel.toggle_word_x, y),
            Some("Match Whole Word")
        );
        assert_eq!(
            panel.tooltip_at(panel.toggle_regex_x, y),
            Some("Use Regular Expression")
        );
        assert_eq!(
            panel.tooltip_at(panel.refresh_x, panel.refresh_y),
            Some("Refresh")
        );
        assert_eq!(
            panel.tooltip_at(panel.clear_x, panel.clear_y),
            Some("Clear Search Results")
        );
        assert_eq!(
            panel.tooltip_at(panel.chevron_x, panel.chevron_y),
            Some("Toggle Replace"),
            "the chevron reads as expand while the Replace row is collapsed"
        );
        // A plain text cell on the query row carries no tooltip.
        assert_eq!(panel.tooltip_at(panel.toggle_case_x, y + 5), None);
    }

    #[test]
    fn search_panel_navigation_clamps() {
        let tmp = TempDir::new().unwrap();
        write(&tmp.path().join("x.txt"), "a needle\nb needle\nc needle\n");
        let mut panel = SearchPanel::new(tmp.path().to_path_buf());
        panel.query = "needle".into();
        panel.run_query();
        assert_eq!(panel.hits.len(), 3);
        panel.move_up();
        assert_eq!(panel.selected, 0);
        panel.move_down();
        panel.move_down();
        panel.move_down(); // would go to 3, but clamped to 2
        assert_eq!(panel.selected, 2);
    }

    fn make_panel_with_hits(tmp: &TempDir, n: usize) -> SearchPanel {
        let mut panel = SearchPanel::new(tmp.path().to_path_buf());
        panel.query = "needle".into();
        panel.hits = (0..n)
            .map(|i| SearchHit {
                path: tmp.path().join("a.txt"),
                line_no: i + 1,
                line_text: format!("line {i}"),
            })
            .collect();
        panel
    }

    #[test]
    fn hovering_an_unselected_result_row_lifts_it() {
        let tmp = TempDir::new().unwrap();
        let mut panel = make_panel_with_hits(&tmp, 20);
        panel.focus_gradient = false; // Croft Dark
        panel.selected = 0;
        panel.scroll = 0;
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 30,
        };
        // Results start at inner.y + 7 (see the click-mapping in `result_at`);
        // hover the second result row, which is not the selected one.
        let row_y = area.y + 1 + 7 + 1;
        panel.hover_pointer = Some((area.x + 1, row_y));
        let mut buf = ratatui::buffer::Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
        assert_eq!(
            buf[(area.x + 1, row_y)].bg,
            Color::Rgb(0x2b, 0x31, 0x42),
            "the hovered unselected result row wears the Croft Dark hover lift"
        );
    }

    #[test]
    fn render_preserves_user_scroll_when_selected_is_at_top() {
        // User-reported bug: scroll appears to "come in batches" because
        // every render yanks scroll back to selected=0. Wheel scrolling
        // must be decoupled from selection auto-follow so streaming hits
        // and re-renders never fight the user's scroll position.
        let tmp = TempDir::new().unwrap();
        let mut panel = make_panel_with_hits(&tmp, 100);
        panel.selected = 0;
        panel.scroll = 30;
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 30,
        };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
        assert_eq!(
            panel.scroll, 30,
            "render must not pull scroll back to selected when the user has wheeled away"
        );
    }

    #[test]
    fn appending_streamed_hits_does_not_reset_user_scroll() {
        // Simulates the streaming engine: user wheels down to row 30,
        // then a new batch arrives via `hits.extend(batch)`. The next
        // render must keep scroll at 30, not jump to 0.
        let tmp = TempDir::new().unwrap();
        let mut panel = make_panel_with_hits(&tmp, 80);
        panel.selected = 0;
        panel.scroll = 30;
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 30,
        };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
        // Streaming batch: another 50 hits arrive.
        let extra: Vec<SearchHit> = (80..130)
            .map(|i| SearchHit {
                path: tmp.path().join("a.txt"),
                line_no: i + 1,
                line_text: format!("line {i}"),
            })
            .collect();
        panel.hits.extend(extra);
        ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
        assert_eq!(
            panel.scroll, 30,
            "streaming hits must not reset the user's scroll offset"
        );
    }

    #[test]
    fn move_down_past_bottom_of_viewport_advances_scroll_to_keep_selection_visible() {
        let tmp = TempDir::new().unwrap();
        let mut panel = make_panel_with_hits(&tmp, 100);
        // Pretend a 30-row panel was rendered: viewport = 30 - 5 = 25 rows
        // (the single-row query field starts results two rows higher than the
        // old 3-row box did).
        panel.last_inner = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 30,
        };
        panel.selected = 24;
        panel.scroll = 0;
        panel.move_down();
        assert_eq!(panel.selected, 25);
        assert_eq!(
            panel.scroll, 1,
            "arrow-down past the bottom row must scroll to keep selection visible"
        );
    }

    #[test]
    fn move_up_above_top_of_viewport_retreats_scroll_to_keep_selection_visible() {
        let tmp = TempDir::new().unwrap();
        let mut panel = make_panel_with_hits(&tmp, 100);
        panel.last_inner = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 30,
        };
        panel.selected = 50;
        panel.scroll = 50;
        panel.move_up();
        assert_eq!(panel.selected, 49);
        assert_eq!(
            panel.scroll, 49,
            "arrow-up above the first visible row must scroll back to keep selection visible"
        );
    }

    #[test]
    fn scroll_down_via_wheel_does_not_change_selection() {
        let tmp = TempDir::new().unwrap();
        let mut panel = make_panel_with_hits(&tmp, 100);
        panel.last_inner = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 30,
        };
        panel.selected = 0;
        panel.scroll = 0;
        panel.scroll_down(20);
        assert_eq!(panel.selected, 0, "wheel-scrolling must not move selection");
        assert_eq!(panel.scroll, 20);
    }

    // ---- files-to-include / files-to-exclude glob filtering ----

    #[test]
    fn path_filter_empty_allows_everything() {
        let f = PathFilter::new("", "");
        assert!(f.is_empty());
        let root = Path::new("/proj");
        assert!(f.allows(root, Path::new("/proj/src/main.rs")));
        assert!(f.allows(root, Path::new("/proj/a/b/c.py")));
    }

    #[test]
    fn path_filter_include_bare_glob_matches_at_any_depth() {
        // VS Code: `*.rs` with no slash means `**/*.rs`.
        let f = PathFilter::new("*.rs", "");
        assert!(!f.is_empty());
        let root = Path::new("/proj");
        assert!(f.allows(root, Path::new("/proj/main.rs")));
        assert!(f.allows(root, Path::new("/proj/src/deep/mod.rs")));
        assert!(!f.allows(root, Path::new("/proj/src/app.py")));
    }

    #[test]
    fn path_filter_include_with_multiple_comma_separated_globs() {
        let f = PathFilter::new("*.rs, *.toml", "");
        let root = Path::new("/proj");
        assert!(f.allows(root, Path::new("/proj/main.rs")));
        assert!(f.allows(root, Path::new("/proj/Cargo.toml")));
        assert!(!f.allows(root, Path::new("/proj/notes.md")));
    }

    #[test]
    fn path_filter_exclude_drops_matching_files() {
        let f = PathFilter::new("", "*.lock");
        let root = Path::new("/proj");
        assert!(f.allows(root, Path::new("/proj/main.rs")));
        assert!(!f.allows(root, Path::new("/proj/Cargo.lock")));
    }

    #[test]
    fn path_filter_exclude_beats_include_for_overlapping_paths() {
        // include *.rs, but exclude anything under target/. A target/*.rs file
        // must be dropped because exclude wins.
        let f = PathFilter::new("*.rs", "target/**");
        let root = Path::new("/proj");
        assert!(f.allows(root, Path::new("/proj/src/main.rs")));
        assert!(!f.allows(root, Path::new("/proj/target/debug/build.rs")));
    }

    #[test]
    fn path_filter_directory_glob_matches_relative_to_root() {
        let f = PathFilter::new("src/**", "");
        let root = Path::new("/proj");
        assert!(f.allows(root, Path::new("/proj/src/a/b.rs")));
        assert!(!f.allows(root, Path::new("/proj/tests/c.rs")));
    }

    #[test]
    fn search_streaming_filtered_honours_include_glob() {
        let tmp = TempDir::new().unwrap();
        write(&tmp.path().join("keep.rs"), "needle here\n");
        write(&tmp.path().join("drop.txt"), "needle here too\n");
        let cancel = Arc::new(AtomicBool::new(false));
        let filter = PathFilter::new("*.rs", "");
        let collected = Arc::new(std::sync::Mutex::new(Vec::new()));
        let c2 = collected.clone();
        search_workspace_streaming_filtered(
            tmp.path(),
            "needle",
            SearchOpts::default(),
            &cancel,
            &HashSet::new(),
            &filter,
            move |batch| c2.lock().unwrap().extend(batch),
        );
        let hits = Arc::try_unwrap(collected).unwrap().into_inner().unwrap();
        assert_eq!(
            hits.len(),
            1,
            "only the .rs file should match the include glob"
        );
        assert!(hits[0].path.ends_with("keep.rs"));
    }

    #[test]
    fn search_streaming_filtered_honours_exclude_glob() {
        let tmp = TempDir::new().unwrap();
        write(&tmp.path().join("a.rs"), "needle\n");
        write(&tmp.path().join("b.rs"), "needle\n");
        let cancel = Arc::new(AtomicBool::new(false));
        let filter = PathFilter::new("", "b.rs");
        let collected = Arc::new(std::sync::Mutex::new(Vec::new()));
        let c2 = collected.clone();
        search_workspace_streaming_filtered(
            tmp.path(),
            "needle",
            SearchOpts::default(),
            &cancel,
            &HashSet::new(),
            &filter,
            move |batch| c2.lock().unwrap().extend(batch),
        );
        let hits = Arc::try_unwrap(collected).unwrap().into_inner().unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].path.ends_with("a.rs"), "b.rs must be excluded");
    }

    #[test]
    fn search_worker_loop_applies_set_filter_to_subsequent_query() {
        let tmp = TempDir::new().unwrap();
        write(&tmp.path().join("keep.rs"), "needle\n");
        write(&tmp.path().join("skip.md"), "needle\n");
        let (q_tx, q_rx) = std::sync::mpsc::channel::<SearchRequest>();
        let (e_tx, e_rx) = std::sync::mpsc::channel::<SearchEvent>();
        let root = tmp.path().to_path_buf();
        let join = std::thread::spawn(move || search_worker_loop(root, q_rx, e_tx));
        q_tx.send(SearchRequest::SetFilter("*.rs".into(), String::new()))
            .unwrap();
        q_tx.send(SearchRequest::Query("needle".into(), SearchOpts::default()))
            .unwrap();
        let (_, hits, done) = collect_until_done(&e_rx, std::time::Duration::from_secs(2));
        assert!(done);
        assert_eq!(hits.len(), 1, "filter must restrict the scan to *.rs");
        assert!(hits[0].path.ends_with("keep.rs"));
        drop(q_tx);
        join.join().unwrap();
    }

    // ---- replace ----

    #[test]
    fn replace_in_text_literal_replaces_all_occurrences() {
        let (out, n) =
            replace_in_text("foo bar foo baz foo", "foo", "X", SearchOpts::default()).unwrap();
        assert_eq!(n, 3);
        assert_eq!(out, "X bar X baz X");
    }

    #[test]
    fn replace_in_text_no_match_returns_zero_and_unchanged() {
        let (out, n) = replace_in_text("hello world", "zzz", "X", SearchOpts::default()).unwrap();
        assert_eq!(n, 0);
        assert_eq!(out, "hello world");
    }

    #[test]
    fn replace_in_text_literal_dollar_is_inserted_verbatim() {
        // A literal replacement containing `$` must not be parsed as a capture
        // reference; it should appear as-is.
        let (out, n) = replace_in_text("price=AMT", "AMT", "$5", SearchOpts::default()).unwrap();
        assert_eq!(n, 1);
        assert_eq!(out, "price=$5");
    }

    #[test]
    fn replace_in_text_case_insensitive_by_default() {
        let (out, n) = replace_in_text("Foo foo FOO", "foo", "x", SearchOpts::default()).unwrap();
        assert_eq!(n, 3);
        assert_eq!(out, "x x x");
    }

    #[test]
    fn replace_in_text_case_sensitive_only_exact_case() {
        let opts = SearchOpts {
            case_sensitive: true,
            ..Default::default()
        };
        let (out, n) = replace_in_text("Foo foo FOO", "foo", "x", opts).unwrap();
        assert_eq!(n, 1);
        assert_eq!(out, "Foo x FOO");
    }

    #[test]
    fn replace_in_text_whole_word_skips_substrings() {
        let opts = SearchOpts {
            whole_word: true,
            ..Default::default()
        };
        let (out, n) = replace_in_text("cat category cat", "cat", "dog", opts).unwrap();
        assert_eq!(n, 2);
        assert_eq!(out, "dog category dog");
    }

    #[test]
    fn replace_in_text_regex_with_capture_reference() {
        let opts = SearchOpts {
            use_regex: true,
            ..Default::default()
        };
        let (out, n) = replace_in_text("a1 b2 c3", r"([a-z])(\d)", "$2$1", opts).unwrap();
        assert_eq!(n, 3);
        assert_eq!(out, "1a 2b 3c");
    }

    #[test]
    fn replace_in_text_invalid_regex_returns_none() {
        let opts = SearchOpts {
            use_regex: true,
            ..Default::default()
        };
        assert!(replace_in_text("anything", "[unclosed", "x", opts).is_none());
    }

    #[test]
    fn replace_in_file_writes_only_when_a_match_is_replaced() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("a.txt");
        write(&p, "old value old\n");
        let n = replace_in_file(&p, "old", "new", SearchOpts::default()).unwrap();
        assert_eq!(n, 2);
        assert_eq!(fs::read_to_string(&p).unwrap(), "new value new\n");
    }

    #[test]
    fn replace_all_on_disk_replaces_across_distinct_hit_files() {
        let tmp = TempDir::new().unwrap();
        write(&tmp.path().join("a.txt"), "foo foo\n");
        write(&tmp.path().join("b.txt"), "foo\n");
        let mut panel = SearchPanel::new(tmp.path().to_path_buf());
        panel.query = "foo".into();
        panel.replace = "bar".into();
        panel.run_query();
        // a.txt contributes 2 hits, b.txt 1 hit (3 hit rows, 2 files).
        let total = panel.replace_all_on_disk();
        assert_eq!(total, 3, "all three matches across both files replaced");
        assert_eq!(
            fs::read_to_string(tmp.path().join("a.txt")).unwrap(),
            "bar bar\n"
        );
        assert_eq!(
            fs::read_to_string(tmp.path().join("b.txt")).unwrap(),
            "bar\n"
        );
    }

    // ---- panel field focus + control hit-testing ----

    #[test]
    fn toggle_replace_expands_and_collapses_the_replace_row() {
        let tmp = TempDir::new().unwrap();
        let mut panel = SearchPanel::new(tmp.path().to_path_buf());
        assert!(!panel.replace_open);
        panel.toggle_replace();
        assert!(panel.replace_open);
        // Focus the replace field, then collapse: focus falls back to Query.
        panel.focus_field(SearchField::Replace);
        panel.toggle_replace();
        assert!(!panel.replace_open);
        assert_eq!(panel.field, SearchField::Query);
    }

    #[test]
    fn tab_cycles_only_through_visible_fields() {
        let tmp = TempDir::new().unwrap();
        let mut panel = SearchPanel::new(tmp.path().to_path_buf());
        // Only Query visible: Tab stays on Query.
        panel.focus_next_field();
        assert_eq!(panel.field, SearchField::Query);
        // Expand replace + details: Query -> Replace -> Include -> Exclude -> Query.
        panel.replace_open = true;
        panel.details_open = true;
        panel.focus_next_field();
        assert_eq!(panel.field, SearchField::Replace);
        panel.focus_next_field();
        assert_eq!(panel.field, SearchField::Include);
        panel.focus_next_field();
        assert_eq!(panel.field, SearchField::Exclude);
        panel.focus_next_field();
        assert_eq!(panel.field, SearchField::Query);
    }

    #[test]
    fn type_char_lands_in_the_focused_field() {
        let tmp = TempDir::new().unwrap();
        let mut panel = SearchPanel::new(tmp.path().to_path_buf());
        panel.replace_open = true;
        panel.type_char('a');
        assert_eq!(panel.query, "a");
        assert_eq!(panel.replace, "");
        panel.focus_field(SearchField::Replace);
        panel.type_char('b');
        assert_eq!(panel.query, "a");
        assert_eq!(panel.replace, "b");
    }

    #[test]
    fn focused_query_field_exposes_a_caret_and_paints_no_static_block() {
        use ratatui::buffer::Buffer;
        let tmp = TempDir::new().unwrap();
        let mut panel = SearchPanel::new(tmp.path().to_path_buf());
        panel.focused = true;
        panel.type_char('a');
        panel.type_char('b');
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 20,
        };
        let mut buf = Buffer::empty(area);
        Widget::render(&mut panel, area, &mut buf);
        // The caret sits on the query row so the app can drive the host's
        // blinking hardware caret there, matching the Source Control commit
        // box, instead of painting a static block that never blinks.
        let (cx, cy) = panel
            .cursor_screen_pos()
            .expect("a focused query field must expose a caret position");
        let r = panel.query_input_rect;
        assert_eq!(cy, r.y, "caret rides the query row");
        assert!(cx > r.x, "caret sits past the magnifier-led text start");
        let row: String = (r.x..r.right())
            .map(|x| buf[(x, r.y)].symbol().to_string())
            .collect();
        assert!(
            !row.contains('\u{2588}'),
            "the static full-block cursor must be gone now that the hardware caret blinks; row was {row:?}"
        );
    }

    #[test]
    fn an_unfocused_search_panel_exposes_no_caret() {
        use ratatui::buffer::Buffer;
        let tmp = TempDir::new().unwrap();
        let mut panel = SearchPanel::new(tmp.path().to_path_buf());
        panel.focused = false;
        panel.type_char('x');
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 20,
        };
        let mut buf = Buffer::empty(area);
        Widget::render(&mut panel, area, &mut buf);
        assert!(
            panel.cursor_screen_pos().is_none(),
            "an unfocused panel must not claim the hardware caret"
        );
    }

    #[test]
    fn backspace_removes_from_the_focused_field() {
        let tmp = TempDir::new().unwrap();
        let mut panel = SearchPanel::new(tmp.path().to_path_buf());
        panel.details_open = true;
        panel.focus_field(SearchField::Include);
        panel.insert_str("*.rs");
        assert_eq!(panel.include, "*.rs");
        assert!(panel.backspace());
        assert_eq!(panel.include, "*.r");
        assert_eq!(panel.query, "");
    }

    #[test]
    fn chevron_ellipsis_and_replace_all_hit_tests_after_render() {
        use ratatui::buffer::Buffer;
        let tmp = TempDir::new().unwrap();
        let mut panel = SearchPanel::new(tmp.path().to_path_buf());
        panel.focused = true;
        panel.replace_open = true;
        panel.details_open = true;
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 24,
        };
        let mut buf = Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
        assert!(panel.chevron_at(panel.chevron_x, panel.chevron_y));
        assert!(panel.ellipsis_at(panel.ellipsis_x, panel.ellipsis_y));
        assert!(panel.replace_all_at(panel.replace_all_x, panel.replace_all_y));
        // Clicking the query input row focuses the Query field.
        let q = panel.query_input_rect;
        assert_eq!(panel.field_at(q.x, q.y), Some(SearchField::Query));
        let r = panel.replace_input_rect;
        assert_eq!(panel.field_at(r.x, r.y), Some(SearchField::Replace));
    }

    #[test]
    fn expanded_rows_push_results_start_down() {
        use ratatui::buffer::Buffer;
        let tmp = TempDir::new().unwrap();
        let mut panel = SearchPanel::new(tmp.path().to_path_buf());
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 30,
        };
        let mut buf = Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
        let collapsed_offset = panel.results_start_offset;
        panel.replace_open = true;
        panel.details_open = true;
        let mut buf2 = Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut panel, area, &mut buf2);
        assert!(
            panel.results_start_offset > collapsed_offset,
            "expanding replace + include/exclude must push results down (was {collapsed_offset}, now {})",
            panel.results_start_offset
        );
        // Single-row fields, each group separated by a hairline row: a divider
        // + replace field (2 rows), plus a divider + caption + field (3) each
        // for include and exclude = 8 rows.
        assert_eq!(panel.results_start_offset, collapsed_offset + 8);
    }

    #[test]
    fn collapsed_panel_results_offset_is_five() {
        use ratatui::buffer::Buffer;
        let tmp = TempDir::new().unwrap();
        let mut panel = SearchPanel::new(tmp.path().to_path_buf());
        let area = Rect {
            x: 0,
            y: 0,
            width: 60,
            height: 20,
        };
        let mut buf = Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
        // header(0) + blank(1) + query(2) + separator(3) + caption(4) → results
        // begin at row 5. The single-row query field saves two rows versus the
        // old 3-row box (which started results at 7).
        assert_eq!(
            panel.results_start_offset, 5,
            "collapsed layout starts results at the 5-row offset"
        );
    }
}
