//! The rewind buffer: the last few minutes of terminal output, in memory (#357).
//!
//! Local history covers saves. Nothing covers "what did that command print
//! before I cleared the screen", because the bytes are gone the moment the
//! screen scrolls. This keeps them — bounded, so a runaway `yes` cannot eat
//! the machine — so a scrubber can replay the session backwards.
//!
//! # Why frames and keyframes rather than a byte log
//!
//! Replaying a terminal means replaying its *state*, and a terminal's state
//! is not a function of the last N bytes: a single escape sequence early in
//! the stream can set a scroll region that changes how everything after it
//! renders. Replaying from an arbitrary offset therefore produces a screen
//! that never existed.
//!
//! So the buffer stores two things. **Frames** are the raw output chunks with
//! their timestamps. **Keyframes** are full screen snapshots taken every so
//! often; rewinding to time `t` means finding the newest keyframe at or before
//! `t` and replaying only the frames between it and `t`. That bounds the
//! replay work and makes the reconstruction exact rather than approximate.
//!
//! # The cap is on bytes, not on frames
//!
//! The obvious ring buffer holds N frames, which is the wrong bound: a frame
//! is anything from one byte to a megabyte, so a frame count says nothing
//! about memory. `yes` produces millions of tiny frames and a `cat` of a
//! large file produces a few enormous ones, and a frame-capped buffer is
//! either useless for the first or unbounded for the second. The cap here is
//! the summed payload length, and eviction is by age until the total fits.
//!
//! A single frame larger than the whole budget is truncated rather than
//! dropped: losing the tail of one enormous write is recoverable, while
//! silently discarding it would leave the replay showing a gap it cannot
//! explain.
//!
//! Keyframes count against the same cap (#694). They are whole screens, one
//! per [`KEYFRAME_INTERVAL_BYTES`] of output, and a budget that covered only
//! frames let hundreds of them sit on top of it.
//!
//! # The cap is shared, not per pane
//!
//! Every pane's buffer draws on one [`RewindBudget`], split evenly between
//! the panes that are open, so the total is a single figure however many
//! panes there are. It is the `terminal_rewind_mb` setting, with a smaller
//! default on a remote host ([`configured_budget_bytes`]).
//!
//! # Why this module never renders a screen itself
//!
//! [`RewindBuffer::push`] takes bytes and returns a bool asking for a
//! keyframe; it never reaches for a terminal. That is deliberate, and it is a
//! locking constraint rather than a taste one. The pane's shared state has a
//! documented lock order — `term` → `clock` → `line_times` — and the reader
//! thread calls `push` from inside the section where it already holds `term`.
//! A buffer that rendered its own keyframe would have to take `term` again
//! there, which is the deadlock. So the buffer asks, and the caller — which
//! already holds the lock, or takes it later during a render — supplies the
//! screen through [`RewindBuffer::push_keyframe`].

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, Weak};

/// The rewind memory ALL panes share on a local machine, in bytes (#694).
///
/// A process-wide figure rather than a per-pane one. The per-pane 64 MiB
/// this replaced made the ceiling a function of how many panes were open —
/// ten busy panes was 640 MiB before a single keyframe — and nothing replays
/// the buffer yet, so every byte of it was pure cost. The budget is split
/// evenly across live panes by [`RewindBudget`].
pub const DEFAULT_BUDGET_BYTES: usize = 256 * 1024 * 1024;

/// The shared budget on a remote host (an SSH session), in bytes.
///
/// Remotes are where croft was OOM-killed: small VPSes running croft next
/// to rust-analyzer and builds, where the whole machine may have 8 GB.
pub const REMOTE_DEFAULT_BUDGET_BYTES: usize = 64 * 1024 * 1024;

/// The largest `terminal_rewind_mb` honoured. A typo of a few extra zeros
/// must not hand the rewind buffer the machine.
pub const MAX_BUDGET_MB: usize = 4096;

/// True when croft runs inside an SSH login, which is how a remote croft is
/// launched (`croft remote` execs it over ssh).
pub fn running_over_ssh() -> bool {
    std::env::var_os("SSH_CONNECTION").is_some()
        || std::env::var_os("SSH_TTY").is_some()
        || std::env::var_os("SSH_CLIENT").is_some()
}

/// The shared budget in bytes for a `terminal_rewind_mb` setting.
///
/// Unset picks the default for where croft runs; `0` turns rewind off; any
/// other value is megabytes, clamped to [`MAX_BUDGET_MB`].
pub fn configured_budget_bytes(setting_mb: Option<usize>, remote: bool) -> usize {
    match setting_mb {
        None if remote => REMOTE_DEFAULT_BUDGET_BYTES,
        None => DEFAULT_BUDGET_BYTES,
        Some(mb) => mb.min(MAX_BUDGET_MB) * 1024 * 1024,
    }
}

/// How often a keyframe is taken, in bytes of output between snapshots.
///
/// Time would be the intuitive interval, but it is the wrong axis: an idle
/// session would accumulate keyframes of an unchanging screen, while a flood
/// would go a long way between them and make the replay after each one
/// expensive. Bytes track the work the replay will actually have to do.
pub const KEYFRAME_INTERVAL_BYTES: usize = 256 * 1024;

/// One chunk of terminal output, as it arrived.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    /// Milliseconds since the buffer started, from a MONOTONIC source.
    ///
    /// The pane passes `Instant::elapsed()` rather than a wall clock for a
    /// reason that binds any other caller too: every read here — `span_ms`,
    /// `replay_from`, the orphan-keyframe sweep — compares these values, so a
    /// clock that steps backwards puts a frame before its predecessors and
    /// out of the range a scrubber would ask for. `span_ms` covers such a
    /// frame rather than reporting an inverted range, but the ordering is
    /// still the caller's to keep.
    pub at_ms: u64,
    /// Position in the buffer's own recording order.
    ///
    /// `at_ms` cannot order records on its own: it comes from a millisecond
    /// clock, and a burst of output produces many records sharing a value.
    /// The sweep below has to know whether a frame recorded AFTER a keyframe
    /// was evicted, which a tie makes unanswerable. This counter is total.
    seq: u64,
    pub data: Vec<u8>,
}

/// A full screen snapshot, so replay never has to start from the beginning.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keyframe {
    pub at_ms: u64,
    /// Position in the recording order, from the same counter as [`Frame`].
    seq: u64,
    /// The rendered screen at `at_ms`, one entry per row.
    pub screen: Vec<String>,
    /// What this snapshot costs against the budget: [`keyframe_cost`] of
    /// `screen`, computed once so eviction does not re-walk every row.
    cost: usize,
}

