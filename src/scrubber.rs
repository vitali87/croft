//! Moving through a branch's history (#371).
//!
//! TIMELINE lists a file's history and the COMMITS graph lists the repo's;
//! neither lets you *move* through time. This is the cursor that does: it
//! sits at a commit, steps between them, and knows how to get back.
//!
//! # The invariant
//!
//! **Leaving the scrubber must restore the live buffer exactly, including
//! unsaved changes.** Scrubbing is a way of looking, not of editing, and a
//! feature that loses a user's uncommitted work to answer a question about
//! history would be worse than not having the feature. So the working tree
//! is a POSITION in the cursor rather than a thing the scrubber replaces:
//! there is no state in which the live buffer has been discarded and the
//! scrubber is responsible for putting it back.
//!
//! That is why [`Position::Working`] is a variant rather than `Option::None`
//! over a commit index. An `Option` invites "no commit selected" and "back at
//! the working tree" to be the same value, and they are not — the second is
//! where the user started and must be reachable from anywhere.

/// Where the scrubber is looking.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Position {
    /// The live buffer, with whatever unsaved edits it holds. The scrubber
    /// starts here and `Home` returns here.
    Working,
    /// A commit, by index into the loaded list — 0 is the newest.
    At(usize),
}

/// The scrubber's cursor over a branch's commits.
#[derive(Clone, Debug)]
pub struct Scrubber {
    /// The branch's first-parent history, newest first, so index 0 is HEAD.
    ///
    /// Whole `GraphCommit`s rather than hashes: the slider needs `short_hash`
    /// for its labels and `parents` for the per-commit gutter delta, and
    /// re-fetching them once the widget exists would mean two sources for
    /// one list.
    commits: Vec<crate::git::GraphCommit>,
    position: Position,
}

impl Scrubber {
    /// A scrubber over `commits` (newest first), parked at the working tree.
    pub fn new(commits: Vec<crate::git::GraphCommit>) -> Self {
        Self {
            commits,
            position: Position::Working,
        }
    }

