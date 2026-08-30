//! Per-agent ledger of the files an agent changed while you were looking
//! elsewhere (#345).
//!
//! The daily pain with a coding agent is losing track of what it touched.
//! croft already owns the file watcher and the agent's pane status, so the
//! ledger is the join: while an agent pane is `Working`, every workspace
//! write is attributed to that agent, and each attributed file carries a
//! REVIEW BASELINE — the content as of the last time you marked it reviewed.
//! A file whose current content matches its baseline is not in the queue,
//! which is what makes "mark reviewed" mean something rather than merely
//! hiding a row.
//!
//! Two design points are load-bearing:
//!
//! * **A tie attributes to BOTH agents, and says so.** Two agents working at
//!   once cannot be told apart by a filesystem event, and picking one would
//!   be a guess presented as a fact. The row is flagged `shared`, and the
//!   reader sees why the same file sits in two lanes.
//! * **Attribution needs a working agent, not merely a seated one.** A quiet
//!   agent's pane is not a reason to blame it for your own save, so a write
//!   with no agent working is attributed to nobody and never enters a lane.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One file in one agent's lane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaneFile {
    pub path: PathBuf,
    /// Content hash when the user last marked it reviewed; `None` until the
    /// first review, which is what makes a newly-touched file unreviewed.
    pub reviewed_hash: Option<u64>,
    /// Content hash at the most recent attributed write.
    pub current_hash: u64,
    /// How many attributed writes have landed since the last review.
    pub writes_since_review: u32,
    /// Monotonic sequence of the most recent attributed write, so the lane
    /// can be ordered by RECENCY. Without it the ordering was by path,
    /// which put the file an agent touched an hour ago above the one it
    /// just wrote — the opposite of what a review queue is for.
    pub last_write_seq: u64,
    /// Another agent was also working when this landed, so the attribution
    /// is ambiguous and is shown as such rather than guessed.
    pub shared: bool,
}

impl LaneFile {
    /// Whether this row belongs in the review queue: the content has moved
    /// since the baseline (or there is no baseline yet).
    pub fn unreviewed(&self) -> bool {
        self.reviewed_hash != Some(self.current_hash)
    }
}

/// Every agent's ledger, keyed by agent name.
#[derive(Clone, Debug, Default)]
pub struct AgentLedger {
    lanes: BTreeMap<String, BTreeMap<PathBuf, LaneFile>>,
    /// Ticks once per recorded write, so every row can be ordered against
    /// every other regardless of lane.
    write_seq: u64,
    /// The file watcher overflowed at least once since the queue was last
    /// emptied, so every count is a LOWER BOUND: writes happened that never
    /// reached a lane. Carried here rather than only announced in a status
    /// line, because a transient line the next keystroke clears would leave
    /// the user believing a partial queue is a complete one — the one thing
    /// this module must never do.
    may_be_incomplete: bool,
}

/// Files above this are not hashed: the ledger runs on the frame loop
/// while an agent is working (exactly when writes are frequent), and a
/// checked-in fixture or generated asset would otherwise be read whole on
/// every touch. Such a file is recorded at a size-and-mtime stamp instead,
/// which still changes when it changes.
pub const MAX_HASH_BYTES: u64 = 4 * 1024 * 1024;

/// A stable content hash. FNV-1a over the bytes: the ledger only ever asks
/// "is this the same content I showed you", so a fast non-cryptographic
/// hash is the right tool, and it keeps the baseline to 8 bytes per file
/// rather than a copy of the content.
pub fn content_hash(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// What reading a file for its baseline produced.
///
/// "Gone" and "unreadable" must never collapse: clearing a row because the
/// file could not be read this instant marks content the user has NOT seen
/// as reviewed, and the state is unrecoverable — a later write landing on
/// the same hash is a no-op, so the unseen content stays clear forever.
/// That is precisely the promise this module exists to keep.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Baseline {
    /// The content hashed cleanly.
    Hash(u64),
    /// The path no longer exists: there is nothing left to review.
    Gone,
    /// It exists but could not be read (EMFILE under a build storm, a
    /// transient lock, EACCES after the agent chmod'd it). The row must
    /// stay in the queue.
    Unreadable,
}