/// A keyframe's charge against the byte budget: each row's text plus the
/// `String` header that holds it.
///
/// Counted because a keyframe is a whole screen, and one is asked for every
/// [`KEYFRAME_INTERVAL_BYTES`] of output. Leaving them out of the cap (as the
/// first version did) let ~256 of them ride on top of a full frame budget.
fn keyframe_cost(screen: &[String]) -> usize {
    screen
        .iter()
        .map(|row| row.len() + std::mem::size_of::<String>())
        .sum()
}

/// The session's recent output, bounded by total bytes held.
#[derive(Debug)]
pub struct RewindBuffer {
    frames: VecDeque<Frame>,
    keyframes: VecDeque<Keyframe>,
    /// Summed `data.len()` of every frame held plus the [`keyframe_cost`] of
    /// every keyframe — the quantity actually capped.
    bytes: usize,
    capacity: usize,
    /// Bytes seen since the last keyframe, driving the next snapshot.
    since_keyframe: usize,
    /// Hands out [`Frame::seq`] / [`Keyframe::seq`]. Shared by both so the
    /// two interleave in one order rather than two.
    next_seq: u64,
}

impl RewindBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            frames: VecDeque::new(),
            keyframes: VecDeque::new(),
            bytes: 0,
            capacity,
            since_keyframe: 0,
            next_seq: 0,
        }
    }

    /// Record one chunk of output. Returns `true` when the caller should take
    /// a keyframe — the buffer cannot render a screen itself, so it asks.
    pub fn push(&mut self, at_ms: u64, data: &[u8]) -> bool {
        if self.capacity == 0 {
            return false;
        }
        // An empty write records nothing. `bytes` sums payload lengths and
        // `evict` runs while `bytes > capacity`, so a zero-length frame adds
        // an entry while adding nothing to the figure that would evict it —
        // repeated empty pushes would grow `frames` without bound, in the one
        // structure here whose purpose is to stay bounded. The reader cannot
        // emit one today (`Ok(0)` ends its loop), but this is public and the
        // replay half will add callers.
        if data.is_empty() {
            return false;
        }
        // One frame bigger than the whole budget is truncated to the budget
        // rather than dropped: the replay then shows a shortened write, which
        // is explicable, instead of a hole that looks like lost output.
        let data = if data.len() > self.capacity {
            &data[data.len() - self.capacity..]
        } else {
            data
        };
        self.bytes += data.len();
        let seq = self.take_seq();
        self.frames.push_back(Frame {
            at_ms,
            seq,
            data: data.to_vec(),
        });
        self.evict();
        // Counts RETAINED bytes: a write truncated to the budget, or one
        // evicted moments later, contributes what the buffer actually holds.
        self.since_keyframe += data.len();
        if self.since_keyframe >= KEYFRAME_INTERVAL_BYTES {
            // Reduced, not zeroed. A single write spanning several intervals
            // would otherwise ask once and discard the surplus, so the next
            // keyframe would come a full interval after a write that had
            // already earned three.
            self.since_keyframe -= KEYFRAME_INTERVAL_BYTES;
            return true;
        }
        false
    }

    /// Store a screen snapshot the caller rendered after [`push`] asked for one.
    ///
    /// The snapshot counts toward the cap like output does, so storing one
    /// can evict the oldest frames. A snapshot bigger than the whole budget
    /// is not stored: making room for it would evict every frame, and a
    /// start point with nothing after it to replay is worth nothing.
    pub fn push_keyframe(&mut self, at_ms: u64, screen: Vec<String>) {
        let cost = keyframe_cost(&screen);
        if self.capacity == 0 || cost > self.capacity {
            return;
        }
        let seq = self.take_seq();
        self.bytes += cost;
        self.keyframes.push_back(Keyframe {
            at_ms,
            seq,
            screen,
            cost,
        });
        // `evict` ends with the orphan sweep, so this both makes room and
        // collapses keyframes taken before any output.
        self.evict();
    }

    /// Change the cap, evicting at once if the buffer now holds too much.
    ///
    /// [`RewindBudget`] calls this when a pane opens or closes, so the shared
    /// budget is re-split without waiting for each pane's next write. A
    /// capacity of 0 empties the buffer entirely.
    pub fn set_capacity(&mut self, capacity: usize) {
        self.capacity = capacity;
        if capacity == 0 {
            self.frames.clear();
            self.keyframes.clear();
            self.bytes = 0;
            self.since_keyframe = 0;
            return;
        }
        self.evict();
    }

    /// The cap currently in force.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    fn take_seq(&mut self) -> u64 {
        let s = self.next_seq;
        self.next_seq += 1;
        s
    }

    /// Drop the oldest records until everything held fits the cap.
    ///
    /// Frames go first, oldest first, and each one can orphan keyframes that
    /// then go with it. The newest frame is kept while keyframes remain: once
    /// only one frame is left, what still exceeds the cap is keyframes, and
    /// they go oldest first before that last frame does. The last frame goes
    /// only when the cap itself shrank below it ([`Self::set_capacity`]).
    fn evict(&mut self) {
        while self.bytes > self.capacity {
            if self.frames.len() > 1 {
                if let Some(f) = self.frames.pop_front() {
                    self.bytes -= f.data.len();
                }
                self.drop_orphan_keyframes();
            } else if self.pop_keyframe_front() {
                // Loop again: the remaining keyframes may still not fit.
            } else if let Some(f) = self.frames.pop_front() {
                self.bytes -= f.data.len();
            } else {
                // Unreachable while `bytes` is the sum of what is held, but
                // a `while` on a counter that a future change could desync
                // is worth ending rather than spinning.
                self.bytes = 0;
                break;
            }
        }
        self.drop_orphan_keyframes();
    }

    /// Drop the oldest keyframe and its cost; false when there was none.
    fn pop_keyframe_front(&mut self) -> bool {
        match self.keyframes.pop_front() {
            Some(k) => {
                self.bytes -= k.cost;
                true
            }
            None => false,
        }
    }

    /// Discard keyframes that can no longer start an exact replay.
    ///
    /// A keyframe is a valid start point only if every frame between it and
    /// the target still survives. Once eviction removes output that followed
    /// a keyframe, replaying from it and applying what remains renders a
    /// screen that never existed — the frames that bridged the gap are gone.
    ///
    /// So a keyframe strictly OLDER than the oldest surviving frame is
    /// dropped, including the last one. An earlier version kept the final
    /// keyframe unconditionally, on the theory that a scrub to the very
    /// start needs something to replay from; that was wrong in the way that
    /// matters, because the thing it kept was precisely a start point with a
    /// hole after it. Having no keyframe is honest — [`replay_from`] then
    /// replays from a blank screen, which is slower and correct.
    fn drop_orphan_keyframes(&mut self) {
        let Some(oldest_seq) = self.frames.front().map(|f| f.seq) else {
            // No frames at all. This is NOT the orphan case: a keyframe
            // taken before any output has arrived is a valid start point for
            // everything that arrives next, and clearing here would drop the
            // screen a fresh pane replays from. Only the newest is worth
            // keeping; the older ones describe screens no surviving frame
            // can reach.
            //
            // Note this arm is NOT reached by frames "ageing out": eviction
            // is byte-capped, not age-based, and stops as soon as the budget
            // is met, while a pushed write is truncated to at most the
            // capacity — so at any nonzero capacity the just-pushed frame
            // always survives and a buffer that has held frames never
            // returns to empty. Nor is it reached by a shrinking cap: `evict`
            // drops every keyframe before it drops the last frame. The one
            // reachable state is before the first output (a zero capacity
            // stores no keyframes at all).
            while self.keyframes.len() > 1 {
                self.pop_keyframe_front();
            }
            return;
        };
        // Keyed on the SEQUENCE, not the timestamp. A keyframe is a valid
        // start point exactly when every frame recorded after it survives —
        // that is, when nothing between it and the oldest surviving frame was
        // evicted. `at_ms` cannot answer that: it cannot tell a keyframe that
        // predates the frames because output was EVICTED from one that
        // predates them because output had not yet ARRIVED, and a burst of
        // same-millisecond records makes even the ordering ambiguous. The
        // counter is total and increments once per record, so the frame
        // immediately after a keyframe has exactly `seq + 1`.
        while self.keyframes.len() > 1 && self.keyframes[1].seq < oldest_seq {
            self.pop_keyframe_front();
        }
        if self
            .keyframes
            .front()
            .is_some_and(|k| k.seq + 1 < oldest_seq)
        {
            self.pop_keyframe_front();
        }
    }

    /// Bytes currently held, keyframes included. Never exceeds the capacity.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Retained keyframes. Test-only: production reads keyframes through
    /// [`Self::replay_from`], and an unconditional `pub fn` with no caller
    /// would fail the build under `-D warnings` (which implies dead_code).
    #[cfg(test)]
    pub fn keyframe_count(&self) -> usize {
        self.keyframes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// The span the buffer can rewind over, as `(oldest, newest)` in ms.
    ///
    /// Taken as min/max rather than front/back. The pane feeds a monotonic
    /// clock so the two agree there, but `push` is public: a caller using a
    /// wall clock that stepped backwards would otherwise get an INVERTED
    /// span, and a scrubber sizing its timeline from `end - start` on that
    /// would underflow rather than show a short range.
    pub fn span_ms(&self) -> Option<(u64, u64)> {
        let lo = self.frames.iter().map(|f| f.at_ms).min()?;
        let hi = self.frames.iter().map(|f| f.at_ms).max()?;
        Some((lo, hi))
    }

    /// The replay needed to show the screen at `at_ms`: the newest keyframe at
    /// or before it, plus every frame from that keyframe up to and including
    /// `at_ms`.
    ///
    /// Returns no keyframe when none precedes the target — the caller then
    /// replays from a blank screen, which is correct for a buffer whose
    /// keyframes have aged out from under it.
    pub fn replay_from(&self, at_ms: u64) -> (Option<&Keyframe>, Vec<&Frame>) {
        let kf = self.keyframes.iter().rev().find(|k| k.at_ms <= at_ms);
        let start = kf.map(|k| k.at_ms).unwrap_or(0);
        let frames = self
            .frames
            .iter()
            // `>=` on the start: a frame landing in the same millisecond as
            // the keyframe is NOT already in it. The keyframe is rendered
            // from the frames strictly before it, so excluding the tie would
            // drop that output from every replay through this keyframe.
            .filter(|f| f.at_ms >= start && f.at_ms <= at_ms)
            .collect();
        (kf, frames)
    }
}