    /// The commits the scrubber walks, newest first.
    pub fn commits(&self) -> &[crate::git::GraphCommit] {
        &self.commits
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn position(&self) -> Position {
        self.position
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn is_empty(&self) -> bool {
        self.commits.is_empty()
    }

    /// The commit the cursor is on, or `None` at the working tree.
    ///
    /// The whole record, so a caller can label the slider with git's own
    /// `short_hash` rather than byte-slicing the full one — that slice is
    /// safe only while the string is ASCII hex, and the width git shows is
    /// per-repo rather than always seven.
    pub fn commit(&self) -> Option<&crate::git::GraphCommit> {
        match self.position {
            Position::Working => None,
            Position::At(i) => self.commits.get(i),
        }
    }

    /// Step one commit toward the PAST.
    ///
    /// From the working tree that is HEAD (index 0), not index 1: the
    /// working tree and HEAD are different views — one has your unsaved
    /// edits — so stepping back from the tree must show HEAD rather than
    /// skipping it.
    pub fn older(&mut self) {
        if self.commits.is_empty() {
            return;
        }
        self.position = match self.position {
            Position::Working => Position::At(0),
            Position::At(i) if i + 1 < self.commits.len() => Position::At(i + 1),
            // Already at the oldest loaded commit: stay rather than wrap.
            // Wrapping to the present would look like the drag "slipped".
            other => other,
        };
    }

    /// Step one commit toward the PRESENT, arriving at the working tree from
    /// HEAD.
    pub fn newer(&mut self) {
        self.position = match self.position {
            Position::Working => Position::Working,
            Position::At(0) => Position::Working,
            Position::At(i) => Position::At(i - 1),
        };
    }

    /// Jump to the working tree — `Home`, and the only way back that a
    /// caller ever needs.
    pub fn home(&mut self) {
        self.position = Position::Working;
    }

    /// Jump to a commit by index, clamped to what is loaded.
    ///
    /// Not reached from the keyboard — this is what a DRAG on the slider
    /// calls, and the slider widget is the next layer of #371. Kept here
    /// with its tests because the clamping rule is the part with a right
    /// answer, and deciding it alongside the painting would bury it.
    ///
    /// Clamped rather than refused because this is what a DRAG calls: a
    /// pointer beyond the last commit means "as far as it goes", and
    /// refusing would make the slider stick short of its own end.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn seek(&mut self, index: usize) {
        if self.commits.is_empty() {
            self.position = Position::Working;
            return;
        }
        self.position = Position::At(index.min(self.commits.len() - 1));
    }

    /// Move to the stop nearest `f` along the slider, 0.0 the oldest loaded
    /// commit and 1.0 the working tree: the inverse of [`Self::fraction`],
    /// which is what a click or drag on the slider calls.
    pub fn seek_fraction(&mut self, f: f32) {
        let stops = self.commits.len();
        if stops == 0 {
            self.position = Position::Working;
            return;
        }
        let p = (f.clamp(0.0, 1.0) * stops as f32).round() as usize;
        self.position = if p >= stops {
            Position::Working
        } else {
            Position::At(stops - 1 - p)
        };
    }

    /// The commits worth having ready around the cursor, most urgent
    /// first: the one it is on, then the ones a held arrow key reaches
    /// next. Older before newer, because scrubbing starts at the present
    /// and mostly walks back.
    pub fn around(&self) -> Vec<&crate::git::GraphCommit> {
        let at = match self.position {
            Position::Working => None,
            Position::At(i) => Some(i),
        };
        // From the working tree, HEAD (index 0) is the next step back.
        let offsets: [(Option<usize>, isize); 6] = match at {
            Some(i) => [
                (Some(i), 0),
                (Some(i), 1),
                (Some(i), -1),
                (Some(i), 2),
                (Some(i), 3),
                (Some(i), 4),
            ],
            None => [
                (Some(0), 0),
                (Some(0), 1),
                (Some(0), 2),
                (Some(0), 3),
                (None, 0),
                (None, 0),
            ],
        };
        let mut out: Vec<&crate::git::GraphCommit> = Vec::new();
        for (base, d) in offsets {
            let Some(base) = base else { continue };
            let Some(i) = base.checked_add_signed(d) else {
                continue;
            };
            if let Some(c) = self.commits.get(i)
                && !out.iter().any(|o| o.hash == c.hash)
            {
                out.push(c);
            }
        }
        out
    }

    /// How many commits are loaded.
    pub fn len(&self) -> usize {
        self.commits.len()
    }

    /// Where the slider's handle sits, as a fraction from 0.0 (oldest loaded)
    /// to 1.0 (the working tree).
    ///
    /// The working tree is its own stop at the far right rather than sharing
    /// HEAD's position, because they are different views and a slider that
    /// showed them at the same place would give the user no way to tell
    /// which one they are looking at.
    pub fn fraction(&self) -> f32 {
        let stops = self.commits.len();
        if stops == 0 {
            return 1.0;
        }
        match self.position {
            Position::Working => 1.0,
            Position::At(i) => {
                // `stops` intervals between `stops + 1` positions.
                (stops - i - 1) as f32 / stops as f32
            }
        }
    }
}

/// The name the file had at each of `commits` (newest first), given its
/// `--follow` walk `touched` (newest first, from [`crate::git::follow_names`])
/// (#1304). The name changes only in a commit that touched the file, so a
/// commit the walk skips has the name of the nearest older one it lists.
/// Commits older than the walk's oldest entry are left out: the file did not
/// exist there under any name the walk knows.
pub fn names_by_commit(
    commits: &[crate::git::GraphCommit],
    touched: &[(String, String)],
) -> std::collections::HashMap<String, String> {
    let touched: std::collections::HashMap<&str, &str> = touched
        .iter()
        .map(|(hash, path)| (hash.as_str(), path.as_str()))
        .collect();
    let mut name: Option<&str> = None;
    let mut out = std::collections::HashMap::new();
    for c in commits.iter().rev() {
        if let Some(path) = touched.get(c.hash.as_str()) {
            name = Some(path);
        }
        if let Some(name) = name {
            out.insert(c.hash.clone(), name.to_string());
        }
    }
    out
}

/// One file at one commit, to be built into a read-only view.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ViewKey {
    pub hash: String,
    /// Workspace-relative path.
    pub rel: String,
}

