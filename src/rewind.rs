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
/// Overridable per the issue's "default 10 minutes or 64 MB, configurable".
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
    /// Milliseconds since the buffer started, so a scrub position maps to a
    /// frame without consulting a wall clock that may have stepped.
    pub at_ms: u64,
    pub data: Vec<u8>,
}

/// A full screen snapshot, so replay never has to start from the beginning.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keyframe {
    pub at_ms: u64,
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
}

impl RewindBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            frames: VecDeque::new(),
            keyframes: VecDeque::new(),
            bytes: 0,
            capacity,
            since_keyframe: 0,
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
        self.frames.push_back(Frame {
            at_ms,
            data: data.to_vec(),
        });
        self.evict();
        self.since_keyframe += data.len();
        if self.since_keyframe >= KEYFRAME_INTERVAL_BYTES {
            self.since_keyframe = 0;
            return true;
        }
        false
    }

    /// Store a screen snapshot the caller rendered after [`push`] asked for one.
    pub fn push_keyframe(&mut self, at_ms: u64, screen: Vec<String>) {
        self.keyframes.push_back(Keyframe { at_ms, screen });
        self.drop_orphan_keyframes();
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

    /// Discard keyframes older than the oldest surviving frame.
    ///
    /// A keyframe whose following frames have been evicted cannot be replayed
    /// forward to anything, and keeping it would let [`replay_from`] pick a
    /// start point with a gap after it — silently rendering a screen that
    /// never existed. One keyframe at or before the oldest frame is kept,
    /// because that is the one a scrub to the very start replays from.
    fn drop_orphan_keyframes(&mut self) {
        let Some(oldest) = self.frames.front().map(|f| f.at_ms) else {
            // No frames: only the newest keyframe can still be meaningful.
            while self.keyframes.len() > 1 {
                self.keyframes.pop_front();
            }
            return;
        };
        while self.keyframes.len() > 1 && self.keyframes[1].at_ms <= oldest {
            self.keyframes.pop_front();
        }
    }

    /// Bytes currently held. Never exceeds the capacity.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// The span the buffer can rewind over, as `(oldest, newest)` in ms.
    pub fn span_ms(&self) -> Option<(u64, u64)> {
        Some((self.frames.front()?.at_ms, self.frames.back()?.at_ms))
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
        b.push(1, &[b'x'; 100]);
        assert_eq!(b.bytes(), 16, "a huge write must be clamped to the budget");
        assert!(!b.is_empty(), "it must not vanish entirely");
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