/// The rewind memory every pane shares, split evenly between live panes.
///
/// Each pane's buffer is capped at `total / live panes`. Opening a pane
/// shrinks everyone's share and trims the buffers already over it BEFORE the
/// new pane records anything, and closing one grows the shares back, so the
/// sum held stays under `total` however many panes come and go. An even
/// split rather than a first-come pool: a pool lets one flooding pane take
/// the whole budget, and a quiet pane would then keep nothing.
///
/// Lock order is budget → buffer, one buffer at a time. The reader thread
/// holds a buffer (under `term`) and never takes the budget, so a rebalance
/// only ever waits on a push, never deadlocks with one.
#[derive(Debug)]
pub struct RewindBudget {
    inner: Mutex<BudgetState>,
}

#[derive(Debug)]
struct BudgetState {
    total: usize,
    panes: Vec<Weak<Mutex<RewindBuffer>>>,
}

impl RewindBudget {
    pub const fn new(total: usize) -> Self {
        Self {
            inner: Mutex::new(BudgetState {
                total,
                panes: Vec::new(),
            }),
        }
    }

    /// A new pane's buffer, sized to its share. Every other pane is trimmed
    /// to the smaller share before this returns.
    pub fn register(&self) -> Arc<Mutex<RewindBuffer>> {
        let mut st = self.lock();
        let buf = Arc::new(Mutex::new(RewindBuffer::new(0)));
        st.panes.push(Arc::downgrade(&buf));
        Self::rebalance(&mut st);
        buf
    }

    /// Return a closing pane's share to the others and free its buffer now,
    /// even if another handle to it (the reader thread's) outlives the call.
    pub fn release(&self, buf: &Arc<Mutex<RewindBuffer>>) {
        let mut st = self.lock();
        let target = Arc::downgrade(buf);
        st.panes.retain(|w| !w.ptr_eq(&target));
        lock_buffer(buf).set_capacity(0);
        Self::rebalance(&mut st);
    }

    /// Change the shared total (a settings edit) and re-split it.
    pub fn set_total(&self, total: usize) {
        let mut st = self.lock();
        st.total = total;
        Self::rebalance(&mut st);
    }

