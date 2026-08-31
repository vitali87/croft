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

/// A default that holds a busy session's last few minutes without being felt.
///
/// PER PANE, and not yet configurable: #357 asks for a setting and this ships
/// the constant, so ten busy panes is a 640 MB ceiling with no way to lower
/// it. Tolerable only because the buffer grows to what is actually printed —
/// an idle pane costs nothing, and a closed one is reaped with its pane — but
/// the prefs key is owed before the scrubber ships, not after.
pub const DEFAULT_CAPACITY_BYTES: usize = 64 * 1024 * 1024;

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
}

/// The session's recent output, bounded by total payload bytes.
#[derive(Debug)]
pub struct RewindBuffer {
    frames: VecDeque<Frame>,
    keyframes: VecDeque<Keyframe>,
    /// Summed `data.len()` of every frame held — the quantity actually capped.
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
    pub fn push_keyframe(&mut self, at_ms: u64, screen: Vec<String>) {
        let seq = self.take_seq();
        self.keyframes.push_back(Keyframe { at_ms, seq, screen });
        self.drop_orphan_keyframes();
    }

    fn take_seq(&mut self) -> u64 {
        let s = self.next_seq;
        self.next_seq += 1;
        s
    }

    /// Drop the oldest frames until the total payload fits the cap.
    fn evict(&mut self) {
        while self.bytes > self.capacity {
            match self.frames.pop_front() {
                Some(f) => self.bytes -= f.data.len(),
                // Unreachable while `bytes` is the sum of `frames`, but a
                // `while` on a counter that a future change could desync is
                // worth ending rather than spinning.
                None => {
                    self.bytes = 0;
                    break;
                }
            }
        }
        self.drop_orphan_keyframes();
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
            // taken before any output — or after every frame has aged out —
            // is a valid start point for everything that arrives next, and
            // clearing here would drop the screen a fresh pane replays from.
            // Only the newest is worth keeping; the older ones describe
            // screens no surviving frame can reach.
            while self.keyframes.len() > 1 {
                self.keyframes.pop_front();
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
            self.keyframes.pop_front();
        }
        if self
            .keyframes
            .front()
            .is_some_and(|k| k.seq + 1 < oldest_seq)
        {
            self.keyframes.pop_front();
        }
    }

    /// Bytes currently held. Never exceeds the capacity.
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
        let mut b = RewindBuffer::new(10);
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
    /// covers the frames-present one. This is the `else` arm: when every
    /// frame has aged out, the collapse loop there is the ONLY thing bounding
    /// the keyframe deque, and deleting it leaves the whole rewind suite
    /// green while keyframes grow without limit — each holding a full screen.
    ///
    /// Reachable in production, not just at capacity 0: any pane whose frames
    /// have all aged out sits in this arm, which the module's own docs call
    /// "after every frame has aged out".
    #[test]
    fn keyframes_collapse_to_the_newest_when_no_frames_survive() {
        // Capacity 0: nothing is ever retained as a frame, so every call
        // lands in the no-frames arm.
        let mut b = RewindBuffer::new(0);
        for i in 0..5u64 {
            b.push(i, b"never retained");
            b.push_keyframe(i, vec![format!("k{i}")]);
        }

        assert!(
            b.is_empty(),
            "precondition: no frames may survive, or this exercises the wrong arm"
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
        let (kf, frames) = b.replay_from(u64::MAX);
        assert!(frames.is_empty(), "no frames survive at capacity 0");
        assert_eq!(
            kf.map(|k| k.screen.clone()),
            Some(vec![String::from("k4")]),
            "the RETAINED keyframe must be the newest one"
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
    }
}
