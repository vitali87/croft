//! Syntax colours for the diff view (#2020).
//!
//! Each side of a diff is coloured by its language's grammar, the way the
//! editor colours the file, so a change reads as code and not as a block of
//! red or green text. The pass runs on a worker thread the first time a view
//! is painted: a small diff colours in the same frame, a large one opens at
//! once and colours a tick later, when [`DiffSyntax::poll`] takes the result.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError};
use std::time::Duration;

use crate::highlight::{HiSpan, LangKind, LangRegistry, compute_line_starts, highlight_text};

/// A side larger than this is left in the diff's plain colours: a grammar
/// pass over it costs more memory and time than colouring a review is worth.
const MAX_SIDE_BYTES: usize = 4_000_000;

/// How many diff colouring passes may run at once, across every diff view.
/// A large file rewritten again and again (an agent saving it) rebuilds its
/// view each time, and a pass already running for the old text cannot be
/// stopped. Past this many, a view waits for one to finish instead of
/// starting another, so passes for text nobody shows any more never pile up.
const MAX_PASSES: usize = 2;

/// The passes running now; see [`MAX_PASSES`].
static PASSES: AtomicUsize = AtomicUsize::new(0);

/// One of the [`MAX_PASSES`] slots, given back when its pass ends, however it
/// ends.
struct PassSlot;

impl PassSlot {
    /// A free slot, or `None` while every one is taken.
    fn take() -> Option<Self> {
        PASSES
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                (n < MAX_PASSES).then_some(n + 1)
            })
            .ok()
            .map(|_| PassSlot)
    }
}

impl Drop for PassSlot {
    fn drop(&mut self) {
        PASSES.fetch_sub(1, Ordering::SeqCst);
    }
}

/// One stretch of a side's lines, `start..end`, that is one piece of code in
/// one language: a whole file, or one hunk of a `git diff`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CodeRun {
    pub start: usize,
    pub end: usize,
    pub lang: LangKind,
}

/// The runs that cover `langs`, one per stretch of consecutive lines in the
/// same language. A `None` line (a `diff --git` or `@@` header, a commit
/// message) ends a run and starts none, so each hunk is parsed on its own.
pub fn runs_of(langs: &[Option<LangKind>]) -> Vec<CodeRun> {
    let mut runs: Vec<CodeRun> = Vec::new();
    for (i, lang) in langs.iter().enumerate() {
        let Some(lang) = *lang else { continue };
        match runs.last_mut() {
            Some(run) if run.end == i && run.lang == lang => run.end = i + 1,
            _ => runs.push(CodeRun {
                start: i,
                end: i + 1,
                lang,
            }),
        }
    }
    runs
}

/// Per-line spans for both sides.
type Spans = Vec<Vec<HiSpan>>;

enum Job {
    /// No pass has run for the current text and theme.
    NotStarted,
    /// A pass is due, but every slot was taken; the next tick tries again.
    Waiting,
    Running(Receiver<(Spans, Spans)>),
    Done,
}

/// A diff view's syntax colours, and the pass that computes them.
pub struct DiffSyntax {
    /// Which lines are code and in what language, when the view knows better
    /// than its paths: a `git diff` view holds many files' hunks between
    /// header lines. `None` means each side is one file, in its path's
    /// language.
    runs: Option<(Vec<CodeRun>, Vec<CodeRun>)>,
    left: Spans,
    right: Spans,
    job: Job,
}

impl Default for DiffSyntax {
    fn default() -> Self {
        Self {
            runs: None,
            left: Vec::new(),
            right: Vec::new(),
            job: Job::NotStarted,
        }
    }
}

impl Clone for DiffSyntax {
    /// The copy keeps the colours already landed; a pass still running stays
    /// with the original, and the copy starts its own when painted.
    fn clone(&self) -> Self {
        Self {
            runs: self.runs.clone(),
            left: self.left.clone(),
            right: self.right.clone(),
            job: match self.job {
                Job::Done => Job::Done,
                Job::NotStarted | Job::Waiting | Job::Running(_) => Job::NotStarted,
            },
        }
    }
}

/// Two views are the same view whatever their colours: the spans are worked
/// out from the text, which `DiffData` compares on its own.
impl PartialEq for DiffSyntax {
    fn eq(&self, other: &Self) -> bool {
        self.runs == other.runs
    }
}

impl Eq for DiffSyntax {}

impl std::fmt::Debug for DiffSyntax {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiffSyntax")
            .field("runs", &self.runs)
            .field("coloured", &matches!(self.job, Job::Done))
            .finish()
    }
}