    /// The shared total, in bytes.
    pub fn total(&self) -> usize {
        self.lock().total
    }

    /// Bytes held across every live pane — what the budget actually costs.
    pub fn held_bytes(&self) -> usize {
        self.lock()
            .panes
            .iter()
            .filter_map(Weak::upgrade)
            .map(|b| lock_buffer(&b).bytes())
            .sum()
    }

    /// Live panes drawing on the budget.
    pub fn pane_count(&self) -> usize {
        self.lock()
            .panes
            .iter()
            .filter(|w| w.strong_count() > 0)
            .count()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BudgetState> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn rebalance(st: &mut BudgetState) {
        // A pane whose buffer is gone without a `release` (a panic in a
        // constructor, say) stops counting here rather than holding a share.
        st.panes.retain(|w| w.strong_count() > 0);
        let live: Vec<_> = st.panes.iter().filter_map(Weak::upgrade).collect();
        let share = st.total / live.len().max(1);
        for buf in live {
            lock_buffer(&buf).set_capacity(share);
        }
    }
}

/// A buffer's lock, poisoned or not. A panic under it leaves the contents
/// intact, and trimming them to the budget is still correct — skipping a
/// poisoned buffer would exempt it from the cap for the pane's life.
fn lock_buffer(buf: &Mutex<RewindBuffer>) -> std::sync::MutexGuard<'_, RewindBuffer> {
    buf.lock().unwrap_or_else(|e| e.into_inner())
}

