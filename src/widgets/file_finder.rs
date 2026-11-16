use ignore::{WalkBuilder, WalkState};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};
use rayon::prelude::*;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const MAX_RESULTS: usize = 40;
/// The most bytes one `:line` check reads to count the lines of the files a
/// query matches (#2127). A file past it is left in the list, as an
/// unknown, rather than read on.
const LINE_COUNT_BUDGET: u64 = 32 * 1024 * 1024;
/// The most matches one `:line` check looks at, in rank order (#2127). A
/// common name can match thousands of files; the ranking keeps this many,
/// so a file that has the line is found past the first page of matches.
const LINE_CHECK_MAX_FILES: usize = 2_000;
/// How long a keystroke waits for its `:line` check before drawing the
/// matches unchecked (#2127). A local disk answers well inside it, so the
/// list does not flicker; a stalled network mount cannot hold the keystroke
/// past it, and its answer is applied when it comes.
const LINE_CHECK_WAIT: std::time::Duration = std::time::Duration::from_millis(16);

#[derive(Clone, Debug)]
pub struct FileEntry {
    pub path: PathBuf,
    pub rel: String,
    pub rel_lower: String,
    /// Byte offset of the filename within `rel` (rendering's dir/file
    /// split). `rel_lower` gets its own offset below: lowercasing can
    /// change a component's byte length (e.g. `İ` → `i̇`), so one offset
    /// cannot index both strings.
    pub filename_start: usize,
    /// Byte offset of the filename within `rel_lower` — the scoring
    /// string. Slicing `rel_lower` with the `rel` offset panicked on a
    /// non-boundary whenever an earlier component's lowercase changed
    /// byte length (#148 review).
    pub filename_start_lower: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum MatchTier {
    Subsequence = 1,
    PathSubstring = 2,
    FilenameSubstring = 3,
    FilenamePrefix = 4,
    ExactFilename = 5,
}

#[derive(Clone, Debug)]
pub struct ScoredResult {
    pub entry: FileEntry,
}

/// Where in the picked file to land, parsed off the end of the query
/// (#472): `name:236`, `name:236:7` or `name:236-239`. Lines and columns
/// are one-based as typed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineHint {
    pub line: usize,
    pub end: Option<usize>,
    pub col: Option<usize>,
}

#[derive(Default)]
pub struct FileFinder {
    pub query: String,
    pub cursor: usize,
    pub entries: Arc<Vec<FileEntry>>,
    pub results: Vec<ScoredResult>,
    pub selected: usize,
    pub scroll: usize,
    pub last_rect: Rect,
    pub last_inner_height: u16,
    /// Where the terminal's caret goes in the query, set by a `surfaces`
    /// theme's render (which draws no caret of its own); `None` on a boxed
    /// theme, whose caret is a painted block.
    pub last_caret: Option<(u16, u16)>,
    /// Said above the list when no match has the line a `:line` query names
    /// (#2127), so the list shows every match, longest first.
    pub notice: Option<String>,
    /// Lines counted in files a `:line` query listed, kept while the file's
    /// size and time are unchanged so each keystroke reads a file once.
    line_counts: HashMap<PathBuf, LineCount>,
    /// Lines in the text files open in the editor, which a pick opens as
    /// they are, unsaved edits and all: a `:line` query counts these rather
    /// than what is saved (#2127). Set by the app when the finder opens.
    open_lines: HashMap<PathBuf, usize>,
    /// The `:line` check counting off the UI thread, if one is running.
    line_check: Option<LineCheck>,
    /// Bumped by every check started, so an older query's answer is never
    /// applied to a newer one.
    line_check_generation: u64,
    /// The thread the checks run on, started by the first one.
    line_worker: Option<LineWorker>,
}

/// A `:line` check running on its own thread (#2127): the matches it is
/// checking, best first, and where its answer comes back.
struct LineCheck {
    generation: u64,
    needed: usize,
    candidates: Vec<ScoredResult>,
    /// More files matched than the ranking kept.
    more_matched: bool,
    rx: std::sync::mpsc::Receiver<LineCheckDone>,
}

/// What a `:line` check found.
struct LineCheckDone {
    generation: u64,
    /// The lines of each candidate checked, in rank order, counted no
    /// further than the line asked for; `None` for one it could not tell.
    /// Shorter than the candidates when the check stopped early.
    lines: Vec<Option<usize>>,
    /// Counts read from disk, for the cache.
    counted: Vec<(PathBuf, LineCount)>,
}

/// One candidate as a `:line` check gets it: its lines when it is open in
/// the editor, else what the cache holds for it.
struct LineJob {
    path: PathBuf,
    open: Option<usize>,
    cached: Option<LineCount>,
}

/// One query's `:line` check, as the worker takes it.
struct LineBatch {
    generation: u64,
    jobs: Vec<LineJob>,
    needed: usize,
    tx: std::sync::mpsc::Sender<LineCheckDone>,
}

/// What the finder and its worker thread share: the one query waiting to be
/// checked, and whether the finder has gone.
#[derive(Default)]
struct WorkerSlot {
    next: Option<LineBatch>,
    closed: bool,
}

/// The one thread a Quick Open counts lines on (#2127). It holds at most one
/// query waiting, and a newer query replaces it: a file on a stalled network
/// mount blocks this thread alone, and keystrokes typed meanwhile leave one
/// query to check when it is free, not a thread each. Dropped with the
/// finder, it tells the thread to stop once the read it is in returns.
struct LineWorker {
    shared: Arc<(Mutex<WorkerSlot>, std::sync::Condvar)>,
}

impl LineWorker {
    /// Start the thread, or `None` when the system will not give one: the
    /// matches are then listed unchecked rather than the app stopping.
    fn start() -> Option<Self> {
        let shared = Arc::new((Mutex::new(WorkerSlot::default()), std::sync::Condvar::new()));
        let theirs = Arc::clone(&shared);
        std::thread::Builder::new()
            .name(String::from("quick-open-lines"))
            .spawn(move || run_line_worker(&theirs))
            .ok()?;
        Some(Self { shared })
    }

    /// Queue `batch`, in place of any query still waiting.
    fn submit(&self, batch: LineBatch) {
        let (slot, wake) = &*self.shared;
        slot.lock().unwrap_or_else(|e| e.into_inner()).next = Some(batch);
        wake.notify_one();
    }
}

impl Drop for LineWorker {
    fn drop(&mut self) {
        let (slot, wake) = &*self.shared;
        let mut slot = slot.lock().unwrap_or_else(|e| e.into_inner());
        slot.closed = true;
        slot.next = None;
        wake.notify_one();
    }
}

/// The worker thread: check each query handed over, newest first, until the
/// finder goes.
fn run_line_worker(shared: &(Mutex<WorkerSlot>, std::sync::Condvar)) {
    let (slot, wake) = shared;
    loop {
        let batch = {
            let mut slot = slot.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if slot.closed {
                    return;
                }
                if let Some(batch) = slot.next.take() {
                    break batch;
                }
                slot = wake.wait(slot).unwrap_or_else(|e| e.into_inner());
            }
        };
        let (lines, counted) = check_lines(batch.jobs, batch.needed);
        let _ = batch.tx.send(LineCheckDone {
            generation: batch.generation,
            lines,
            counted,
        });
    }
}

/// What counting a file's lines found (#2127).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LineCount {
    /// The lines found, counting no further than the line asked for.
    lines: usize,
    /// The whole file was read: `lines` is all it has.
    complete: bool,
    len: u64,
    modified: Option<std::time::SystemTime>,
}

impl FileFinder {
    pub fn new(entries: Arc<Vec<FileEntry>>) -> Self {
        let mut me = Self {
            query: String::new(),
            cursor: 0,
            entries,
            results: Vec::new(),
            selected: 0,
            scroll: 0,
            last_rect: Rect::default(),
            last_inner_height: 0,
            last_caret: None,
            notice: None,
            line_counts: HashMap::new(),
            open_lines: HashMap::new(),
            line_check: None,
            line_check_generation: 0,
            line_worker: None,
        };
        me.refresh_results();
        me
    }

    /// The lines of each text buffer a pick would open (#2127), for a
    /// `:line` query to count instead of the saved file. The app hands these
    /// over on every tick while Quick Open is up, since a buffer can change
    /// behind it: a reload from disk, an agent's or a collab peer's edit.
    /// Only a change reruns the check, and the row picked stays picked if
    /// it is still listed. True when the list changed.
    pub fn set_open_lines(&mut self, open: HashMap<PathBuf, usize>) -> bool {
        if open == self.open_lines {
            return false;
        }
        self.open_lines = open;
        self.line_counts
            .retain(|path, _| !self.open_lines.contains_key(path));
        if self.line_hint().is_none() {
            return false;
        }
        let chosen = self.selected_entry().map(|e| e.path.clone());
        self.refresh_results();
        self.selected = chosen
            .and_then(|p| self.results.iter().position(|r| r.entry.path == p))
            .unwrap_or(0);
        self.scroll = self.scroll.min(self.selected);
        true
    }

    /// Apply a `:line` check's answer if it has come. True when the list
    /// changed, so the frame is redrawn.
    pub fn poll_line_check(&mut self) -> bool {
        let done = match self.line_check.as_ref().map(|c| c.rx.try_recv()) {
            Some(Ok(done)) => done,
            Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => {
                self.line_check = None;
                return false;
            }
            _ => return false,
        };
        self.apply_line_check(done);
        true
    }

    /// Wait for the running `:line` check's answer and apply it, for a test
    /// that asserts what the check decides.
    #[cfg(test)]
    pub(crate) fn finish_line_check(&mut self) {
        if let Some(done) = self.line_check.as_ref().and_then(|c| c.rx.recv().ok()) {
            self.apply_line_check(done);
        }
    }

    pub fn set_query(&mut self, q: &str) {
        if q == self.query {
            return;
        }
        self.query = q.to_string();
        self.cursor = self.query.chars().count();
        self.refresh_results();
        self.selected = 0;
        self.scroll = 0;
    }

    fn char_count(&self) -> usize {
        self.query.chars().count()
    }

    fn byte_offset(&self, char_idx: usize) -> usize {
        self.query
            .char_indices()
            .nth(char_idx)
            .map(|(b, _)| b)
            .unwrap_or(self.query.len())
    }

    pub fn replace_entries(&mut self, entries: Arc<Vec<FileEntry>>) {
        if Arc::ptr_eq(&self.entries, &entries) {
            return;
        }
        self.entries = entries;
        self.refresh_results();
        if self.results.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.results.len() {
            self.selected = self.results.len() - 1;
        }
        if self.scroll > self.selected {
            self.scroll = self.selected;
        }
    }

    pub fn push_char(&mut self, c: char) {
        let at = self.byte_offset(self.cursor);
        self.query.insert(at, c);
        self.cursor += 1;
        self.refresh_results();
        self.selected = 0;
        self.scroll = 0;
    }

    pub fn pop_char(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let at = self.byte_offset(self.cursor - 1);
        self.query.remove(at);
        self.cursor -= 1;
        self.refresh_results();
        self.selected = 0;
        self.scroll = 0;
    }

    pub fn delete_char(&mut self) {
        if self.cursor >= self.char_count() {
            return;
        }
        let at = self.byte_offset(self.cursor);
        self.query.remove(at);
        self.refresh_results();
        self.selected = 0;
        self.scroll = 0;
    }

