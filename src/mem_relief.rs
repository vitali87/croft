//! Handing freed heap back to the OS (#1707).
//!
//! Opening a large file parses it on background threads (highlighting and
//! the outline both build a tree-sitter tree), and while those trees exist
//! croft needs many times the file's size. The trees are dropped when the
//! passes end, but glibc's allocator keeps the pages they freed instead of
//! returning them, so croft stayed resident at the parse's peak for as long
//! as it ran: a 3 MB source file left ~270 MB resident with ~150 MB of it
//! still in use. A watcher thread looks every few seconds and, once the
//! process has grown well past its low-water mark, asks the allocator to
//! release what is free. Nothing croft still holds is touched, so no
//! feature changes; a released page that is needed again is faulted back in.

use std::time::Duration;

/// How often the watcher looks.
const INTERVAL: Duration = Duration::from_secs(3);

/// How far the measured size must grow past its low-water mark before a
/// release is worth its cost. Releasing walks every free chunk of every
/// arena, so it should not run while nothing has piled up.
pub const THRESHOLD: usize = 8 << 20;

/// The most releases in a row that freed little the wait before the next
/// one doubles for: past it, they come every 63 ticks (about 3 minutes).
const MAX_MISSES: u32 = 7;

/// When to release, given one reading of [`measure`] per tick: once the
/// reading has grown [`THRESHOLD`] past the lowest seen since the last
/// release. The low-water mark restarts from the reading taken right after
/// a release that freed something worthwhile, so memory croft still uses
/// never triggers another. A release that freed little (the parse was still
/// running, its memory live) keeps the old mark, so the next tick tries
/// again once the parse has dropped it. Each further one that frees little
/// waits twice as long before the next: the memory may simply be in use,
/// and a release locks the allocator while it walks the heap.
#[derive(Debug, Default)]
pub struct Policy {
    low: Option<usize>,
    /// Releases in a row that freed little, up to [`MAX_MISSES`].
    misses: u32,
    /// Ticks to sit out before the next release.
    wait: u32,
}

impl Policy {
    /// A policy whose mark starts at `reading`, the size at launch.
    pub fn starting_at(reading: usize) -> Self {
        Self {
            low: Some(reading),
            ..Self::default()
        }
    }

    pub fn should_release(&mut self, reading: usize) -> bool {
        let low = self.low.map_or(reading, |low| low.min(reading));
        self.low = Some(low);
        if self.wait > 0 {
            self.wait -= 1;
            return false;
        }
        reading - low >= THRESHOLD
    }

    pub fn released(&mut self, before: usize, after: usize) {
        if before.saturating_sub(after) >= THRESHOLD {
            self.low = Some(after);
            self.misses = 0;
        } else {
            self.misses = (self.misses + 1).min(MAX_MISSES);
            self.wait = (1 << (self.misses - 1)) - 1;
        }
    }
}

/// Whether this build can ask its allocator to return freed heap: glibc
/// Linux and macOS. musl and the other targets have no such call.
pub const SUPPORTED: bool = cfg!(any(
    all(target_os = "linux", target_env = "gnu"),
    target_os = "macos"
));

/// What [`Policy`] watches: the resident size on Linux, the default
/// malloc zone's allocated-but-unused bytes on macOS. 0 where unsupported.
pub fn measure() -> usize {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(0) as usize;
        std::fs::read_to_string("/proc/self/statm")
            .ok()
            .and_then(|s| s.split_whitespace().nth(1)?.parse::<usize>().ok())
            .map_or(0, |pages| pages * page)
    }
    #[cfg(target_os = "macos")]
    {
        let mut stats = libc::malloc_statistics_t {
            blocks_in_use: 0,
            size_in_use: 0,
            max_size_in_use: 0,
            size_allocated: 0,
        };
        unsafe { libc::malloc_zone_statistics(libc::malloc_default_zone(), &mut stats) };
        stats.size_allocated.saturating_sub(stats.size_in_use)
    }
    #[cfg(not(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos")))]
    {
        0
    }
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    /// `<malloc/malloc.h>`: return free pages of `zone` (every zone when
    /// null) to the OS, up to `goal` bytes (0 for all). Returns the bytes
    /// released.
    fn malloc_zone_pressure_relief(zone: *mut libc::c_void, goal: libc::size_t) -> libc::size_t;
}

/// Ask the allocator to return its free pages to the OS. True when it
/// released any.
pub fn release_free_memory() -> bool {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        unsafe { libc::malloc_trim(0) != 0 }
    }
    #[cfg(target_os = "macos")]
    {
        unsafe { malloc_zone_pressure_relief(std::ptr::null_mut(), 0) > 0 }
    }
    #[cfg(not(any(all(target_os = "linux", target_env = "gnu"), target_os = "macos")))]
    {
        false
    }
}