/// Views kept for the scrubber to step back to, least recently used first.
pub type KeptViews = std::collections::VecDeque<(ViewKey, crate::widgets::editor::Editor)>;

/// What [`ViewBuilder`] needs to build a view for a [`ViewKey`].
#[derive(Clone, Debug)]
pub struct ViewJob {
    pub key: ViewKey,
    /// The file's absolute path: picks the grammar and labels the view.
    pub path: std::path::PathBuf,
    /// git's abbreviation of the commit, for the "did not exist" note.
    pub short: String,
    /// The file's name at the commit and at its parent, which differ from
    /// `key.rel` before a rename (#1304).
    pub read_rel: String,
    pub parent_rel: String,
    /// The commit's first parent, whose version of the file the gutter
    /// marks are drawn against.
    pub parent: Option<String>,
    /// The file's text at the commit and at the parent when the app already
    /// has them (`Some(None)`: it did not exist there), so a lane only runs
    /// `git show` for a version nobody has read yet.
    pub text: Option<Option<std::sync::Arc<str>>>,
    pub parent_text: Option<Option<std::sync::Arc<str>>>,
}

/// A view the builder produced, with the texts it read on the way so the
/// app's text cache learns them too.
pub struct BuiltView {
    pub key: ViewKey,
    /// The finished view (highlighted, gutter-marked) rather than the plain
    /// stand-in shown until it lands.
    pub finished: bool,
    pub text: Option<std::sync::Arc<str>>,
    pub parent_text: Option<(String, Option<std::sync::Arc<str>>)>,
    pub view: crate::widgets::editor::Editor,
    /// The file's outline at the commit, from its own syntax tree: built
    /// with the finished view, since parsing a big file takes far longer
    /// than a frame. `None` from the plain lane.
    pub outline: Option<Vec<crate::lsp::manager::OutlineSymbol>>,
}

/// A queue a worker drains, most urgent first, which the app REPLACES on
/// every step rather than appending to, so holding an arrow key leaves no
/// backlog of commits already passed. `None` tells the worker to stop.
type Lane = std::sync::Arc<(std::sync::Mutex<Option<Vec<ViewJob>>>, std::sync::Condvar)>;

/// Builds historical views off the UI thread (#371), in two lanes.
///
/// Highlighting and diffing a big file costs far more than a frame (the
/// 35k-line `src/app/mod.rs` takes over half a second), and even splitting
/// it into lines takes several milliseconds. So a step never does either:
/// the PLAIN lane has the text of the commits around the cursor split and
/// waiting, which a step shows at once, and the FINISHED lane swaps the
/// highlighted view in behind it when it lands. Separate threads, so a
/// slow finished build never holds up the plain views a held key needs.
pub struct ViewBuilder {
    plain: Lane,
    finished: Lane,
    done: std::sync::mpsc::Receiver<BuiltView>,
}

impl ViewBuilder {
    pub fn start(root: std::path::PathBuf) -> Self {
        let lane = || -> Lane {
            std::sync::Arc::new((
                std::sync::Mutex::new(Some(Vec::new())),
                std::sync::Condvar::new(),
            ))
        };
        let (plain, finished) = (lane(), lane());
        let (tx, done) = std::sync::mpsc::channel();
        for (name, queue, full) in [
            ("scrub-plain", &plain, false),
            ("scrub-views", &finished, true),
        ] {
            let (root, queue, tx) = (root.clone(), std::sync::Arc::clone(queue), tx.clone());
            std::thread::Builder::new()
                .name(name.into())
                .spawn(move || Self::run(&root, &queue, &tx, full))
                .ok();
        }
        Self {
            plain,
            finished,
            done,
        }
    }