    pub fn move_cursor_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn move_cursor_right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.char_count());
    }

    pub fn move_cursor_home(&mut self) {
        self.cursor = 0;
    }

    pub fn move_cursor_end(&mut self) {
        self.cursor = self.char_count();
    }

    pub fn select_next(&mut self) {
        if self.results.is_empty() {
            return;
        }
        if self.selected + 1 < self.results.len() {
            self.selected += 1;
        }
    }

    pub fn select_prev(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    /// The line suffix typed after the file part of the query, if any.
    pub fn line_hint(&self) -> Option<LineHint> {
        split_line_hint(&self.query).1
    }

    pub fn selected_entry(&self) -> Option<&FileEntry> {
        self.results.get(self.selected).map(|r| &r.entry)
    }

    /// The result index at screen row `y`, if `y` lands on a visible row.
    /// The list body starts three rows below `last_rect.y` (top border, the
    /// query prompt, then the separator) and runs `last_inner_height` rows,
    /// so this stays in lock-step with [`render_file_finder`]. Used to map
    /// a mouse click to a result row.
    pub fn row_index_at(&self, y: u16) -> Option<usize> {
        let list_top = self.last_rect.y.saturating_add(3);
        if y < list_top || y - list_top >= self.last_inner_height {
            return None;
        }
        let idx = self.scroll + (y - list_top) as usize;
        (idx < self.results.len()).then_some(idx)
    }

    #[cfg(test)]
    pub fn selected_index(&self) -> usize {
        self.selected
    }

    #[cfg(test)]
    pub fn visible_results(&self) -> &[ScoredResult] {
        &self.results
    }

    fn refresh_results(&mut self) {
        // Only the file part takes part in matching: `alpha:236-239` names
        // alpha.rs and a place in it, and a needle carrying the suffix would
        // match nothing at all (#472).
        let (file_part, hint) = split_line_hint(&self.query);
        self.notice = None;
        // A check still running answers the query before this one.
        self.line_check = None;
        let mut needle: String = file_part.trim().to_lowercase();
        // A hint with no file (`:236`) names a line in nothing: listing every
        // file and opening the first at that line would be a surprise, so it
        // matches nothing, as it did before hints existed. The cut, not a
        // finished hint, is the test: `:236-` and a bare `:` have no file
        // part either, and listing every file for them would flip the list
        // from empty to everything between two keystrokes. Decided before
        // the exact-name pass below, which these queries can never need.
        let cut = file_part.len() != self.query.trim_end().len();
        if needle.is_empty() && (hint.is_some() || cut) {
            self.results = Vec::new();
            return;
        }
        // A trailing `:` (or a half-typed hint) is also how a real file name
        // can end on Linux. When a file is named exactly what was typed, the
        // query is that name and is matched literally; otherwise the file
        // part goes on matching while the user types the rest of the hint.
        // The exact-name check is a second pass over the corpus, taken only
        // on the keystrokes where the query was cut (`alpha:`, `alpha:23-`);
        // it short-circuits on a hit and costs one traversal on a miss.
        if hint.is_none() {
            let raw = self.query.trim().to_lowercase();
            if raw != needle && entries_have_filename(&self.entries, &raw) {
                needle = raw;
            }
        }
        if needle.is_empty() {
            let mut scored: Vec<ScoredResult> = Vec::with_capacity(MAX_RESULTS);
            for entry in self.entries.iter().take(MAX_RESULTS) {
                scored.push(ScoredResult {
                    entry: entry.clone(),
                });
            }
            scored.sort_by(|a, b| a.entry.rel.cmp(&b.entry.rel));
            self.results = scored;
            return;
        }
        let entries: &[FileEntry] = &self.entries;
        // A `:line` query ranks more than a page of matches, so the check
        // can find the files that have the line further down (#2127).
        let keep = if hint.is_some() {
            LINE_CHECK_MAX_FILES
        } else {
            MAX_RESULTS
        };
        // Score in parallel with rayon and accumulate per-thread top-K
        // bounded heaps, then merge. With N=1.5M entries on a typical
        // home dir, this drops keystroke latency from ~300 ms (serial,
        // full sort) to single-digit ms (multi-core, heap of K=40).
        let (merged, matched): (BinaryHeap<RankedSlot>, usize) = entries
            .par_iter()
            .enumerate()
            .fold(
                || (BinaryHeap::<RankedSlot>::with_capacity(MAX_RESULTS + 1), 0),
                |(mut heap, matched), (idx, entry)| {
                    let Some((tier, score)) =
                        score_entry(&needle, &entry.rel_lower, entry.filename_start_lower)
                    else {
                        return (heap, matched);
                    };
                    push_topk(
                        &mut heap,
                        RankedSlot {
                            tier,
                            rel_len: entry.rel.len() as u32,
                            score,
                            idx,
                        },
                        keep,
                    );
                    (heap, matched + 1)
                },
            )
            .reduce(
                || (BinaryHeap::<RankedSlot>::with_capacity(MAX_RESULTS + 1), 0),
                |(mut a, n), (b, m)| {
                    for slot in b {
                        push_topk(&mut a, slot, keep);
                    }
                    (a, n + m)
                },
            );
        // Our Ord orders BEST as LEAST (so the max-heap root is the
        // worst-of-top-K, evictable in O(log K)). into_sorted_vec
        // therefore returns ascending = best-first already.
        let top: Vec<RankedSlot> = merged.into_sorted_vec();
        let ranked: Vec<ScoredResult> = top
            .into_iter()
            .map(|slot| ScoredResult {
                entry: entries[slot.idx].clone(),
            })
            .collect();
        match hint {
            Some(hint) => {
                let more_matched = matched > ranked.len();
                self.start_line_check(ranked, hint.end.unwrap_or(hint.line), more_matched);
            }
            None => self.results = ranked,
        }
    }

    /// With a `:line` hint, list only the matches long enough to have that
    /// line, or the last line of a `:line-line` range (#2127): `a.ts:318`
    /// names the one `a.ts` that has a line 318, not every `a.ts`. When no
    /// match has it, every match stays, longest first, and [`Self::notice`]
    /// says so, since the line is likely mistyped.
    ///
    /// The counting reads files, so it runs on its own thread: a file on a
    /// stalled network mount must not freeze the UI. The keystroke waits
    /// [`LINE_CHECK_WAIT`] for the answer and otherwise draws the matches
    /// unchecked until [`Self::poll_line_check`] applies it. Matches are
    /// read in rank order, each only as far as the line asked for, until a
    /// page of them is known to have the line; each count is kept between
    /// keystrokes. A file that can't be read or counted the editor's way
    /// stays listed, as an unknown.
    fn start_line_check(
        &mut self,
        candidates: Vec<ScoredResult>,
        needed: usize,
        more_matched: bool,
    ) {
        self.results = candidates.iter().take(MAX_RESULTS).cloned().collect();
        if candidates.is_empty() {
            return;
        }
        self.line_check_generation += 1;
        let generation = self.line_check_generation;
        let jobs: Vec<LineJob> = candidates
            .iter()
            .map(|r| LineJob {
                path: r.entry.path.clone(),
                open: self.open_lines.get(&r.entry.path).copied(),
                cached: self.line_counts.get(&r.entry.path).copied(),
            })
            .collect();
        if self.line_worker.is_none() {
            self.line_worker = LineWorker::start();
        }
        let Some(worker) = self.line_worker.as_ref() else {
            return;
        };
        let (tx, rx) = std::sync::mpsc::channel();
        worker.submit(LineBatch {
            generation,
            jobs,
            needed,
            tx,
        });
        let answer = rx.recv_timeout(LINE_CHECK_WAIT).ok();
        self.line_check = Some(LineCheck {
            generation,
            needed,
            candidates,
            more_matched,
            rx,
        });
        if let Some(done) = answer {
            self.apply_line_check(done);
        }
    }

    /// Filter the matches by what a `:line` check found. An answer to an
    /// older query only fills the cache.
    fn apply_line_check(&mut self, done: LineCheckDone) {
        for (path, count) in done.counted {
            self.line_counts.insert(path, count);
        }
        let Some(check) = self.line_check.take_if(|c| c.generation == done.generation) else {
            return;
        };
        let kept: Vec<ScoredResult> = check
            .candidates
            .iter()
            .enumerate()
            .filter(|(i, _)| match done.lines.get(*i) {
                Some(lines) => lines.is_none_or(|n| n >= check.needed),
                // Past where the check stopped: not looked at, so unknown.
                None => true,
            })
            .map(|(_, r)| r.clone())
            .take(MAX_RESULTS)
            .collect();
        // An answer that comes after the keystroke can find a row already
        // picked from the unchecked list: it stays picked if it is still
        // listed, so Enter opens the file chosen, not the first one.
        let chosen = self
            .results
            .get(self.selected)
            .map(|r| r.entry.path.clone());
        let keep_choice = |list: &[ScoredResult]| {
            chosen
                .as_ref()
                .and_then(|p| list.iter().position(|r| &r.entry.path == p))
        };
        if !kept.is_empty() {
            self.notice = None;
            self.selected = keep_choice(&kept).unwrap_or(0);
            self.scroll = self.scroll.min(self.selected);
            self.results = kept;
            return;
        }
        // Every match was counted whole and is short of the line. A stable
        // sort keeps the match order among files of one length.
        let mut ranked: Vec<(ScoredResult, usize)> = check
            .candidates
            .into_iter()
            .zip(done.lines)
            .map(|(r, lines)| (r, lines.unwrap_or(0)))
            .collect();
        ranked.sort_by_key(|(_, lines)| std::cmp::Reverse(*lines));
        let needed = check.needed;
        self.notice = Some(if check.more_matched {
            format!(
                "none of the first {} matches has line {needed} \u{b7} longest first",
                LINE_CHECK_MAX_FILES
            )
        } else {
            format!("no match has line {needed} \u{b7} longest first")
        });
        self.results = ranked
            .into_iter()
            .take(MAX_RESULTS)
            .map(|(r, _)| r)
            .collect();
        self.selected = keep_choice(&self.results).unwrap_or(0);
        self.scroll = self.scroll.min(self.selected);
    }
}

/// Count, in rank order, the lines of the matches a `:line` query names,
/// until a page of them is known to reach `needed` or can't be told (#2127).
/// Off the UI thread: every read here may wait on the filesystem. Returns
/// each checked candidate's lines (`None` when unknown) and the counts read
/// from disk, for the cache.
fn check_lines(
    jobs: Vec<LineJob>,
    needed: usize,
) -> (Vec<Option<usize>>, Vec<(PathBuf, LineCount)>) {
    let mut budget = LINE_COUNT_BUDGET;
    let mut lines = Vec::with_capacity(MAX_RESULTS);
    let mut counted = Vec::new();
    let mut kept = 0;
    for job in jobs {
        if kept == MAX_RESULTS {
            break;
        }
        let found = match job.open {
            Some(n) => Some(n),
            None => {
                disk_line_count(&job.path, job.cached, needed, &mut budget).map(|(c, fresh)| {
                    if fresh {
                        counted.push((job.path.clone(), c));
                    }
                    c.lines
                })
            }
        };
        if found.is_none_or(|n| n >= needed) {
            kept += 1;
        }
        lines.push(found);
    }
    (lines, counted)
}

/// The lines of the saved file at `path`, counted no further than `needed`,
/// from `cached` while the file's size and time are unchanged; and whether
/// the count is new. `None` for anything but a regular file (a FIFO would
/// wait for a writer, a device could stream forever) and for a file
/// [`count_lines`] can't count.
fn disk_line_count(
    path: &Path,
    cached: Option<LineCount>,
    needed: usize,
    budget: &mut u64,
) -> Option<(LineCount, bool)> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let (len, modified) = (meta.len(), meta.modified().ok());
    if let Some(c) = cached
        && c.len == len
        && c.modified == modified
        && (c.complete || c.lines >= needed)
    {
        return Some((c, false));
    }
    let (lines, complete) = count_lines(path, needed, budget)?;
    Some((
        LineCount {
            lines,
            complete,
            len,
            modified,
        },
        true,
    ))
}

/// The lines in the file at `path` as the editor splits them (#2127): `\n`,
/// `\r\n` and a lone `\r` each end one, a last line with no ending counts,
/// and an empty file has one. Read no further than line `cap`: `(cap, false)`
/// as soon as that line has begun, `(n, true)` with the whole count when the
/// file has fewer. `None` when the file can't be read, holds a NUL byte, or
/// `budget` (bytes, shared by one check's reads) runs out first. A NUL is
/// UTF-16 text, which the editor decodes before it splits lines and whose
/// bytes hold `\n` that are not line ends, or a binary the editor shows as
/// hex; every other encoding the editor reads ends lines with these bytes.
fn count_lines(path: &Path, cap: usize, budget: &mut u64) -> Option<(usize, bool)> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).ok()?;
    let mut buf = vec![0u8; 64 * 1024];
    // Line ends seen; whether a line has begun since the last; whether the
    // last byte was a `\r`, which a `\n` right after only completes.
    let (mut ended, mut begun, mut after_cr) = (0usize, false, false);
    loop {
        if ended + usize::from(begun) >= cap {
            return Some((cap, false));
        }
        if *budget == 0 {
            return None;
        }
        let n = file.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        *budget = budget.saturating_sub(n as u64);
        let chunk = &buf[..n];
        if memchr::memchr(0, chunk).is_some() {
            return None;
        }
        let mut pos = 0;
        for at in memchr::memchr2_iter(b'\n', b'\r', chunk) {
            if at > pos {
                begun = true;
                after_cr = false;
            }
            if chunk[at] == b'\n' && after_cr {
                after_cr = false;
            } else {
                ended += 1;
                begun = false;
                after_cr = chunk[at] == b'\r';
            }
            pos = at + 1;
            if ended >= cap {
                return Some((cap, false));
            }
        }
        if pos < n {
            begun = true;
            after_cr = false;
        }
    }
    Some(((ended + usize::from(begun)).max(1), true))
}

/// One ranked match candidate. Ordering is "worse comes first" so a
/// `BinaryHeap` (max-heap) keeps the WORST of the top-K at the root,
/// letting us evict it in O(log K) when a better candidate arrives.
#[derive(Clone, Copy, Debug)]
struct RankedSlot {
    tier: MatchTier,
    rel_len: u32,
    score: i32,
    idx: usize,
}