/// The hash of `path`'s current content, distinguishing a vanished file
/// from one that merely could not be read right now.
pub fn read_baseline(path: &Path) -> Baseline {
    // One `metadata` call is cheaper than the read it guards, and it is
    // also how a too-large file is stamped rather than slurped.
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Baseline::Gone,
        Err(_) => return Baseline::Unreadable,
    };
    if meta.len() > MAX_HASH_BYTES {
        // With no mtime the stamp would be the LENGTH alone, and every
        // same-size write to a large file would be invisible with no
        // signal at all. Fall through to the honest read instead.
        if let Some(stamp) = stamp_hash(&meta) {
            return Baseline::Hash(stamp);
        }
    }
    match std::fs::read(path) {
        Ok(bytes) => Baseline::Hash(content_hash(&bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Baseline::Gone,
        Err(_) => Baseline::Unreadable,
    }
}

/// The stand-in hash for a file too large to read on the frame loop: its
/// length and mtime, or `None` when the mtime is unavailable — in which
/// case the caller reads the file rather than standing in with a
/// length-only stamp that would hide every same-size write. Weaker than
/// content addressing (a same-size write inside one mtime granule is
/// missed) but bounded, and the alternative is reading hundreds of
/// megabytes during a draw.
fn stamp_hash(meta: &std::fs::Metadata) -> Option<u64> {
    let mtime = meta.modified().ok()?;
    let since = mtime.duration_since(std::time::UNIX_EPOCH).ok()?;
    let mut bytes = meta.len().to_le_bytes().to_vec();
    bytes.extend_from_slice(&since.as_nanos().to_le_bytes());
    Some(content_hash(&bytes))
}

impl AgentLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that the watcher dropped events: every count from here on is
    /// a lower bound until the queue is next emptied.
    pub fn note_dropped_events(&mut self) {
        self.may_be_incomplete = true;
    }

    /// Whether the queue may be missing files the watcher never reported.
    pub fn may_be_incomplete(&self) -> bool {
        self.may_be_incomplete
    }

    /// Attribute one write of `path` (already hashed) to every agent in
    /// `working`. With no working agent the write belongs to the user and
    /// is dropped: attributing it would put your own edits in an agent's
    /// review queue.
    ///
    /// Returns whether any lane changed, so the caller can skip a repaint.
    pub fn record_write(&mut self, path: &Path, hash: u64, working: &[String]) -> bool {
        if working.is_empty() {
            return false;
        }
        let shared = working.len() > 1;
        self.write_seq = self.write_seq.saturating_add(1);
        let seq = self.write_seq;
        let mut changed = false;
        for agent in working {
            let lane = self.lanes.entry(agent.clone()).or_default();
            match lane.get_mut(path) {
                Some(entry) => {
                    // A write that lands on content the user has already
                    // reviewed (an agent reverting its own change, say) is
                    // still a write, but it does not resurrect the row —
                    // and it is not evidence of ambiguity either, so the
                    // `shared` promotion rides inside the same guard.
                    if entry.current_hash != hash {
                        entry.current_hash = hash;
                        entry.writes_since_review = entry.writes_since_review.saturating_add(1);
                        entry.last_write_seq = seq;
                        entry.shared |= shared;
                        changed = true;
                    }
                }
                None => {
                    lane.insert(
                        path.to_path_buf(),
                        LaneFile {
                            path: path.to_path_buf(),
                            reviewed_hash: None,
                            current_hash: hash,
                            writes_since_review: 1,
                            last_write_seq: seq,
                            shared,
                        },
                    );
                    changed = true;
                }
            }
        }
        changed
    }

    /// The rows in one agent's lane: everything awaiting review first, most
    /// recently written first within that group, then the reviewed
    /// remainder. Recency is what a review queue is ordered by — the file
    /// the agent just touched is the one you want at the top.
    pub fn lane(&self, agent: &str) -> Vec<&LaneFile> {
        let Some(lane) = self.lanes.get(agent) else {
            return Vec::new();
        };
        let mut rows: Vec<&LaneFile> = lane.values().collect();
        rows.sort_by(|a, b| {
            (
                !a.unreviewed(),
                std::cmp::Reverse(a.last_write_seq),
                &a.path,
            )
                .cmp(&(
                    !b.unreviewed(),
                    std::cmp::Reverse(b.last_write_seq),
                    &b.path,
                ))
        });
        rows
    }

    /// Every agent that has a lane, in name order.
    pub fn agents(&self) -> Vec<&str> {
        self.lanes.keys().map(String::as_str).collect()
    }

    /// How many files in `agent`'s lane still await review — the lane
    /// header's badge.
    pub fn unreviewed_count(&self, agent: &str) -> usize {
        self.lanes
            .get(agent)
            .map(|lane| lane.values().filter(|f| f.unreviewed()).count())
            .unwrap_or(0)
    }

    /// Whether any lane holds `path` unreviewed — the file tree's dot
    /// decoration.
    pub fn is_unreviewed(&self, path: &Path) -> bool {
        self.lanes
            .values()
            .any(|lane| lane.get(path).is_some_and(|f| f.unreviewed()))
    }

    /// Mark one file reviewed in one lane: the CURRENT content becomes the
    /// baseline, so a later write re-queues it. `current` is the content
    /// hash now, which may differ from the last attributed write if the
    /// user edited it themselves in between — reviewing means "I have seen
    /// what is on disk", not "I have seen what the agent last wrote".
    pub fn mark_reviewed(&mut self, agent: &str, path: &Path, current: u64) -> bool {
        let Some(lane) = self.lanes.get_mut(agent) else {
            return false;
        };
        let Some(entry) = lane.get_mut(path) else {
            return false;
        };
        entry.reviewed_hash = Some(current);
        entry.current_hash = current;
        entry.writes_since_review = 0;
        true
    }

    /// Mark every file in one lane reviewed, using `read` to see each
    /// file's current content.
    ///
    /// Returns `(reviewed, dropped)`: rows whose current content became the
    /// baseline, and rows removed because the file is gone. They are counted
    /// apart because telling the user a deleted file was "reviewed" claims
    /// they looked at something that is not there.
    pub fn mark_lane_reviewed(
        &mut self,
        agent: &str,
        read: impl Fn(&Path) -> Baseline,
    ) -> (usize, usize) {
        let Some(lane) = self.lanes.get_mut(agent) else {
            return (0, 0);
        };
        let mut cleared = 0;
        let mut gone: Vec<PathBuf> = Vec::new();
        for entry in lane.values_mut() {
            if !entry.unreviewed() {
                continue;
            }
            match read(&entry.path) {
                Baseline::Hash(current) => {
                    entry.reviewed_hash = Some(current);
                    entry.current_hash = current;
                    entry.writes_since_review = 0;
                    cleared += 1;
                }
                // Nothing left to review: the row goes rather than sitting
                // in the queue forever pointing at a file that is not there.
                Baseline::Gone => gone.push(entry.path.clone()),
                // Unreadable RIGHT NOW is not reviewed: leave it queued and
                // do not count it, so the status line's number is honest.
                Baseline::Unreadable => {}
            }
        }
        for path in &gone {
            lane.remove(path);
        }
        self.settle_if_empty();
        (cleared, gone.len())
    }

    /// An empty queue has nothing left to be wrong about, so a
    /// dropped-event window stops mattering once every row is reviewed.
    /// Anything still waiting keeps the doubt, since a dropped write may be
    /// among it.
    fn settle_if_empty(&mut self) {
        if self.may_be_incomplete && self.total_unreviewed() == 0 {
            self.may_be_incomplete = false;
        }
    }

    /// Drop `path` from every lane — the file is gone, so there is nothing
    /// left for anyone to review.
    pub fn forget_path(&mut self, path: &Path) -> bool {
        let mut any = false;
        for lane in self.lanes.values_mut() {
            any |= lane.remove(path).is_some();
        }
        if any {
            self.settle_if_empty();
        }
        any
    }

    /// Drop an agent's lane entirely (it is gone and the user dismissed it).
    pub fn forget(&mut self, agent: &str) -> bool {
        self.lanes.remove(agent).is_some()
    }

    /// Total unreviewed ROWS across every lane — the per-lane badges sum to
    /// this, so a file two agents both touched counts twice, once in each
    /// lane. For "how many files must the user look at", which is what a
    /// single global badge means, use [`Self::unreviewed_files`].
    ///
    /// Exposed for the deferred sidebar section that will render those
    /// badges; nothing in production reads it yet.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn total_unreviewed(&self) -> usize {
        self.lanes
            .values()
            .flat_map(|lane| lane.values())
            .filter(|f| f.unreviewed())
            .count()
    }

    /// Distinct FILES awaiting review across every lane. A shared row is one
    /// file to open, not two: a count that drops by two when the user
    /// reviews one file is telling them something untrue.
    pub fn unreviewed_files(&self) -> usize {
        self.lanes
            .values()
            .flat_map(|lane| lane.values())
            .filter(|f| f.unreviewed())
            .map(|f| &f.path)
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    /// The core ledger shape: a working agent's writes land in its lane
    /// unreviewed, marking reviewed clears them, and a later write
    /// re-queues the file.
    #[test]
    fn a_working_agents_writes_queue_for_review_and_clear_when_marked() {
        let mut led = AgentLedger::new();
        let claude = vec![String::from("claude")];

        assert!(led.record_write(&p("/w/src/a.rs"), 1, &claude));
        assert!(led.record_write(&p("/w/src/b.rs"), 2, &claude));
        assert_eq!(led.unreviewed_count("claude"), 2);
        assert_eq!(led.total_unreviewed(), 2);
        assert!(led.is_unreviewed(&p("/w/src/a.rs")));
        assert_eq!(led.agents(), vec!["claude"]);

        // Reviewing one clears just that row, and it leaves the lane's
        // ordering with the unreviewed file first.
        assert!(led.mark_reviewed("claude", &p("/w/src/a.rs"), 1));
        assert_eq!(led.unreviewed_count("claude"), 1);
        assert!(!led.is_unreviewed(&p("/w/src/a.rs")));
        let rows = led.lane("claude");
        assert_eq!(rows[0].path, p("/w/src/b.rs"), "unreviewed sorts first");
        assert!(rows[0].last_write_seq > 0);
        assert_eq!(rows[1].writes_since_review, 0);

        // A NEW write to the reviewed file re-queues it: the baseline is
        // content, not a dismissal.
        assert!(led.record_write(&p("/w/src/a.rs"), 99, &claude));
        assert!(led.is_unreviewed(&p("/w/src/a.rs")));
        assert_eq!(led.unreviewed_count("claude"), 2);

        // A write that restores the reviewed content does NOT re-queue it.
        led.mark_reviewed("claude", &p("/w/src/a.rs"), 99);
        assert!(!led.record_write(&p("/w/src/a.rs"), 99, &claude));
        assert!(!led.is_unreviewed(&p("/w/src/a.rs")));
    }

    /// A write while two agents are working is attributed to BOTH and
    /// flagged, rather than guessed at.
    #[test]
    fn a_tie_between_two_working_agents_is_shared_not_guessed() {
        let mut led = AgentLedger::new();
        let both = vec![String::from("claude"), String::from("codex")];
        led.record_write(&p("/w/x.rs"), 7, &both);

        assert_eq!(led.agents(), vec!["claude", "codex"]);
        assert!(led.lane("claude")[0].shared);
        assert!(led.lane("codex")[0].shared);
        assert_eq!(led.total_unreviewed(), 2, "the file sits in both lanes");
        assert_eq!(
            led.unreviewed_files(),
            1,
            "but it is ONE file to open: a chip that drops by two when the \
             user reviews one file is telling them something untrue"
        );

        // Reviewing it in one lane leaves the other's row standing: each
        // agent's queue is its own.
        led.mark_reviewed("claude", &p("/w/x.rs"), 7);
        assert_eq!(led.unreviewed_count("claude"), 0);
        assert_eq!(led.unreviewed_count("codex"), 1);
        assert_eq!(led.total_unreviewed(), 1, "one row left");
        assert_eq!(
            led.unreviewed_files(),
            1,
            "the chip holds at one file until EVERY lane holding it clears"
        );
        assert!(
            led.is_unreviewed(&p("/w/x.rs")),
            "the tree dot stays while ANY lane has it unreviewed"
        );

        // A later solo write clears the shared flag for nobody: the history
        // of ambiguity stands, but the new write is attributed alone.
        led.record_write(&p("/w/y.rs"), 8, &[String::from("codex")]);
        assert!(
            !led.lane("codex")
                .iter()
                .find(|f| f.path == p("/w/y.rs"))
                .unwrap()
                .shared
        );
    }

    /// With no agent working, a write is the USER's and never enters a lane
    /// — otherwise your own saves fill an agent's review queue.
    #[test]
    fn a_write_with_no_working_agent_belongs_to_nobody() {
        let mut led = AgentLedger::new();
        assert!(!led.record_write(&p("/w/mine.rs"), 5, &[]));
        assert!(led.agents().is_empty());
        assert_eq!(led.total_unreviewed(), 0);
        assert!(!led.is_unreviewed(&p("/w/mine.rs")));
    }

    /// "Mark all reviewed" snapshots what is on DISK now, not what the
    /// agent last wrote, and a vanished file is cleared rather than stuck.
    #[test]
    fn marking_a_whole_lane_baselines_the_current_disk_content() {
        let mut led = AgentLedger::new();
        let agent = vec![String::from("claude")];
        led.record_write(&p("/w/a.rs"), 1, &agent);
        led.record_write(&p("/w/gone.rs"), 2, &agent);
        assert_eq!(led.unreviewed_count("claude"), 2);

        // `a.rs` moved on disk since the agent's write (the user edited it);
        // `gone.rs` no longer exists.
        let (cleared, dropped) = led.mark_lane_reviewed("claude", |path| {
            if path == p("/w/a.rs") {
                Baseline::Hash(42)
            } else {
                Baseline::Gone
            }
        });
        assert_eq!(
            (cleared, dropped),
            (1, 1),
            "one file was reviewed and one was gone: reporting two as \
             reviewed would claim the user looked at a file that is not there"
        );
        assert_eq!(led.unreviewed_count("claude"), 0);
        let rows = led.lane("claude");
        assert!(
            !rows.iter().any(|f| f.path == p("/w/gone.rs")),
            "a vanished file leaves the lane rather than lingering reviewed"
        );
        let a = rows.iter().find(|f| f.path == p("/w/a.rs")).unwrap();
        assert_eq!(
            a.reviewed_hash,
            Some(42),
            "the baseline is what is on disk now, not the agent's last write"
        );

        // A subsequent agent write against the disk content re-queues it.
        led.record_write(&p("/w/a.rs"), 43, &agent);
        assert_eq!(led.unreviewed_count("claude"), 1);

        assert!(led.forget("claude"));
        assert!(led.agents().is_empty());
        assert!(!led.forget("claude"));
    }

    /// The queue is ordered by RECENCY, not by path: the file the agent
    /// just wrote belongs at the top even when its name sorts last.
    #[test]
    fn the_lane_lists_the_most_recently_written_file_first() {
        let mut led = AgentLedger::new();
        let a = vec![String::from("claude")];
        led.record_write(&p("/w/a.rs"), 1, &a);
        led.record_write(&p("/w/z.rs"), 2, &a);
        assert_eq!(
            led.lane("claude")[0].path,
            p("/w/z.rs"),
            "z.rs was written last, so it heads the queue despite the name"
        );

        // A new write to the older file moves it back to the top.
        led.record_write(&p("/w/a.rs"), 3, &a);
        assert_eq!(led.lane("claude")[0].path, p("/w/a.rs"));

        // Reviewed rows sink below every unreviewed one regardless of when
        // they were written.
        led.mark_reviewed("claude", &p("/w/a.rs"), 3);
        assert_eq!(led.lane("claude")[0].path, p("/w/z.rs"));
        assert_eq!(led.lane("claude")[1].path, p("/w/a.rs"));
    }

    /// A dropped-events window makes every count a LOWER BOUND, and says
    /// so until the queue is empty — a warning the next keystroke erases
    /// would leave the user believing a partial queue was complete.
    #[test]
    fn a_dropped_event_window_marks_the_queue_a_lower_bound() {
        let mut led = AgentLedger::new();
        let a = vec![String::from("claude")];
        assert!(!led.may_be_incomplete());

        led.record_write(&p("/w/a.rs"), 1, &a);
        led.note_dropped_events();
        assert!(
            led.may_be_incomplete(),
            "the watcher overflowed: counts are a floor"
        );

        // Reviewing SOME of the queue keeps the doubt: a dropped write may
        // be among what is left.
        led.record_write(&p("/w/b.rs"), 2, &a);
        led.mark_reviewed("claude", &p("/w/a.rs"), 1);
        assert!(led.may_be_incomplete());

        // Emptying it settles the question: nothing is left to be wrong.
        let (reviewed, _) = led.mark_lane_reviewed("claude", |_| Baseline::Hash(2));
        assert_eq!(reviewed, 1);
        assert!(
            !led.may_be_incomplete(),
            "an empty queue has nothing left to be a lower bound of"
        );
    }

    /// A file that cannot be READ right now is not reviewed. Clearing it
    /// would mark content the user never saw as seen, and unrecoverably: a
    /// later write landing on the same hash is a no-op, so the row would
    /// stay clear forever. This is the promise the module exists to keep.
    #[test]
    fn an_unreadable_file_stays_in_the_queue_rather_than_being_cleared() {
        let mut led = AgentLedger::new();
        led.record_write(&p("/w/locked.rs"), 100, &[String::from("claude")]);
        assert_eq!(led.unreviewed_count("claude"), 1);

        // The read fails this instant, whatever the disk holds.
        let (cleared, dropped) = led.mark_lane_reviewed("claude", |_| Baseline::Unreadable);
        assert_eq!(
            (cleared, dropped),
            (0, 0),
            "an unreadable file is neither reviewed nor gone"
        );
        assert_eq!(
            led.unreviewed_count("claude"),
            1,
            "it stays queued: 'could not read' is not 'reviewed'"
        );

        // And it is still recoverable once the read succeeds.
        assert_eq!(
            led.mark_lane_reviewed("claude", |_| Baseline::Hash(555)),
            (1, 0)
        );
        assert_eq!(led.unreviewed_count("claude"), 0);
    }

    /// A file too large to read on the frame loop is stamped rather than
    /// slurped, and a vanished file is distinguished from an unreadable one.
    #[test]
    fn baselines_bound_the_read_and_separate_gone_from_unreadable() {
        let dir = tempfile::TempDir::new().unwrap();
        let small = dir.path().join("small.rs");
        std::fs::write(&small, b"hello").unwrap();
        assert_eq!(
            read_baseline(&small),
            Baseline::Hash(content_hash(b"hello")),
            "a small file is content-addressed"
        );
        assert_eq!(read_baseline(&dir.path().join("nope.rs")), Baseline::Gone);

        let big = dir.path().join("big.bin");
        std::fs::write(&big, vec![0u8; (MAX_HASH_BYTES + 1) as usize]).unwrap();
        let stamped = read_baseline(&big);
        assert!(matches!(stamped, Baseline::Hash(_)));
        assert_ne!(
            stamped,
            Baseline::Hash(content_hash(b"")),
            "past the cap the stamp stands in for the content, never reading it"
        );
        // The stamp still moves when the file does.
        std::fs::write(&big, vec![1u8; (MAX_HASH_BYTES + 2) as usize]).unwrap();
        assert_ne!(read_baseline(&big), stamped);
    }

    /// The hash is content-addressed and stable, which is what makes a
    /// baseline comparison meaningful.
    #[test]
    fn content_hashing_is_stable_and_distinguishes_content() {
        assert_eq!(content_hash(b"fn main() {}"), content_hash(b"fn main() {}"));
        assert_ne!(
            content_hash(b"fn main() {}"),
            content_hash(b"fn main() { }")
        );
        assert_ne!(content_hash(b""), content_hash(b"\n"));

        let dir = tempfile::TempDir::new().unwrap();
        let f = dir.path().join("x.rs");
        std::fs::write(&f, b"hello").unwrap();
        assert_eq!(read_baseline(&f), Baseline::Hash(content_hash(b"hello")));
        assert_eq!(read_baseline(&dir.path().join("nope.rs")), Baseline::Gone);
    }
}