/// Start the watcher thread. A no-op where [`SUPPORTED`] is false.
pub fn start() {
    if !SUPPORTED {
        return;
    }
    // The mark starts from the size at launch, before any file is open. The
    // first tick comes later, after a file opened at launch (a restored
    // tab) has been parsed: taken as the mark, that parse's peak was kept
    // for as long as croft ran.
    let mut policy = Policy::starting_at(measure());
    let _ = std::thread::Builder::new()
        .name("croft-mem-relief".into())
        .spawn(move || {
            loop {
                std::thread::sleep(INTERVAL);
                let before = measure();
                if policy.should_release(before) {
                    release_free_memory();
                    policy.released(before, measure());
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    const MB: usize = 1 << 20;

    #[test]
    fn growth_past_the_low_water_mark_releases() {
        let mut p = Policy::default();
        assert!(!p.should_release(40 * MB));
        assert!(!p.should_release(45 * MB));
        assert!(p.should_release(40 * MB + THRESHOLD));
    }

    #[test]
    fn small_growth_and_shrinking_do_not_release() {
        let mut p = Policy::default();
        assert!(!p.should_release(40 * MB));
        assert!(!p.should_release(40 * MB + THRESHOLD - 1));
        assert!(!p.should_release(30 * MB));
        assert!(!p.should_release(30 * MB + THRESHOLD - 1));
        assert!(
            p.should_release(30 * MB + THRESHOLD),
            "the mark followed it down"
        );
    }

    #[test]
    fn after_a_release_the_mark_restarts_from_what_is_left() {
        let mut p = Policy::default();
        assert!(!p.should_release(40 * MB));
        assert!(p.should_release(300 * MB));
        p.released(300 * MB, 150 * MB);
        assert!(
            !p.should_release(150 * MB),
            "memory in use never retriggers"
        );
        assert!(!p.should_release(150 * MB + THRESHOLD - 1));
        assert!(p.should_release(150 * MB + THRESHOLD));
    }

    /// A release while the parse still holds its memory frees little, so
    /// the old mark stays and the next tick, after the parse has dropped
    /// it, releases again rather than taking the peak as the new floor.
    #[test]
    fn a_release_that_frees_little_is_retried() {
        let mut p = Policy::default();
        assert!(!p.should_release(40 * MB));
        assert!(p.should_release(300 * MB));
        p.released(300 * MB, 299 * MB);
        assert!(p.should_release(299 * MB), "still far above the old mark");
        p.released(299 * MB, 60 * MB);
        assert!(!p.should_release(60 * MB));
    }

    /// A file opened at launch is parsed before the first tick: measured
    /// from launch, its freed peak is released rather than taken as the
    /// mark.
    #[test]
    fn a_peak_reached_before_the_first_tick_is_released() {
        let mut p = Policy::starting_at(20 * MB);
        assert!(
            p.should_release(180 * MB),
            "the first tick, after the parse"
        );
        p.released(180 * MB, 90 * MB);
        assert!(!p.should_release(90 * MB));
    }

    /// Memory that is in use frees nothing however often it is asked: each
    /// release that frees little puts the next off twice as long, up to
    /// every 63 ticks, rather than locking the allocator every tick.
    #[test]
    fn releases_that_keep_freeing_little_back_off() {
        let mut p = Policy::starting_at(20 * MB);
        let mut gaps = Vec::new();
        let mut since = 0;
        while gaps.len() < 9 {
            if p.should_release(60 * MB) {
                gaps.push(since);
                since = 0;
                p.released(60 * MB, 59 * MB);
            } else {
                since += 1;
            }
        }
        assert_eq!(gaps, [0, 0, 1, 3, 7, 15, 31, 63, 63]);
    }

    /// A parse that outlasts several ticks is still released once it ends:
    /// the backoff delays the release, it never gives it up.
    #[test]
    fn a_long_parse_is_released_once_it_ends() {
        let mut p = Policy::starting_at(20 * MB);
        let mut tick = 0;
        loop {
            tick += 1;
            let parsing = tick < 40;
            if p.should_release(300 * MB) {
                if parsing {
                    p.released(300 * MB, 299 * MB);
                } else {
                    p.released(300 * MB, 60 * MB);
                    break;
                }
            }
            assert!(tick < 80, "released by twice the parse's length");
        }
        assert!(!p.should_release(60 * MB), "what is left is the mark");
    }

    /// Free 32 MB of small blocks that a live block keeps from the top of
    /// the heap, as a dropped parse tree does: the allocator holds on to
    /// them until asked, and releasing hands them back.
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    #[test]
    fn freed_heap_in_the_middle_of_the_heap_is_released() {
        std::thread::spawn(|| {
            let blocks: Vec<Vec<u8>> = (0..1024).map(|_| vec![1u8; 32 * 1024]).collect();
            let pin = vec![1u8; 32 * 1024];
            drop(blocks);
            assert!(
                release_free_memory(),
                "the freed blocks went back to the OS"
            );
            drop(pin);
        })
        .join()
        .unwrap();
    }

    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    #[test]
    fn measuring_reads_the_resident_size() {
        let statm = std::fs::read_to_string("/proc/self/statm").unwrap();
        let pages: usize = statm.split_whitespace().nth(1).unwrap().parse().unwrap();
        assert!(pages > 0);
        assert!(measure() >= pages * 4096 / 2, "{} bytes", measure());
    }
}