impl PartialEq for RankedSlot {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for RankedSlot {}

impl PartialOrd for RankedSlot {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RankedSlot {
    /// Mirrors the old sort_by in refresh_results, but inverted so
    /// the BinaryHeap (max-heap) root is the WORST candidate in the
    /// current top-K. Tie-break order, BEST -> WORST:
    ///   1. Higher MatchTier (ExactFilename > Subsequence)
    ///   2. Shorter rel.len (workspace-root storage.py beats deep)
    ///   3. Higher fuzzy score
    ///   4. Lexicographic rel ascending (handled at idx tiebreak)
    fn cmp(&self, other: &Self) -> Ordering {
        // Worse-first: invert each comparison vs "best".
        other
            .tier
            .cmp(&self.tier)
            .then_with(|| self.rel_len.cmp(&other.rel_len))
            .then_with(|| other.score.cmp(&self.score))
            .then_with(|| self.idx.cmp(&other.idx))
    }
}

fn push_topk(heap: &mut BinaryHeap<RankedSlot>, slot: RankedSlot, k: usize) {
    if heap.len() < k {
        heap.push(slot);
        return;
    }
    // heap.peek() is the worst-of-top-K under our Ord. If incoming is
    // strictly better (= less under cmp), evict + insert.
    if let Some(worst) = heap.peek()
        && slot.cmp(worst) == Ordering::Less
    {
        heap.pop();
        heap.push(slot);
    }
}

/// Split a trailing line hint off a quick-open query (#472): the file part
/// and, when the query ends in `:line`, `:line:col` or `:line-line`, the
/// parsed hint. The first `:` whose remainder parses as a hint is the cut,
/// so a name that itself contains a colon keeps it. A `:` followed by a hint
/// still being typed (`alpha:`, `alpha:236-`) is cut with no hint, so the
/// file part keeps matching between keystrokes; `refresh_results` restores
/// the literal query when a file is named exactly that. Any other unparsed
/// suffix (`foo:bar`) stays part of the file name.
fn split_line_hint(query: &str) -> (&str, Option<LineHint>) {
    let q = query.trim_end();
    for (i, _) in q.match_indices(':') {
        let rest = &q[i + 1..];
        if let Some(hint) = parse_line_hint(rest) {
            return (&q[..i], Some(hint));
        }
        // A hint still being typed (`alpha:`, `alpha:236-`): keep matching
        // on the file part so the list does not empty between keystrokes,
        // with no hint until it parses.
        if is_partial_line_hint(rest) {
            return (&q[..i], None);
        }
    }
    (q, None)
}

/// Digits, at most one `-` or `:` separator, digits: every prefix of a hint
/// a user types on the way to a complete one, the empty string included.
fn is_partial_line_hint(s: &str) -> bool {
    let mut seen_sep = false;
    for b in s.bytes() {
        match b {
            b'0'..=b'9' => {}
            b'-' | b':' if !seen_sep => seen_sep = true,
            _ => return false,
        }
    }
    true
}

fn parse_line_hint(s: &str) -> Option<LineHint> {
    // One-based as typed; a `0` is not a line anyone can land on.
    // A number too large for usize saturates rather than vanishing: the open
    // path clamps out-of-range lines to the file, and a hint that silently
    // disappeared would open the file at the top with no signal.
    let num = |t: &str| {
        (!t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()))
            .then(|| t.parse::<usize>().unwrap_or(usize::MAX))
            .filter(|n| *n > 0)
    };
    if let Some((a, b)) = s.split_once('-') {
        // `239-236` is the same range as `236-239`; the caller clamps the
        // selection from `line` to `end`, so the pair is ordered here.
        let (a, b) = (num(a)?, num(b)?);
        return Some(LineHint {
            line: a.min(b),
            end: Some(a.max(b)),
            col: None,
        });
    }
    if let Some((a, b)) = s.split_once(':') {
        if b.is_empty() || !b.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        // A zero column is common 0-based tool output: the line is kept and
        // the column dropped, as the terminal's path:line:col parser does,
        // rather than the whole hint vanishing.
        return Some(LineHint {
            line: num(a)?,
            end: None,
            col: num(b),
        });
    }
    Some(LineHint {
        line: num(s)?,
        end: None,
        col: None,
    })
}

/// Whether some entry's file name is exactly `name` (already lower-cased).
fn entries_have_filename(entries: &[FileEntry], name: &str) -> bool {
    entries
        .par_iter()
        .any(|e| &e.rel_lower[e.filename_start_lower..] == name)
}

pub fn score_entry(
    needle: &str,
    hay_lower: &str,
    filename_start: usize,
) -> Option<(MatchTier, i32)> {
    if needle.is_empty() {
        return Some((MatchTier::Subsequence, 0));
    }
    let filename = &hay_lower[filename_start..];
    if filename == needle {
        return Some((MatchTier::ExactFilename, 10_000));
    }
    if filename.starts_with(needle) {
        return Some((MatchTier::FilenamePrefix, 5_000));
    }
    if filename.contains(needle) {
        return Some((MatchTier::FilenameSubstring, 2_500));
    }
    if hay_lower.contains(needle) {
        return Some((MatchTier::PathSubstring, 1_000));
    }
    fuzzy_score(needle, hay_lower, filename_start).map(|s| (MatchTier::Subsequence, s))
}

pub fn fuzzy_score(needle: &str, hay_lower: &str, filename_start: usize) -> Option<i32> {
    if needle.is_empty() {
        return Some(0);
    }
    // Chars, not bytes: comparing UTF-8 bytes let the pieces of two
    // different characters combine into a match (`é` found inside `ã©`),
    // which translated command titles and non-ASCII names hit. Offsets stay
    // byte offsets, which is what `filename_start` counts.
    let mut needle_chars = needle.chars().peekable();
    let mut score: i32 = 0;
    let mut consecutive: i32 = 0;
    let mut prev_match: Option<usize> = None;
    let mut prev: Option<(usize, char)> = None;
    for (i, c) in hay_lower.char_indices() {
        let Some(&want) = needle_chars.peek() else {
            break;
        };
        if c == want {
            let mut bonus: i32 = 1;
            if i >= filename_start {
                bonus += 4;
            }
            let at_word_boundary = i == 0
                || i == filename_start
                || matches!(prev, Some((_, '/' | '_' | '-' | '.' | ' ')));
            if at_word_boundary {
                bonus += 5;
            }
            if prev_match.is_some() && prev_match == prev.map(|(p, _)| p) {
                consecutive += 1;
                bonus += consecutive * 5;
            } else {
                if let Some(p) = prev_match {
                    let gap = i.saturating_sub(p + 1) as i32;
                    bonus -= gap.min(20);
                }
                consecutive = 0;
            }
            score += bonus;
            prev_match = Some(i);
            needle_chars.next();
        }
        prev = Some((i, c));
    }
    if needle_chars.peek().is_none() {
        Some(score - (hay_lower.len() as i32 / 4))
    } else {
        None
    }
}

/// Directory names that are vendored / generated / VCS-internal and
/// will never be a useful Cmd+P target. Including these in the index
/// would drown signal in noise (every `.git/objects/*` blob,
/// `node_modules/**`, `target/debug/incremental/*`, etc.) and balloon
/// the walk by orders of magnitude. The match is exact on the path
/// component name - a real workspace file literally named `target.md`
/// is still indexed.
const NOISE_DIR_NAMES: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".bzr",
    "node_modules",
    "target",
    "__pycache__",
    ".venv",
    "venv",
    ".tox",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    ".next",
    ".nuxt",
    ".cache",
    "dist",
    "build",
    ".gradle",
    ".idea",
    ".vscode-test",
    ".DS_Store",
    // Package-manager, language-toolchain, and agent caches that live in
    // $HOME. Never edit targets, and several (.cargo, .claude, .npm) are
    // written to continuously, so watching them turns a home-dir workspace
    // into a perpetual FS-event storm. Measured on a Linux remote opened at
    // /root: ~7000 inotify watches and 39% of CPU stuck in readlink
    // path-walks re-scanning these trees (~200% CPU total). They dominate
    // both the watch count and the churn, so excluding them is the fix.
    ".cargo",
    ".rustup",
    ".npm",
    ".nvm",
    ".bun",
    ".pnpm-store",
    ".deno",
    ".gem",
    ".pyenv",
    ".rbenv",
    ".m2",
    ".local",
    ".docker",
    ".claude",
    ".codex",
    // The same class of $HOME toolchain cache for the .NET, Dart and Go
    // ecosystems, plus oh-my-zsh's plugin tree. Measured on a Linux
    // remote opened at /root: .nuget 1127 dirs, .pub-cache 1271,
    // go/pkg/mod 1357 (below, by path) — together the top consumers of a
    // readlink storm that held ~210% CPU for 19 minutes without
    // finishing the walk.
    ".nuget",
    ".pub-cache",
    ".dart-tool",
    ".dotnet",
    ".oh-my-zsh",
];

/// Noise trees whose LEADING component is too generic to blacklist by
/// name: a workspace really can hold a `go/` source directory, so only
/// the module/checksum caches GOPATH parks in `$HOME` are pruned. Matched
/// as a consecutive component run anywhere in the path, so both the cache
/// root and everything under it are covered.
const NOISE_PATH_RUNS: &[&[&str]] = &[&["go", "pkg", "mod"], &["go", "pkg", "sumdb"]];

/// Number of `Normal` components in `path` — its depth, ignoring the root
/// prefix and any `.` / `..` the caller left in.
fn normal_component_count(path: &Path) -> usize {
    path.components()
        .filter(|c| matches!(c, std::path::Component::Normal(_)))
        .count()
}

/// True when `run` appears as consecutive `Normal` components of `path`
/// ENDING deeper than `min_end` components in — that is, the run must reach
/// below the first `min_end` components. `0` matches a run anywhere.
///
/// The depth bound is what lets a run straddle a workspace root: the head of
/// the run may lie in the root's own path as long as its tail does not.
fn has_component_run(path: &Path, run: &[&str], min_end: usize) -> bool {
    let comps: Vec<&std::ffi::OsStr> = path
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s),
            _ => None,
        })
        .collect();
    comps.windows(run.len()).enumerate().any(|(i, w)| {
        i + run.len() > min_end
            && w.iter()
                .zip(run)
                .all(|(a, b)| *a == std::ffi::OsStr::new(b))
    })
}

pub fn is_noise_dir(name: &std::ffi::OsStr) -> bool {
    name.to_str().is_some_and(|n| NOISE_DIR_NAMES.contains(&n))
}

/// True when `path` contains one of the [`NOISE_PATH_RUNS`] cache trees,
/// measured RELATIVE TO `root`: the run must REACH BELOW the root.
///
/// The relative part is load-bearing. The workspace root's OWN path is not
/// part of the workspace, so matching the absolute path would prune the
/// entire tree of anyone whose checkout happens to live under a matching
/// ancestor — opening a dependency inside `~/go/pkg/mod/...` to read it
/// would index zero files.
///
/// "Reaches below the root" rather than "lies below it", because a run can
/// STRADDLE the root. A workspace opened at `~/go` — the ordinary GOPATH —
/// keeps its cache at `pkg/mod`, a run whose first component is the root's
/// own last one. Stripping the root off and matching what remained missed
/// exactly that, re-admitting the tree this rule exists to prune.
pub fn has_noise_path_run(path: &Path, root: &Path) -> bool {
    // Components at or above the root cannot make an entry noise on their
    // own; they can only supply the head of a run whose tail is below it. A
    // path outside the root has no root to be measured against, so every
    // component of it counts, as it did before roots entered this rule.
    let depth = if path.starts_with(root) {
        normal_component_count(root)
    } else {
        0
    };
    NOISE_PATH_RUNS
        .iter()
        .any(|r| has_component_run(path, r, depth))
}

/// True when `path` is noise measured RELATIVE TO `root`: it carries a
/// noise-named component, or one of the [`NOISE_PATH_RUNS`] cache trees, at
/// or below the root.
///
/// This is the reading every EVENT path wants. [`is_path_under_noise_dir`]
/// answers the same question about a path in isolation, which is right for
/// the walk (it starts at the root, so everything it reaches is already
/// below it) and wrong for a watcher event, whose path is absolute: a
/// checkout at `~/build/myapp` would have `build` matched among its own
/// ancestors and every file in it classified as noise. The root's ancestors
/// are not part of the workspace, exactly as in [`has_noise_path_run`].
///
/// A `path` that is not under `root` is measured whole. There is no root for
/// its components to be relative to, and a path outside the workspace is not
/// workspace content whichever way it is read.
pub fn is_noise_path_under_root(path: &Path, root: &Path) -> bool {
    let rel = path.strip_prefix(root).unwrap_or(path);
    is_path_under_noise_dir(rel) || has_noise_path_run(path, root)
}

/// True when `path` sits inside (or names) one of the noise dirs the
/// finder index already excludes from its walk. Callers use this to
/// avoid kicking a full index rebuild for FS events the finder would
/// have ignored anyway — e.g. cargo writing into `target/`, npm into
/// `node_modules/`, git into `.git/`. Without this gate, a sustained
/// cargo build produces a continuous index-rebuild storm that pegs
/// every core via the parallel walker.
pub fn is_path_under_noise_dir(path: &Path) -> bool {
    path.components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s),
            _ => None,
        })
        .any(is_noise_dir)
}

/// Absolute paths to prune from the walk for two reasons:
///   1. `Library/Containers` and `Library/Group Containers` trigger a
///      macOS TCC consent prompt ("App would like to access data from
///      other apps") because they live behind
///      kTCCServiceSystemPolicyAppData.
///   2. The Library/Caches, Library/Application Support, Library/Mobile
///      Documents, Library/Developer, Library/CoreSimulator,
///      Library/Mail, Library/Messages, Library/CloudStorage,
///      Library/Safari, Library/Cookies trees collectively account
///      for ~1.4M of the ~1.5M files found by a `~`-rooted walk on a
///      typical developer Mac. None of those are useful Cmd+P
///      targets, and including them blows per-keystroke match
///      latency from ~5ms to ~300ms. Library/Logs is kept in because
///      log files DO get opened (the user's lsp.log lives at
///      `~/.croft/lsp.log` but `~/Library/Logs/*` is a reasonable
///      Cmd+P target too).
fn macos_blocked_paths() -> Vec<PathBuf> {
    match std::env::var_os("HOME") {
        Some(home) => macos_blocked_paths_for(&PathBuf::from(home)),
        None => Vec::new(),
    }
}

/// The pure half of [`macos_blocked_paths`], taking the home directory as
/// an argument so tests never repoint the process-global `$HOME` (#37):
/// the env var is resolved at call time by every concurrent reader (the
/// PDF rasteriser's cache dir, the session paths), and a substituted value
/// leaking across test threads made unrelated tests fail on paths inside
/// the fake home.
fn macos_blocked_paths_for(home: &Path) -> Vec<PathBuf> {
    let lib = home.join("Library");
    [
        "Containers",
        "Group Containers",
        "Caches",
        "Application Support",
        "Mobile Documents",
        "CloudStorage",
        "Developer",
        "CoreSimulator",
        "Mail",
        "Messages",
        "Safari",
        "Cookies",
        "Metadata",
        "Suggestions",
        "Biome",
        "Calendars",
        "Reminders",
        "PersonalizationPortrait",
        "HomeKit",
        "Photos",
        "Daemon Containers",
        "WebKit",
    ]
    .iter()
    .map(|leaf| lib.join(leaf))
    .collect()
}

pub fn build_file_index(root: &Path) -> Vec<FileEntry> {
    build_file_index_with_blocked(root, macos_blocked_paths())
}

/// The Quick Open index over EVERY workspace root (#147). One root is the
/// plain per-root index; with more, each root's entries carry a
/// `<root-name>/` prefix on `rel` — the VS Code multi-root label rule — so
/// same-named files in different roots stay distinguishable and ranking
/// never mixes two projects' bare paths. `filename_start` shifts with the
/// prefix, keeping the filename-tier scoring anchored on the filename.
pub fn build_multi_file_index(roots: &[PathBuf]) -> Vec<FileEntry> {
    if roots.len() == 1 {
        return build_file_index(&roots[0]);
    }
    let labels = crate::workspace::root_display_labels(roots);
    let mut out = Vec::new();
    for (root, label) in roots.iter().zip(&labels) {
        let prefix = format!("{label}/");
        for mut e in build_file_index(root) {
            e.rel = format!("{prefix}{}", e.rel);
            // Recompute BOTH offsets from the final strings: hand-shifting
            // by prefix.len() poisoned rel_lower's offset whenever the
            // label's lowercase changed byte length (#148 review).
            e.rel_lower = e.rel.to_lowercase();
            e.filename_start = e.rel.rfind('/').map(|i| i + 1).unwrap_or(0);
            e.filename_start_lower = e.rel_lower.rfind('/').map(|i| i + 1).unwrap_or(0);
            out.push(e);
        }
    }
    out
}