    fn run(
        root: &std::path::Path,
        queue: &(std::sync::Mutex<Option<Vec<ViewJob>>>, std::sync::Condvar),
        tx: &std::sync::mpsc::Sender<BuiltView>,
        full: bool,
    ) {
        let (lock, ready) = queue;
        loop {
            let job = {
                let Ok(mut pending) = lock.lock() else { return };
                loop {
                    let Some(jobs) = pending.as_mut() else { return };
                    if !jobs.is_empty() {
                        break jobs.remove(0);
                    }
                    let Ok(next) = ready.wait(pending) else {
                        return;
                    };
                    pending = next;
                }
            };
            let read = |known: Option<Option<std::sync::Arc<str>>>, rev: &str, rel: &str| {
                known.unwrap_or_else(|| {
                    crate::git::read_file_at_rev(root, rev, rel)
                        .ok()
                        .map(std::sync::Arc::from)
                })
            };
            let text = read(job.text.clone(), &job.key.hash, &job.read_rel);
            let built = if full {
                let parent_text = job
                    .parent
                    .as_ref()
                    .map(|p| (p.clone(), read(job.parent_text.clone(), p, &job.parent_rel)));
                let baseline = parent_text
                    .as_ref()
                    .and_then(|(_, t)| t.as_deref())
                    .map(crate::widgets::editor::split_into_lines)
                    .unwrap_or_default();
                let view = historical_view(
                    &job.path,
                    &job.key.rel,
                    &job.short,
                    text.as_deref(),
                    baseline,
                );
                // A file the commit predates has no outline; its view is a
                // note, not the file.
                let outline = Some(if text.is_some() {
                    crate::outline_syntax::symbols_for_lines(&job.path, &view.lines)
                } else {
                    Vec::new()
                });
                BuiltView {
                    key: job.key,
                    finished: true,
                    text,
                    parent_text,
                    view,
                    outline,
                }
            } else {
                let view = plain_view(&job.path, &job.key.rel, &job.short, text.as_deref());
                BuiltView {
                    key: job.key,
                    finished: false,
                    text,
                    parent_text: None,
                    view,
                    outline: None,
                }
            };
            if tx.send(built).is_err() {
                return;
            }
        }
    }

    fn replace(lane: &Lane, jobs: Vec<ViewJob>) {
        let (lock, ready) = &**lane;
        if let Ok(mut pending) = lock.lock()
            && let Some(slot) = pending.as_mut()
        {
            *slot = jobs;
            ready.notify_one();
        }
    }

    /// Replace what waits in each lane: `plain` wants stand-ins, `finished`
    /// wants highlighted views, each most urgent first. A job already under
    /// way finishes regardless.
    pub fn want(&self, plain: Vec<ViewJob>, finished: Vec<ViewJob>) {
        Self::replace(&self.plain, plain);
        Self::replace(&self.finished, finished);
    }

    /// Every view produced since the last call.
    pub fn drain(&self) -> Vec<BuiltView> {
        self.done.try_iter().collect()
    }
}

impl Drop for ViewBuilder {
    fn drop(&mut self) {
        for lane in [&self.plain, &self.finished] {
            let (lock, ready) = &**lane;
            if let Ok(mut pending) = lock.lock() {
                *pending = None;
            }
            ready.notify_one();
        }
    }
}

/// The stand-in for `rel` at commit `short` until its finished view lands:
/// the text alone, or the note that the file did not exist yet.
pub fn plain_view(
    path: &std::path::Path,
    rel: &str,
    short: &str,
    text: Option<&str>,
) -> crate::widgets::editor::Editor {
    match text {
        Some(text) => crate::widgets::editor::Editor::historical_plain(path, text),
        None => missing_view(rel, short),
    }
}

/// The finished read-only view of `rel` at commit `short`: highlighted and
/// marked against `baseline`, or a note when the file did not exist yet.
pub fn historical_view(
    path: &std::path::Path,
    rel: &str,
    short: &str,
    text: Option<&str>,
    baseline: Vec<String>,
) -> crate::widgets::editor::Editor {
    match text {
        Some(text) => crate::widgets::editor::Editor::historical(path, text, baseline),
        None => missing_view(rel, short),
    }
}