impl DiffSyntax {
    /// Colours for a view whose lines are the code runs given, per side.
    pub fn with_runs(left: Vec<CodeRun>, right: Vec<CodeRun>) -> Self {
        Self {
            runs: Some((left, right)),
            ..Self::default()
        }
    }

    /// The spans of the left side's line `i`; empty when it has none (yet).
    pub fn left(&self, i: usize) -> &[HiSpan] {
        self.left.get(i).map_or(&[], Vec::as_slice)
    }

    /// The spans of the right side's line `i`; empty when it has none (yet).
    pub fn right(&self, i: usize) -> &[HiSpan] {
        self.right.get(i).map_or(&[], Vec::as_slice)
    }

    /// Start the pass if none has run for this text and theme, and wait up to
    /// `wait` for it to land; `None` colours here, on the calling thread.
    /// `langs` gives each side's language when the view has no runs of its
    /// own; it is asked only when a pass starts, not on every frame. Only the
    /// call that starts a pass waits: one already running is left to
    /// [`Self::poll`], so a slow pass never holds back the frames after it.
    /// With every slot taken the view waits for one (see [`MAX_PASSES`]), and
    /// [`Self::waiting`] says so.
    pub fn ensure(
        &mut self,
        left_lines: &[String],
        right_lines: &[String],
        langs: impl FnOnce() -> (Option<LangKind>, Option<LangKind>),
        wait: Option<Duration>,
    ) {
        if !matches!(self.job, Job::NotStarted | Job::Waiting) {
            return;
        }
        let slot = match wait {
            None => None,
            Some(_) => match PassSlot::take() {
                Some(slot) => Some(slot),
                None => {
                    self.job = Job::Waiting;
                    return;
                }
            },
        };
        let langs = match self.runs {
            Some(_) => (None, None),
            None => langs(),
        };
        self.start(left_lines, right_lines, langs, slot);
        let (Job::Running(rx), Some(wait)) = (&self.job, wait) else {
            return;
        };
        match rx.recv_timeout(wait) {
            Ok((left, right)) => self.land(left, right),
            Err(RecvTimeoutError::Timeout) => {}
            // The worker died without a result: paint the plain colours,
            // not spans left from an older theme or text.
            Err(RecvTimeoutError::Disconnected) => self.land(Vec::new(), Vec::new()),
        }
    }

    /// True while a pass is due but waits for a free slot: the tick calls
    /// [`Self::ensure`] again until it starts.
    pub fn waiting(&self) -> bool {
        matches!(self.job, Job::Waiting)
    }

    /// Take a pass that finished since the last tick. True when the colours
    /// changed, so the frame repaints in them.
    pub fn poll(&mut self) -> bool {
        let Job::Running(rx) = &self.job else {
            return false;
        };
        match rx.try_recv() {
            Ok((left, right)) => {
                self.land(left, right);
                true
            }
            Err(TryRecvError::Empty) => false,
            // The worker died without a result: plain colours, as above.
            Err(TryRecvError::Disconnected) => {
                self.land(Vec::new(), Vec::new());
                true
            }
        }
    }

    /// Work the colours out again on the next paint, for a new theme: the
    /// spans hold the old palette's colours. They stay on screen until the
    /// new ones land, so the view does not flash plain.
    pub fn restart(&mut self) {
        self.job = Job::NotStarted;
    }

    /// Keep the colours of a rebuilt view's sides whose lines did not change,
    /// so a file saved on one side does not flash the other side plain.
    pub fn carry_from(&mut self, old: &DiffSyntax, left_same: bool, right_same: bool) {
        if self.runs != old.runs {
            return;
        }
        if left_same {
            self.left = old.left.clone();
        }
        if right_same {
            self.right = old.right.clone();
        }
        if left_same && right_same && matches!(old.job, Job::Done) {
            self.job = Job::Done;
        }
    }

    fn land(&mut self, left: Spans, right: Spans) {
        self.left = left;
        self.right = right;
        self.job = Job::Done;
    }

