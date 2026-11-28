use crate::file_index::{FileEntry, build_file_index};
use crate::widgets::picker_colours;
use crate::widgets::picker_query;
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
    /// The files the last `:line` query ranked for its check, so an open
    /// buffer's change reruns the check only when it is one of them.
    line_candidates: Vec<PathBuf>,
    /// Bumped by every check started, so an older query's answer is never
    /// applied to a newer one.
    line_check_generation: u64,
    /// The thread the checks run on, started by the first one.
    line_worker: Option<LineWorker>,
}

/// A `:line` check running on its own thread (#2127): the matches it is
/// checking, best first. Its answer comes back on the worker's channel.
struct LineCheck {
    generation: u64,
    needed: usize,
    candidates: Vec<ScoredResult>,
    /// More files matched than the ranking kept.
    more_matched: bool,
}

/// What a `:line` check found.
struct LineCheckDone {
    generation: u64,
    /// The lines of each candidate checked, in rank order, counted no
    /// further than the line asked for; `None` for one it could not tell.
    /// Shorter than the candidates when the check stopped early: a page of
    /// them reached the line, or a newer query replaced it.
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
/// query to check when it is free, not a thread each. A check a newer query
/// replaced stops at the next file, as does one whose finder has gone, so
/// the thread ends once the file it is reading is done. Every check answers
/// on one channel, kept for the finder's life, so even a check cut short
/// fills the cache, and the next one starts from what it read.
struct LineWorker {
    shared: Arc<(Mutex<WorkerSlot>, std::sync::Condvar)>,
    answers: std::sync::mpsc::Receiver<LineCheckDone>,
}

impl LineWorker {
    /// Start the thread, or `None` when the system will not give one: the
    /// matches are then listed unchecked rather than the app stopping.
    fn start() -> Option<Self> {
        let shared = Arc::new((Mutex::new(WorkerSlot::default()), std::sync::Condvar::new()));
        let theirs = Arc::clone(&shared);
        let (tx, answers) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name(String::from("quick-open-lines"))
            .spawn(move || run_line_worker(&theirs, &tx))
            .ok()?;
        Some(Self { shared, answers })
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
/// finder goes. A check stops early once a newer query waits or the finder
/// has gone, and answers with what it counted so far.
fn run_line_worker(
    shared: &(Mutex<WorkerSlot>, std::sync::Condvar),
    answers: &std::sync::mpsc::Sender<LineCheckDone>,
) {
    let (slot, wake) = shared;
    let replaced = || {
        let slot = slot.lock().unwrap_or_else(|e| e.into_inner());
        slot.closed || slot.next.is_some()
    };
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
        let (lines, counted) = check_lines(batch.jobs, batch.needed, &replaced);
        let sent = answers.send(LineCheckDone {
            generation: batch.generation,
            lines,
            counted,
        });
        if sent.is_err() {
            return;
        }
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
            line_candidates: Vec::new(),
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
    /// Only a change to a file the `:line` query ranked reruns the check:
    /// an edit to any other (an agent writing elsewhere) changes nothing it
    /// lists, so a stream of them cannot keep restarting it. The row picked
    /// stays picked if it is still listed. True when the list changed.
    pub fn set_open_lines(&mut self, open: HashMap<PathBuf, usize>) -> bool {
        if open == self.open_lines {
            return false;
        }
        let before = std::mem::replace(&mut self.open_lines, open);
        self.line_counts
            .retain(|path, _| !self.open_lines.contains_key(path));
        let changed = |path: &PathBuf| before.get(path) != self.open_lines.get(path);
        if self.line_hint().is_none() || !self.line_candidates.iter().any(changed) {
            return false;
        }
        self.rerun_keeping_the_pick();
        true
    }

    /// Rerun the query, unchanged, against what changed behind it: the file
    /// index or an open buffer's lines (#2127). The row picked stays picked
    /// while its file is listed. A `:line` check is not waited for. While it
    /// runs, the list on screen stays, rather than the unchecked best
    /// matches, which need not hold the row picked: Enter meanwhile opens
    /// that row, and the answer keeps it picked if it is still listed. An
    /// empty list, as before the first index lands, has no row to keep, so
    /// the unchecked matches are drawn at once.
    fn rerun_keeping_the_pick(&mut self) {
        let chosen = self.selected_entry().map(|e| e.path.clone());
        let shown = (
            std::mem::take(&mut self.results),
            self.selected,
            self.scroll,
            self.notice.take(),
        );
        self.refresh_results_waiting(std::time::Duration::ZERO);
        if self.line_check.is_some() && chosen.is_some() {
            (self.results, self.selected, self.scroll, self.notice) = shown;
            return;
        }
        // A row whose file is no longer listed keeps its place, inside the
        // list.
        self.selected = chosen
            .and_then(|p| self.results.iter().position(|r| r.entry.path == p))
            .unwrap_or_else(|| shown.1.min(self.results.len().saturating_sub(1)));
        self.scroll = self.scroll.min(self.selected);
    }

    /// Apply the `:line` checks' answers that have come. True when the list
    /// changed, so the frame is redrawn.
    pub fn poll_line_check(&mut self) -> bool {
        let mut changed = false;
        while let Some(worker) = self.line_worker.as_ref() {
            match worker.answers.try_recv() {
                Ok(done) => changed |= self.apply_line_check(done),
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                // The thread is gone: the matches stay listed unchecked,
                // and the next `:line` query starts another.
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.line_worker = None;
                    self.line_check = None;
                    break;
                }
            }
        }
        changed
    }

    /// Apply answers as they come, for up to `wait`, until the running
    /// check's own lands.
    fn await_line_check(&mut self, wait: std::time::Duration) {
        let deadline = std::time::Instant::now() + wait;
        while self.line_check.is_some() {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            let Some(done) = self
                .line_worker
                .as_ref()
                .and_then(|w| w.answers.recv_timeout(left).ok())
            else {
                return;
            };
            self.apply_line_check(done);
        }
    }

    /// Wait for the running `:line` check's answer and apply it, for a test
    /// that asserts what the check decides.
    #[cfg(test)]
    pub(crate) fn finish_line_check(&mut self) {
        while self.line_check.is_some() {
            let Some(done) = self
                .line_worker
                .as_ref()
                .and_then(|w| w.answers.recv().ok())
            else {
                return;
            };
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

    pub fn replace_entries(&mut self, entries: Arc<Vec<FileEntry>>) {
        if Arc::ptr_eq(&self.entries, &entries) {
            return;
        }
        self.entries = entries;
        self.rerun_keeping_the_pick();
    }

    pub fn push_char(&mut self, c: char) {
        picker_query::insert(&mut self.query, &mut self.cursor, c);
        self.refresh_results();
        self.selected = 0;
        self.scroll = 0;
    }

    pub fn pop_char(&mut self) {
        if !picker_query::backspace(&mut self.query, &mut self.cursor) {
            return;
        }
        self.refresh_results();
        self.selected = 0;
        self.scroll = 0;
    }

    pub fn delete_char(&mut self) {
        if !picker_query::delete(&mut self.query, self.cursor) {
            return;
        }
        self.refresh_results();
        self.selected = 0;
        self.scroll = 0;
    }

    pub fn move_cursor_left(&mut self) {
        picker_query::left(&mut self.cursor);
    }

    pub fn move_cursor_right(&mut self) {
        picker_query::right(&self.query, &mut self.cursor);
    }

    pub fn move_cursor_home(&mut self) {
        self.cursor = 0;
    }

    pub fn move_cursor_end(&mut self) {
        picker_query::end(&self.query, &mut self.cursor);
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
        self.refresh_results_waiting(LINE_CHECK_WAIT);
    }

    /// [`Self::refresh_results`], waiting `wait` for a `:line` check's
    /// answer; none leaves it for [`Self::poll_line_check`].
    fn refresh_results_waiting(&mut self, wait: std::time::Duration) {
        // Only the file part takes part in matching: `alpha:236-239` names
        // alpha.rs and a place in it, and a needle carrying the suffix would
        // match nothing at all (#472).
        let (file_part, hint) = split_line_hint(&self.query);
        self.notice = None;
        // A check still running answers the query before this one: its
        // answer, when it comes, only fills the cache.
        self.line_check = None;
        self.line_candidates.clear();
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
                self.start_line_check(ranked, hint.end.unwrap_or(hint.line), more_matched, wait);
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
        wait: std::time::Duration,
    ) {
        self.results = candidates.iter().take(MAX_RESULTS).cloned().collect();
        if candidates.is_empty() {
            return;
        }
        self.line_check_generation += 1;
        let generation = self.line_check_generation;
        self.line_candidates = candidates.iter().map(|r| r.entry.path.clone()).collect();
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
        worker.submit(LineBatch {
            generation,
            jobs,
            needed,
        });
        self.line_check = Some(LineCheck {
            generation,
            needed,
            candidates,
            more_matched,
        });
        // No wait means no look: the answer is applied when it is polled,
        // never here, however soon it comes.
        if !wait.is_zero() {
            self.await_line_check(wait);
        }
    }

    /// Filter the matches by what a `:line` check found. An answer to an
    /// older query only fills the cache. True when the list changed.
    fn apply_line_check(&mut self, done: LineCheckDone) -> bool {
        for (path, count) in done.counted {
            self.line_counts.insert(path, count);
        }
        let Some(check) = self.line_check.take_if(|c| c.generation == done.generation) else {
            return false;
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
        // picked from the list on screen: it stays picked if it is still
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
            return true;
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
        true
    }
}

/// Count, in rank order, the lines of the matches a `:line` query names,
/// until a page of them is known to reach `needed` or can't be told (#2127),
/// or `replaced` says a newer query waits or the finder has gone. Off the UI
/// thread: every read here may wait on the filesystem. Returns each checked
/// candidate's lines (`None` when unknown) and the counts read from disk,
/// for the cache.
fn check_lines(
    jobs: Vec<LineJob>,
    needed: usize,
    replaced: &dyn Fn() -> bool,
) -> (Vec<Option<usize>>, Vec<(PathBuf, LineCount)>) {
    let mut budget = LINE_COUNT_BUDGET;
    let mut lines = Vec::with_capacity(MAX_RESULTS);
    let mut counted = Vec::new();
    let mut kept = 0;
    for job in jobs {
        if kept == MAX_RESULTS || replaced() {
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
        .border_style(Style::default().fg(theme.ui(picker_colours::BORDER)))
        .title(title.clone())
        .style(Style::default().bg(theme.ui(picker_colours::BODY)));
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
        theme.ui(picker_colours::SELECTION)
    };

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let query_style = Style::default()
        .fg(theme.ui(picker_colours::TEXT))
        .add_modifier(Modifier::BOLD);
    let caret_style = Style::default()
        .fg(theme.ui(picker_colours::BODY))
        .bg(theme.ui(picker_colours::TEXT))
        .add_modifier(Modifier::SLOW_BLINK);
    let cursor = finder.cursor.min(finder.query.chars().count());
    let before: String = finder.query.chars().take(cursor).collect();
    let at: String = finder.query.chars().skip(cursor).take(1).collect();
    let after: String = finder.query.chars().skip(cursor + 1).collect();
    let caret_glyph = if at.is_empty() { String::from(" ") } else { at };
    let prompt_line = Line::from(vec![
        Span::styled("> ", Style::default().fg(theme.ui(picker_colours::PROMPT))),
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
                Style::default().fg(theme.ui(picker_colours::DIM)),
            ))
        } else {
            Line::from(Span::styled(
                format!("  No matches for '{}'", finder.query),
                Style::default().fg(theme.ui(picker_colours::DIM)),
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
            Style::default().fg(theme.ui(picker_colours::TEXT))
        };
        let dir_style = if is_selected {
            Style::default()
                .bg(sel_bg)
                .fg(theme.ui(picker_colours::DETAIL_BRIGHT))
        } else {
            Style::default().fg(theme.ui(picker_colours::DETAIL))
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

    /// #2127 review: a check a newer query replaced stops at the next file
    /// rather than reading on to its end, and what it counted still reaches
    /// the cache, through the one channel every check answers on, so the
    /// next check starts from it.
    #[test]
    fn a_replaced_check_stops_early_and_still_fills_the_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let jobs = |n: usize| -> Vec<LineJob> {
            (0..n)
                .map(|i| LineJob {
                    path: file_with_lines(tmp.path(), &format!("d{i}/a.ts"), 2).path,
                    open: None,
                    cached: None,
                })
                .collect()
        };
        let (lines, counted) = check_lines(jobs(5), 10, &|| false);
        assert_eq!((lines.len(), counted.len()), (5, 5), "all short: all read");
        let asked = std::cell::Cell::new(0);
        let replaced = || {
            asked.set(asked.get() + 1);
            asked.get() > 2
        };
        let (lines, counted) = check_lines(jobs(5), 10, &replaced);
        assert_eq!(
            (lines.len(), counted.len()),
            (2, 2),
            "replaced after two files, it reads no third"
        );

        let short = file_with_lines(tmp.path(), "web/a.ts", 2);
        let path = short.path.clone();
        let mut finder = FileFinder::new(Arc::new(vec![short]));
        query(&mut finder, "a.ts:5");
        finder.line_counts.clear();
        // The answer to a query since replaced lists nothing, but its
        // counts land.
        finder
            .line_worker
            .as_ref()
            .expect("the query started it")
            .submit(LineBatch {
                generation: 0,
                jobs: vec![LineJob {
                    path: path.clone(),
                    open: None,
                    cached: None,
                }],
                needed: 5,
            });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !finder.line_counts.contains_key(&path) {
            assert!(!finder.poll_line_check(), "an older answer changes no list");
            assert!(
                std::time::Instant::now() < deadline,
                "the older answer fills the cache"
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    /// #2127 review: an edit to an open file the `:line` query did not rank
    /// leaves its check be, so a stream of edits elsewhere (an agent writing
    /// to another file) cannot keep restarting it. An edit to a ranked one,
    /// or closing it, reruns the check.
    #[test]
    fn an_edit_to_a_file_the_query_did_not_rank_reruns_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let entries = vec![
            file_with_lines(tmp.path(), "web/a.ts", 50),
            file_with_lines(tmp.path(), "api/a.ts", 400),
            file_with_lines(tmp.path(), "notes.md", 5),
        ];
        let (web, notes) = (entries[0].path.clone(), entries[2].path.clone());
        let mut finder = FileFinder::new(Arc::new(entries));
        query(&mut finder, "a.ts:318");
        assert_eq!(listed(&finder), ["api/a.ts"]);
        let checks = finder.line_check_generation;
        for n in 6..20 {
            assert!(!finder.set_open_lines(HashMap::from([(notes.clone(), n)])));
        }
        assert_eq!(finder.line_check_generation, checks, "no check restarted");

        assert!(finder.set_open_lines(HashMap::from([(notes.clone(), 20), (web.clone(), 400)])));
        assert_eq!(finder.line_check_generation, checks + 1);
        finder.finish_line_check();
        assert_eq!(listed(&finder).len(), 2, "the grown buffer has the line");
        assert!(
            finder.set_open_lines(HashMap::from([(notes, 21)])),
            "closing a ranked file's tab reruns too"
        );
        finder.finish_line_check();
        assert_eq!(listed(&finder), ["api/a.ts"], "its saved file is short");
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
        finder.line_check = Some(LineCheck {
            generation,
            needed: 5,
            candidates: candidates.clone(),
            more_matched: false,
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

    /// #2127 review: a recheck after an open buffer's count changes keeps
    /// the list and the row picked on screen until its answer lands, so
    /// Enter meanwhile opens the row picked, not the best-ranked unchecked
    /// match; the answer keeps that row, or one the user picks meanwhile.
    #[test]
    fn a_recheck_keeps_the_list_and_the_row_picked_until_it_lands() {
        let tmp = tempfile::tempdir().unwrap();
        // Forty-one short `x.ts` rank above the two long ones, by length.
        let mut entries: Vec<FileEntry> = (0..41)
            .map(|i| file_with_lines(tmp.path(), &format!("s{i}/x.ts"), 2))
            .collect();
        entries.push(file_with_lines(tmp.path(), "long/a/x.ts", 10));
        entries.push(file_with_lines(tmp.path(), "long/b/x.ts", 10));
        let a = entries[41].path.clone();
        let b = entries[42].path.clone();
        let mut finder = FileFinder::new(Arc::new(entries));
        query(&mut finder, "x.ts:5");
        assert_eq!(listed(&finder), ["long/a/x.ts", "long/b/x.ts"]);
        finder.select_next();

        assert!(finder.set_open_lines(HashMap::from([(a.clone(), 20)])));
        assert!(finder.line_check.is_some(), "the recheck is still running");
        assert_eq!(
            listed(&finder),
            ["long/a/x.ts", "long/b/x.ts"],
            "the list on screen stays until the answer lands"
        );
        assert_eq!(
            finder.selected_entry().map(|e| &e.path),
            Some(&b),
            "Enter meanwhile opens the row picked"
        );
        finder.finish_line_check();
        assert_eq!(finder.selected_entry().map(|e| &e.path), Some(&b));

        // Two rechecks before the first lands keep the same row.
        assert!(finder.set_open_lines(HashMap::from([(a.clone(), 21)])));
        assert!(finder.set_open_lines(HashMap::from([(a.clone(), 22)])));
        assert_eq!(finder.selected_entry().map(|e| &e.path), Some(&b));
        finder.finish_line_check();
        assert_eq!(finder.selected_entry().map(|e| &e.path), Some(&b));

        // A row the user picks while a recheck runs is the one kept.
        assert!(finder.set_open_lines(HashMap::from([(a.clone(), 30)])));
        assert!(finder.line_check.is_some(), "the recheck is still running");
        finder.select_prev();
        finder.finish_line_check();
        assert_eq!(finder.selected_entry().map(|e| &e.path), Some(&a));
    }

    /// #2127 review: a file index refreshed while Quick Open is up (a file
    /// made or removed in the workspace) reruns the query the way an open
    /// buffer's recheck does. A `:line` query keeps the list and the row
    /// picked on screen until the answer lands, and the answer keeps that
    /// row. With no hint, the row picked follows its file to where the new
    /// index ranks it, so Enter never opens the file a new one pushed up.
    #[test]
    fn an_index_refresh_keeps_the_row_picked() {
        let tmp = tempfile::tempdir().unwrap();
        // Forty-one short `x.ts` rank above the long ones, by length.
        let mut entries: Vec<FileEntry> = (0..41)
            .map(|i| file_with_lines(tmp.path(), &format!("s{i}/x.ts"), 2))
            .collect();
        entries.push(file_with_lines(tmp.path(), "long/a/x.ts", 10));
        entries.push(file_with_lines(tmp.path(), "long/b/x.ts", 10));
        let b = entries[42].path.clone();
        let mut finder = FileFinder::new(Arc::new(entries.clone()));
        query(&mut finder, "x.ts:5");
        assert_eq!(listed(&finder), ["long/a/x.ts", "long/b/x.ts"]);
        finder.select_next();

        entries.push(file_with_lines(tmp.path(), "long/c/x.ts", 10));
        finder.replace_entries(Arc::new(entries));
        assert!(finder.line_check.is_some(), "the recheck is still running");
        assert_eq!(
            listed(&finder),
            ["long/a/x.ts", "long/b/x.ts"],
            "the list on screen stays until the answer lands"
        );
        assert_eq!(
            finder.selected_entry().map(|e| &e.path),
            Some(&b),
            "Enter meanwhile opens the row picked"
        );
        finder.finish_line_check();
        assert_eq!(
            listed(&finder),
            ["long/a/x.ts", "long/b/x.ts", "long/c/x.ts"],
            "the new file is checked and listed"
        );
        assert_eq!(finder.selected_entry().map(|e| &e.path), Some(&b));

        // Opened before the first index landed, the list is empty: there is
        // no row to keep, so the new matches show unchecked at once.
        let mut finder = FileFinder::new(Arc::new(Vec::new()));
        query(&mut finder, "x.ts:5");
        assert!(listed(&finder).is_empty());
        finder.replace_entries(Arc::new(vec![
            file_with_lines(tmp.path(), "short/x.ts", 2),
            file_with_lines(tmp.path(), "long/a/x.ts", 10),
        ]));
        assert!(finder.line_check.is_some(), "the check is still running");
        assert_eq!(listed(&finder), ["short/x.ts", "long/a/x.ts"]);
        finder.finish_line_check();
        assert_eq!(listed(&finder), ["long/a/x.ts"]);

        let mut finder = FileFinder::new(Arc::new(vec![entry("a/x.ts"), entry("b/x.ts")]));
        query(&mut finder, "x.ts");
        finder.select_next();
        assert_eq!(
            finder.selected_entry().map(|e| e.rel.as_str()),
            Some("b/x.ts")
        );
        finder.replace_entries(Arc::new(vec![
            entry("x.ts"),
            entry("a/x.ts"),
            entry("b/x.ts"),
        ]));
        assert_eq!(listed(&finder), ["x.ts", "a/x.ts", "b/x.ts"]);
        assert_eq!(
            finder.selected_entry().map(|e| e.rel.as_str()),
            Some("b/x.ts"),
            "a better-ranked new file does not take the row picked"
        );
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
}