/// [`build_file_index`] with the blocked-path set injected, so tests drive
/// the macOS exclusion rules against a fake home without mutating `$HOME`.
fn build_file_index_with_blocked(root: &Path, blocked: Vec<PathBuf>) -> Vec<FileEntry> {
    let collected: Arc<Mutex<Vec<FileEntry>>> = Arc::new(Mutex::new(Vec::new()));
    let root_buf = root.to_path_buf();
    let filter_root = root.to_path_buf();
    let walker = WalkBuilder::new(root)
        .git_ignore(true)
        .require_git(false)
        // .hidden(false) so user dot-dirs (.croft, .cursor, .config,
        // .continue, .databridge) ARE walked - the user explicitly
        // wants to Cmd+P into them. NOISE_DIR_NAMES below prunes the
        // vendored / generated dot-dirs (.git, .venv, .mypy_cache,
        // .next, .cache, ...) that would otherwise drown the index.
        .hidden(false)
        .filter_entry(move |entry| {
            let depth = entry.depth();
            if depth == 0 {
                return true;
            }
            if is_noise_dir(entry.file_name()) {
                return false;
            }
            if has_noise_path_run(entry.path(), &filter_root) {
                return false;
            }
            // Hard-skip macOS app-data containers so readdir does not
            // trip a TCC consent prompt on every launch. starts_with
            // matches both the container root and any descendant.
            for b in &blocked {
                if entry.path().starts_with(b) {
                    return false;
                }
            }
            true
        })
        .build_parallel();
    walker.run(|| {
        let collected = collected.clone();
        let root_buf = root_buf.clone();
        Box::new(move |entry| {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => return WalkState::Continue,
            };
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                return WalkState::Continue;
            }
            let path = entry.path().to_path_buf();
            let rel_path = path.strip_prefix(&root_buf).unwrap_or(&path);
            let rel = rel_path.to_string_lossy().replace('\\', "/");
            if rel.is_empty() {
                return WalkState::Continue;
            }
            let rel_lower = rel.to_lowercase();
            let filename_start = rel.rfind('/').map(|i| i + 1).unwrap_or(0);
            let filename_start_lower = rel_lower.rfind('/').map(|i| i + 1).unwrap_or(0);
            if let Ok(mut g) = collected.lock() {
                g.push(FileEntry {
                    path,
                    rel,
                    rel_lower,
                    filename_start,
                    filename_start_lower,
                });
            }
            WalkState::Continue
        })
    });
    let mut out = Arc::try_unwrap(collected)
        .map(|m| m.into_inner().unwrap_or_default())
        .unwrap_or_default();
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    out
}

/// Draw Go to File over `area`. `center` is Quick Input Position: Center;
/// `backdrop` lets a `surfaces` theme dim what is behind the card (the App
/// withholds it where inline images would not dim with the text).
pub fn render_file_finder(
    finder: &mut FileFinder,
    area: Rect,
    buf: &mut Buffer,
    theme: crate::theme::Theme,
    center: bool,
    backdrop: bool,
) {
    if let Some(t) = crate::widgets::surface::tones(theme) {
        render_finder_card(finder, area, buf, theme, t, center, backdrop);
        return;
    }
    finder.last_caret = None;
    let width = area.width.saturating_mul(7) / 10;
    let width = width.clamp(40, 100).min(area.width);
    let height = area.height.saturating_mul(6) / 10;
    let height = height.max(10).min(area.height);
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    // Quick Input Position: Top anchors in the upper third (VS Code's
    // default); Center pins it to the vertical middle.
    let y = if center {
        area.y + (area.height.saturating_sub(height)) / 2
    } else {
        area.y + (area.height.saturating_sub(height)) / 4
    };
    let rect = Rect {
        x,
        y,
        width,
        height,
    };
    finder.last_rect = rect;

    Widget::render(Clear, rect, buf);
    let title = Span::styled(
        " Go to File — Esc to close, ↑/↓ to navigate, Enter to open ",
        Style::default()
            .fg(theme.ui(Color::Rgb(0xff, 0xff, 0xff)))
            .add_modifier(Modifier::BOLD),
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.ui(Color::Rgb(0x4e, 0x9a, 0xff))))
        .title(title.clone())
        .style(Style::default().bg(theme.ui(Color::Rgb(0x16, 0x18, 0x1f))));
    let inner = Rect {
        x: rect.x + 1,
        y: rect.y + 1,
        width: rect.width.saturating_sub(2),
        height: rect.height.saturating_sub(2),
    };
    Widget::render(block, rect, buf);
    // Black theme: gradient border over the solid one, then re-stamp the title.
    if theme.framed_overlays() {
        crate::widgets::surface::paint_overlay_frame(buf, rect, theme);
        buf.set_span(rect.x + 1, rect.y, &title, title.width() as u16);
    }
    let sel_bg = if theme.gradient() {
        let (r, g, b) = crate::gradient::POPUP_SEL_BG;
        Color::Rgb(r, g, b)
    } else {
        theme.ui(Color::Rgb(0x1e, 0x3a, 0x6e))
    };

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let query_style = Style::default()
        .fg(theme.ui(Color::Rgb(0xec, 0xef, 0xf4)))
        .add_modifier(Modifier::BOLD);
    let caret_style = Style::default()
        .fg(theme.ui(Color::Rgb(0x16, 0x18, 0x1f)))
        .bg(theme.ui(Color::Rgb(0xec, 0xef, 0xf4)))
        .add_modifier(Modifier::SLOW_BLINK);
    let cursor = finder.cursor.min(finder.query.chars().count());
    let before: String = finder.query.chars().take(cursor).collect();
    let at: String = finder.query.chars().skip(cursor).take(1).collect();
    let after: String = finder.query.chars().skip(cursor + 1).collect();
    let caret_glyph = if at.is_empty() { String::from(" ") } else { at };
    let prompt_line = Line::from(vec![
        Span::styled(
            "> ",
            Style::default().fg(theme.ui(Color::Rgb(0x88, 0xc0, 0xd0))),
        ),
        Span::styled(before, query_style),
        Span::styled(caret_glyph, caret_style),
        Span::styled(after, query_style),
    ]);
    let prompt_rect = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: 1,
    };
    Widget::render(Paragraph::new(prompt_line), prompt_rect, buf);

    let separator_rect = Rect {
        x: inner.x,
        y: inner.y + 1,
        width: inner.width,
        height: 1,
    };
    let rule = Style::default().fg(theme.ui(Color::Rgb(0x3b, 0x42, 0x52)));
    let sep_line = match finder.notice.as_deref() {
        // No match has the line asked for (#2127): said on the rule, above
        // the list it explains.
        Some(notice) => {
            let label = format!("\u{2500} {notice} ");
            let fill = (separator_rect.width as usize).saturating_sub(label.chars().count());
            Line::from(vec![
                Span::styled(
                    label,
                    Style::default().fg(theme.ui(Color::Rgb(0xeb, 0xcb, 0x8b))),
                ),
                Span::styled("\u{2500}".repeat(fill), rule),
            ])
        }
        None => Line::from(Span::styled(
            "\u{2500}".repeat(separator_rect.width as usize),
            rule,
        )),
    };
    Widget::render(Paragraph::new(sep_line), separator_rect, buf);

    let list_rect = Rect {
        x: inner.x,
        y: inner.y + 2,
        width: inner.width,
        height: inner.height.saturating_sub(2),
    };
    finder.last_inner_height = list_rect.height;
    if list_rect.height == 0 {
        return;
    }

    let visible = list_rect.height as usize;
    let total = finder.results.len();
    if finder.selected >= finder.scroll + visible {
        finder.scroll = finder.selected + 1 - visible;
    }
    if finder.selected < finder.scroll {
        finder.scroll = finder.selected;
    }
    let end = (finder.scroll + visible).min(total);

    if total == 0 {
        let empty = if finder.query.trim().is_empty() {
            Line::from(Span::styled(
                "  (no files indexed)",
                Style::default().fg(theme.ui(Color::Rgb(0x7a, 0x82, 0x90))),
            ))
        } else {
            Line::from(Span::styled(
                format!("  No matches for '{}'", finder.query),
                Style::default().fg(theme.ui(Color::Rgb(0x7a, 0x82, 0x90))),
            ))
        };
        Widget::render(Paragraph::new(empty), list_rect, buf);
        return;
    }

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(end - finder.scroll);
    for (offset, result) in finder.results[finder.scroll..end].iter().enumerate() {
        let row_idx = finder.scroll + offset;
        let is_selected = row_idx == finder.selected;
        let row_style = if is_selected {
            Style::default().bg(sel_bg).fg(theme.ui(Color::White))
        } else {
            Style::default().fg(theme.ui(Color::Rgb(0xec, 0xef, 0xf4)))
        };
        let dir_style = if is_selected {
            Style::default()
                .bg(sel_bg)
                .fg(theme.ui(Color::Rgb(0xa0, 0xb4, 0xd8)))
        } else {
            Style::default().fg(theme.ui(Color::Rgb(0x8e, 0x95, 0xa4)))
        };
        let (dir_part, file_part) = split_dir_file(&result.entry.rel, result.entry.filename_start);
        let prefix = if is_selected { "> " } else { "  " };
        let spans: Vec<Span<'static>> = vec![
            Span::styled(prefix.to_string(), row_style),
            Span::styled(
                file_part.to_string(),
                row_style.add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                if dir_part.is_empty() {
                    String::new()
                } else {
                    format!("  {dir_part}")
                },
                dir_style,
            ),
        ];
        lines.push(Line::from(spans));
    }
    Widget::render(Paragraph::new(lines), list_rect, buf);
}

/// Go to File as a `surfaces` theme draws it: the shared quick-input card
/// ([`crate::widgets::quick_input`]) with no title on its edge, a `›`
/// prompt, and rows that read like the Explorer's: the file-type icon, the
/// file name in body ink, its folder muted after it, and the query's
/// matched characters in the accent.
fn render_finder_card(
    finder: &mut FileFinder,
    area: Rect,
    buf: &mut Buffer,
    theme: crate::theme::Theme,
    t: crate::widgets::surface::Tones,
    center: bool,
    backdrop: bool,
) {
    use crate::widgets::quick_input::{self as qi, Card, Input, Row};
    let width = (area.width.saturating_mul(7) / 10).clamp(40, 100);
    let max_h = (area.height.saturating_mul(6) / 10).max(10);
    let total = finder.results.len();
    let card = Card::layout(area, width, max_h, total, center);
    finder.last_rect = card.rect;
    finder.last_inner_height = card.list.height;
    if backdrop {
        crate::widgets::surface::dim_backdrop(buf, card.rect, theme);
    }
    let (file_query, _) = split_line_hint(&finder.query);
    let file_query = file_query.trim();
    let count = if let Some(notice) = &finder.notice {
        // No match has the line asked for (#2127): the count's place says so.
        notice.clone()
    } else if finder.query.trim().is_empty() {
        qi::count_label(finder.entries.len(), false, false, "file", "files")
    } else {
        qi::count_label(total, true, total >= MAX_RESULTS, "file", "files")
    };
    finder.last_caret = qi::paint_card(
        buf,
        &card,
        theme,
        t,
        &Input {
            glyph: "\u{203a}",
            query: &finder.query,
            cursor: finder.cursor,
            placeholder: "Search files by name \u{2014} append :line to jump",
            count: &count,
            verb: "open",
            keep: None,
        },
    );
    let visible = card.list.height as usize;
    if visible == 0 {
        return;
    }
    if total == 0 {
        let empty = if finder.entries.is_empty() {
            "No files indexed"
        } else {
            "No matching files"
        };
        qi::paint_empty(buf, &card, t, empty);
        return;
    }
    if finder.selected >= finder.scroll + visible {
        finder.scroll = finder.selected + 1 - visible;
    }
    if finder.selected < finder.scroll {
        finder.scroll = finder.selected;
    }
    let end = (finder.scroll + visible).min(total);
    let hit = qi::hit_style(t);
    for (offset, result) in finder.results[finder.scroll..end].iter().enumerate() {
        let selected = finder.scroll + offset == finder.selected;
        let entry = &result.entry;
        let (dir_part, file_part) = split_dir_file(&entry.rel, entry.filename_start);
        let offsets = finder_match_offsets(&entry.rel, entry.filename_start, file_query);
        let suffix = Path::new(file_part)
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();
        let icon = crate::icons::for_path(file_part, &suffix);
        let glyph = Span::styled(
            icon.glyph.to_string(),
            Style::default().fg(theme.ui(icon.color)),
        );
        let mut label = qi::highlighted(
            file_part,
            entry.filename_start,
            &offsets,
            Style::default().fg(t.text()),
            hit,
        );
        if !dir_part.is_empty() {
            let dir_style = Style::default().fg(t.muted());
            label.push(Span::styled("  ", dir_style));
            label.extend(qi::highlighted(dir_part, 0, &offsets, dir_style, hit));
        }
        qi::paint_row(
            buf,
            &card,
            offset as u16,
            t,
            Row {
                selected,
                glyph: Some(glyph),
                label,
                trail: Vec::new(),
            },
        );
    }
    qi::paint_scroll_cue(buf, &card, t, total, finder.scroll);
}

/// The byte offsets in `rel` to light for `query`, in the order the finder
/// ranks: a run inside the file name first (the name tiers), then a run
/// anywhere in the path, then the fuzzy subsequence over the path.
fn finder_match_offsets(rel: &str, filename_start: usize, query: &str) -> Vec<usize> {
    use crate::widgets::quick_input::{match_offsets, run_offsets};
    if query.is_empty() {
        return Vec::new();
    }
    match run_offsets(&rel[filename_start..], query) {
        Some(run) => run.into_iter().map(|b| b + filename_start).collect(),
        None => match_offsets(rel, query),
    }
}