    /// Start the pass on a worker holding `slot`, or colour here when there
    /// is no slot (the caller asked to wait however long it takes).
    fn start(
        &mut self,
        left_lines: &[String],
        right_lines: &[String],
        (left_lang, right_lang): (Option<LangKind>, Option<LangKind>),
        slot: Option<PassSlot>,
    ) {
        let whole = |lines: &[String], lang: Option<LangKind>| -> Vec<CodeRun> {
            lang.filter(|_| !lines.is_empty())
                .map(|lang| CodeRun {
                    start: 0,
                    end: lines.len(),
                    lang,
                })
                .into_iter()
                .collect()
        };
        let (left_runs, right_runs) = match &self.runs {
            Some(runs) => runs.clone(),
            None => (whole(left_lines, left_lang), whole(right_lines, right_lang)),
        };
        let left = side_texts(left_lines, &left_runs);
        let right = side_texts(right_lines, &right_runs);
        if left.is_empty() && right.is_empty() {
            self.job = Job::Done;
            return;
        }
        let (left_len, right_len) = (left_lines.len(), right_lines.len());
        let Some(slot) = slot else {
            self.land(colour(left_len, left), colour(right_len, right));
            return;
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name(String::from("croft-diff-highlight"))
            .spawn(move || {
                let _slot = slot;
                // Nobody listening (the tab closed, or the view was rebuilt)
                // is fine: the result is simply dropped.
                let _ = tx.send((colour(left_len, left), colour(right_len, right)));
            });
        match spawned {
            Ok(_) => self.job = Job::Running(rx),
            // No thread to be had (the slot went with the closure): colour
            // here.
            Err(_) => self.land(
                colour(left_len, side_texts(left_lines, &left_runs)),
                colour(right_len, side_texts(right_lines, &right_runs)),
            ),
        }
    }
}

/// Each run of `lines` joined into the text its grammar parses, with the run.
/// Empty when the side is too large to colour.
fn side_texts(lines: &[String], runs: &[CodeRun]) -> Vec<(CodeRun, String)> {
    let bytes: usize = runs
        .iter()
        .filter_map(|run| lines.get(run.start..run.end.min(lines.len())))
        .flatten()
        .map(|l| l.len() + 1)
        .sum();
    if bytes > MAX_SIDE_BYTES {
        return Vec::new();
    }
    runs.iter()
        .filter_map(|run| {
            let code = lines.get(run.start..run.end.min(lines.len()))?;
            (!code.is_empty()).then(|| (*run, code.join("\n")))
        })
        .collect()
}

/// The spans for a side of `len` lines whose code is `texts`. A side with no
/// code holds no per-line list at all: [`DiffSyntax::left`] and
/// [`DiffSyntax::right`] read a missing line as one with no spans.
fn colour(len: usize, texts: Vec<(CodeRun, String)>) -> Spans {
    if texts.is_empty() {
        return Vec::new();
    }
    let mut out: Spans = vec![Vec::new(); len];
    let mut registry = LangRegistry::new();
    for (run, text) in texts {
        let starts = compute_line_starts(text.as_bytes());
        let spans = highlight_text(&mut registry, run.lang, text.as_bytes(), &starts);
        for (slot, line) in out[run.start..].iter_mut().zip(spans) {
            *slot = line;
        }
    }
    out.iter_mut().for_each(Vec::shrink_to_fit);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUST: (Option<LangKind>, Option<LangKind>) = (Some(LangKind::Rust), Some(LangKind::Rust));

    fn lines(s: &[&str]) -> Vec<String> {
        s.iter().map(|l| l.to_string()).collect()
    }

    #[test]
    fn runs_split_at_headers_and_language_changes() {
        let rs = Some(LangKind::Rust);
        let py = Some(LangKind::Python);
        assert_eq!(
            runs_of(&[None, rs, rs, None, rs, py, py, None]),
            vec![
                CodeRun {
                    start: 1,
                    end: 3,
                    lang: LangKind::Rust
                },
                CodeRun {
                    start: 4,
                    end: 5,
                    lang: LangKind::Rust
                },
                CodeRun {
                    start: 5,
                    end: 7,
                    lang: LangKind::Python
                },
            ]
        );
        assert!(runs_of(&[None, None]).is_empty());
    }

    #[test]
    fn a_file_in_a_known_language_gets_its_colours_on_both_sides() {
        let left = lines(&["fn old() {}", "let x = 1;"]);
        let right = lines(&["fn new() {}", "let x = 2;"]);
        let mut syntax = DiffSyntax::default();
        syntax.ensure(&left, &right, || RUST, None);
        assert!(!syntax.left(0).is_empty(), "the old side is coloured");
        assert!(!syntax.right(0).is_empty(), "the new side is coloured");
        let keyword = |spans: &[HiSpan]| {
            spans
                .iter()
                .find(|s| s.range() == (0..2))
                .map(|s| s.style)
                .expect("`fn` has a span of its own")
        };
        let ident = |spans: &[HiSpan]| {
            spans
                .iter()
                .find(|s| s.range().start == 3)
                .map(|s| s.style)
                .expect("the name has a span")
        };
        assert_ne!(
            keyword(syntax.left(0)),
            ident(syntax.left(0)),
            "a keyword and a name read in different colours"
        );
        assert!(syntax.left(5).is_empty(), "past the end there is nothing");
    }

    #[test]
    fn a_file_with_no_grammar_stays_plain() {
        let text = lines(&["just words"]);
        let mut syntax = DiffSyntax::default();
        syntax.ensure(&text, &text, || (None, None), None);
        assert!(syntax.left(0).is_empty() && syntax.right(0).is_empty());
        assert!(!syntax.poll(), "nothing is pending");
    }

    #[test]
    fn only_the_runs_are_coloured() {
        let left = lines(&["diff --git a/x.rs b/x.rs", "@@ -1 +1 @@", "fn a() {}"]);
        let runs = runs_of(&[None, None, Some(LangKind::Rust)]);
        let mut syntax = DiffSyntax::with_runs(runs.clone(), runs);
        syntax.ensure(&left, &left, || unreachable!("the runs say"), None);
        assert!(syntax.left(0).is_empty(), "a header line is not code");
        assert!(syntax.left(1).is_empty(), "a hunk header is not code");
        assert!(!syntax.left(2).is_empty(), "the hunk's code is");
    }

    #[test]
    fn a_side_too_large_to_colour_stays_plain() {
        let big = vec!["let x = 1;".to_string(); MAX_SIDE_BYTES / 10 + 1];
        let small = lines(&["let x = 1;"]);
        let mut syntax = DiffSyntax::default();
        syntax.ensure(&big, &small, || RUST, None);
        assert!(syntax.left(0).is_empty(), "the oversized side is skipped");
        assert!(syntax.left.is_empty(), "and holds no per-line lists");
        assert!(!syntax.right(0).is_empty(), "the other side still colours");
    }

    #[test]
    fn a_theme_change_keeps_the_old_colours_until_the_new_ones_land() {
        let text = lines(&["fn a() {}"]);
        let mut syntax = DiffSyntax::default();
        syntax.ensure(&text, &text, || RUST, None);
        let before = syntax.left(0).len();
        syntax.restart();
        assert_eq!(syntax.left(0).len(), before, "no flash to plain");
        syntax.ensure(&text, &text, || RUST, None);
        assert!(!syntax.left(0).is_empty());
    }

    #[test]
    fn a_pass_waits_for_a_free_slot_instead_of_piling_up() {
        let text = lines(&["fn a() {}"]);
        let mut syntax = DiffSyntax::default();
        // Every slot taken, counted relative to whatever other tests run.
        PASSES.fetch_add(MAX_PASSES, Ordering::SeqCst);
        syntax.ensure(&text, &text, || RUST, Some(Duration::ZERO));
        let waited = syntax.waiting();
        PASSES.fetch_sub(MAX_PASSES, Ordering::SeqCst);
        assert!(waited, "with no slot free the pass waits");
        assert!(syntax.left(0).is_empty(), "and nothing is coloured yet");
        assert!(!syntax.poll(), "nothing is running to land");
        syntax.ensure(&text, &text, || RUST, None);
        assert!(!syntax.waiting());
        assert!(!syntax.left(0).is_empty(), "asked again, it runs");
    }

    #[test]
    fn a_pass_that_dies_leaves_plain_colours_not_stale_ones() {
        let text = lines(&["fn a() {}"]);
        let mut syntax = DiffSyntax::default();
        syntax.ensure(&text, &text, || RUST, None);
        assert!(!syntax.left(0).is_empty());
        // A theme switch, then a worker that ends without a result.
        syntax.restart();
        let (tx, rx) = std::sync::mpsc::channel();
        drop(tx);
        syntax.job = Job::Running(rx);
        assert!(syntax.poll(), "the view repaints");
        assert!(syntax.left(0).is_empty(), "in plain colours");
        assert!(!syntax.waiting() && matches!(syntax.job, Job::Done));
    }

    #[test]
    fn a_rebuilt_view_keeps_the_unchanged_sides_colours() {
        let text = lines(&["fn a() {}"]);
        let mut old = DiffSyntax::default();
        old.ensure(&text, &text, || RUST, None);
        let mut fresh = DiffSyntax::default();
        fresh.carry_from(&old, true, false);
        assert!(
            !fresh.left(0).is_empty(),
            "the unchanged side keeps its colours"
        );
        assert!(
            fresh.right(0).is_empty(),
            "the changed side waits for its pass"
        );
        assert!(
            matches!(fresh.job, Job::NotStarted),
            "and a pass still runs for it"
        );
        let mut same = DiffSyntax::default();
        same.carry_from(&old, true, true);
        assert!(matches!(same.job, Job::Done), "nothing left to colour");
    }
}