/// The view shown for `rel` at a commit (`short`) where it did not exist.
pub fn missing_view(rel: &str, short: &str) -> crate::widgets::editor::Editor {
    crate::widgets::editor::Editor::historical(
        std::path::Path::new("history.txt"),
        &format!("({rel} did not exist at {short})"),
        Vec::new(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeking_by_fraction_inverts_the_handle_position() {
        let mut s = Scrubber::new((0..4).map(commit).collect());
        for p in [
            Position::Working,
            Position::At(0),
            Position::At(2),
            Position::At(3),
        ] {
            let mut probe = Scrubber::new((0..4).map(commit).collect());
            probe.position = p;
            s.seek_fraction(probe.fraction());
            assert_eq!(s.position(), p);
        }
        s.seek_fraction(-3.0);
        assert_eq!(
            s.position(),
            Position::At(3),
            "past the left end is the oldest"
        );
        s.seek_fraction(9.0);
        assert_eq!(s.position(), Position::Working);
    }

    fn commit(i: usize) -> crate::git::GraphCommit {
        crate::git::GraphCommit {
            hash: format!("c{i}"),
            short_hash: format!("c{i}"),
            parents: Vec::new(),
            refs: Vec::new(),
            summary: format!("commit {i}"),
            author: String::from("t"),
            age_secs: 0,
        }
    }

    #[test]
    fn a_commit_the_follow_walk_skips_has_the_name_of_the_nearest_older_one() {
        // Newest first: c0 edits new.py, c1 touches something else, c2
        // renames old.py to new.py, c3 and c4 edit old.py, c5 predates it.
        let commits: Vec<_> = (0..6).map(commit).collect();
        let touched = [
            ("c0", "new.py"),
            ("c2", "new.py"),
            ("c3", "old.py"),
            ("side", "elsewhere.py"),
            ("c4", "old.py"),
        ]
        .map(|(h, p)| (h.to_string(), p.to_string()));
        let names = names_by_commit(&commits, &touched);
        let name = |h: &str| names.get(h).map(String::as_str);
        assert_eq!(name("c0"), Some("new.py"));
        assert_eq!(name("c1"), Some("new.py"), "skipped, so the rename's name");
        assert_eq!(name("c2"), Some("new.py"));
        assert_eq!(name("c3"), Some("old.py"));
        assert_eq!(name("c4"), Some("old.py"));
        assert_eq!(name("c5"), None, "older than the file: no name");
        assert_eq!(name("side"), None, "only the walked commits are named");
    }

    #[test]
    fn an_empty_follow_walk_names_nothing() {
        let commits: Vec<_> = (0..3).map(commit).collect();
        assert!(names_by_commit(&commits, &[]).is_empty());
    }

    fn scrubber(n: usize) -> Scrubber {
        Scrubber::new((0..n).map(commit).collect())
    }

    /// The working tree is always reachable, from every position and by
    /// stepping as well as by `Home`.
    ///
    /// This is the module's invariant in its testable form: the live buffer
    /// with its unsaved edits is a POSITION, so there is no state the
    /// scrubber can reach from which the user cannot get back to what they
    /// were editing.
    #[test]
    fn the_working_tree_is_reachable_from_everywhere() {
        let mut s = scrubber(5);
        assert_eq!(s.position(), Position::Working, "starts at the live buffer");

        // From the oldest commit, by Home.
        s.seek(4);
        assert_eq!(s.position(), Position::At(4));
        s.home();
        assert_eq!(s.position(), Position::Working);

        // And by stepping forward, which must ARRIVE rather than stop at
        // HEAD — a scrubber that stranded the user one step short of their
        // own edits would be the invariant failing quietly.
        s.seek(3);
        for _ in 0..4 {
            s.newer();
        }
        assert_eq!(
            s.position(),
            Position::Working,
            "stepping forward from any commit reaches the live buffer"
        );

        // Already there: stepping forward again is a no-op, not a wrap.
        s.newer();
        assert_eq!(s.position(), Position::Working);
    }

    /// Stepping back from the working tree lands on HEAD, not past it.
    ///
    /// The working tree and HEAD are different views — one carries unsaved
    /// edits — so skipping HEAD would hide the commit the user most wants to
    /// compare against.
    #[test]
    fn stepping_back_from_the_tree_shows_head_first() {
        let mut s = scrubber(3);
        s.older();
        assert_eq!(
            s.position(),
            Position::At(0),
            "HEAD, not the commit below it"
        );
        assert_eq!(s.commit().map(|c| c.hash.as_str()), Some("c0"));
        s.older();
        assert_eq!(s.position(), Position::At(1));
    }

    /// The far end stops rather than wrapping.
    ///
    /// A drag that ran off the oldest commit and reappeared at the present
    /// would read as the slider slipping, and the user would not know which
    /// end they were at.
    #[test]
    fn the_oldest_commit_is_a_wall_not_a_wrap() {
        let mut s = scrubber(3);
        for _ in 0..10 {
            s.older();
        }
        assert_eq!(s.position(), Position::At(2), "stopped at the oldest");
        assert_eq!(s.commit().map(|c| c.hash.as_str()), Some("c2"));
    }

    /// A drag beyond the end clamps, because a pointer past the last commit
    /// means "as far as it goes".
    #[test]
    fn a_seek_past_the_end_clamps_to_the_oldest() {
        let mut s = scrubber(3);
        s.seek(99);
        assert_eq!(s.position(), Position::At(2));
        assert_eq!(s.commit().map(|c| c.hash.as_str()), Some("c2"));
    }

    /// An empty history has nowhere to go, and says so by staying put.
    ///
    /// A repo with no commits is ordinary — a fresh `git init` — so this is
    /// the empty state, not an error case.
    #[test]
    fn an_empty_history_stays_at_the_working_tree() {
        let mut s = scrubber(0);
        assert!(s.is_empty());
        s.older();
        assert_eq!(s.position(), Position::Working, "nothing to step back to");
        s.seek(3);
        assert_eq!(s.position(), Position::Working);
        assert_eq!(s.commit().map(|c| c.hash.as_str()), None);
        assert_eq!(s.fraction(), 1.0);
    }

    /// The handle's position is monotonic across every stop, and the working
    /// tree has its own place at the far right.
    ///
    /// Asserted as an ordering over the whole range rather than at a few
    /// points: a formula that is right at the ends and wrong in the middle
    /// would pass spot checks, and the middle is where a drag spends its
    /// time.
    #[test]
    fn the_handle_moves_monotonically_from_oldest_to_the_tree() {
        let mut s = scrubber(4);
        let mut seen = Vec::new();
        for i in (0..4).rev() {
            s.seek(i);
            seen.push(s.fraction());
        }
        s.home();
        seen.push(s.fraction());

        assert_eq!(
            seen.first().copied(),
            Some(0.0),
            "oldest sits at the left edge"
        );
        assert_eq!(
            seen.last().copied(),
            Some(1.0),
            "the tree sits at the right"
        );
        for pair in seen.windows(2) {
            assert!(pair[1] > pair[0], "the handle went backwards: {seen:?}");
        }
        // HEAD and the working tree are DIFFERENT stops, so the user can see
        // which of the two they are looking at.
        s.seek(0);
        assert!(s.fraction() < 1.0, "HEAD must not sit on top of the tree");
    }

    #[test]
    fn around_lists_the_cursor_then_the_steps_a_held_key_reaches() {
        let mut s = scrubber(8);
        let hashes = |s: &Scrubber| -> Vec<String> {
            s.around().into_iter().map(|c| c.hash.clone()).collect()
        };
        // From the working tree, HEAD is next, then further back.
        assert_eq!(hashes(&s), ["c0", "c1", "c2", "c3"]);
        s.seek(3);
        assert_eq!(hashes(&s), ["c3", "c4", "c2", "c5", "c6", "c7"]);
        // Near the oldest end nothing is listed past it.
        s.seek(7);
        assert_eq!(hashes(&s), ["c7", "c6"]);
        assert!(Scrubber::new(Vec::new()).around().is_empty());
    }
}
