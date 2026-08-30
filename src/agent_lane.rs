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
}

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

/// The hash of `path`'s current content, or `None` when it cannot be read
/// (deleted, or a directory).
pub fn hash_file(path: &Path) -> Option<u64> {
    std::fs::read(path).ok().map(|b| content_hash(&b))
}

impl AgentLedger {
    pub fn new() -> Self {
        Self::default()
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
        let mut changed = false;
        for agent in working {
            let lane = self.lanes.entry(agent.clone()).or_default();
            match lane.get_mut(path) {
                Some(entry) => {
                    // A write that lands on content the user has already
                    // reviewed (an agent reverting its own change, say) is
                    // still a write, but it does not resurrect the row.
                    if entry.current_hash != hash {
                        entry.current_hash = hash;
                        entry.writes_since_review = entry.writes_since_review.saturating_add(1);
                        changed = true;
                    }
                    if shared && !entry.shared {
                        entry.shared = true;
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
                            shared,
                        },
                    );
                    changed = true;
                }
            }
        }
        changed
    }

    /// The rows in one agent's lane, most recently written first among
    /// unreviewed ones, then the reviewed remainder.
    pub fn lane(&self, agent: &str) -> Vec<&LaneFile> {
        let Some(lane) = self.lanes.get(agent) else {
            return Vec::new();
        };
        let mut rows: Vec<&LaneFile> = lane.values().collect();
        rows.sort_by_key(|f| (!f.unreviewed(), f.path.clone()));
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

    /// Mark every file in one lane reviewed, using `hash_of` to read each
    /// file's current content. Returns how many rows were cleared.
    pub fn mark_lane_reviewed(
        &mut self,
        agent: &str,
        hash_of: impl Fn(&Path) -> Option<u64>,
    ) -> usize {
        let Some(lane) = self.lanes.get_mut(agent) else {
            return 0;
        };
        let mut cleared = 0;
        for entry in lane.values_mut() {
            if !entry.unreviewed() {
                continue;
            }
            // A file that has vanished is cleared rather than left in the
            // queue forever: there is nothing left to review.
            let current = hash_of(&entry.path).unwrap_or(entry.current_hash);
            entry.reviewed_hash = Some(current);
            entry.current_hash = current;
            entry.writes_since_review = 0;
            cleared += 1;
        }
        cleared
    }

    /// Drop an agent's lane entirely (it is gone and the user dismissed it).
    pub fn forget(&mut self, agent: &str) -> bool {
        self.lanes.remove(agent).is_some()
    }

    /// Total unreviewed rows across every lane — the sidebar section badge.
    pub fn total_unreviewed(&self) -> usize {
        self.lanes
            .values()
            .flat_map(|lane| lane.values())
            .filter(|f| f.unreviewed())
            .count()
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

        // Reviewing it in one lane leaves the other's row standing: each
        // agent's queue is its own.
        led.mark_reviewed("claude", &p("/w/x.rs"), 7);
        assert_eq!(led.unreviewed_count("claude"), 0);
        assert_eq!(led.unreviewed_count("codex"), 1);
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
        let cleared =
            led.mark_lane_reviewed(
                "claude",
                |path| {
                    if path == p("/w/a.rs") { Some(42) } else { None }
                },
            );
        assert_eq!(cleared, 2);
        assert_eq!(led.unreviewed_count("claude"), 0);
        let rows = led.lane("claude");
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
        assert_eq!(hash_file(&f), Some(content_hash(b"hello")));
        assert_eq!(hash_file(&dir.path().join("nope.rs")), None);
    }
}
