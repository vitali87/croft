//! The pilot state's turn books: a fresh seat, a file's live lines, whether a turn is
//! running, arming a turn and undoing a staged one, what the navigator last saw of a file,
//! and the bounded lock the App's threads take on the state.

use super::*;

/// What [`PairState::stage_turn`] overwrote, so a send that never leaves
/// the process can be undone ([`PairState::unstage_turn`]).
pub(crate) struct StagedTurn {
    file: String,
    /// The previous look at `file`; also the yield-diff baseline the
    /// caller reads before sending.
    pub(crate) prior_seen: Option<String>,
    prior_comment_only: bool,
    prior_target: Option<String>,
    /// Which staging this undo belongs to: a rollback that lands after a
    /// NEWER turn has staged (its send failed while the pilot mutex was
    /// wedged, so the unstage waited on a thread) is stale and must not
    /// stomp the live turn's books.
    stage_gen: u64,
}

impl PairState {
    /// A fresh seat over `session`: no turn in flight, nothing streamed yet.
    /// `events` seats the pilot inside croft (its voice becomes
    /// [`crate::pair_host::PairEvent`]s); None keeps stdio.
    pub(crate) fn new(
        session: CollabSession,
        events: Option<Sender<crate::pair_host::PairEvent>>,
    ) -> Self {
        Self {
            session,
            region: None,
            discarding: false,
            cancelled: false,
            can_interrupt: false,
            turn_active: false,
            target_file: None,
            pending_note: None,
            stage_gen: 0,
            notes: Vec::new(),
            next_note_id: 1,
            note_in_flight: None,
            events,
            comment_only: false,
            pending_caret: None,
            last_seen: std::collections::HashMap::new(),
        }
    }

    /// The live replica text of `file`, split into lines (None = not live).
    pub(crate) fn doc_lines(&self, file: &str) -> Option<Vec<String>> {
        self.session
            .doc_text(file)
            .map(|t| t.split('\n').map(String::from).collect())
    }

    /// True between sending a user turn and seeing its result. A new turn
    /// must not be sent while this holds: the shared `comment_only` flag and
    /// the target file would be clobbered mid-stream, defeating the
    /// host-enforced comment-only guarantee on a yield.
    pub(crate) fn turn_active(&self) -> bool {
        self.turn_active
    }

    /// Arm a host turn targeting `file`: mode, target, and the last-seen
    /// bookkeeping the yield diff reads. Returns what the navigator
    /// previously saw of the file (None = first look).
    pub(crate) fn begin_turn(
        &mut self,
        file: &str,
        content: &str,
        comment_only: bool,
    ) -> Option<String> {
        self.comment_only = comment_only;
        self.target_file = Some(file.to_string());
        self.session.request_file(file);
        // Notes deliberately survive the new turn: they are open comment
        // boxes, and only the driver closes them (Ignore / Clear All).
        self.last_seen.insert(file.to_string(), content.to_string())
    }

    /// [`Self::begin_turn`] plus everything needed to undo it: the caller
    /// stages the look BEFORE the send, and a failed send must not keep it
    /// (`last_seen` is the yield-diff baseline — a retry would diff the
    /// file against itself — and a latched `comment_only` would silently
    /// edit-suppress the next successful turn).
    pub(crate) fn stage_turn(
        &mut self,
        file: &str,
        content: &str,
        comment_only: bool,
    ) -> StagedTurn {
        let prior_comment_only = self.comment_only;
        let prior_target = self.target_file.clone();
        let prior_seen = self.begin_turn(file, content, comment_only);
        self.stage_gen += 1;
        StagedTurn {
            file: file.to_string(),
            prior_seen,
            prior_comment_only,
            prior_target,
            stage_gen: self.stage_gen,
        }
    }

    /// Put back what [`Self::stage_turn`] overwrote (the send never left).
    /// A no-op when a NEWER turn has staged since: this rollback ran late
    /// (see [`StagedTurn::stage_gen`]) and its books are stale.
    pub(crate) fn unstage_turn(&mut self, staged: StagedTurn) {
        if self.stage_gen != staged.stage_gen {
            return;
        }
        // An applied undo re-opens the generation beneath it, so undo
        // records unwind LIFO all the way back to the original state
        // (turn A fails parked, turn B stages over the residue and fails:
        // B's undo restores A's books AND re-admits A's undo).
        self.stage_gen = staged.stage_gen - 1;
        self.comment_only = staged.prior_comment_only;
        self.target_file = staged.prior_target;
        match staged.prior_seen {
            Some(old) => {
                self.last_seen.insert(staged.file, old);
            }
            None => {
                self.last_seen.remove(&staged.file);
            }
        }
    }

    /// What the navigator last saw of `file` (None = it never looked).
    /// The proactive trigger diffs the live buffer against this.
    pub(crate) fn last_seen_of(&self, file: &str) -> Option<String> {
        self.last_seen.get(file).cloned()
    }
}

/// Bounded lock for anything that DECIDES or ACTS on the pilot's state
/// from the App's threads (is a turn running? send this ask; land this
/// note): transient contention is normal — the pilot's pump holds the
/// mutex for microseconds every 25ms poll — so an instant try_lock
/// spuriously refuses keypresses and misreports a quiet seat as busy. But
/// an unbounded lock joins the deadlock cycle when a pilot thread is
/// wedged in a blocking relay write. Retry briefly, then give up.
/// Per-frame render reads never wait at all (see `PairHost::caret_sites`):
/// they have a next frame, a decision does not.
pub(crate) fn lock_briefly(
    state: &Mutex<PairState>,
) -> Option<std::sync::MutexGuard<'_, PairState>> {
    let deadline = std::time::Instant::now() + Duration::from_millis(100);
    loop {
        if let Ok(guard) = state.try_lock() {
            return Some(guard);
        }
        if std::time::Instant::now() > deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}
