use globset::{Glob, GlobSet, GlobSetBuilder};
use grep_matcher::{LineMatchKind, LineTerminator, Match, Matcher, NoCaptures, NoError};
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{BinaryDetection, MmapChoice, Searcher, SearcherBuilder, Sink, SinkMatch};
use ignore::{WalkBuilder, WalkState};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Widget,
};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use unicode_width::UnicodeWidthStr;

use crate::glob_list::split_globs;

mod matcher;
mod path_filter;
mod walk;
pub(crate) use matcher::Pattern;
use matcher::*;
pub use path_filter::PathFilter;
use path_filter::*;
pub(crate) use walk::as_typed;
pub use walk::line_may_match;
pub use walk::search_workspace_streaming_filtered;
use walk::*;

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
fn draw_field_divider(buf: &mut Buffer, x0: u16, x1: u16, y: u16, theme: crate::theme::Theme) {
    let style = Style::default().fg(theme.ui(FIELD_BAR_IDLE));
    for x in x0..=x1 {
        buf.set_string(x, y, "─", style);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchHit {
    pub path: PathBuf,
    /// 1-indexed line number, the way humans / editors talk about lines.
    pub line_no: usize,
    /// The matched line without its line break, length capped, and with
    /// any indent before the first match trimmed.
    pub line_text: String,
    /// How many times the query matches on this line, counted as Replace All
    /// replaces them. The header counts these, as VS Code does, not lines
    /// (#860).
    pub matches: usize,
}

/// Mode toggles that drive `search_workspace`. Mirror VS Code's three
/// search input toggles, in the same left-to-right order:
///   - `case_sensitive` (Aa)
///   - `whole_word` (ab with underline)
///   - `use_regex` (.*)
///   - `preserve_case` (AB, on the replace row): each replacement takes the
///     case of the text it replaces (#1612)
///
/// All-false matches the original case-insensitive substring behaviour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct SearchOpts {
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub use_regex: bool,
    pub preserve_case: bool,
}

impl SearchOpts {
    /// Whether `other` finds the same hits: Preserve Case shapes the
    /// replacements only, so a worker's results under either value of it
    /// still belong to the query.
    pub fn same_matching(&self, other: &SearchOpts) -> bool {
        self.case_sensitive == other.case_sensitive
            && self.whole_word == other.whole_word
            && self.use_regex == other.use_regex
    }
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
    // Lowercasing that changes any character's byte length (`ẞ`, `İ`, the
    // Kelvin sign) shifts every offset after it, even when the total length
    // happens to come out equal: those lines go through the regex path,
    // whose case folding matches on the original bytes.
    let shifts = |t: &str| {
        t.chars()
            .any(|c| c.to_lowercase().map(char::len_utf8).sum::<usize>() != c.len_utf8())
    };
    if !opts.case_sensitive && (shifts(line) || shifts(needle)) {
        let literal = SearchOpts {
            use_regex: true,
            ..opts
        };
        return split_for_highlight_regex(line, &regex::escape(needle), literal);
    }
    if search_for.is_empty() {
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
    let Some(re) = Pattern::for_find(needle, opts) else {
        return vec![(line.to_string(), false)];
    };
    let mut out: Vec<(String, bool)> = Vec::new();
    let mut last = 0usize;
    for (start, end) in re.spans(line) {
        if start > last {
            out.push((line[last..start].to_string(), false));
        }
        // Zero-width matches (`\b`, `^`) still count as match positions —
        // the replace preview inserts the expansion there, mirroring what
        // `replace_in_text` writes — and `last` must advance regardless, or
        // the text before the NEXT match re-emits and the row doubles
        // ("foofoo" for `\b` on "foo"; #124 review).
        if end == start {
            out.push((String::new(), true));
            last = end;
            continue;
        }
        out.push((line[start..end].to_string(), true));
        last = end;
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
    /// Replace the whole root set (multi-root workspaces, #147): the scan
    /// walks every root, so hits come from all workspace folders.
    SetRoots(Vec<PathBuf>),
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
    root: PathBuf,
    rx: std::sync::mpsc::Receiver<SearchRequest>,
    tx: std::sync::mpsc::Sender<SearchEvent>,
) {
    let mut roots: Vec<PathBuf> = vec![root];
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
                roots = vec![new_root];
                continue;
            }
            SearchRequest::SetRoots(new_roots) if !new_roots.is_empty() => {
                roots = new_roots;
                continue;
            }
            SearchRequest::SetRoots(_) => continue,
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
                Ok(SearchRequest::SetRoot(new_root)) => roots = vec![new_root],
                Ok(SearchRequest::SetRoots(new_roots)) if !new_roots.is_empty() => {
                    roots = new_roots
                }
                Ok(SearchRequest::SetRoots(_)) => {}
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
        if as_typed(&query).is_empty() {
            if tx.send(SearchEvent::Done(query, opts)).is_err() {
                return;
            }
            continue;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_for_thread = cancel.clone();
        let tx_for_thread = tx.clone();
        let roots_for_thread = roots.clone();
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
                // Filter globs are root-relative: judge against the root
                // that owns the buffer's path (the first containing one;
                // roots rarely nest and the glob answer matches VS Code's
                // folder-relative include semantics either way).
                let owner = roots_for_thread
                    .iter()
                    .find(|r| path.starts_with(r))
                    .unwrap_or(&roots_for_thread[0]);
                if !filter_for_thread.is_empty() && !filter_for_thread.allows(owner, path) {
                    continue;
                }
                collect_matches_in_text(path, content, &query_for_thread, opts, &mut buf_hits);
            }
            if !buf_hits.is_empty() {
                let _ = tx_for_emit.send(SearchEvent::Hits(q_for_emit.clone(), opts, buf_hits));
            }
            // Every workspace root in order: one balanced Hits stream, one
            // Done after the last root (#147).
            for scan_root in &roots_for_thread {
                if cancel_for_thread.load(Ordering::Relaxed) {
                    break;
                }
                let tx_for_root = tx_for_emit.clone();
                let q_for_root = q_for_emit.clone();
                search_workspace_streaming_filtered(
                    scan_root,
                    &query_for_thread,
                    opts,
                    &cancel_for_thread,
                    &skip,
                    &filter_for_thread,
                    move |batch| {
                        let _ =
                            tx_for_root.send(SearchEvent::Hits(q_for_root.clone(), opts, batch));
                    },
                );
            }
            let _ = tx_for_thread.send(SearchEvent::Done(query_for_thread, opts));
        });
        current = Some((handle, cancel));
    }
    if let Some((handle, cancel)) = current.take() {
        cancel.store(true, Ordering::Relaxed);
        let _ = handle.join();
    }
}

/// Compile the search query + `SearchOpts` into a `regex::Regex`, mirroring
/// the pattern construction used by `split_for_highlight_regex` so replace
/// matches exactly what the highlighter shows. In literal (non-regex) mode the
/// query is escaped so metacharacters are matched verbatim. Returns `None` for
/// an empty query or an uncompilable pattern.
fn build_replace_regex(query: &str, opts: SearchOpts) -> Option<Pattern> {
    let q = as_typed(query);
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
    // Applied to one line at a time (see `replace_in_text`), as the search
    // matches, so `^` and `$` are that line's ends. Whole-word is checked on
    // each match by the search's own rule, not `\b`, which never matches
    // beside a query that starts or ends with punctuation (`$var`, `@x`).
    pattern.push_str(&body);
    Pattern::new(&pattern)
}