/// The process-wide budget every terminal pane registers with.
///
/// Starts at the local default; the pane constructor sets the configured
/// total ([`configured_budget_bytes`]) before registering, so a settings
/// edit applies at the next pane without a relaunch.
pub fn budget() -> &'static RewindBudget {
    static BUDGET: RewindBudget = RewindBudget::new(DEFAULT_BUDGET_BYTES);
    &BUDGET
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cap is on bytes, and it holds under a flood of tiny writes.
    ///
    /// The acceptance criterion from #357: "memory stays under the configured
    /// cap on a `yes`-style flood". A frame-counted buffer passes a test like
    /// this while using unbounded memory, so the assertion is on `bytes()`.
    #[test]
    fn a_flood_of_small_writes_stays_under_the_cap() {
        let mut b = RewindBuffer::new(1024);
        for i in 0..10_000u64 {
            b.push(i, b"y\n");
        }
        assert!(
            b.bytes() <= 1024,
            "the buffer grew past its cap: {} bytes",
            b.bytes()
        );
        // And it did not simply throw everything away.
        assert!(!b.is_empty(), "the buffer evicted everything");
    }

    /// A single write larger than the whole budget is truncated, not dropped.
    ///
    /// Dropping it would leave the replay with a gap where a large `cat`
    /// happened; keeping its tail shows a shortened write, which a reader can
    /// interpret.
    #[test]
    fn a_write_larger_than_the_budget_keeps_its_tail() {
        let mut b = RewindBuffer::new(16);
        // DISTINGUISHABLE payload: every byte differs, so the assertion can
        // tell the tail from the head. A uniform `[b'x'; 100]` would pass
        // just as happily against a slice taken from the wrong end.
        let big: Vec<u8> = (0u8..100).collect();
        b.push(1, &big);
        assert_eq!(b.bytes(), 16, "a huge write must be clamped to the budget");
        assert!(!b.is_empty(), "it must not vanish entirely");

        let (_, frames) = b.replay_from(u64::MAX);
        let kept: Vec<u8> = frames.iter().flat_map(|f| f.data.clone()).collect();
        assert_eq!(
            kept,
            (84u8..100).collect::<Vec<u8>>(),
            "the LAST 16 bytes must survive, not the first: a replay shows \
             what the command most recently printed"
        );
    }

    /// Eviction is oldest-first, so the buffer keeps the RECENT past.
    ///
    /// A rewind scrubber that kept the oldest bytes and dropped the newest
    /// would be exactly backwards: the whole point is the last few minutes.
    #[test]
    fn eviction_drops_the_oldest_output_first() {
        let mut b = RewindBuffer::new(10);
        b.push(1, b"aaaaa");
        b.push(2, b"bbbbb");
        b.push(3, b"ccccc");
        let (_, frames) = b.replay_from(3);
        let kept: Vec<&[u8]> = frames.iter().map(|f| f.data.as_slice()).collect();
        assert!(
            !kept.contains(&b"aaaaa".as_slice()),
            "the oldest write should have been evicted: {kept:?}"
        );
        assert!(
            kept.contains(&b"ccccc".as_slice()),
            "the newest write must survive: {kept:?}"
        );
    }

    /// Replay starts from the newest keyframe at or before the target.
    ///
    /// Replaying from the beginning is correct but unboundedly slow; replaying
    /// from an arbitrary offset renders a screen that never existed. The
    /// keyframe is what makes a bounded replay also an exact one.
    #[test]
    fn replay_starts_from_the_newest_keyframe_at_or_before_the_target() {
        let mut b = RewindBuffer::new(1 << 20);
        b.push(10, b"one");
        b.push_keyframe(20, vec![String::from("screen at 20")]);
        b.push(30, b"two");
        b.push_keyframe(40, vec![String::from("screen at 40")]);
        b.push(50, b"three");

        let (kf, frames) = b.replay_from(50);
        assert_eq!(
            kf.map(|k| k.at_ms),
            Some(40),
            "must pick the newest keyframe at or before the target"
        );
        let data: Vec<&[u8]> = frames.iter().map(|f| f.data.as_slice()).collect();
        assert_eq!(
            data,
            vec![b"three".as_slice()],
            "only the frames after that keyframe are replayed"
        );

        // Scrubbing further back picks the earlier keyframe.
        let (kf, frames) = b.replay_from(35);
        assert_eq!(kf.map(|k| k.at_ms), Some(20));
        let data: Vec<&[u8]> = frames.iter().map(|f| f.data.as_slice()).collect();
        assert_eq!(data, vec![b"two".as_slice()]);

        // The boundary itself: "at or before" must include AT. A search using
        // `<` rather than `<=` survives every other test in this module — it
        // falls back to None and replays from a blank screen, reaching the
        // right result by a slower path — so only an equality case catches it.
        let (kf_eq, frames_eq) = b.replay_from(20);
        assert_eq!(
            kf_eq.map(|k| k.at_ms),
            Some(20),
            "a keyframe exactly AT the target must be selected, not skipped"
        );
        // No frames come back: the range is [start, at_ms] = [20, 20], and
        // "two" landed at 30. The KEYFRAME selection is what this case pins;
        // asserting the frames too would only restate the range filter, which
        // `replay_from(35)` above already covers. (I first asserted ["two"]
        // here by copying that case without re-deriving it for a different
        // target -- the empty result is correct.)
        let data_eq: Vec<&[u8]> = frames_eq.iter().map(|f| f.data.as_slice()).collect();
        assert!(
            data_eq.is_empty(),
            "the replay range is [20, 20]; \"two\" at 30 is outside it: {data_eq:?}"
        );
    }

    /// A frame in the same millisecond as the keyframe is replayed, not skipped.
    ///
    /// The keyframe is rendered from the output strictly before it, so a frame
    /// sharing its timestamp is not yet reflected in that screen. Excluding it
    /// would silently lose that output from every replay through the keyframe
    /// — a wrong screen rather than a slow one.
    #[test]
    fn a_frame_sharing_the_keyframes_timestamp_is_still_replayed() {
        let mut b = RewindBuffer::new(1 << 20);
        b.push_keyframe(20, vec![String::from("screen at 20")]);
        b.push(20, b"same tick");
        let (kf, frames) = b.replay_from(30);
        assert_eq!(kf.map(|k| k.at_ms), Some(20));
        let data: Vec<&[u8]> = frames.iter().map(|f| f.data.as_slice()).collect();
        assert_eq!(
            data,
            vec![b"same tick".as_slice()],
            "the co-timestamped frame must not be swallowed by the keyframe"
        );
    }

    /// Before any keyframe, replay reports none and starts from blank.
    #[test]
    fn a_target_before_every_keyframe_replays_from_blank() {
        let mut b = RewindBuffer::new(1 << 20);
        b.push(10, b"early");
        b.push_keyframe(20, vec![String::from("later")]);
        let (kf, frames) = b.replay_from(15);
        assert!(kf.is_none(), "no keyframe precedes the target");
        let data: Vec<&[u8]> = frames.iter().map(|f| f.data.as_slice()).collect();
        assert_eq!(data, vec![b"early".as_slice()]);
    }

    /// Keyframes whose frames have aged out do not outlive them.
    ///
    /// A keyframe with no surviving frames after it would let `replay_from`
    /// choose a start point with a gap behind the remaining output, rendering
    /// a screen that never existed. One keyframe at or before the oldest frame
    /// is kept, because a scrub to the very start replays from it.
    #[test]
    fn keyframes_do_not_outlive_the_frames_they_precede() {
        // Room for the three keyframes and two of the frames, so the third
        // frame is what evicts the first. Keyframes count against the cap
        // (#694); a bare 10 bytes would now refuse every screen.
        let kf_bytes = ["ancient", "old", "recent"]
            .iter()
            .map(|s| keyframe_cost(&[String::from(*s)]))
            .sum::<usize>();
        let mut b = RewindBuffer::new(kf_bytes + 10);
        b.push_keyframe(1, vec![String::from("ancient")]);
        b.push(2, b"aaaaa");
        b.push_keyframe(3, vec![String::from("old")]);
        b.push(4, b"bbbbb");
        b.push_keyframe(5, vec![String::from("recent")]);
        // Evicts the frame at 2, so the keyframes at 1 and 3 are stale.
        b.push(6, b"ccccc");

        let (kf, _) = b.replay_from(6);
        assert_eq!(
            kf.map(|k| k.screen[0].as_str()),
            Some("recent"),
            "replay must not start from a keyframe whose frames are gone"
        );
        assert!(
            b.keyframes.len() <= 2,
            "stale keyframes accumulated: {:?}",
            b.keyframes.iter().map(|k| k.at_ms).collect::<Vec<_>>()
        );
    }

    /// Replay never starts from a keyframe with evicted output after it.
    ///
    /// The sweep used to keep the LAST keyframe unconditionally, so a lone
    /// stale one survived however far it sat before the oldest frame. A
    /// backward scrub then replayed that keyframe plus the frames after the
    /// gap, silently rendering a screen that never existed — the precise
    /// failure the sweep exists to prevent, reachable through the public API
    /// alone. Asserted at a MIDDLE target: checking only the newest one
    /// picks the newest keyframe and never sees the gap.
    #[test]
    fn replay_never_starts_from_a_keyframe_with_evicted_frames_after_it() {
        let mut b = RewindBuffer::new(10);
        b.push_keyframe(1, vec![String::from("t=1")]);
        b.push(2, b"aaaaa");
        b.push(3, b"bbbbb");
        // Evicts the frame at 2, so the keyframe at 1 now has a hole after it.
        b.push(4, b"ccccc");

        let oldest = b.span_ms().expect("frames remain").0;
        let (kf, _) = b.replay_from(4);
        assert!(
            kf.is_none_or(|k| k.at_ms >= oldest),
            "replay starts at keyframe {:?} but the oldest surviving frame is \
             {oldest}: the output between them was evicted",
            kf.map(|k| k.at_ms)
        );

        // POSITIVE CONTROL. The assertion above short-circuits on `None`, so
        // it is satisfied by a buffer that simply never offers a keyframe at
        // all — it proves no BAD start point is returned, not that a good one
        // still is. Without this half, a sweep that discarded every keyframe
        // would pass, which is exactly the regression that reached review.
        let mut b = RewindBuffer::new(1 << 20);
        b.push_keyframe(10, vec![String::from("valid")]);
        b.push(20, b"x");
        b.push(30, b"y");
        let (kf, _) = b.replay_from(30);
        assert_eq!(
            kf.map(|k| k.at_ms),
            Some(10),
            "an unorphaned keyframe must still be offered as a start point"
        );
    }

    /// Eviction inside ONE millisecond still invalidates the keyframe.
    ///
    /// A timestamp cannot see this: the evicted frame, the survivors and the
    /// keyframe all share `at_ms`, so any `<` comparison says the keyframe is
    /// fine while output that followed it has gone. The records carry a
    /// sequence number for exactly this case — bursts of output routinely
    /// land within one millisecond, so this is the common shape, not a
    /// contrived one.
    #[test]
    fn eviction_within_one_millisecond_still_invalidates_the_keyframe() {
        let mut b = RewindBuffer::new(15);
        b.push_keyframe(5, vec![String::from("before any output")]);
        b.push(5, b"aaaaa");
        b.push(5, b"bbbbb");
        b.push(5, b"ccccc");
        // Evicts "aaaaa", which followed the keyframe and is not in it.
        b.push(5, b"ddddd");

        let (kf, frames) = b.replay_from(5);
        let data: Vec<&[u8]> = frames.iter().map(|f| f.data.as_slice()).collect();
        assert!(
            !data.contains(&b"aaaaa".as_slice()),
            "precondition: aaaaa must have been evicted, or this proves nothing"
        );
        assert!(
            kf.is_none(),
            "the keyframe was kept although output recorded after it was \
             evicted: replaying it plus {data:?} shows a screen that never \
             existed"
        );
    }

    /// Timestamps that go backwards are not silently swallowed.
    ///
    /// Every read assumes `frames` is ordered by `at_ms`. The pane now feeds
    /// a monotonic clock so this cannot arise there, but `push` is public and
    /// a caller with a wall clock would otherwise lose output with no signal:
    /// the frame lands before its predecessors and falls outside both
    /// `span_ms` and every `replay_from` window.
    #[test]
    fn a_backwards_timestamp_is_clamped_rather_than_hidden() {
        let mut b = RewindBuffer::new(1 << 20);
        b.push(1000, b"first");
        b.push(500, b"clock stepped back");
        b.push(1100, b"after");

        let (_, frames) = b.replay_from(u64::MAX);
        let seen: Vec<&[u8]> = frames.iter().map(|f| f.data.as_slice()).collect();
        assert!(
            seen.contains(&b"clock stepped back".as_slice()),
            "output recorded across a backward step became unreachable: {seen:?}"
        );
        // The span must COVER every frame, not merely be non-inverted: taken
        // positionally it reports (1000, 1100) while a frame sits at 500,
        // and a scrubber sizing its timeline from that cannot address it.
        let (start, end) = b.span_ms().expect("frames remain");
        assert_eq!(
            (start, end),
            (500, 1100),
            "the span must cover the earliest and latest frame, not the first \
             and last recorded"
        );
    }

    /// A write spanning several keyframe intervals does not lose the surplus.
    #[test]
    fn a_multi_interval_write_keeps_its_remainder() {
        let mut b = RewindBuffer::new(1 << 30);
        assert!(
            b.push(1, &[b'x'; KEYFRAME_INTERVAL_BYTES * 3]),
            "three intervals in one write must ask for a keyframe"
        );
        // The surplus carries: two intervals' worth is still outstanding, so
        // two more requests are owed before the count restarts.
        assert!(
            b.push(2, b"x"),
            "the remainder from the big write was discarded"
        );
        assert!(b.push(3, b"x"), "the second owed keyframe was discarded");
        // Negative control: three intervals owe exactly three, not more.
        assert!(
            !b.push(4, b"x"),
            "a fourth keyframe was requested for three intervals of output"
        );
    }

    /// SEVERAL adjacent stale keyframes are all swept, not just the last.
    ///
    /// The trailing `if` in `drop_orphan_keyframes` removes one stale
    /// keyframe; the `while` above it is what removes a RUN of them. Nothing
    /// else in this module builds that state, so deleting the loop leaves
    /// every other test green while orphans accumulate — measured: a brute
    /// force over 16384 push/keyframe interleavings retains 7900 of them
    /// without it.
    ///
    /// Several keyframes taken back to back with no frames between them is
    /// the ordinary shape here: the reader asks for a keyframe when a write
    /// crosses the interval, and a burst of large writes asks repeatedly
    /// before the next frame lands.
    #[test]
    fn a_run_of_stale_keyframes_is_swept_not_just_the_newest() {
        // 16 bytes of budget: each 8-byte frame evicts what came before it.
        let mut b = RewindBuffer::new(16);
        b.push(10, b"aaaaaaaa");
        // Three keyframes back to back, all describing screens that only the
        // first frame can reach.
        b.push_keyframe(11, vec![String::from("s1")]);
        b.push_keyframe(12, vec![String::from("s2")]);
        b.push_keyframe(13, vec![String::from("s3")]);
        b.push(20, b"bbbbbbbb");
        b.push(30, b"cccccccc");

        // PRESENCE half, so the count assertion cannot pass over an empty
        // buffer: the surviving frames are the two most recent writes.
        let (kf, frames) = b.replay_from(u64::MAX);
        let data: Vec<&[u8]> = frames.iter().map(|f| f.data.as_slice()).collect();
        assert_eq!(
            data,
            vec![b"bbbbbbbb".as_slice(), b"cccccccc".as_slice()],
            "the two newest writes must survive in the 16-byte budget"
        );
        // Every keyframe predates the oldest surviving frame, so none is a
        // valid start point and at most one may be retained.
        assert!(
            b.keyframe_count() <= 1,
            "a run of stale keyframes was left behind: {} retained",
            b.keyframe_count()
        );
        // And the one that may remain must not claim to describe a screen
        // reachable from the surviving frames.
        if let Some(k) = kf {
            assert!(
                k.seq + 1 >= frames[0].seq,
                "retained keyframe seq {} is orphaned before frame seq {}",
                k.seq,
                frames[0].seq
            );
        }
    }

    /// With NO surviving frames, keyframes still collapse to the newest.
    ///
    /// `drop_orphan_keyframes` has two arms, and the sibling test above only
    /// covers the frames-present one. This is the `else` arm, reached when no
    /// frame survives: the collapse loop there is the ONLY thing bounding the
    /// keyframe deque, and deleting it leaves the whole rewind suite green
    /// while keyframes grow without limit — each holding a full screen.
    ///
    /// Its one reachable state is a keyframe taken before any output has
    /// arrived, at an ordinary capacity. Frames "ageing out" does NOT reach
    /// it — eviction is byte-capped rather than age-based and always leaves
    /// the just-pushed frame, so a buffer that has held frames never returns
    /// to empty. A zero capacity used to reach it too; since keyframes count
    /// against the cap (#694) a zero capacity stores none, pinned below.
    #[test]
    fn keyframes_collapse_to_the_newest_when_no_frames_survive() {
        let mut b = RewindBuffer::new(1 << 20);
        for i in 0..5u64 {
            b.push_keyframe(i, vec![format!("pre{i}")]);
        }
        assert!(
            b.is_empty(),
            "precondition: no output has arrived, so there are no frames"
        );
        assert_eq!(
            b.keyframe_count(),
            1,
            "keyframes must collapse to the newest when no frame survives"
        );
        // PRESENCE half: pin WHICH keyframe survived. A count of 1 alone
        // cannot tell "kept the newest" from "kept the oldest" — the two are
        // indistinguishable by count, and only the newest is a valid start
        // point for output arriving next.
        let (kf, _) = b.replay_from(u64::MAX);
        assert_eq!(
            kf.map(|k| k.screen.clone()),
            Some(vec![String::from("pre4")]),
            "the newest pre-output keyframe must be the one retained"
        );
        assert_eq!(
            b.bytes(),
            keyframe_cost(&[String::from("pre4")]),
            "the collapsed keyframes must give their bytes back"
        );

        // Capacity 0 holds nothing, keyframes included: a disabled buffer
        // that still kept a screen would be neither off nor useful.
        let mut b = RewindBuffer::new(0);
        for i in 0..5u64 {
            b.push(i, b"never retained");
            b.push_keyframe(i, vec![format!("k{i}")]);
        }
        assert!(b.is_empty());
        assert_eq!(b.keyframe_count(), 0);
        assert_eq!(b.bytes(), 0);
    }

    /// Empty writes must not accumulate frames the cap can never evict.
    ///
    /// `bytes` sums payload lengths, and `evict` runs `while bytes > capacity`
    /// — so a zero-length write adds a `Frame` while adding nothing to the
    /// figure that triggers eviction. Repeated empty pushes therefore grew
    /// `frames` without bound, in a buffer whose entire purpose is to be
    /// bounded. The reader cannot emit one today (`Ok(0) => break` ends the
    /// loop), but `push` is public and the replay half will add callers.
    #[test]
    fn empty_writes_do_not_accumulate_unevictable_frames() {
        let mut b = RewindBuffer::new(1 << 20);
        for _ in 0..10_000 {
            b.push(1, b"");
        }
        assert!(
            b.is_empty(),
            "empty writes must record nothing at all, not unevictable frames"
        );
        assert_eq!(b.bytes(), 0);

        // PRESENCE half: a real write still records, so the guard rejects
        // only the empty case rather than everything.
        b.push(2, b"real");
        let (_, frames) = b.replay_from(u64::MAX);
        let kept: Vec<&[u8]> = frames.iter().map(|f| f.data.as_slice()).collect();
        assert_eq!(
            kept,
            vec![b"real".as_slice()],
            "a non-empty write must still be recorded"
        );
    }

    /// A zero capacity records nothing rather than panicking.
    #[test]
    fn a_zero_capacity_buffer_records_nothing() {
        let mut b = RewindBuffer::new(0);
        assert!(!b.push(1, b"anything"));
        assert!(b.is_empty());
        assert_eq!(b.bytes(), 0);
        assert_eq!(b.span_ms(), None);
    }

    /// The span is the range a scrubber can address.
    #[test]
    fn the_span_reports_the_rewindable_range() {
        let mut b = RewindBuffer::new(1 << 20);
        assert_eq!(b.span_ms(), None, "an empty buffer spans nothing");
        b.push(100, b"a");
        b.push(900, b"b");
        assert_eq!(b.span_ms(), Some((100, 900)));
    }

    /// The keyframe request fires on bytes, not on frame count.
    ///
    /// Asserted through the public signal rather than the private counter, so
    /// the test still means something if the accounting changes.
    #[test]
    fn a_keyframe_is_requested_once_the_byte_interval_is_passed() {
        let mut b = RewindBuffer::new(1 << 30);
        let chunk = vec![b'x'; 1024];
        let mut asked = 0;
        // Just over one interval's worth of output.
        for i in 0..(KEYFRAME_INTERVAL_BYTES / 1024) as u64 + 1 {
            if b.push(i, &chunk) {
                asked += 1;
            }
        }
        assert_eq!(asked, 1, "exactly one keyframe should have been requested");

        // Many small writes totalling less than an interval ask for none.
        let mut b = RewindBuffer::new(1 << 30);
        let mut asked = 0;
        for i in 0..1000u64 {
            if b.push(i, b"y\n") {
                asked += 1;
            }
        }
        assert_eq!(asked, 0, "2 KB of output must not trigger a keyframe");

        // The boundary is `>=`, so a write landing EXACTLY on the interval
        // asks. Mutating it to `>` survives every other test here.
        let mut b2 = RewindBuffer::new(1 << 20);
        assert!(
            b2.push(1, &vec![b'x'; KEYFRAME_INTERVAL_BYTES]),
            "a write landing exactly on the interval must ask for a keyframe"
        );
    }

    /// What the buffer holds, summed from the records themselves, to check
    /// the running `bytes` counter against.
    fn recount(b: &RewindBuffer) -> usize {
        b.frames.iter().map(|f| f.data.len()).sum::<usize>()
            + b.keyframes.iter().map(|k| k.cost).sum::<usize>()
    }

    fn screen(rows: usize, width: usize) -> Vec<String> {
        vec!["x".repeat(width); rows]
    }

    /// #694: keyframes count toward the cap, so they cannot pile up on top
    /// of a full frame budget.
    ///
    /// The first version capped frame payloads only. Pushing a screen per
    /// interval then grew memory by a whole screen each time while `bytes()`
    /// reported the buffer within its cap.
    #[test]
    fn keyframes_count_toward_the_cap() {
        let cap = 64 * 1024;
        let mut b = RewindBuffer::new(cap);
        for i in 0..2_000u64 {
            b.push(i, &[b'y'; 512]);
            // A 24-row, 80-column screen: about 2.5 KB with row headers.
            b.push_keyframe(i, screen(24, 80));
        }
        assert!(
            b.bytes() <= cap,
            "frames plus keyframes grew past the cap: {} > {cap}",
            b.bytes()
        );
        assert_eq!(
            b.bytes(),
            recount(&b),
            "the running total drifted from what is actually held"
        );
        // PRESENCE half: the keyframes were counted, not simply refused.
        assert!(b.keyframe_count() > 0, "no keyframe survived at all");
        assert!(
            b.keyframe_count() < 2_000,
            "keyframes accumulated without bound: {}",
            b.keyframe_count()
        );
    }

    /// Storing a keyframe makes room by evicting the OLDEST frames, and the
    /// newest frame survives it.
    #[test]
    fn a_keyframe_evicts_old_output_to_make_room() {
        let kf = screen(2, 10);
        let cap = 30 + keyframe_cost(&kf);
        let mut b = RewindBuffer::new(cap);
        b.push(1, &[b'a'; 10]);
        b.push(2, &[b'b'; 10]);
        b.push(3, &[b'c'; 10]);
        b.push(4, &[b'd'; 10]);
        b.push_keyframe(5, kf);
        assert!(b.bytes() <= cap);
        assert_eq!(b.bytes(), recount(&b));
        assert_eq!(b.keyframe_count(), 1, "the keyframe was stored");
        let kept: Vec<u8> = b.frames.iter().map(|f| f.data[0]).collect();
        assert!(!kept.contains(&b'a'), "the oldest frame must go: {kept:?}");
        assert!(kept.contains(&b'd'), "the newest frame must stay: {kept:?}");
    }

    /// A screen bigger than the whole budget is refused, not stored at the
    /// cost of every frame.
    #[test]
    fn a_keyframe_larger_than_the_budget_is_not_stored() {
        let mut b = RewindBuffer::new(100);
        b.push(1, b"output worth keeping");
        b.push_keyframe(2, screen(10, 80));
        assert_eq!(b.keyframe_count(), 0, "an oversized keyframe was stored");
        let (_, frames) = b.replay_from(u64::MAX);
        assert_eq!(
            frames.len(),
            1,
            "refusing the keyframe must not cost the output it would have evicted"
        );
    }

    /// Lowering the cap trims at once, keyframes included; zero empties it.
    #[test]
    fn set_capacity_trims_immediately_and_zero_empties() {
        let mut b = RewindBuffer::new(1 << 20);
        for i in 0..100u64 {
            b.push(i, &[b'z'; 1000]);
            b.push_keyframe(i, screen(4, 40));
        }
        let before = b.bytes();
        b.set_capacity(10_000);
        assert!(b.bytes() <= 10_000, "{} bytes after shrinking", b.bytes());
        assert!(b.bytes() < before);
        assert_eq!(b.bytes(), recount(&b));
        assert!(!b.is_empty(), "shrinking must keep the recent past");

        // Below a single frame: nothing fits, so nothing is left — and the
        // counter agrees.
        b.set_capacity(10);
        assert_eq!(b.bytes(), recount(&b));
        assert!(b.bytes() <= 10);

        b.set_capacity(0);
        assert!(b.is_empty());
        assert_eq!(b.keyframe_count(), 0);
        assert_eq!(b.bytes(), 0);
        assert!(!b.push(1, b"after"), "a zero cap records nothing");
    }

    /// Fill a buffer from the budget with more than its share.
    fn flood(buf: &Arc<Mutex<RewindBuffer>>, bytes: usize) {
        let mut b = buf.lock().unwrap();
        for i in 0..(bytes / 1024) as u64 {
            b.push(i, &[b'q'; 1024]);
        }
    }

    /// #694: the budget is one figure for every pane, not a per-pane cap.
    ///
    /// Ten flooding panes under a per-pane cap hold ten caps. Under the
    /// shared budget they hold one, and opening another pane trims the
    /// existing ones BEFORE it records, so the sum never overshoots.
    #[test]
    fn the_budget_is_shared_across_panes() {
        let total = 1024 * 1024;
        let budget = RewindBudget::new(total);
        let panes: Vec<_> = (0..10).map(|_| budget.register()).collect();
        for p in &panes {
            flood(p, total);
        }
        assert!(
            budget.held_bytes() <= total,
            "ten panes hold {} against a shared {total}",
            budget.held_bytes()
        );
        // PRESENCE: every pane kept something, so the bound is a split and
        // not one pane starving the rest.
        for p in &panes {
            assert!(!p.lock().unwrap().is_empty(), "a pane was starved");
        }

        // An eleventh pane: the ten are trimmed to the smaller share first.
        let extra = budget.register();
        assert!(budget.held_bytes() <= total);
        flood(&extra, total);
        assert!(
            budget.held_bytes() <= total,
            "the new pane pushed the total over: {}",
            budget.held_bytes()
        );
        assert_eq!(budget.pane_count(), 11);
        assert_eq!(panes[0].lock().unwrap().capacity(), total / 11);
    }

    /// Closing a pane frees its buffer now and returns its share.
    #[test]
    fn releasing_a_pane_frees_it_and_returns_its_share() {
        let total = 1024 * 1024;
        let budget = RewindBudget::new(total);
        let a = budget.register();
        let b = budget.register();
        flood(&a, total);
        assert_eq!(b.lock().unwrap().capacity(), total / 2);

        // The reader thread's clone of `a` can outlive the pane: releasing
        // must empty the buffer rather than wait for the last handle.
        let reader_clone = a.clone();
        budget.release(&a);
        assert!(reader_clone.lock().unwrap().is_empty());
        assert_eq!(reader_clone.lock().unwrap().capacity(), 0);
        assert_eq!(budget.pane_count(), 1);
        assert_eq!(
            b.lock().unwrap().capacity(),
            total,
            "the survivor must get the freed share back"
        );
    }

    /// A settings edit re-splits the new total across open panes.
    #[test]
    fn set_total_resizes_every_pane() {
        let budget = RewindBudget::new(1 << 20);
        let a = budget.register();
        let b = budget.register();
        flood(&a, 1 << 20);
        budget.set_total(4096);
        assert_eq!(budget.total(), 4096);
        assert_eq!(a.lock().unwrap().capacity(), 2048);
        assert_eq!(b.lock().unwrap().capacity(), 2048);
        assert!(budget.held_bytes() <= 4096);
        budget.set_total(0);
        assert_eq!(budget.held_bytes(), 0, "a zero total turns rewind off");
    }

    /// Unset follows where croft runs; 0 is off; values are megabytes and
    /// clamped.
    #[test]
    fn the_setting_maps_to_a_budget() {
        assert_eq!(configured_budget_bytes(None, false), DEFAULT_BUDGET_BYTES);
        assert_eq!(
            configured_budget_bytes(None, true),
            REMOTE_DEFAULT_BUDGET_BYTES
        );
        const { assert!(REMOTE_DEFAULT_BUDGET_BYTES < DEFAULT_BUDGET_BYTES) };
        assert_eq!(configured_budget_bytes(Some(0), false), 0);
        assert_eq!(configured_budget_bytes(Some(32), true), 32 << 20);
        assert_eq!(
            configured_budget_bytes(Some(usize::MAX), false),
            MAX_BUDGET_MB << 20,
            "an absurd value must clamp, not overflow"
        );
    }
}