fn split_dir_file(rel: &str, filename_start: usize) -> (&str, &str) {
    if filename_start == 0 {
        ("", rel)
    } else {
        let dir = &rel[..filename_start.saturating_sub(1)];
        let file = &rel[filename_start..];
        (dir, file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loch_finder(names: &[&str], query: &str) -> (FileFinder, Buffer) {
        let entries: Vec<FileEntry> = names
            .iter()
            .map(|rel| {
                let start = rel.rfind('/').map_or(0, |i| i + 1);
                FileEntry {
                    path: PathBuf::from(rel),
                    rel: rel.to_string(),
                    rel_lower: rel.to_lowercase(),
                    filename_start: start,
                    filename_start_lower: start,
                }
            })
            .collect();
        let mut f = FileFinder::new(Arc::new(entries));
        f.set_query(query);
        let area = Rect::new(0, 0, 120, 40);
        let mut buf = Buffer::empty(area);
        render_file_finder(
            &mut f,
            area,
            &mut buf,
            crate::theme::Theme::from_id("loch"),
            false,
            false,
        );
        (f, buf)
    }

    fn row_text(buf: &Buffer, rect: Rect, y: u16) -> String {
        (rect.x..rect.right())
            .map(|x| buf[(x, y)].symbol().to_string())
            .collect()
    }

    /// Croft Loch: a titleless card, a row per file with its icon (in its
    /// own colour), the name in body ink with the match in bold strong ink
    /// (the accent is the selection's alone), the folder muted, and the
    /// selected row's accent bar on the card's left edge.
    #[test]
    fn croft_loch_finder_rows_read_like_the_explorer() {
        let t = crate::widgets::surface::tones(crate::theme::Theme::from_id("loch")).unwrap();
        let (f, buf) = loch_finder(&["src/app/main.rs", "README.md"], "main");
        let rect = f.last_rect;
        let top = row_text(&buf, rect, rect.y);
        assert!(!top.contains("Go to File"), "{top:?}");
        let y = rect.y + 3;
        let row = row_text(&buf, rect, y);
        let icon = crate::icons::for_path("main.rs", ".rs");
        assert!(
            row.starts_with(&format!("\u{258e} {} main.rs  src/app", icon.glyph)),
            "{row:?}"
        );
        assert_eq!(buf[(rect.x, y)].fg, t.accent(), "the selection bar");
        let name = rect.x + 4;
        assert_eq!(buf[(name, y)].fg, t.strong(), "`main` matched");
        assert_eq!(
            buf[(rect.x + 2, y)].fg,
            crate::theme::Theme::from_id("loch").ui(icon.color),
            "the icon keeps its colour"
        );
        assert!(buf[(name, y)].modifier.contains(Modifier::BOLD));
        assert_eq!(buf[(name + 4, y)].fg, t.text(), "the rest of the name");
        assert!(!buf[(name + 4, y)].modifier.contains(Modifier::BOLD));
        assert_eq!(buf[(name + 9, y)].fg, t.muted(), "the folder");
        let input = row_text(&buf, rect, rect.y + 1);
        assert!(input.contains("\u{203a} main"), "{input:?}");
        assert!(input.contains("1 match"), "{input:?}");
        assert_eq!(f.last_caret, Some((rect.x + 8, rect.y + 1)));
    }

    /// Croft Loch: with nothing typed the placeholder says what to type and
    /// the count is the index's size.
    #[test]
    fn croft_loch_finder_shows_a_placeholder_and_the_index_size() {
        let (f, buf) = loch_finder(&["a.rs", "b.rs", "c.rs"], "");
        let input = row_text(&buf, f.last_rect, f.last_rect.y + 1);
        assert!(input.contains("Search files by name"), "{input:?}");
        assert!(input.contains("3 files"), "{input:?}");
        let (f, buf) = loch_finder(&["a.rs"], "zzz");
        let body: String = (f.last_rect.y..f.last_rect.bottom())
            .map(|y| row_text(&buf, f.last_rect, y))
            .collect();
        assert!(body.contains("No matching files"), "{body:?}");
    }

    #[test]
    fn a_name_match_lights_the_name_before_a_path_match() {
        assert_eq!(
            finder_match_offsets("src/main/main.rs", 9, "main"),
            [9, 10, 11, 12]
        );
        assert_eq!(finder_match_offsets("src/app/x.rs", 8, "app"), [4, 5, 6]);
        assert_eq!(finder_match_offsets("src/foo/bar.rs", 8, "fbr"), [4, 8, 10]);
        assert!(finder_match_offsets("a.rs", 0, "").is_empty());
    }

    #[test]
    fn fuzzy_matching_compares_characters_not_bytes() {
        assert_eq!(fuzzy_score("é", "ã©", 0), None);
        assert!(fuzzy_score("é", "café", 0).is_some());
    }

    /// The click hit-test starts three rows below the popup top (border,
    /// prompt, separator), scrolls with the list, refuses the bottom border
    /// (one past `last_inner_height`), and refuses empty rows past the last
    /// result.
    #[test]
    fn row_index_at_maps_the_quick_pick_list_geometry() {
        let entry = |name: &str| FileEntry {
            path: std::path::PathBuf::from(name),
            rel: name.to_string(),
            rel_lower: name.to_lowercase(),
            filename_start: 0,
            filename_start_lower: 0,
        };
        let mut f = FileFinder::new(Arc::new(vec![entry("a.rs"), entry("b.rs"), entry("c.rs")]));
        f.last_rect = Rect {
            x: 10,
            y: 5,
            width: 60,
            height: 10,
        };
        // inner height 8, minus prompt and separator = 6 list rows;
        // the popup bottom border sits at y = 14.
        f.last_inner_height = 6;
        assert_eq!(f.row_index_at(7), None, "the separator is not a row");
        assert_eq!(
            f.row_index_at(8),
            Some(0),
            "the list starts below the separator"
        );
        assert_eq!(f.row_index_at(10), Some(2));
        assert_eq!(f.row_index_at(11), None, "below the last result is empty");
        assert_eq!(f.row_index_at(14), None, "the bottom border is not a row");
        f.scroll = 2;
        assert_eq!(f.row_index_at(8), Some(2), "scroll offsets the mapping");
    }

    #[test]
    fn multi_root_index_survives_root_names_whose_lowercase_changes_byte_length() {
        // `İ` (U+0130) lowercases to `i` + U+0307 — one byte LONGER — so a
        // hand-shifted shared offset lands mid-char in rel_lower and
        // scoring panics on the slice (#148 review). Both offsets are now
        // computed per string.
        let a = tempfile::tempdir().unwrap();
        let root_i = a.path().join("İİ");
        let root_b = a.path().join("beta");
        std::fs::create_dir(&root_i).unwrap();
        std::fs::create_dir(&root_b).unwrap();
        std::fs::write(
            root_i.join("main.rs"),
            "x
",
        )
        .unwrap();
        std::fs::write(
            root_b.join("main.rs"),
            "y
",
        )
        .unwrap();
        let idx = build_multi_file_index(&[root_i, root_b]);
        let turkish = idx
            .iter()
            .find(|e| e.rel.starts_with("İİ/"))
            .expect("the entry keeps its root label");
        assert_eq!(&turkish.rel[turkish.filename_start..], "main.rs");
        assert_eq!(
            &turkish.rel_lower[turkish.filename_start_lower..],
            "main.rs",
            "the scoring string slices at ITS own offset"
        );
        assert_eq!(
            score_entry("main.rs", &turkish.rel_lower, turkish.filename_start_lower)
                .map(|(t, _)| t),
            Some(MatchTier::ExactFilename),
            "filename-tier scoring works through the unicode root label"
        );
    }

    #[test]
    fn multi_root_index_disambiguates_same_named_roots() {
        let outer = tempfile::tempdir().unwrap();
        let work = outer.path().join("work").join("api");
        let archive = outer.path().join("archive").join("api");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(&archive).unwrap();
        std::fs::write(
            work.join("main.rs"),
            "a
",
        )
        .unwrap();
        std::fs::write(
            archive.join("main.rs"),
            "b
",
        )
        .unwrap();
        let idx = build_multi_file_index(&[work, archive]);
        let rels: Vec<&str> = idx.iter().map(|e| e.rel.as_str()).collect();
        assert!(
            rels.contains(&"api (work)/main.rs") && rels.contains(&"api (archive)/main.rs"),
            "same-named roots label with their parents: {rels:?}"
        );
    }

    #[test]
    fn multi_root_index_prefixes_rels_with_the_root_name_and_keeps_filename_scoring() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let a_root = a.path().join("alpha");
        let b_root = b.path().join("beta");
        std::fs::create_dir(&a_root).unwrap();
        std::fs::create_dir(&b_root).unwrap();
        // The SAME relative path in both roots: the multi-root prefix is
        // the only thing keeping the two rows distinguishable.
        std::fs::write(a_root.join("main.rs"), "a\n").unwrap();
        std::fs::write(b_root.join("main.rs"), "b\n").unwrap();

        let idx = build_multi_file_index(&[a_root.clone(), b_root.clone()]);
        let rels: Vec<&str> = idx.iter().map(|e| e.rel.as_str()).collect();
        assert!(rels.contains(&"alpha/main.rs"), "rels: {rels:?}");
        assert!(rels.contains(&"beta/main.rs"), "rels: {rels:?}");
        for e in &idx {
            assert_eq!(
                &e.rel[e.filename_start..],
                "main.rs",
                "filename_start must shift with the prefix so filename-tier scoring holds"
            );
            assert!(e.path.is_absolute() && e.path.ends_with("main.rs"));
        }

        // One root: the plain index, no prefixes.
        let single = build_multi_file_index(&[a_root]);
        assert_eq!(single.len(), 1);
        assert_eq!(single[0].rel, "main.rs");
    }

    /// `n` numbered lines written to `rel` under `root`, and its entry.
    fn file_with_lines(root: &Path, rel: &str, n: usize) -> FileEntry {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let text: String = (1..=n).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&path, text).unwrap();
        FileEntry { path, ..entry(rel) }
    }

    /// Type `q` and wait for the `:line` check it starts, if any.
    fn query(finder: &mut FileFinder, q: &str) {
        finder.set_query(q);
        finder.finish_line_check();
    }

    fn listed(finder: &FileFinder) -> Vec<&str> {
        finder
            .visible_results()
            .iter()
            .map(|r| r.entry.rel.as_str())
            .collect()
    }

    /// #2127 review: every keystroke of a `:line` query checks on the one
    /// worker thread the finder started, not on a thread of its own, so a
    /// stalled read cannot pile threads up; and the thread ends when the
    /// finder goes.
    #[test]
    fn line_checks_share_one_worker_that_ends_with_the_finder() {
        let tmp = tempfile::tempdir().unwrap();
        let entries = vec![
            file_with_lines(tmp.path(), "web/a.ts", 50),
            file_with_lines(tmp.path(), "api/a.ts", 400),
        ];
        let mut finder = FileFinder::new(Arc::new(entries));
        for line in 1..=30 {
            query(&mut finder, &format!("a.ts:{line}"));
        }
        let worker = finder
            .line_worker
            .as_ref()
            .expect("the first check starts it");
        assert_eq!(
            Arc::strong_count(&worker.shared),
            2,
            "the finder and one thread hold the worker, after 30 queries"
        );
        let gone = Arc::downgrade(&worker.shared);
        drop(finder);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while gone.strong_count() > 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "the worker thread must end with the finder"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// #2127 review: an answer that lands after the keystroke can find a row
    /// already picked from the unchecked list. It stays picked when it is
    /// still listed, so Enter opens the file chosen, not the first one.
    #[test]
    fn a_late_line_check_answer_keeps_the_chosen_row() {
        let tmp = tempfile::tempdir().unwrap();
        let entries = vec![
            file_with_lines(tmp.path(), "a/x.ts", 10),
            file_with_lines(tmp.path(), "b/x.ts", 2),
            file_with_lines(tmp.path(), "c/x.ts", 10),
        ];
        let mut finder = FileFinder::new(Arc::new(entries));
        query(&mut finder, "x.ts");
        let short = |r: &ScoredResult| r.entry.rel.starts_with("b/");
        // As a check still running when the list was drawn would leave it.
        let candidates = finder.results.clone();
        finder.line_check_generation += 1;
        let generation = finder.line_check_generation;
        let (_tx, rx) = std::sync::mpsc::channel();
        finder.line_check = Some(LineCheck {
            generation,
            needed: 5,
            candidates: candidates.clone(),
            more_matched: false,
            rx,
        });
        // The last long match is picked; one long match is listed above it.
        finder.selected = candidates.iter().rposition(|r| !short(r)).unwrap();
        let chosen = finder.selected_entry().unwrap().path.clone();

        finder.apply_line_check(LineCheckDone {
            generation,
            lines: candidates
                .iter()
                .map(|r| Some(if short(r) { 2 } else { 10 }))
                .collect(),
            counted: Vec::new(),
        });
        assert_eq!(listed(&finder).len(), 2, "the short file goes");
        assert_eq!(finder.selected_entry().map(|e| &e.path), Some(&chosen));
        assert_eq!(finder.selected_index(), 1);
    }

    /// #2127: `a.ts:318` lists only the `a.ts` long enough to have a line
    /// 318, not every `a.ts` in the workspace; a range needs its last line.
    #[test]
    fn a_line_hint_lists_only_files_that_have_the_line() {
        let tmp = tempfile::tempdir().unwrap();
        let entries = vec![
            file_with_lines(tmp.path(), "web/a.ts", 50),
            file_with_lines(tmp.path(), "api/a.ts", 400),
            file_with_lines(tmp.path(), "cli/a.ts", 90),
        ];
        let mut finder = FileFinder::new(Arc::new(entries));
        query(&mut finder, "a.ts");
        assert_eq!(listed(&finder).len(), 3, "no hint: every a.ts");
        query(&mut finder, "a.ts:318");
        assert_eq!(listed(&finder), ["api/a.ts"]);
        assert_eq!(finder.notice, None);
        query(&mut finder, "a.ts:90");
        assert_eq!(listed(&finder).len(), 2, "the last line counts");
        assert!(!listed(&finder).contains(&"web/a.ts"));
        query(&mut finder, "a.ts:40-120");
        assert_eq!(listed(&finder), ["api/a.ts"], "a range needs its last line");
        query(&mut finder, "a.ts:7:3");
        assert_eq!(listed(&finder).len(), 3, "every one has line 7");
    }

    /// #2127 negative: when no match has the line, every match is listed,
    /// longest first, and the finder says why, since the line is likely
    /// mistyped.
    #[test]
    fn a_line_no_match_has_lists_every_match_longest_first() {
        let tmp = tempfile::tempdir().unwrap();
        let entries = vec![
            file_with_lines(tmp.path(), "web/a.ts", 50),
            file_with_lines(tmp.path(), "api/a.ts", 400),
            file_with_lines(tmp.path(), "cli/a.ts", 90),
        ];
        let mut finder = FileFinder::new(Arc::new(entries));
        query(&mut finder, "a.ts:5000");
        assert_eq!(listed(&finder), ["api/a.ts", "cli/a.ts", "web/a.ts"]);
        assert_eq!(
            finder.notice.as_deref(),
            Some("no match has line 5000 \u{b7} longest first")
        );
        query(&mut finder, "a.ts:318");
        assert_eq!(finder.notice, None, "the notice goes once a match has it");
    }

    /// #2127: a count is kept between keystrokes while the file is
    /// unchanged, and a file that grows is counted again.
    #[test]
    fn a_line_hint_recounts_a_file_that_changed() {
        let tmp = tempfile::tempdir().unwrap();
        let short = file_with_lines(tmp.path(), "a.ts", 10);
        let path = short.path.clone();
        let mut finder = FileFinder::new(Arc::new(vec![short]));
        query(&mut finder, "a.ts:200");
        assert_eq!(
            finder.notice.as_deref().map(|n| n.contains("200")),
            Some(true)
        );
        let text: String = (1..=300).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&path, text).unwrap();
        query(&mut finder, "a.ts:20");
        query(&mut finder, "a.ts:200");
        assert_eq!(listed(&finder), ["a.ts"]);
        assert_eq!(finder.notice, None, "the grown file has line 200");
    }

    /// #2127: what counts as a line, and the cap that stops the read.
    #[test]
    fn count_lines_counts_a_last_line_without_a_newline_and_stops_at_the_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let file = |name: &str, text: &str| {
            let p = tmp.path().join(name);
            std::fs::write(&p, text).unwrap();
            p
        };
        let mut budget = LINE_COUNT_BUDGET;
        assert_eq!(
            count_lines(&file("e", ""), 5, &mut budget),
            Some((1, true)),
            "an empty file has its one line"
        );
        assert_eq!(
            count_lines(&file("n", "a\nb\n"), 5, &mut budget),
            Some((2, true))
        );
        assert_eq!(
            count_lines(&file("t", "a\nb"), 5, &mut budget),
            Some((2, true))
        );
        assert_eq!(
            count_lines(&file("c", "1\n2\n3\n4\n"), 3, &mut budget),
            Some((3, false))
        );
        assert_eq!(count_lines(&tmp.path().join("gone"), 3, &mut budget), None);
        let mut spent = 0;
        assert_eq!(
            count_lines(&file("b", "a\n"), 5, &mut spent),
            None,
            "no budget left"
        );
    }

    /// #2127 review: lines end where the editor ends them. A lone `\r` ends
    /// one, `\r\n` ends one, and the read stops once the line asked for has
    /// begun rather than at its end. A NUL means UTF-16 or a binary, whose
    /// lines these bytes do not tell.
    #[test]
    fn count_lines_ends_lines_where_the_editor_does() {
        let tmp = tempfile::tempdir().unwrap();
        let file = |name: &str, bytes: &[u8]| -> PathBuf {
            let p = tmp.path().join(name);
            std::fs::write(&p, bytes).unwrap();
            p
        };
        let mut budget = LINE_COUNT_BUDGET;
        let mut lines = |p: PathBuf, cap: usize| count_lines(&p, cap, &mut budget);
        assert_eq!(lines(file("cr", b"a\rb\rc\r"), 9), Some((3, true)));
        assert_eq!(lines(file("crlf", b"a\r\nb\r\nc"), 9), Some((3, true)));
        assert_eq!(
            lines(file("mixed", b"a\rb\nc\r\n\r\nd"), 9),
            Some((5, true))
        );
        assert_eq!(lines(file("blank", b"\n\n"), 9), Some((2, true)));
        assert_eq!(lines(file("begun", b"x\ny"), 2), Some((2, false)));
        assert_eq!(lines(file("utf16", b"\xff\xfea\x00\n\x00b\x00"), 9), None);
        let long = file("long", &vec![b'x'; 300 * 1024]);
        let mut small = 64 * 1024;
        assert_eq!(
            count_lines(&long, 1, &mut small),
            Some((1, false)),
            "line 1 has begun: no need to read to its end"
        );
    }

    /// #2127 review: a file open in the editor is counted as it is open,
    /// unsaved lines included, since picking it opens that buffer.
    #[test]
    fn a_line_hint_counts_an_open_buffer_not_the_saved_file() {
        let tmp = tempfile::tempdir().unwrap();
        let entries = vec![
            file_with_lines(tmp.path(), "web/a.ts", 50),
            file_with_lines(tmp.path(), "api/a.ts", 400),
        ];
        let web = entries[0].path.clone();
        let mut finder = FileFinder::new(Arc::new(entries));
        query(&mut finder, "a.ts:318");
        assert_eq!(listed(&finder), ["api/a.ts"]);
        finder.set_open_lines(HashMap::from([(web, 400)]));
        finder.finish_line_check();
        let mut both = listed(&finder);
        both.sort_unstable();
        assert_eq!(both, ["api/a.ts", "web/a.ts"]);
    }

    /// #2127 review: a buffer that grows while Quick Open is up is
    /// rechecked, and the row picked stays picked. The same counts again
    /// rerun nothing, since the app hands them over on every tick.
    #[test]
    fn open_lines_that_change_recheck_and_keep_the_row_picked() {
        let tmp = tempfile::tempdir().unwrap();
        let entries = vec![
            file_with_lines(tmp.path(), "web/a.ts", 50),
            file_with_lines(tmp.path(), "api/a.ts", 400),
            file_with_lines(tmp.path(), "lib/a.ts", 400),
        ];
        let web = entries[0].path.clone();
        let mut finder = FileFinder::new(Arc::new(entries));
        assert!(
            !finder.set_open_lines(HashMap::from([(web.clone(), 50)])),
            "no query, nothing to recheck"
        );
        query(&mut finder, "a.ts:318");
        let mut two = listed(&finder);
        two.sort_unstable();
        assert_eq!(two, ["api/a.ts", "lib/a.ts"]);
        let picked = finder.visible_results()[1].entry.rel.clone();
        finder.select_next();

        assert!(finder.set_open_lines(HashMap::from([(web.clone(), 400)])));
        finder.finish_line_check();
        assert_eq!(listed(&finder).len(), 3, "the grown buffer has the line");
        assert_eq!(
            finder.selected_entry().map(|e| e.rel.as_str()),
            Some(picked.as_str()),
            "the row picked stays picked"
        );

        let checks = finder.line_check_generation;
        assert!(
            !finder.set_open_lines(HashMap::from([(web, 400)])),
            "the same counts rerun nothing"
        );
        assert_eq!(finder.line_check_generation, checks, "no check started");
    }

    /// #2127 review: the check looks past the first page of matches, so a
    /// file that has the line is found when forty shorter ones rank above it.
    #[test]
    fn a_line_hint_finds_a_match_ranked_below_the_first_page() {
        let tmp = tempfile::tempdir().unwrap();
        let mut entries: Vec<FileEntry> = (0..MAX_RESULTS + 5)
            .map(|i| file_with_lines(tmp.path(), &format!("d{i:02}/a.ts"), 10))
            .collect();
        entries.push(file_with_lines(tmp.path(), "a/much/deeper/dir/a.ts", 400));
        let mut finder = FileFinder::new(Arc::new(entries));
        query(&mut finder, "a.ts");
        assert!(!listed(&finder).contains(&"a/much/deeper/dir/a.ts"));
        query(&mut finder, "a.ts:318");
        assert_eq!(listed(&finder), ["a/much/deeper/dir/a.ts"]);
        assert_eq!(finder.notice, None);
    }

    /// #2127 review: a FIFO is never opened, since opening one waits for a
    /// writer; it stays listed as a file the check can't tell.
    #[cfg(unix)]
    #[test]
    fn a_line_hint_never_opens_a_fifo() {
        let tmp = tempfile::tempdir().unwrap();
        let fifo = tmp.path().join("pipe/a.ts");
        std::fs::create_dir_all(fifo.parent().unwrap()).unwrap();
        let made = std::process::Command::new("mkfifo").arg(&fifo).status();
        if !made.is_ok_and(|s| s.success()) {
            return;
        }
        let entries = vec![
            FileEntry {
                path: fifo,
                ..entry("pipe/a.ts")
            },
            file_with_lines(tmp.path(), "web/a.ts", 3),
        ];
        let mut finder = FileFinder::new(Arc::new(entries));
        query(&mut finder, "a.ts:5");
        assert_eq!(listed(&finder), ["pipe/a.ts"]);
    }

    /// #2127 review: the answer to an older query only fills the cache; it
    /// never filters the list a newer query drew.
    #[test]
    fn an_older_line_check_answer_is_not_applied() {
        let tmp = tempfile::tempdir().unwrap();
        let entries = vec![
            file_with_lines(tmp.path(), "web/a.ts", 50),
            file_with_lines(tmp.path(), "api/a.ts", 400),
        ];
        let web = entries[0].path.clone();
        let mut finder = FileFinder::new(Arc::new(entries));
        query(&mut finder, "a.ts");
        let before = listed(&finder).len();
        let count = LineCount {
            lines: 50,
            complete: true,
            len: 0,
            modified: None,
        };
        finder.apply_line_check(LineCheckDone {
            generation: finder.line_check_generation + 7,
            lines: vec![Some(1)],
            counted: vec![(web.clone(), count)],
        });
        assert_eq!(listed(&finder).len(), before);
        assert_eq!(finder.line_counts.get(&web), Some(&count));
        assert!(!finder.poll_line_check(), "nothing is running");
    }

    fn entry(rel: &str) -> FileEntry {
        let rel_lower = rel.to_lowercase();
        let filename_start = rel.rfind('/').map(|i| i + 1).unwrap_or(0);
        let filename_start_lower = rel_lower.rfind('/').map(|i| i + 1).unwrap_or(0);
        FileEntry {
            path: PathBuf::from(rel),
            rel: rel.to_string(),
            rel_lower,
            filename_start,
            filename_start_lower,
        }
    }

    #[test]
    fn fuzzy_score_returns_some_for_subsequence_match() {
        let score = fuzzy_score(
            "ab",
            "alpha/beta.rs",
            "alpha/beta.rs".rfind('/').unwrap() + 1,
        );
        assert!(
            score.is_some(),
            "'ab' must subsequence-match 'alpha/beta.rs'"
        );
    }

    #[test]
    fn fuzzy_score_returns_none_when_chars_are_out_of_order() {
        let score = fuzzy_score("zx", "abc/def.rs", 4);
        assert!(score.is_none());
    }

    #[test]
    fn filename_matches_rank_above_directory_matches() {
        let dir_match = fuzzy_score(
            "be",
            "beta_dir/zoom.rs",
            "beta_dir/zoom.rs".rfind('/').unwrap() + 1,
        );
        let file_match = fuzzy_score("be", "zoom/beta.rs", "zoom/beta.rs".rfind('/').unwrap() + 1);
        assert!(
            file_match > dir_match,
            "matches inside the filename must score higher than matches in directory segments (file={file_match:?}, dir={dir_match:?})"
        );
    }

    #[test]
    fn consecutive_chars_score_higher_than_split_chars() {
        let consec = fuzzy_score("alp", "alpha.rs", 0).unwrap();
        let split = fuzzy_score("alp", "a_l_p_xx.rs", 0).unwrap();
        assert!(
            consec > split,
            "'alp' against 'alpha.rs' must score higher than against 'a_l_p_xx.rs'"
        );
    }

    #[test]
    fn file_finder_with_empty_query_lists_files_alphabetically() {
        let entries = Arc::new(vec![
            entry("zeta.rs"),
            entry("alpha.rs"),
            entry("mid/beta.rs"),
        ]);
        let finder = FileFinder::new(entries);
        let names: Vec<&str> = finder
            .visible_results()
            .iter()
            .map(|r| r.entry.rel.as_str())
            .collect();
        assert_eq!(names, vec!["alpha.rs", "mid/beta.rs", "zeta.rs"]);
    }

    #[test]
    fn cursor_supports_mid_string_insert_and_delete() {
        let finder_entries = Arc::new(vec![entry("a.rs")]);
        let mut f = FileFinder::new(finder_entries);
        for c in "abc".chars() {
            f.push_char(c);
        }
        assert_eq!((f.query.as_str(), f.cursor), ("abc", 3));
        f.move_cursor_left();
        f.move_cursor_left();
        assert_eq!(f.cursor, 1);
        f.push_char('X');
        assert_eq!((f.query.as_str(), f.cursor), ("aXbc", 2));
        f.pop_char();
        assert_eq!((f.query.as_str(), f.cursor), ("abc", 1));
        f.delete_char();
        assert_eq!((f.query.as_str(), f.cursor), ("ac", 1));
        f.move_cursor_home();
        assert_eq!(f.cursor, 0);
        f.move_cursor_end();
        assert_eq!(f.cursor, 2);
        f.move_cursor_right();
        assert_eq!(
            f.cursor, 2,
            "cursor must not move past the end of the query"
        );
    }

    #[test]
    fn query_with_path_separator_matches_the_relative_path() {
        let entries = Arc::new(vec![
            entry("md2pdf/core.py"),
            entry("other/core.py"),
            entry("core.py"),
        ]);
        let mut finder = FileFinder::new(entries);
        finder.set_query("md2pdf/core.py");
        let names: Vec<&str> = finder
            .visible_results()
            .iter()
            .map(|r| r.entry.rel.as_str())
            .collect();
        assert_eq!(
            names.first().copied(),
            Some("md2pdf/core.py"),
            "a query containing '/' must match against the relative path; got {names:?}"
        );
    }

    #[test]
    fn a_line_suffix_is_split_off_the_query_and_the_file_part_still_matches() {
        // #472: `fileA:236-239` names a file AND where to land in it. The
        // suffix is not part of the file name, so it must not take part in
        // matching, and it has to come back parsed for the open to use.
        let entries = Arc::new(vec![entry("src/alpha.rs"), entry("src/beta.rs")]);
        let mut finder = FileFinder::new(entries);
        for (query, hint) in [
            (
                "alpha:236-239",
                LineHint {
                    line: 236,
                    end: Some(239),
                    col: None,
                },
            ),
            (
                "alpha:236",
                LineHint {
                    line: 236,
                    end: None,
                    col: None,
                },
            ),
            (
                "alpha:236:7",
                LineHint {
                    line: 236,
                    end: None,
                    col: Some(7),
                },
            ),
        ] {
            finder.set_query(query);
            let names: Vec<&str> = finder
                .visible_results()
                .iter()
                .map(|r| r.entry.rel.as_str())
                .collect();
            assert_eq!(
                names,
                vec!["src/alpha.rs"],
                "{query}: the file part alone selects the file; got {names:?}"
            );
            assert_eq!(finder.line_hint(), Some(hint), "{query}");
        }
        // A reversed range is the same range; a zero is no line at all.
        finder.set_query("alpha:239-236");
        assert_eq!(
            finder.line_hint(),
            Some(LineHint {
                line: 236,
                end: Some(239),
                col: None,
            }),
            "a reversed range normalises"
        );
        finder.set_query("alpha:0");
        assert_eq!(finder.line_hint(), None, "line 0 is not a line");
        finder.set_query("alpha");
        assert_eq!(finder.line_hint(), None, "no suffix, no hint");
        // A colon with nothing numeric after it is not a line suffix.
        finder.set_query("alpha:");
        assert_eq!(finder.line_hint(), None, "a bare colon is not a hint");
        // The keystrokes on the way to a complete hint keep the file matched.
        for partial in ["alpha:", "alpha:236-", "alpha:236:"] {
            finder.set_query(partial);
            let names: Vec<&str> = finder
                .visible_results()
                .iter()
                .map(|r| r.entry.rel.as_str())
                .collect();
            assert_eq!(
                names,
                vec!["src/alpha.rs"],
                "{partial}: still matching on the file part"
            );
            assert_eq!(
                finder.line_hint(),
                None,
                "{partial}: no hint until it parses"
            );
        }
        // A hint with no file part names a line in nothing, whether or not
        // it has finished parsing.
        for q in [":236", ":", "::", ":236-", ":0"] {
            finder.set_query(q);
            assert!(
                finder.visible_results().is_empty(),
                "{q:?} alone matches nothing"
            );
        }
        // A zero column keeps the line: the hint is `236`, not nothing.
        finder.set_query("alpha:236:0");
        assert_eq!(
            finder.line_hint().map(|h| (h.line, h.col)),
            Some((236, None)),
            "`:236:0` is line 236 with no column"
        );
        // A number past usize saturates instead of vanishing.
        finder.set_query("alpha:99999999999999999999");
        assert_eq!(finder.line_hint().map(|h| h.line), Some(usize::MAX));
    }

    #[test]
    fn a_filename_that_ends_in_a_colon_still_wins_an_exact_match() {
        // A trailing colon is also how a hint starts, so `alpha:` keeps the
        // list matching on `alpha` while the user types; but on Linux a file
        // can be named `alpha:` and that exact name must still rank first,
        // even beside a file named `alpha`, which the file part alone would
        // rank as the exact match instead.
        let entries = Arc::new(vec![entry("src/alpha"), entry("src/alpha:")]);
        let mut finder = FileFinder::new(entries);
        finder.set_query("alpha:");
        let names: Vec<&str> = finder
            .visible_results()
            .iter()
            .map(|r| r.entry.rel.as_str())
            .collect();
        assert_eq!(
            names.first().copied(),
            Some("src/alpha:"),
            "the file whose name IS the query ranks first; got {names:?}"
        );
        assert_eq!(finder.line_hint(), None);
    }

    #[test]
    fn typing_filters_to_subsequence_matches_only() {
        let entries = Arc::new(vec![
            entry("alpha.rs"),
            entry("sub/beta.rs"),
            entry("gamma.rs"),
        ]);
        let mut finder = FileFinder::new(entries);
        finder.set_query("be");
        let names: Vec<&str> = finder
            .visible_results()
            .iter()
            .map(|r| r.entry.rel.as_str())
            .collect();
        assert_eq!(
            names,
            vec!["sub/beta.rs"],
            "filter 'be' must keep only beta.rs"
        );
    }

    #[test]
    fn selection_clamps_when_results_shrink_under_typing() {
        let entries = Arc::new(vec![entry("alpha.rs"), entry("beta.rs")]);
        let mut finder = FileFinder::new(entries);
        finder.select_next();
        assert_eq!(finder.selected_index(), 1);
        finder.set_query("alp");
        assert_eq!(
            finder.selected_index(),
            0,
            "selection must reset to 0 when the result list changes shape"
        );
    }

    #[test]
    fn select_next_caps_at_last_result() {
        let entries = Arc::new(vec![entry("a.rs"), entry("b.rs")]);
        let mut finder = FileFinder::new(entries);
        finder.select_next();
        finder.select_next();
        finder.select_next();
        assert_eq!(
            finder.selected_index(),
            1,
            "select_next must not overshoot the last index"
        );
    }

    #[test]
    fn select_prev_caps_at_zero() {
        let entries = Arc::new(vec![entry("a.rs"), entry("b.rs")]);
        let mut finder = FileFinder::new(entries);
        finder.select_prev();
        finder.select_prev();
        assert_eq!(finder.selected_index(), 0);
    }

    #[test]
    fn build_file_index_walks_workspace_and_honours_gitignore() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("alpha.rs"), "").unwrap();
        std::fs::create_dir(tmp.path().join("sub")).unwrap();
        std::fs::write(tmp.path().join("sub").join("beta.rs"), "").unwrap();
        std::fs::write(tmp.path().join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(tmp.path().join("ignored.txt"), "").unwrap();
        let entries = build_file_index(tmp.path());
        let names: Vec<&str> = entries.iter().map(|e| e.rel.as_str()).collect();
        assert!(names.contains(&"alpha.rs"));
        assert!(names.contains(&"sub/beta.rs"));
        assert!(
            !names.contains(&"ignored.txt"),
            "gitignored files must be excluded from the Cmd+P index"
        );
    }

    #[test]
    fn build_file_index_includes_user_owned_dot_directories() {
        // Regression for the user's "Cmd+P does not find ~/.croft/lsp.log
        // even when I type lsp.log" report. The old build_file_index
        // called .hidden(true) which silently excluded every dot-dir,
        // including the user's own ~/.croft, ~/.cursor, ~/.continue,
        // ~/.config, etc. - exactly the directories whose contents the
        // Cmd+P finder is supposed to help locate.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join(".croft")).unwrap();
        std::fs::write(tmp.path().join(".croft").join("lsp.log"), b"").unwrap();
        std::fs::create_dir(tmp.path().join(".config")).unwrap();
        std::fs::write(tmp.path().join(".config").join("init.toml"), b"").unwrap();
        let entries = build_file_index(tmp.path());
        let names: Vec<&str> = entries.iter().map(|e| e.rel.as_str()).collect();
        assert!(
            names.contains(&".croft/lsp.log"),
            "Cmd+P index must include files under user dot-dirs like .croft/; got {names:?}"
        );
        assert!(
            names.contains(&".config/init.toml"),
            "Cmd+P index must include files under .config/ and similar user dot-dirs; got {names:?}"
        );
    }

    #[test]
    fn build_file_index_skips_macos_app_data_containers() {
        // Regression for the user's "iTerm.app would like to access
        // data from other apps" TCC prompt that fired repeatedly
        // after the 0.1.148 background-index landed. macOS gates
        // ~/Library/Containers/<bundle-id> and ~/Library/Group
        // Containers/<group-id> behind kTCCServiceSystemPolicyAppData;
        // any process that readdir's into those paths trips a
        // consent prompt on the parent terminal app. None of those
        // files are useful Cmd+P targets, so prune the two directory
        // trees outright. Test simulates a workspace rooted at a fake
        // $HOME so the path-shape match fires deterministically.
        let tmp = tempfile::tempdir().unwrap();
        let fake_home = tmp.path();
        let containers = fake_home
            .join("Library")
            .join("Containers")
            .join("com.example.app")
            .join("Data");
        std::fs::create_dir_all(&containers).unwrap();
        std::fs::write(containers.join("private.sqlite"), b"").unwrap();
        let group = fake_home
            .join("Library")
            .join("Group Containers")
            .join("group.example")
            .join("shared");
        std::fs::create_dir_all(&group).unwrap();
        std::fs::write(group.join("shared.db"), b"").unwrap();
        // A normal Library subdir the user DOES want indexed - Logs
        // is the canonical lsp.log location regression target.
        let logs = fake_home.join("Library").join("Logs");
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(logs.join("app.log"), b"").unwrap();
        // The blocked set is injected instead of repointing `$HOME` (#37):
        // concurrent tests resolve the env var at call time, and a
        // substituted value made unrelated readers (the PDF rasteriser's
        // cache dir) fail on paths inside the fake home.
        let entries = build_file_index_with_blocked(fake_home, macos_blocked_paths_for(fake_home));
        let names: Vec<&str> = entries.iter().map(|e| e.rel.as_str()).collect();
        assert!(
            names.contains(&"Library/Logs/app.log"),
            "Library/Logs/app.log must still be indexed; got {names:?}"
        );
        assert!(
            !names.iter().any(|n| n.starts_with("Library/Containers/")),
            "Library/Containers/** must be excluded to avoid macOS AppData TCC prompts; got {names:?}"
        );
        assert!(
            !names
                .iter()
                .any(|n| n.starts_with("Library/Group Containers/")),
            "Library/Group Containers/** must be excluded for the same TCC reason; got {names:?}"
        );
    }

    #[test]
    fn build_file_index_skips_vendored_noise_directories() {
        // Including hidden dirs would otherwise drown the index in
        // .git/objects/* and node_modules/* — paths the user never
        // wants to open via Cmd+P. Make sure the noise-dir filter
        // keeps those out even though .hidden(false) is now in
        // effect.
        let tmp = tempfile::tempdir().unwrap();
        for noise in [
            ".git",
            "node_modules",
            "target",
            "__pycache__",
            ".venv",
            ".tox",
            ".mypy_cache",
        ] {
            let dir = tmp.path().join(noise);
            std::fs::create_dir(&dir).unwrap();
            std::fs::write(dir.join("blob"), b"").unwrap();
        }
        std::fs::write(tmp.path().join("real.txt"), b"").unwrap();
        let entries = build_file_index(tmp.path());
        let names: Vec<&str> = entries.iter().map(|e| e.rel.as_str()).collect();
        assert!(
            names.contains(&"real.txt"),
            "the regular workspace file must still be indexed; got {names:?}"
        );
        for noise in [
            ".git/blob",
            "node_modules/blob",
            "target/blob",
            "__pycache__/blob",
            ".venv/blob",
            ".tox/blob",
            ".mypy_cache/blob",
        ] {
            assert!(
                !names.contains(&noise),
                "{noise} must be filtered out so vendored / generated dirs do not drown the Cmd+P index; got {names:?}"
            );
        }
    }

    #[test]
    fn exact_filename_match_beats_every_substring_or_subsequence_match() {
        let entries = Arc::new(vec![
            entry("packages/anterior-dev-py/tests/test_citations_storage.py"),
            entry("app/oncohealth/tasks/main/tests/test_configure_agent_v1.py"),
            entry("packages/anterior-dev-py/src/anterior/dev/citations/storage.py"),
        ]);
        let mut finder = FileFinder::new(entries);
        finder.set_query("storage.py");
        let rels: Vec<&str> = finder
            .visible_results()
            .iter()
            .map(|r| r.entry.rel.as_str())
            .collect();
        assert_eq!(
            rels.first().copied(),
            Some("packages/anterior-dev-py/src/anterior/dev/citations/storage.py"),
            "exact filename match 'storage.py' MUST come before test_citations_storage.py (filename-substring) and test_configure_agent_v1.py (only a scattered subsequence) — got {rels:?}. Subsequence tier swamping exact match is the bug the user yelled about at 10:54 on 2026-05-13."
        );
    }

    #[test]
    fn filename_prefix_beats_filename_substring_which_beats_path_substring_which_beats_subsequence()
    {
        let entries = Arc::new(vec![
            entry("zzz_other/random_storage_thing.py"),
            entry("dir_with_storage_in_name/other.py"),
            entry("a/test_storage.py"),
            entry("b/storage_helper.py"),
        ]);
        let mut finder = FileFinder::new(entries);
        finder.set_query("storage");
        let rels: Vec<&str> = finder
            .visible_results()
            .iter()
            .map(|r| r.entry.rel.as_str())
            .collect();
        assert_eq!(
            rels,
            vec![
                "b/storage_helper.py",
                "a/test_storage.py",
                "zzz_other/random_storage_thing.py",
                "dir_with_storage_in_name/other.py",
            ],
            "tier order must be: filename starts-with > filename contains > path contains > scattered subsequence — got {rels:?}"
        );
    }

    #[test]
    fn within_the_same_tier_the_shorter_relative_path_wins() {
        let entries = Arc::new(vec![
            entry("a/very/deeply/nested/dir/structure/storage.py"),
            entry("storage.py"),
            entry("b/c/storage.py"),
        ]);
        let mut finder = FileFinder::new(entries);
        finder.set_query("storage.py");
        let rels: Vec<&str> = finder
            .visible_results()
            .iter()
            .map(|r| r.entry.rel.as_str())
            .collect();
        assert_eq!(
            rels,
            vec![
                "storage.py",
                "b/c/storage.py",
                "a/very/deeply/nested/dir/structure/storage.py",
            ],
            "all three are exact-filename matches; tie-break must be ascending rel.len() so the workspace-root storage.py wins — got {rels:?}"
        );
    }

    /// Manual diagnostic: walks the real `$HOME`, then types
    /// "lsp.log" one char at a time, printing per-keystroke latency.
    /// Marked #[ignore] so it never runs in CI/regular `cargo test`.
    /// Invoke as:
    ///   cargo test --release --bin croft -- --ignored --nocapture bench_cmd_p_real_home
    #[test]
    #[ignore]
    fn bench_cmd_p_real_home() {
        let home = std::env::var("HOME").expect("$HOME");
        let root = std::path::PathBuf::from(home);
        let t0 = std::time::Instant::now();
        let entries = build_file_index(&root);
        let walk_ms = t0.elapsed().as_secs_f64() * 1000.0;
        eprintln!("walk: indexed {} files in {:.0} ms", entries.len(), walk_ms);
        let arc = std::sync::Arc::new(entries);
        let mut finder = FileFinder::new(arc.clone());
        for c in "lsp.log".chars() {
            let t = std::time::Instant::now();
            finder.push_char(c);
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            eprintln!(
                "type '{}' -> query={:?} results={} latency={:.2} ms",
                c,
                finder.query,
                finder.visible_results().len(),
                ms
            );
        }
    }

    #[test]
    fn replace_entries_re_ranks_results_against_the_new_index() {
        let entries = Arc::new(vec![entry("alpha.rs"), entry("beta.rs")]);
        let mut finder = FileFinder::new(entries);
        finder.set_query("resolver.ts");
        assert!(
            finder.visible_results().is_empty(),
            "no resolver.ts in initial index"
        );
        let new_entries = Arc::new(vec![
            entry("alpha.rs"),
            entry("beta.rs"),
            entry("packages/ant-ts-lib-noggin/src/entities/files/resolver.ts"),
        ]);
        finder.replace_entries(new_entries);
        let rels: Vec<&str> = finder
            .visible_results()
            .iter()
            .map(|r| r.entry.rel.as_str())
            .collect();
        assert_eq!(
            rels.first().copied(),
            Some("packages/ant-ts-lib-noggin/src/entities/files/resolver.ts"),
            "after replace_entries the new index must drive the result list; got {rels:?}"
        );
    }

    #[test]
    fn replace_entries_clamps_selection_when_results_shrink() {
        let entries = Arc::new(vec![entry("alpha.rs"), entry("beta.rs"), entry("gamma.rs")]);
        let mut finder = FileFinder::new(entries);
        finder.select_next();
        finder.select_next();
        assert_eq!(finder.selected_index(), 2);
        finder.replace_entries(Arc::new(vec![entry("alpha.rs")]));
        assert_eq!(
            finder.selected_index(),
            0,
            "selection must clamp to last visible row when the index shrinks"
        );
    }

    #[test]
    fn fuzzy_score_handles_mixed_case_via_lowercased_haystack() {
        let rel = "Sub/BetaFile.rs";
        let rel_lower = rel.to_lowercase();
        let filename_start = rel.rfind('/').map(|i| i + 1).unwrap_or(0);
        let score = fuzzy_score("beta", &rel_lower, filename_start);
        assert!(score.is_some());
    }

    #[test]
    fn is_path_under_noise_dir_flags_cargo_and_npm_output_paths() {
        // Root-cause guard: a sample run on a live croft pegged ~150%
        // CPU showed 50+ workers in `ignore::WalkBuilder::build_parallel`
        // because every cargo write into `target/` triggered a Cmd+P
        // index rebuild — even though the rebuild itself skips target/.
        // The gate lives in this helper; if it ever stops recognising
        // cargo/npm/git output paths, the storm comes back.
        assert!(is_path_under_noise_dir(Path::new(
            "/Users/v/proj/target/debug/build/croft-abcd"
        )));
        assert!(is_path_under_noise_dir(Path::new(
            "/Users/v/proj/node_modules/foo/bar.js"
        )));
        assert!(is_path_under_noise_dir(Path::new(
            "/Users/v/proj/.git/objects/ff/abcd"
        )));
    }

    #[test]
    fn is_path_under_noise_dir_flags_home_tool_caches() {
        // Root-cause guard: a Linux remote opened at /root pegged ~200% CPU
        // because .cargo/.npm/.nvm/.bun/.claude under $HOME were watched and
        // re-walked on every cache write. If these stop being recognised,
        // the home-dir FS-event storm comes back.
        for p in [
            "/root/.cargo/registry/src/index/foo-1.0/src/lib.rs",
            "/root/.claude/projects/x/session.jsonl",
            "/root/.npm/_cacache/index-v5/aa/bb",
            "/root/.nvm/versions/node/v20/lib/x.js",
            "/root/.bun/install/cache/pkg/index.js",
            "/root/.local/share/pipx/venvs/tool/bin/x",
        ] {
            assert!(is_path_under_noise_dir(Path::new(p)), "{p}");
        }
    }

    /// The root-relative reading, which is the one every EVENT path wants:
    /// the workspace root's own ancestors are not part of the workspace.
    #[test]
    fn noise_is_measured_from_the_workspace_root_not_the_filesystem_root() {
        let root = Path::new("/home/dev/build/myapp");
        assert!(
            !is_noise_path_under_root(&root.join("src/a.rs"), root),
            "a source file in a workspace under build/ is not noise"
        );
        assert!(
            !is_noise_path_under_root(root, root),
            "nor is the root itself"
        );
        // Noise at or below the root still counts, by name and by run.
        for rel in ["target/debug/x", "node_modules/p/i.js", "go/pkg/mod/x"] {
            assert!(
                is_noise_path_under_root(&root.join(rel), root),
                "{rel} is noise below the root"
            );
        }
        // A path under NEITHER root is measured whole: there is no root for
        // its components to be relative to.
        assert!(is_noise_path_under_root(
            Path::new("/elsewhere/target/x"),
            root
        ));
        // The contrast that was the bug: read absolutely, the workspace's
        // own source file is noise because an ANCESTOR is named `build`.
        assert!(is_path_under_noise_dir(&root.join("src/a.rs")));
    }

    #[test]
    fn is_path_under_noise_dir_does_not_flag_workspace_source_paths() {
        assert!(!is_path_under_noise_dir(Path::new(
            "/Users/v/proj/src/app.rs"
        )));
        assert!(!is_path_under_noise_dir(Path::new(
            "/Users/v/proj/README.md"
        )));
        // A file literally named "target.md" must NOT be classified as
        // noise — the gate matches whole path components, not substrings.
        assert!(!is_path_under_noise_dir(Path::new(
            "/Users/v/proj/docs/target.md"
        )));
    }

    #[test]
    fn noise_rules_flag_dotnet_dart_and_go_module_caches() {
        // Root-cause guard for the second home-dir readlink storm: these
        // trees were absent from the ignore set, so opening /root walked
        // and watched all of them. Measured there: .nuget 1127 dirs,
        // .pub-cache 1271, go/pkg/mod 1357.
        // Name-matched, from a $HOME workspace root.
        for p in [
            "/root/.nuget/packages/microsoft.codeanalysis.common/5.0.0/lib/x.dll",
            "/root/.pub-cache/hosted/pub.dev/args-2.4.2/lib/args.dart",
            "/root/.dart-tool/pub/bin/x",
            "/root/.dotnet/sdk/8.0.100/x.dll",
            "/root/.oh-my-zsh/plugins/git/git.plugin.zsh",
        ] {
            assert!(is_path_under_noise_dir(Path::new(p)), "{p}");
        }
        // Run-matched, measured relative to the workspace root.
        for p in [
            "/root/go/pkg/mod/golang.org/x/tools@v0.28.0/go/analysis/x.go",
            "/root/go/pkg/sumdb/sum.golang.org/lookup/x",
        ] {
            assert!(has_noise_path_run(Path::new(p), Path::new("/root")), "{p}");
        }
    }

    #[test]
    fn build_file_index_indexes_a_workspace_opened_inside_the_module_cache() {
        // The root's own path is not part of the workspace: opening a
        // checked-out dependency to read it must still index its files.
        // Matching the absolute path pruned every entry at depth >= 1.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("go/pkg/mod/github.com/x/y@v1.0.0");
        std::fs::create_dir_all(root.join("internal")).unwrap();
        std::fs::write(root.join("main.go"), "x").unwrap();
        std::fs::write(root.join("internal/a.go"), "x").unwrap();
        let idx = build_file_index(&root);
        let mut rels: Vec<&str> = idx.iter().map(|e| e.rel.as_str()).collect();
        rels.sort_unstable();
        assert_eq!(rels, vec!["internal/a.go", "main.go"]);
    }

    #[test]
    fn build_file_index_indexes_a_workspace_under_a_noise_named_ancestor() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("build/myapp");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), "x").unwrap();
        let idx = build_file_index(&root);
        let rels: Vec<&str> = idx.iter().map(|e| e.rel.as_str()).collect();
        assert_eq!(rels, vec!["src/main.rs"]);
    }

    #[test]
    fn noise_rules_keep_a_real_go_source_tree() {
        // `go` alone is a legitimate source directory name, so only the
        // GOPATH cache runs are pruned. A project's own go/ must survive,
        // and so must a go/pkg that is not the module cache.
        let root = Path::new("/Users/v/proj");
        for p in [
            "/Users/v/proj/go/main.go",
            "/Users/v/proj/go/pkg/handler/handler.go",
            "/Users/v/proj/src/mod/thing.rs",
            "/Users/v/proj/pkg/mod/thing.go",
        ] {
            assert!(!has_noise_path_run(Path::new(p), root), "{p}");
            assert!(!is_path_under_noise_dir(Path::new(p)), "{p}");
        }
        // And the root's OWN path never makes its contents noise.
        assert!(!has_noise_path_run(
            Path::new("/home/u/go/pkg/mod/x@v1/main.go"),
            Path::new("/home/u/go/pkg/mod/x@v1")
        ));
    }

    /// A cache run can STRADDLE the root, and both halves of the rule have
    /// to hold at once.
    #[test]
    fn a_gopath_root_still_prunes_the_module_cache_below_it() {
        // A workspace opened at `~/go` - the ordinary GOPATH - keeps its
        // cache at `pkg/mod`, a run whose first component IS the root's own
        // last one. Stripping the root and matching only what was left
        // missed it, re-admitting the tree this rule exists to prune.
        let gopath = Path::new("/home/u/go");
        for p in [
            "/home/u/go/pkg/mod/x@v1/a.go",
            "/home/u/go/pkg/sumdb/lookup/y",
        ] {
            assert!(has_noise_path_run(Path::new(p), gopath), "{p}");
        }
        // The sibling source tree under that same root is not noise.
        assert!(!has_noise_path_run(
            Path::new("/home/u/go/src/app/main.go"),
            gopath
        ));
        // And the other half, unchanged: opening a dependency INSIDE the
        // cache indexes it, because there the whole run is the root's own
        // path rather than anything below it.
        assert!(!has_noise_path_run(
            Path::new("/home/u/go/pkg/mod/x@v1/a.go"),
            Path::new("/home/u/go/pkg/mod/x@v1")
        ));
        // The boundary that decides `>` from `>=`: the run ends exactly at
        // the root's last component, so the cache IS the root and its
        // contents are what the user asked to open.
        assert!(!has_noise_path_run(
            Path::new("/home/u/go/pkg/mod/x/a.go"),
            Path::new("/home/u/go/pkg/mod")
        ));
        // One component shallower, the run's tail lands below the root and
        // the same path is noise again.
        assert!(has_noise_path_run(
            Path::new("/home/u/go/pkg/mod/x/a.go"),
            Path::new("/home/u/go/pkg")
        ));
    }

    #[test]
    fn has_component_run_matches_only_consecutive_whole_components() {
        assert!(has_component_run(
            Path::new("/a/go/pkg/mod/b"),
            &["go", "pkg", "mod"],
            0
        ));
        // Non-consecutive: go/pkg/OTHER/mod must not match.
        assert!(!has_component_run(
            Path::new("/a/go/pkg/other/mod"),
            &["go", "pkg", "mod"],
            0
        ));
        // Substring of a component must not match.
        assert!(!has_component_run(
            Path::new("/a/cargo/pkg/mod"),
            &["go", "pkg", "mod"],
            0
        ));
        // The depth bound: the same run is ignored when it ends at or above
        // `min_end`, which is how a run inside the ROOT'S own path is kept
        // from making the workspace's contents noise.
        assert!(!has_component_run(
            Path::new("/a/go/pkg/mod/b"),
            &["go", "pkg", "mod"],
            4
        ));
        assert!(has_component_run(
            Path::new("/a/go/pkg/mod/b"),
            &["go", "pkg", "mod"],
            3
        ));
    }

    #[test]
    fn build_file_index_skips_the_go_module_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("go/pkg/mod/golang.org/x")).unwrap();
        std::fs::write(root.join("go/pkg/mod/golang.org/x/cached.go"), "x").unwrap();
        std::fs::create_dir_all(root.join("go/cmd")).unwrap();
        std::fs::write(root.join("go/cmd/main.go"), "x").unwrap();
        let idx = build_file_index(root);
        let rels: Vec<&str> = idx.iter().map(|e| e.rel.as_str()).collect();
        assert!(rels.contains(&"go/cmd/main.go"), "{rels:?}");
        assert!(
            !rels.iter().any(|r| r.contains("pkg/mod")),
            "module cache leaked into the index: {rels:?}"
        );
    }

    /// The degenerate root: the walk STARTS at the GOPATH, so the cache run
    /// straddles the root instead of lying below it.
    ///
    /// `build_file_index_skips_the_go_module_cache` cannot see this: it roots
    /// at `tmp` with the cache at `tmp/go/pkg/mod`, where the whole run is
    /// below the root, so it passed both before and after the straddle fix.
    #[test]
    fn build_file_index_skips_the_module_cache_of_a_gopath_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("go");
        std::fs::create_dir_all(root.join("pkg/mod/golang.org/x")).unwrap();
        std::fs::write(root.join("pkg/mod/golang.org/x/cached.go"), "x").unwrap();
        std::fs::create_dir_all(root.join("cmd")).unwrap();
        std::fs::write(root.join("cmd/main.go"), "x").unwrap();
        let idx = build_file_index(&root);
        let rels: Vec<&str> = idx.iter().map(|e| e.rel.as_str()).collect();
        assert!(rels.contains(&"cmd/main.go"), "{rels:?}");
        assert!(
            !rels.iter().any(|r| r.contains("pkg/mod")),
            "a GOPATH root's own module cache leaked into the index: {rels:?}"
        );
    }
}