/// Replace the matches in ONE line (no terminator), returning the new text
/// and how many were replaced. Matches are found by the SAME grep matcher
/// the search used, so replace touches exactly the hits it listed: the
/// `regex` crate's leftmost-first choice missed a whole-word match that
/// only a longer alternative (`foo|foobar` on `foobar`) satisfies. `full`
/// (the pattern anchored to a whole match) only expands capture groups.
/// An empty match is replaced unless it touches the previous replacement,
/// as `Regex::replace_all` does, so `^` inserts at every line start.
fn replace_in_line(
    line: &str,
    matcher: &SearchMatcher,
    full: Option<&Pattern>,
    replacement: &str,
    opts: SearchOpts,
) -> (String, usize) {
    let mut out = String::with_capacity(line.len());
    let mut last = 0;
    let mut pos = 0;
    let mut count = 0;
    let mut prev_end: Option<usize> = None;
    while pos <= line.len() {
        let Ok(Some(m)) = matcher.find_at(line.as_bytes(), pos) else {
            break;
        };
        let (start, end) = (m.start(), m.end());
        if !line.is_char_boundary(start) || !line.is_char_boundary(end) {
            break;
        }
        let adjacent_empty = start == end && prev_end == Some(start);
        if !adjacent_empty {
            out.push_str(&line[last..start]);
            let expanded = full
                .filter(|_| opts.use_regex)
                .and_then(|re| re.expand_at(line, start, end, replacement))
                .unwrap_or_else(|| replacement.to_string());
            if opts.preserve_case {
                out.push_str(&preserve_case(&line[start..end], &expanded));
            } else {
                out.push_str(&expanded);
            }
            count += 1;
            last = end;
            prev_end = Some(end);
        }
        pos = if start == end {
            match line[end..].chars().next() {
                Some(c) => end + c.len_utf8(),
                None => break,
            }
        } else {
            end
        };
    }
    out.push_str(&line[last..]);
    (out, count)
}

/// How many matches ONE line holds, by exactly the rule `replace_in_line`
/// replaces them with, so the Search header's result count and the Replace
/// All confirmation always agree (#860).
fn count_in_line(line: &str, matcher: &SearchMatcher) -> usize {
    replace_in_line(line, matcher, None, "", SearchOpts::default()).1
}

/// The leading context a result row keeps before its first match when that
/// match, and `need` cells with it (the match plus any replace preview), would
/// not fit in `avail` cells after `lead`; `None` when it fits, so a row whose
/// match shows is drawn exactly as its line reads (#860). The row then opens
/// on the last cells of `lead`, half the room the match leaves, so context
/// shows on both sides. Cut plainly, with no "…" marker: croft draws none.
fn trim_leading_context(lead: &str, avail: usize, need: usize) -> Option<String> {
    use unicode_width::UnicodeWidthChar;
    if lead.width() + need <= avail {
        return None;
    }
    let keep = avail.saturating_sub(need) / 2;
    let mut width = 0;
    let mut start = lead.len();
    for (i, c) in lead.char_indices().rev() {
        width += c.width().unwrap_or(0);
        if width > keep {
            break;
        }
        start = i;
    }
    Some(lead[start..].to_string())
}

/// `replacement` made safe for `Captures::expand`, read the way
/// VS Code reads it. A numbered reference is braced (`$1_old` to
/// `${1}_old`): the regex crate reads `$1_old` as a group NAMED `1_old`,
/// which expands to nothing and deletes the match, where VS Code reads group
/// 1 then `_old`. `$&` is the whole match and `$$` a literal dollar. A
/// `$NAME` or `${NAME}` expands only when `is_group` knows that name;
/// any other `$` is text (#1399), so a shell, PHP or Perl variable such as
/// `$PREFIX` survives instead of expanding to nothing. A `$N` or `${N}`
/// with no group N is text too (#1636); `groups` counts them, group 0
/// included.
fn brace_refs(replacement: &str, groups: usize, is_group: impl Fn(&str) -> bool) -> String {
    let is_name = |c: &char| c.is_ascii_alphanumeric() || *c == '_';
    let mut out = String::with_capacity(replacement.len());
    let mut rest = replacement;
    while let Some(at) = rest.find('$') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let digits = after.len() - after.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        let name_len = after
            .chars()
            .take_while(is_name)
            .map(char::len_utf8)
            .sum::<usize>();
        let braced = after
            .strip_prefix('{')
            .and_then(|b| b.find('}').map(|end| &b[..end]));
        let (expansion, used) = if after.starts_with('$') {
            (String::from("$$"), 1)
        } else if after.starts_with('&') {
            (String::from("${0}"), 1)
        } else if digits > 0 {
            numbered_ref(&after[..digits], groups)
        } else if let Some(name) = braced {
            let group = if !name.is_empty() && name.chars().all(|c| c.is_ascii_digit()) {
                name.parse::<usize>().is_ok_and(|g| g < groups)
            } else {
                is_group(name)
            };
            if group {
                (format!("${{{name}}}"), name.len() + 2)
            } else {
                (String::from("$$"), 0)
            }
        } else if name_len > 0 && is_group(&after[..name_len]) {
            (format!("${{{}}}", &after[..name_len]), name_len)
        } else {
            (String::from("$$"), 0)
        };
        out.push_str(&expansion);
        rest = &after[used..];
    }
    out.push_str(rest);
    out
}

/// `$` then `digits`, read the way VS Code reads it: the longest prefix of
/// the digits that numbers a group of the pattern, the rest left as text, so
/// `$10` with one group is group 1 then `0`. With no such prefix the `$` is
/// text (#1636): a price like `$5` or a shell `$2` survives instead of
/// expanding to nothing. Returns the expansion and the digits it used.
fn numbered_ref(digits: &str, groups: usize) -> (String, usize) {
    (1..=digits.len())
        .rev()
        .find(|&n| digits[..n].parse::<usize>().is_ok_and(|g| g < groups))
        .map_or((String::from("$$"), 0), |n| {
            (format!("${{{}}}", &digits[..n]), n)
        })
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
    let q = as_typed(query);
    let matcher = build_matcher(q, opts)?;
    // The pattern itself, for `$1` expansion only. Not wrapped in anything:
    // a `(?x)` query's trailing `# comment` swallowed a closing `)$`.
    let full = if opts.use_regex {
        let case = if opts.case_sensitive { "" } else { "(?i)" };
        Some(Pattern::new(&format!("{case}{q}"))?)
    } else {
        None
    };
    // Line by line, exactly as the search matched: over the whole file,
    // `[^;]*`, `\s` or `\W` ran across line breaks, so Replace All deleted
    // lines the search had never listed.
    let mut out = String::with_capacity(content.len());
    let mut count = 0;
    for piece in content.split_inclusive('\n') {
        let body = piece
            .strip_suffix("\r\n")
            .or_else(|| piece.strip_suffix('\n'))
            .unwrap_or(piece);
        let (line, n) = replace_in_line(body, &matcher, full.as_ref(), replacement, opts);
        out.push_str(&line);
        out.push_str(&piece[body.len()..]);
        count += n;
    }
    Some((out, count))
}

/// `replacement` in the case of `matched`, VS Code's Preserve Case rules
/// (`buildReplaceStringWithCasePreserved`): an all-upper match gives an
/// all-upper replacement, all-lower gives all-lower, and a capitalised one a
/// capitalised one. When both split into the same number of `-` (or `_`)
/// parts, each part is shaped by its own, so `User-Name` over `account-id`
/// gives `Account-Id`. Anything else goes in as typed (#1612).
pub fn preserve_case(matched: &str, replacement: &str) -> String {
    if matched.is_empty() {
        return replacement.to_string();
    }
    let splits = |sep: char| {
        matched.contains(sep)
            && replacement.contains(sep)
            && matched.split(sep).count() == replacement.split(sep).count()
    };
    let (hyphens, underscores) = (splits('-'), splits('_'));
    if hyphens != underscores {
        let sep = if hyphens { '-' } else { '_' };
        return matched
            .split(sep)
            .zip(replacement.split(sep))
            .map(|(m, r)| preserve_case(m, r))
            .collect::<Vec<_>>()
            .join(&sep.to_string());
    }
    if matched.to_uppercase() == matched {
        return replacement.to_uppercase();
    }
    if matched.to_lowercase() == matched {
        return replacement.to_lowercase();
    }
    let (Some(first), Some(lead)) = (matched.chars().next(), replacement.chars().next()) else {
        return replacement.to_string();
    };
    let rest = &replacement[lead.len_utf8()..];
    if first.is_uppercase() {
        lead.to_uppercase().chain(rest.chars()).collect()
    } else if first.is_lowercase() {
        lead.to_lowercase().chain(rest.chars()).collect()
    } else {
        replacement.to_string()
    }
}

/// Expand the replacement for ONE matched slice — regex capture references
/// (`$1`, `${name}`) honoured in regex mode, verbatim otherwise, then shaped
/// to the match's case under Preserve Case (#1612). Drives the result rows'
/// inline old→new preview (#123).
pub fn expand_replacement(
    matched: &str,
    query: &str,
    replacement: &str,
    opts: SearchOpts,
) -> String {
    let expanded = match build_replace_regex(query, opts) {
        Some(re) if opts.use_regex => re
            .spans(matched)
            .first()
            .and_then(|&(start, end)| {
                let expanded = re.expand_at(matched, start, end, replacement)?;
                Some(format!(
                    "{}{expanded}{}",
                    &matched[..start],
                    &matched[end..]
                ))
            })
            .unwrap_or_else(|| matched.to_string()),
        _ => replacement.to_string(),
    };
    if opts.preserve_case {
        preserve_case(matched, &expanded)
    } else {
        expanded
    }
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
    // Written aside and renamed in, keeping the file's mode: written in
    // place, a failed write (a full disk) left the file truncated.
    crate::prefs::replace_file_contents(path, new_content.as_bytes()).ok()?;
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
    /// Every workspace root (#147); `root` stays the primary. Display
    /// strips a hit against its OWNING root, prefixed with the root's
    /// name when more than one folder is open.
    pub roots: Vec<PathBuf>,
    pub opts: SearchOpts,
    /// `(query, opts, error)` behind `query_error`, so the caption doesn't
    /// rebuild the regex every frame.
    query_error_memo: Option<(String, SearchOpts, Option<String>)>,
    /// Per-toggle absolute screen column captured by the most recent render.
    /// Used by `App::handle_mouse` to map clicks on `Aa`, `ab`, `.*` into
    /// flag flips. Zero means the row was too narrow to render that toggle.
    pub toggle_case_x: u16,
    pub toggle_word_x: u16,
    pub toggle_regex_x: u16,
    /// The replace row's `AB` (Preserve Case) chip, 2 cells from
    /// `toggle_preserve_x` on `toggle_preserve_y`; x 0 while the row is hidden.
    pub toggle_preserve_x: u16,
    pub toggle_preserve_y: u16,
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
    /// True once the worker's `Done` for the CURRENT query has drained:
    /// Replace All refuses while hits are still streaming, or files found
    /// after confirmation would be silently missed (#124 review).
    pub complete: bool,
    /// Selection range within the currently focused field's text.
    pub field_selection: Option<(usize, usize)>,
    /// Caret of each field as a char index (#1729), indexed by
    /// [`SearchField::index`]. `None` is "at the end of the text, wherever
    /// that is now": a field nobody moved a caret in follows its text when
    /// the text is assigned from outside. Reads clamp, so a mid-text caret
    /// left past a shortened text falls back to the end too.
    carets: [Option<usize>; 4],
    /// Where each field's text starts on screen and the first char it shows,
    /// captured each render so a click can place the caret.
    field_origin: [(u16, usize); 4],

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
    /// The "use the project's exclusions" checkbox row under the exclude box
    /// (#1345); zero-sized when the project excludes nothing.
    pub project_exclude_rect: Rect,
    /// The project's exclusion globs (`files_exclude` then
    /// `search_exclude`, #1345), applied on top of the exclude box while
    /// `use_exclude_settings` is on.
    pub project_exclude: Vec<String>,
    /// VS Code's "Use Exclude Settings and Ignore Files" toggle: off, a
    /// search covers the folders the project hides.
    pub use_exclude_settings: bool,
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

impl SearchField {
    fn index(self) -> usize {
        match self {
            SearchField::Query => 0,
            SearchField::Replace => 1,
            SearchField::Include => 2,
            SearchField::Exclude => 3,
        }
    }
}

impl SearchPanel {
    pub fn new(root: PathBuf) -> Self {
        Self {
            roots: vec![root.clone()],
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
            query_error_memo: None,
            toggle_case_x: 0,
            toggle_word_x: 0,
            toggle_regex_x: 0,
            toggle_preserve_x: 0,
            toggle_preserve_y: 0,
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
            complete: false,
            field_selection: None,
            carets: [None; 4],
            field_origin: [(0, 0); 4],
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
            project_exclude_rect: Rect::default(),
            project_exclude: Vec::new(),
            use_exclude_settings: true,
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

    /// Replace the query and both filter lists wholesale (the terminal-seed
    /// path). Text replacement invalidates any byte-range selection into the
    /// old text — a stale range into shorter text panics on the next edit —
    /// and focus moves to the query the caller just filled.
    pub fn seed(&mut self, query: String, include: String, exclude: String) {
        self.query = query;
        self.include = include;
        self.exclude = exclude;
        self.field = SearchField::Query;
        self.selection = None;
        self.field_selection = None;
        for f in [
            SearchField::Query,
            SearchField::Include,
            SearchField::Exclude,
        ] {
            self.carets[f.index()] = None;
        }
    }

    /// The focused field's caret as a char index into its text.
    pub fn caret(&self) -> usize {
        self.field_caret(self.field)
    }

    fn field_text(&self, field: SearchField) -> &str {
        match field {
            SearchField::Query => &self.query,
            SearchField::Replace => &self.replace,
            SearchField::Include => &self.include,
            SearchField::Exclude => &self.exclude,
        }
    }

    fn field_caret(&self, field: SearchField) -> usize {
        let text = self.field_text(field);
        crate::widgets::line_edit::clamp(text, self.carets[field.index()].unwrap_or(usize::MAX))
    }

    /// Record `at` as `field`'s caret; the end of the text is stored as
    /// "follow the end".
    fn store_caret(&mut self, field: SearchField, at: usize) {
        let len = crate::widgets::line_edit::char_len(self.field_text(field));
        self.carets[field.index()] = (at < len).then_some(at);
    }

    fn set_caret(&mut self, at: usize) {
        self.store_caret(self.field, at);
    }

    /// Move the focused field's caret with `step(text, caret)`, dropping the
    /// field's selection as any caret key does.
    pub fn move_caret(&mut self, step: impl FnOnce(&str, usize) -> usize) {
        let at = step(self.active_text(), self.caret());
        self.set_active_selection(None);
        self.set_caret(at);
    }

    /// Put the focused field's caret under screen column `col`, from a click
    /// in it (the text origin is the one the last render captured).
    pub fn click_caret(&mut self, col: u16) {
        let (text_x, start) = self.field_origin[self.field.index()];
        let at = crate::widgets::line_edit::char_at_cell(
            self.active_text(),
            start,
            usize::from(col.saturating_sub(text_x)),
        );
        self.set_active_selection(None);
        self.set_caret(at);
    }

    /// Apply `edit(text, caret) -> (text, caret)` to the focused field.
    fn edit_active(&mut self, edit: impl FnOnce(&str, usize) -> (String, usize)) -> bool {
        let (text, caret) = edit(self.active_text(), self.caret());
        let changed = text != self.active_text();
        *self.active_text_mut() = text;
        self.set_caret(caret);
        changed
    }

    pub fn select_all_query(&mut self) {
        if self.query.is_empty() {
            self.selection = None;
        } else {
            self.selection = Some((0, self.query.len()));
        }
        self.carets[SearchField::Query.index()] = None;
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
        let at = crate::widgets::line_edit::char_of(&self.query, a);
        self.store_caret(SearchField::Query, at);
        true
    }

    pub fn selection_text(&self) -> String {
        match self.selection_range() {
            Some((a, b)) if a < b => self.query[a..b].to_string(),
            _ => String::new(),
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
        self.set_caret(0);
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
        let typed = c.to_string();
        self.edit_active(|t, at| crate::widgets::line_edit::insert_at(t, at, &typed));
    }

    /// Backspace one char (or the selection) from the focused field. Returns
    /// true when the text changed.
    pub fn backspace(&mut self) -> bool {
        if self.delete_active_selection() {
            return true;
        }
        self.edit_active(|t, at| crate::widgets::line_edit::delete_before(t, at, false))
    }

    /// Delete the cluster (or, with `word`, the run) before the caret, or the
    /// selection. Returns true when the text changed.
    pub fn backspace_by(&mut self, word: bool) -> bool {
        if self.delete_active_selection() {
            return true;
        }
        self.edit_active(|t, at| crate::widgets::line_edit::delete_before(t, at, word))
    }

    /// Delete ahead of the caret (the Delete key), or the selection.
    /// Returns true when the text changed.
    pub fn delete_forward(&mut self, word: bool) -> bool {
        if self.delete_active_selection() {
            return true;
        }
        self.edit_active(|t, at| crate::widgets::line_edit::delete_at(t, at, word))
    }

    /// Insert `s` into the focused field, replacing the selection and stripping
    /// newlines (all Search inputs are single-line).
    pub fn insert_str(&mut self, s: &str) {
        self.delete_active_selection();
        self.edit_active(|t, at| crate::widgets::line_edit::insert_at(t, at, s));
    }

    /// Select the whole text of the focused field.
    pub fn select_all_active(&mut self) {
        let len = self.active_text().len();
        if len == 0 {
            self.set_active_selection(None);
        } else {
            self.set_active_selection(Some((0, len)));
        }
        let end = crate::widgets::line_edit::char_len(self.active_text());
        self.set_caret(end);
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
        let at = crate::widgets::line_edit::char_of(self.active_text(), a);
        self.set_caret(at);
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
        if self.replace_open {
            // VS Code focuses the revealed input; leaving focus in the
            // query made the next keystrokes silently extend the search
            // (#123).
            self.focus_field(SearchField::Replace);
        } else if self.field == SearchField::Replace {
            self.focus_field(SearchField::Query);
        }
    }

    /// Expand the Replace row if it is collapsed and focus it: VS Code's
    /// Replace in Files (#860). Unlike the chevron, it never collapses.
    pub fn open_replace(&mut self) {
        self.replace_open = true;
        self.focus_field(SearchField::Replace);
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
        PathFilter::new(&self.include, &self.effective_exclude())
    }

    /// The exclude box plus, while `use_exclude_settings` is on, the
    /// project's exclusion globs (#1345): what the next scan leaves out.
    pub fn effective_exclude(&self) -> String {
        if !self.use_exclude_settings || self.project_exclude.is_empty() {
            return self.exclude.clone();
        }
        let mut parts: Vec<&str> = Vec::new();
        if !self.exclude.trim().is_empty() {
            parts.push(&self.exclude);
        }
        parts.extend(self.project_exclude.iter().map(String::as_str));
        parts.join(", ")
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
        // The keyboard route rides the label, as in VS Code (#860).
        if self.ellipsis_at(col, row) {
            return Some("Toggle Search Details (Alt+D)");
        }
        if self.replace_all_at(col, row) {
            return Some("Replace All");
        }
        if let Some(toggle) = self.toggle_at(col, row) {
            return Some(match toggle {
                SearchToggle::CaseSensitive => "Match Case (Alt+C)",
                SearchToggle::WholeWord => "Match Whole Word (Alt+W)",
                SearchToggle::UseRegex => "Use Regular Expression (Alt+R)",
                SearchToggle::PreserveCase => "Preserve Case (Alt+P)",
            });
        }
        None
    }

    /// The full path of the result row under `(col, row)`, for its hover hint
    /// (#2008): a row names only the file, so hits in same-named files
    /// (`mod.rs`, `index.ts`) read the same. `None` off the rows and on the
    /// scrollbar.
    pub fn result_path_at(&self, col: u16, row: u16) -> Option<&Path> {
        let inner = self.last_inner;
        // The scrollbar takes the right column once the hits overflow, as the
        // render draws it. Judged by the hits listed now, not the last
        // frame's bar: hits that stream in under a resting pointer can make
        // the list overflow before the next render.
        let overflows = self.hits.len() > self.results_viewport_height();
        let right = inner.x + inner.width - u16::from(overflows && inner.width > 0);
        if col < inner.x || col >= right {
            return None;
        }
        self.hit_at_y(row).map(|i| self.hits[i].path.as_path())
    }

    /// Clear the search like VS Code's "Clear Search Results": empty the query
    /// (and the replace text typed against it) and drop every hit, resetting
    /// selection and scroll. The include/exclude glob filters are intentionally
    /// preserved — they are scoping settings, not search results.
    pub fn clear_all(&mut self) {
        self.query.clear();
        self.replace.clear();
        self.carets[SearchField::Query.index()] = None;
        self.carets[SearchField::Replace.index()] = None;
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

    /// True when a click lands on the project-exclusions checkbox row
    /// (#1345).
    pub fn project_exclude_toggle_at(&self, col: u16, row: u16) -> bool {
        let r = self.project_exclude_rect;
        r.width != 0 && row == r.y && col >= r.x && col < r.x + r.width
    }

    /// Replace every match in every distinct file among the current hits with
    /// `self.replace`, honouring the active `SearchOpts`. Returns the total
    /// number of matches replaced across all files. The caller is responsible
    /// for re-running the query afterwards so the (now stale) hit list updates.
    pub fn replace_all_on_disk(&self) -> usize {
        let needle = as_typed(&self.query);
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

    /// Count what Replace All WOULD do — `(occurrences, files)` — without
    /// writing anything. Feeds the confirmation modal (#123).
    pub fn count_replacements(&self, skip: &HashSet<PathBuf>) -> (usize, usize) {
        let needle = as_typed(&self.query);
        if needle.is_empty() {
            return (0, 0);
        }
        let mut seen: HashSet<PathBuf> = HashSet::new();
        let mut occurrences = 0usize;
        let mut files = 0usize;
        for hit in &self.hits {
            if !seen.insert(hit.path.clone()) {
                continue;
            }
            if skip.contains(&hit.path) {
                // The confirmed action will not touch these; counting them
                // made the modal overpromise (#124 review).
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&hit.path) else {
                continue;
            };
            if let Some((_, n)) = replace_in_text(&content, needle, &self.replace, self.opts)
                && n > 0
            {
                occurrences += n;
                files += 1;
            }
        }
        (occurrences, files)
    }

    /// Replace All, skipping `skip` (files open with unsaved edits — a disk
    /// write under a dirty buffer forces the tab into conflict divergence).
    /// Returns `(occurrences replaced, the skipped hit files, the files that
    /// could not be rewritten)`. The last are named rather than dropped: a
    /// file that is not UTF-8 still shows hits (the search reads bytes) but
    /// is never replaced, and "replaced N" alone hid that.
    pub fn replace_all_skipping(
        &self,
        skip: &HashSet<PathBuf>,
    ) -> (usize, Vec<PathBuf>, Vec<PathBuf>) {
        let needle = as_typed(&self.query);
        if needle.is_empty() {
            return (0, Vec::new(), Vec::new());
        }
        let mut failed = Vec::new();
        let mut seen: HashSet<PathBuf> = HashSet::new();
        let mut total = 0usize;
        let mut skipped = Vec::new();
        for hit in &self.hits {
            if !seen.insert(hit.path.clone()) {
                continue;
            }
            if skip.contains(&hit.path) {
                skipped.push(hit.path.clone());
                continue;
            }
            match replace_in_file(&hit.path, needle, &self.replace, self.opts) {
                Some(n) => total += n,
                None => failed.push(hit.path.clone()),
            }
        }
        (total, skipped, failed)
    }

    /// Where a hit belongs in the list (#1340): by workspace root, in the
    /// order the workspace lists them, then by the path under that root,
    /// then by line. The walker is parallel, so files arrive in whatever
    /// order its threads finish them.
    fn hit_order<'a>(&self, hit: &'a SearchHit) -> (usize, &'a Path, usize) {
        let roots = if self.roots.is_empty() {
            std::slice::from_ref(&self.root)
        } else {
            self.roots.as_slice()
        };
        // The deepest root that holds it, as the row label picks.
        let owner = roots
            .iter()
            .enumerate()
            .filter(|(_, r)| hit.path.starts_with(r))
            .max_by_key(|(_, r)| r.components().count());
        match owner {
            Some((i, r)) => (
                i,
                hit.path.strip_prefix(r).unwrap_or(&hit.path),
                hit.line_no,
            ),
            None => (usize::MAX, hit.path.as_path(), hit.line_no),
        }
    }

    /// Take in streamed hits, each landing in its final place
    /// ([`Self::hit_order`]) so the list reads the same however the walk
    /// went. The selection stays on its hit.
    ///
    /// This runs on the UI thread for every drain while a scan streams
    /// (#2007), so its cost must not grow with the hits already listed: each
    /// arriving file finds its place by binary search, and the listed hits
    /// are only moved, never compared one by one. Comparing every listed hit
    /// on each drain made a broad query's results crawl in, the UI busier
    /// with each file that arrived.
    pub fn add_hits(&mut self, mut batch: Vec<SearchHit>) {
        if batch.is_empty() {
            return;
        }
        batch.sort_by_cached_key(|h| {
            let (root, rel, line) = self.hit_order(h);
            (root, rel.to_path_buf(), line)
        });
        let old = std::mem::take(&mut self.hits);
        let selected = (self.selected < old.len()).then_some(self.selected);
        let mut merged = Vec::with_capacity(old.len() + batch.len());
        let mut rest = old.into_iter();
        // How many of the listed hits are already in `merged`.
        let mut moved = 0usize;
        let mut prev_path: Option<PathBuf> = None;
        for hit in batch {
            let pending = rest.as_slice();
            // The batch is sorted, so every pending hit sorts after the
            // previous arrival. When that was this same file, a pending hit
            // of another file is past this whole file: only one of this file
            // can still go first, and the search is skipped without it.
            let same_file = prev_path.as_deref() == Some(hit.path.as_path());
            let n = if same_file && pending.first().is_none_or(|o| o.path != hit.path) {
                0
            } else {
                // A listed hit stays ahead of an equal one arriving.
                let key = self.hit_order(&hit);
                pending.partition_point(|o| self.hit_order(o) <= key)
            };
            if let Some(s) = selected
                && (moved..moved + n).contains(&s)
            {
                self.selected = merged.len() + (s - moved);
            }
            merged.extend(rest.by_ref().take(n));
            moved += n;
            if !same_file {
                prev_path = Some(hit.path.clone());
            }
            merged.push(hit);
        }
        if let Some(s) = selected
            && s >= moved
        {
            self.selected = merged.len() + (s - moved);
        }
        merged.extend(rest);
        self.hits = merged;
    }

    /// `query_error` for the current query and toggles, compiled once per
    /// change rather than on every frame.
    pub fn query_error(&mut self) -> Option<String> {
        let fresh = matches!(&self.query_error_memo,
            Some((q, o, _)) if *q == self.query && *o == self.opts);
        if !fresh {
            let error = query_error(&self.query, self.opts);
            self.query_error_memo = Some((self.query.clone(), self.opts, error));
        }
        self.query_error_memo
            .as_ref()
            .and_then(|(_, _, e)| e.clone())
    }

    /// Run the current query, store the results, and reset selection.
    pub fn run_query(&mut self) {
        self.hits.clear();
        self.add_hits(search_workspace(&self.root, &self.query, self.opts));
        self.selected = 0;
        self.scroll = 0;
    }

    /// If the cell `(col, row)` falls on one of the three toggle glyphs
    /// (each rendered as a 2-cell pair), return a mutable pointer to the
    /// corresponding flag so the caller can flip it. Returns `None`
    /// otherwise.
    pub fn toggle_at(&self, col: u16, row: u16) -> Option<SearchToggle> {
        let preserve = self.toggle_preserve_x;
        if preserve != 0 && row == self.toggle_preserve_y && col >= preserve && col < preserve + 2 {
            return Some(SearchToggle::PreserveCase);
        }
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

    /// Flip one of the mode toggles (`Aa` / `ab` / `.*`, and the replace
    /// row's `AB`), from a click or `Alt+C` / `Alt+W` / `Alt+R` / `Alt+P`
    /// (#860, #1612). Returns the new state.
    pub fn toggle_opt(&mut self, toggle: SearchToggle) -> bool {
        let flag = match toggle {
            SearchToggle::CaseSensitive => &mut self.opts.case_sensitive,
            SearchToggle::WholeWord => &mut self.opts.whole_word,
            SearchToggle::UseRegex => &mut self.opts.use_regex,
            SearchToggle::PreserveCase => &mut self.opts.preserve_case,
        };
        *flag = !*flag;
        *flag
    }

    /// `(results, files)` for the header: every occurrence counts, two on
    /// one line being two results, as in VS Code and the Replace All
    /// confirmation (#860).
    pub fn result_counts(&self) -> (usize, usize) {
        let results = self.hits.iter().map(|h| h.matches).sum();
        // Drawn on every render, so a path is hashed once per run of hits
        // rather than once per hit: a file's hits arrive together, and one
        // whose hits come in more than one run still counts once.
        let files = self
            .hits
            .first()
            .into_iter()
            .chain(
                self.hits
                    .windows(2)
                    .filter(|w| w[0].path != w[1].path)
                    .map(|w| &w[1]),
            )
            .map(|h| &h.path)
            .collect::<HashSet<_>>()
            .len();
        (results, files)
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

/// Identifies which of the search-mode toggles a click landed on, or
/// an `Alt` key named (#860).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchToggle {
    CaseSensitive,
    WholeWord,
    UseRegex,
    /// `AB` on the replace row (#1612).
    PreserveCase,
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
    /// The caret as a char index into `text` (#1729).
    caret: usize,
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

/// A glob box's caption. An entry that doesn't compile replaces the label
/// with its name in red, right above the box, so a typo can't silently
/// change what the search covers (#1251).
fn draw_glob_caption(
    buf: &mut Buffer,
    (x, y, right): (u16, u16, u16),
    label: &str,
    raw: &str,
    style: Style,
    theme: crate::theme::Theme,
) {
    let bad = invalid_globs(raw);
    if bad.is_empty() {
        buf.set_string(x, y, label, style);
        return;
    }
    buf.set_stringn(
        x,
        y,
        format!("invalid glob: {}", bad.join(", ")),
        right.saturating_sub(x) as usize + 1,
        Style::default().fg(theme.ui(Color::Rgb(0xf1, 0x4c, 0x4c))),
    );
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
fn render_field(
    buf: &mut Buffer,
    args: FieldArgs,
    theme: crate::theme::Theme,
    caret_out: &mut Option<(u16, u16)>,
    origin_out: &mut (u16, usize),
) -> Rect {
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
        theme.ui(FIELD_BAR_IDLE)
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

    // Croft Loch writes the field in its ink ramp: the text in body ink,
    // the placeholder and an idle lead glyph muted. A boxed theme keeps its
    // historical colours.
    let tones = crate::widgets::surface::tones(theme);
    let lead_w: u16 = if args.lead_glyph.is_some() { 2 } else { 0 };
    if let Some(g) = args.lead_glyph {
        let lead_color = match tones {
            _ if args.field_focused => args.accent,
            Some(t) => t.muted(),
            None => theme.ui(Color::Rgb(0x9d, 0xa5, 0xb4)),
        };
        buf.set_string(fill_x, args.y, g, Style::default().fg(lead_color).bg(fill));
    }

    // Editable span: after the lead glyph, up to the reserved control cells.
    let text_x = fill_x + lead_w;
    let text_right = args.fill_right.saturating_sub(args.reserve_right);
    let text_w = text_right.saturating_sub(text_x).saturating_add(1);

    let mut spans: Vec<Span> = Vec::with_capacity(4);
    let win = crate::widgets::line_edit::window(args.text, args.caret, usize::from(text_w));
    *origin_out = (text_x, win.start);
    if args.text.is_empty() {
        let ink = match tones {
            Some(t) => t.muted(),
            None => theme.ui(Color::Rgb(0x6c, 0x7d, 0x9c)),
        };
        spans.push(Span::styled(
            args.placeholder.to_string(),
            Style::default()
                .fg(ink)
                .bg(fill)
                .add_modifier(Modifier::ITALIC),
        ));
    } else {
        let ink = match tones {
            Some(t) => t.text(),
            None => theme.ui(Color::White),
        };
        let plain = Style::default().fg(ink).bg(fill);
        let selected = Style::default()
            .fg(ink)
            .bg(theme.ui(Color::Rgb(0x26, 0x4f, 0x78)));
        // Only the part of the text the row has room for, scrolled so the
        // caret is in view; the selection is clipped to it.
        let (sb, eb) = (
            crate::widgets::line_edit::byte_of(args.text, win.start),
            crate::widgets::line_edit::byte_of(args.text, win.end),
        );
        let shown = &args.text[sb..eb];
        let sel = args
            .selection
            .map(|(a, b)| (a.max(sb) - sb.min(a.max(sb)), b.min(eb).saturating_sub(sb)))
            .filter(|&(a, b)| a < b && b <= shown.len());
        match sel {
            Some((a, b)) => {
                if a > 0 {
                    spans.push(Span::styled(shown[..a].to_string(), plain));
                }
                spans.push(Span::styled(shown[a..b].to_string(), selected));
                if b < shown.len() {
                    spans.push(Span::styled(shown[b..].to_string(), plain));
                }
            }
            None => spans.push(Span::styled(shown.to_string(), plain)),
        }
    }
    if text_w > 0 {
        buf.set_line(text_x, args.y, &Line::from(spans), text_w);
    }

    // The caret rides just past the text (at the field's left edge when empty),
    // clamped inside the editable span. The app shows the host's blinking
    // hardware caret here only while this field is focused, so we record the
    // cell for the focused field alone. The caret is a char index, the
    // commit box's model (`message_cursor`), drawn where the text window put it.
    if args.field_focused && text_w > 0 {
        let off = (win.caret_cell as u16).min(text_w.saturating_sub(1));
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
        let focus_blue = self.theme.ui(Color::Rgb(0x4e, 0x9a, 0xff));
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
        let outer = crate::widgets::surface::view_block(self.theme, true, outer_style);
        let inner = outer.inner(area);
        outer.render(area, buf);
        // Black theme: replace the solid focus border with the brand gradient.
        // No title sits on this border (the SEARCH header is inside), so there
        // is nothing to re-stamp.
        crate::widgets::surface::paint_view_focus(
            buf,
            area,
            self.theme,
            self.focused,
            self.focus_gradient,
        );
        self.last_area = area;
        self.last_inner = inner;
        // Clear last frame's caret up front so an early return (too small to
        // lay out a field) never leaves a stale hardware-caret position.
        self.caret_cell = None;
        if inner.height == 0 || inner.width == 0 {
            return;
        }

        // Croft Loch's inks and fills; `None` on a boxed theme, which keeps
        // the colours below exactly as they were.
        let tones = crate::widgets::surface::tones(self.theme);

        // Row 0: "SEARCH" header (top-left) plus the VS Code SEARCH header
        // actions (refresh / clear) at the top-right. Croft Loch titles the
        // view as the Explorer does: the strong ink while it has the
        // keyboard, muted otherwise, and never bold.
        let title_style = match tones {
            Some(t) => Style::default().fg(if self.focused { t.strong() } else { t.muted() }),
            None => Style::default()
                .fg(self.theme.ui(Color::Rgb(0xb0, 0xb8, 0xc8)))
                .add_modifier(Modifier::BOLD),
        };
        buf.set_string(inner.x, inner.y, "SEARCH", title_style);

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
                // Croft Loch: the side bar's header actions, muted at rest
                // and lifted to the strong ink on a faint chip under the
                // pointer; the accent stays for focus.
                let style = if tones.is_some() {
                    crate::widgets::header_pill::action_style(self.theme, hot)
                } else if hot {
                    Style::default().fg(accent)
                } else {
                    Style::default().fg(self.theme.ui(Color::Rgb(0x8b, 0x94, 0xa4)))
                };
                buf.set_string(x, inner.y, glyph.to_string(), style);
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
        // Croft Loch: the side bar's codicon chevron in the muted ink, an
        // idle icon like the Explorer's; the field's bar already says where
        // focus is.
        let chevron_style = match tones {
            Some(t) => Style::default().fg(t.muted()),
            None => Style::default()
                .fg(if self.focused {
                    accent
                } else {
                    Color::DarkGray
                })
                .add_modifier(Modifier::BOLD),
        };
        let chevron_glyph = crate::widgets::surface::chevron(self.theme, self.replace_open);
        buf.set_string(
            chevron_x,
            content_y,
            chevron_glyph.to_string(),
            chevron_style,
        );
        self.chevron_x = chevron_x;
        self.chevron_y = content_y;

        // Caret cell of whichever field is focused. `render_field` writes it
        // only for the focused field (and never clears it for the others), so
        // it accumulates the single focused field's caret across the calls
        // below; committed to `self.caret_cell` once the cluster is laid out.
        let mut caret: Option<(u16, u16)> = None;
        let mut origins = self.field_origin;

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
                caret: self.field_caret(SearchField::Query),
                placeholder: "Search",
                selection: self.selection_range(),
                field_focused: query_focused,
                accent,
                bg,
                reserve_right: query_reserve,
            },
            self.theme,
            &mut caret,
            &mut origins[SearchField::Query.index()],
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
        // Croft Loch: an on toggle is the strong ink on the selection chip
        // (no bold: weight is rare), an off one the muted ink.
        let active_style = match tones {
            Some(t) => Style::default()
                .fg(t.strong())
                .bg(crate::gradient::rgb_color(crate::gradient::POPUP_SEL_BG)),
            None => Style::default()
                .fg(Color::White)
                .bg(crate::gradient::rgb_color(crate::gradient::POPUP_SEL_BG))
                .add_modifier(Modifier::BOLD),
        };
        let inactive_style = Style::default()
            .fg(match tones {
                Some(t) => t.muted(),
                None => self.theme.ui(Color::Rgb(0x79, 0x82, 0x8f)),
            })
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
        let ellipsis_style = match tones {
            Some(t) if self.details_open => Style::default().fg(t.accent()),
            Some(t) => Style::default().fg(t.muted()),
            None if self.details_open => Style::default().fg(accent).add_modifier(Modifier::BOLD),
            None => Style::default().fg(self.theme.ui(Color::Rgb(0x79, 0x82, 0x8f))),
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
        self.toggle_preserve_x = 0;
        self.replace_all_x = 0;
        self.replace_all_y = 0;
        if self.replace_open && next_y + 1 < inner.y + inner.height {
            // A faint hairline between the query and replace fields so they read
            // as two separate inputs instead of one continuous filled container.
            draw_field_divider(buf, bar_x, query_fill_right, next_y, self.theme);
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
                    caret: self.field_caret(SearchField::Replace),
                    placeholder: "Replace (Tab switches)",
                    selection: if self.field == SearchField::Replace {
                        self.active_selection()
                    } else {
                        None
                    },
                    field_focused: replace_focused,
                    accent,
                    bg,
                    reserve_right: 3,
                },
                self.theme,
                &mut caret,
                &mut origins[SearchField::Replace.index()],
            );
            // `AB` (Preserve Case) inset at the replace field's right edge,
            // where VS Code keeps it, styled like the query's toggles.
            let preserve_x = replace_fill_right.saturating_sub(2);
            if preserve_x > bar_x + 1 {
                let fill = lift_bg(
                    bg,
                    if replace_focused {
                        FIELD_FILL_LIFT_FOCUS
                    } else {
                        FIELD_FILL_LIFT_IDLE
                    },
                );
                let style = if self.opts.preserve_case {
                    active_style
                } else {
                    inactive_style.bg(fill)
                };
                buf.set_string(preserve_x, next_y, "AB", style);
                self.toggle_preserve_x = preserve_x;
                self.toggle_preserve_y = next_y;
            }
            buf.set_string(
                replace_all_x,
                next_y,
                crate::icons::SEARCH_REPLACE_ALL.to_string(),
                Style::default().fg(match tones {
                    // Croft Loch: an action icon, muted like the header's.
                    Some(t) => t.muted(),
                    None if self.focused => accent,
                    None => self.theme.ui(Color::Rgb(0x9d, 0xa5, 0xb4)),
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
        self.project_exclude_rect = Rect::default();
        if self.details_open {
            let detail_fill_right = inner_right.saturating_sub(1).max(bar_x + 1);
            let caption_style = Style::default().fg(match tones {
                Some(t) => t.muted(),
                None => self.theme.ui(Color::Rgb(0x9d, 0xa5, 0xb4)),
            });

            if next_y + 2 < inner.y + inner.height {
                // Hairline before the include group, matching the query/replace
                // separation so every input reads as its own row.
                draw_field_divider(buf, bar_x, detail_fill_right, next_y, self.theme);
                next_y += 1;
                draw_glob_caption(
                    buf,
                    (bar_x, next_y, detail_fill_right),
                    "files to include",
                    &self.include,
                    caption_style,
                    self.theme,
                );
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
                        caret: self.field_caret(SearchField::Include),
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
                    self.theme,
                    &mut caret,
                    &mut origins[SearchField::Include.index()],
                );
                next_y += 1;
            }
            if next_y + 2 < inner.y + inner.height {
                // Hairline before the exclude group, same as above.
                draw_field_divider(buf, bar_x, detail_fill_right, next_y, self.theme);
                next_y += 1;
                draw_glob_caption(
                    buf,
                    (bar_x, next_y, detail_fill_right),
                    "files to exclude",
                    &self.exclude,
                    caption_style,
                    self.theme,
                );
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
                        caret: self.field_caret(SearchField::Exclude),
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
                    self.theme,
                    &mut caret,
                    &mut origins[SearchField::Exclude.index()],
                );
                next_y += 1;
                if !self.project_exclude.is_empty() && next_y + 1 < inner.y + inner.height {
                    // VS Code's "Use Exclude Settings and Ignore Files", as a
                    // checkbox naming what the project hides (#1345): one
                    // click searches everything.
                    let text = format!(
                        "[{}] project excludes: {}",
                        if self.use_exclude_settings { "x" } else { " " },
                        self.project_exclude.join(", ")
                    );
                    let width = detail_fill_right.saturating_sub(bar_x) + 1;
                    let style = if self.use_exclude_settings {
                        caption_style
                    } else {
                        caption_style.add_modifier(Modifier::CROSSED_OUT)
                    };
                    buf.set_stringn(bar_x, next_y, &text, width as usize, style);
                    self.project_exclude_rect = Rect {
                        x: bar_x,
                        y: next_y,
                        width: (text.chars().count() as u16).min(width),
                        height: 1,
                    };
                    next_y += 1;
                }
            }
        }
        // Commit the focused field's caret now that the whole input cluster is
        // laid out. Always runs (the optional rows above only skip their own
        // render), so the app sees `None` whenever no field carried a caret.
        self.caret_cell = caret;
        self.field_origin = origins;

        // ---- Separator + match-count caption ----
        let separator_y = next_y;
        if separator_y < inner.y + inner.height {
            // Croft Loch: a hairline seam hugging the results below, as
            // Source Control draws above its changes, not a line twice as
            // bright as every other seam in the side bar.
            let (sep_glyph, sep_style) = match tones {
                Some(t) => ("\u{2581}", Style::default().fg(t.hairline())),
                None => (
                    "─",
                    Style::default().fg(self.theme.ui(Color::Rgb(0x40, 0x48, 0x58))),
                ),
            };
            for x in inner.x..inner.x + inner.width {
                buf.set_string(x, separator_y, sep_glyph, sep_style);
            }
        }
        let caption_y = separator_y + 1;
        let mut results_start_y = caption_y + 1;
        if caption_y < inner.y + inner.height && !as_typed(&self.query).is_empty() {
            if let Some(error) = self.query_error() {
                // Wrapped, not cut off: the sidebar is narrow and the
                // reason sits at the end of the message.
                let style = Style::default().fg(self.theme.ui(Color::Rgb(0xf1, 0x4c, 0x4c)));
                let rows = crate::sarif::render::wrap(&error, inner.width.max(1) as usize);
                let mut y = caption_y;
                for row in rows {
                    if y >= inner.y + inner.height {
                        break;
                    }
                    buf.set_stringn(inner.x, y, &row, inner.width as usize, style);
                    y += 1;
                }
                results_start_y = y.max(caption_y + 1);
            } else {
                let (results, files) = self.result_counts();
                let (r_s, f_s) = (
                    if results == 1 { "" } else { "s" },
                    if files == 1 { "" } else { "s" },
                );
                let caption = match tones {
                    // Croft Loch: a quiet sentence in the muted ink whose
                    // two counts, the part worth reading, are body text.
                    Some(t) => {
                        let (num, word) = (
                            Style::default().fg(t.text()),
                            Style::default().fg(t.muted()),
                        );
                        Line::from(vec![
                            Span::styled(results.to_string(), num),
                            Span::styled(format!(" result{r_s} in "), word),
                            Span::styled(files.to_string(), num),
                            Span::styled(format!(" file{f_s}"), word),
                        ])
                    }
                    None => Line::from(Span::styled(
                        format!("{results} result{r_s} in {files} file{f_s}"),
                        Style::default()
                            .fg(self.theme.ui(Color::Rgb(0x9d, 0xa5, 0xb4)))
                            .add_modifier(Modifier::ITALIC),
                    )),
                };
                buf.set_line(inner.x, caption_y, &caption, inner.width);
            }
        }
        // Record where results begin so scrolling / click mapping stay aligned.
        self.results_start_offset = results_start_y.saturating_sub(inner.y);

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
            let is_selected = hit_idx == self.selected;
            // The fill under the row's text on Croft Loch (chrome, the hover
            // lift or the selected band), which a match's amber tint is
            // blended from so it reads the same on every row.
            let mut row_fill = tones.map(|t| t.chrome());
            if !is_selected
                && let Some(bg) =
                    crate::widgets::hover::row_hover_bg(row_rect, self.hover_pointer, self.theme)
            {
                let fill = Style::default().bg(bg);
                for rx in 0..row_width {
                    buf[(inner.x + rx, y)].set_symbol(" ").set_style(fill);
                }
                row_fill = row_fill.map(|_| bg);
            }
            // Croft Loch: the one selected-row anatomy, the band across the
            // full row with the accent bar standing in the pane's left
            // padding column (as the Explorer's does) so the names keep
            // their column; greyed, barless, while another pane has the
            // keyboard.
            if is_selected && let Some(t) = tones {
                crate::widgets::surface::paint_selected_row(
                    buf,
                    Rect {
                        x: area.x,
                        width: row_width.saturating_add(inner.x - area.x),
                        ..row_rect
                    },
                    t,
                    t.chrome,
                    self.focused,
                );
                row_fill = Some(crate::gradient::rgb_color(
                    crate::widgets::surface::selected_row_fill(t, t.chrome, self.focused),
                ));
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
                    let owner = self
                        .roots
                        .iter()
                        .filter(|r| hit.path.starts_with(r))
                        .max_by_key(|r| r.components().count());
                    match owner {
                        Some(r) if self.roots.len() > 1 => {
                            let idx = self.roots.iter().position(|x| x == r).unwrap_or(0);
                            let name =
                                crate::workspace::root_display_labels(&self.roots).swap_remove(idx);
                            format!(
                                "{name}/{}",
                                hit.path
                                    .strip_prefix(r)
                                    .unwrap_or(hit.path.as_path())
                                    .display()
                            )
                        }
                        Some(r) => hit
                            .path
                            .strip_prefix(r)
                            .unwrap_or(hit.path.as_path())
                            .display()
                            .to_string(),
                        None => hit.path.display().to_string(),
                    }
                });
            // Black theme: selected result row matches the Explorer's
            // white-on-dark-teal selection; Croft Dark keeps black-on-blue.
            let header_style = if let Some(t) = tones {
                // Croft Loch: the file name is body text on every row; the
                // band says which one is selected.
                Style::default().fg(t.text())
            } else if is_selected {
                if self.focus_gradient {
                    Style::default()
                        .fg(Color::White)
                        .bg(crate::gradient::rgb_color(crate::gradient::POPUP_SEL_BG))
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                        .fg(Color::Black)
                        .bg(self.theme.ui(Color::Rgb(0x4e, 0x9a, 0xff)))
                        .add_modifier(Modifier::BOLD)
                }
            } else {
                Style::default().fg(self.theme.ui(Color::White))
            };
            let needle = as_typed(&self.query);
            // High-contrast yellow background like ripgrep / VS Code's
            // editor.findMatchHighlightBackground. Different treatment when
            // the row is selected so the highlight stays readable against
            // the blue row bg instead of fighting it.
            let highlight_style = Style::default()
                .fg(Color::Black)
                .bg(self.theme.ui(Color::Rgb(0xff, 0xd7, 0x4a)))
                .add_modifier(Modifier::BOLD);
            // Croft Loch reads a row in two inks: the file name in body
            // text, the line number and the preview muted, with each match
            // tinted amber over the row's own fill. The line number is where
            // the result is, a fact read as often as the name, so it never
            // drops to the faint ink kept for marks. On the selected row the
            // preview steps up an ink, so it keeps its contrast on the band;
            // the muted line number already does. The selected row's first
            // match is the one opening it lands on, so it takes the current
            // match's stronger tint, as it will in the editor.
            let (plain_style, line_no_style) = match tones {
                Some(t) if is_selected => (
                    Style::default().fg(t.text()),
                    Style::default().fg(t.muted()),
                ),
                Some(t) => (
                    Style::default().fg(t.muted()),
                    Style::default().fg(t.muted()),
                ),
                None => (
                    Style::default().fg(self.theme.ui(Color::Gray)),
                    Style::default().fg(self.theme.ui(Color::Rgb(0xeb, 0xcb, 0x8b))),
                ),
            };
            let match_style = |active: bool| match (tones, row_fill) {
                (Some(t), Some(fill)) => crate::widgets::surface::match_style(
                    plain_style.bg(fill),
                    t,
                    self.theme,
                    active,
                ),
                _ => highlight_style,
            };
            let mut active_pending = is_selected;
            let mut spans: Vec<Span> = vec![
                Span::styled(format!(" {path_display}"), header_style),
                Span::styled(format!(":{}: ", hit.line_no), line_no_style),
            ];
            // Inline replace preview (#123): while a replacement is typed,
            // each match renders dim and crossed out with the expanded
            // replacement (capture refs honoured) in green beside it — the
            // bad-pattern catch before Replace All commits anything.
            let preview = self.replace_open && !self.replace.is_empty();
            let mut chunks = split_for_highlight(&hit.line_text, needle, self.opts);
            // A match past the row's edge was clipped away with everything
            // after it (#860): drop leading context so it shows.
            if let Some(first) = chunks.iter().position(|(_, is_match)| *is_match) {
                let prefix_w: usize = spans.iter().map(Span::width).sum();
                let avail = usize::from(row_width).saturating_sub(prefix_w);
                let matched = &chunks[first].0;
                let mut need = matched.width();
                if preview {
                    need += expand_replacement(matched, needle, &self.replace, self.opts).width();
                }
                let lead: String = chunks[..first].iter().map(|(t, _)| t.as_str()).collect();
                if let Some(kept) = trim_leading_context(&lead, avail, need) {
                    chunks.splice(..first, std::iter::once((kept, false)));
                }
            }
            for (chunk, is_match) in chunks {
                if is_match && preview {
                    let new_text = expand_replacement(&chunk, needle, &self.replace, self.opts);
                    spans.push(Span::styled(
                        chunk,
                        Style::default()
                            .fg(self.theme.ui(Color::Rgb(0x8b, 0x93, 0xa5)))
                            .add_modifier(Modifier::CROSSED_OUT),
                    ));
                    spans.push(Span::styled(
                        new_text,
                        Style::default()
                            .fg(self.theme.ui(Color::Rgb(0xb6, 0xee, 0xc4)))
                            .add_modifier(Modifier::BOLD),
                    ));
                    continue;
                }
                let style = if is_match {
                    let active = std::mem::take(&mut active_pending);
                    match_style(active)
                } else {
                    plain_style
                };
                spans.push(Span::styled(chunk, style));
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
mod tests;
