//! Local history: a per-save snapshot store, VS Code's Local History /
//! Timeline "local" source. Every write of an editor buffer records the file's
//! contents under `~/.config/croft/history/<key>/<epoch-millis>.snap`, so a
//! version is recoverable even when it was never committed. Snapshots surface
//! in the Explorer TIMELINE alongside git commits and can be diffed against the
//! working file or restored.
//!
//! The store is keyed by a hash of the file's absolute path (no index file to
//! keep in sync), consecutive identical saves are de-duplicated, and each
//! file keeps at most [`MAX_SNAPSHOTS_PER_FILE`] snapshots so the directory
//! can't grow without bound.

use std::path::{Path, PathBuf};

/// Cap per file so a hot save loop can't fill the disk; oldest pruned first.
const MAX_SNAPSHOTS_PER_FILE: usize = 50;

/// Saves this close to the newest snapshot replace it instead of appending
/// (VS Code's `workbench.localHistory.mergeWindow`, default 10s). Without it
/// the 1s auto save turns the capped store into a log of the last minute.
const MERGE_WINDOW_MILLIS: u64 = 10_000;

const SNAP_EXT: &str = "snap";

/// One recorded version of a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    /// Milliseconds since the Unix epoch when the snapshot was taken.
    pub millis: u64,
    /// The `.snap` file holding the version's contents.
    pub file: PathBuf,
}

/// The real history root, `~/.config/croft/history`. The App caches this in a
/// field so tests can point it at a tempdir.
pub fn history_dir() -> PathBuf {
    crate::prefs::config_dir().join("history")
}

/// The `short_hash` sentinel prefix marking a TIMELINE row as a local
/// snapshot rather than a git commit; the milliseconds follow.
pub const LOCAL_PREFIX: &str = "local:";

/// Map snapshots to TIMELINE rows (`crate::git::FileHistoryEntry`) so they
/// render in the same panel as commits. The author reads `You`, the summary
/// `Local snapshot`, and the `short_hash` carries the [`LOCAL_PREFIX`]
/// sentinel + millis so a click opens the snapshot diff instead of `git show`.
pub fn to_timeline_entries(
    snaps: &[Snapshot],
    now_millis: u64,
) -> Vec<crate::git::FileHistoryEntry> {
    snaps
        .iter()
        .map(|s| crate::git::FileHistoryEntry {
            short_hash: format!("{LOCAL_PREFIX}{}", s.millis),
            summary: "Local snapshot".to_string(),
            author: "You".to_string(),
            age_secs: (now_millis.saturating_sub(s.millis) / 1000) as i64,
        })
        .collect()
}

/// Merge git commits and local snapshots into one newest-first TIMELINE, the
/// way VS Code's Timeline interleaves its sources. Both inputs are already
/// newest-first; the stable sort by age keeps a snapshot and a commit from the
/// same instant in commit-before-snapshot order.
pub fn merged_timeline(
    commits: Vec<crate::git::FileHistoryEntry>,
    snaps: &[Snapshot],
    now_millis: u64,
) -> Vec<crate::git::FileHistoryEntry> {
    let mut all = commits;
    all.extend(to_timeline_entries(snaps, now_millis));
    all.sort_by_key(|e| e.age_secs);
    all
}

/// The snapshot millis encoded in a TIMELINE row's `short_hash`, or `None`
/// when the row is an ordinary git commit.
pub fn snapshot_millis(short_hash: &str) -> Option<u64> {
    short_hash.strip_prefix(LOCAL_PREFIX)?.parse().ok()
}

/// The `.snap` file for `abs_path` at `millis` under `root_dir`, if it exists.
pub fn snapshot_file_in(root_dir: &Path, abs_path: &Path, millis: u64) -> Option<PathBuf> {
    let file = dir_for(root_dir, abs_path).join(format!("{millis}.{SNAP_EXT}"));
    file.is_file().then_some(file)
}

/// Stable directory name for `abs_path`: FNV-1a 64 of its string form, so two
/// files never collide and no path-to-key index is needed. Hand-rolled because
/// the store outlives the binary: `DefaultHasher`'s algorithm is unspecified
/// across Rust releases, and a key that moves orphans every stored snapshot.
fn key_for(abs_path: &Path) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in abs_path.to_string_lossy().as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    format!("{h:016x}")
}

/// The key pre-FNV builds used (`DefaultHasher`); only consulted to migrate.
fn legacy_key_for(abs_path: &Path) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    abs_path.to_string_lossy().hash(&mut h);
    format!("{:016x}", h.finish())
}

/// The store directory for `abs_path`, renaming a legacy-keyed directory to
/// the stable key on first touch so pre-FNV snapshots stay reachable.
fn dir_for(root_dir: &Path, abs_path: &Path) -> PathBuf {
    let dir = root_dir.join(key_for(abs_path));
    if !dir.exists() {
        let legacy = root_dir.join(legacy_key_for(abs_path));
        if legacy.is_dir() {
            let _ = std::fs::rename(&legacy, &dir);
        }
    }
    dir
}

/// Record `content` (the file's raw on-disk bytes, so non-UTF-8 encodings
/// round-trip) as a new snapshot of `abs_path` under `root_dir` (the history
/// root; separated out so tests can point it at a tempdir). A no-op when
/// `content` matches the newest snapshot; a save within
/// [`MERGE_WINDOW_MILLIS`] of the newest replaces it instead of appending.
/// Prunes to [`MAX_SNAPSHOTS_PER_FILE`]. Best-effort: any IO error is
/// returned but a caller may ignore it (history is a convenience, not a
/// guarantee).
pub fn record_in(
    root_dir: &Path,
    abs_path: &Path,
    content: &[u8],
    millis: u64,
) -> std::io::Result<()> {
    let dir = dir_for(root_dir, abs_path);
    if let Some(latest) = entries_in(root_dir, abs_path).first() {
        // Skip when the newest snapshot already holds this exact content.
        if std::fs::read(&latest.file).is_ok_and(|prev| prev == content) {
            return Ok(());
        }
        // Inside the merge window: this save supersedes the newest snapshot
        // rather than appending, so a 1s auto save can't churn out history.
        if millis.saturating_sub(latest.millis) < MERGE_WINDOW_MILLIS {
            // The sidecar described the bytes about to be replaced, at either
            // timestamp: two saves in one millisecond overwrite `<millis>.snap`
            // in place, and a sidecar left beside it would describe the
            // previous content. Removed BEFORE the replacement is published,
            // so an interruption in between leaves a snapshot with no seats
            // (every line unknown) rather than a snapshot wearing the seats
            // of text it does not hold.
            let _ = std::fs::remove_file(seats_path(&dir, millis));
            write_snapshot(&dir, millis, content)?;
            if latest.millis != millis {
                let _ = std::fs::remove_file(&latest.file);
                let _ = std::fs::remove_file(seats_path(&dir, latest.millis));
            }
            return Ok(());
        }
    }
    std::fs::create_dir_all(&dir)?;
    write_snapshot(&dir, millis, content)?;
    prune_in(root_dir, abs_path);
    Ok(())
}

/// Put `content` at `<millis>.snap` under `dir` so that the entry is never
/// listed before its bytes are all there. The save path records off the UI
/// thread while readers (`entries_in` from the history picker, or a test's
/// byte-exact check) list the same directory, and a plain `fs::write`
/// creates the file empty first: a reader in that window saw a listed
/// snapshot with no bytes (#492). The bytes go to a sibling staging name
/// (see [`staging_path`]), which `entries_in` skips on extension, and are
/// renamed over the final name afterwards; a rename is atomic on the same
/// filesystem. On failure the staging file is removed rather than left to
/// look like history.
///
/// The writer holds an exclusive advisory lock on its staging file from the
/// write through the rename, so the sweep of abandoned staging files (see
/// `sweep_abandoned_staging`) can tell a writer that is merely slow from one
/// that died: age alone must not decide it, or a writer paused past the
/// window would come back to find its bytes swept and its rename failing.
fn write_snapshot(dir: &Path, millis: u64, content: &[u8]) -> std::io::Result<()> {
    write_staged(dir, millis, SNAP_EXT, content)
}

/// The staged write behind [`write_snapshot`], for any of a snapshot's files
/// (`ext` names which): bytes to the staging name, then a rename over
/// `<millis>.<ext>`.
fn write_staged(dir: &Path, millis: u64, ext: &str, content: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let final_path = dir.join(format!("{millis}.{ext}"));
    let tmp = staging_path(dir, millis);
    let written = (|| {
        let mut file = std::fs::File::create(&tmp)?;
        file.lock()?;
        file.write_all(content)?;
        // Windows cannot rename a file that is open; the lock is released
        // with the handle there, and the sweep falls back to age alone.
        #[cfg(windows)]
        drop(file);
        std::fs::rename(&tmp, &final_path)?;
        #[cfg(not(windows))]
        drop(file);
        Ok(())
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// The extension every staging file carries, so `entries_in` and the
/// cleanup in `prune_in` recognise one without parsing the rest of its name.
const STAGING_EXT: &str = "tmp";

/// Staging files older than this, and not locked by a live writer, are
/// abandoned (a crash between the write and the rename) and swept by
/// `prune_in`. The age keeps the sweep away from the instant between a
/// writer creating its file and taking the lock; the lock is what protects
/// a writer that is merely slow.
const STAGING_ABANDONED_AFTER: std::time::Duration = std::time::Duration::from_secs(60);

/// A staging name unique to this writer: `<millis>.snap.<pid>-<seq>.tmp`
/// (the `snap` is fixed whatever the final extension: a `.seats` sidecar
/// stages under the same shape, and the sweep keys on `.tmp` alone).
/// Two saves of one file in the same millisecond (two croft instances, or
/// an explicit save racing the auto save) must not share a staging file, or
/// each would report success while the final snapshot held only one's
/// bytes. The pid separates processes and the counter separates threads
/// inside one.
fn staging_path(dir: &Path, millis: u64) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let pid = std::process::id();
    dir.join(format!("{millis}.{SNAP_EXT}.{pid}-{seq}.{STAGING_EXT}"))
}

/// Remove staging files left behind by a writer that died between its write
/// and its rename. Listing and retention both ignore them, so without this
/// sweep every interrupted save would leave a hidden snapshot-sized file
/// forever. A file goes only when it is older than
/// [`STAGING_ABANDONED_AFTER`] AND its writer's lock can be taken: a writer
/// still alive holds that lock (see `write_snapshot`), however long it has
/// been paused, so it is never swept out from under a rename.
fn sweep_abandoned_staging(dir: &Path, now: std::time::SystemTime) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.extension().and_then(|x| x.to_str()) != Some(STAGING_EXT) {
            continue;
        }
        let old_enough = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|at| now.duration_since(at).ok())
            .is_some_and(|age| age > STAGING_ABANDONED_AFTER);
        if !old_enough {
            continue;
        }
        // Taking the lock proves no writer holds it; it is released with
        // the handle once the file is gone.
        let Ok(file) = std::fs::File::open(&path) else {
            continue;
        };
        if file.try_lock().is_ok() {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// The extension of the sidecar that records who typed each line of a
/// snapshot (#349): `<millis>.seats` beside `<millis>.snap`, JSON, written
/// through the same staging-and-rename as the snapshot so it is never
/// listed half-written, and removed whenever its snapshot is.
const SEATS_EXT: &str = "seats";

/// Record who typed each line beside the snapshot taken at `millis` (#349).
/// A map with nothing attributed writes nothing: an absent sidecar and an
/// empty one read the same, and the common case (a file nobody's seat has
/// touched) should not grow the history dir.
fn record_seats_in(
    root_dir: &Path,
    abs_path: &Path,
    millis: u64,
    seats: &crate::provenance::Provenance,
) -> std::io::Result<()> {
    if seats.attributed() == 0 {
        return Ok(());
    }
    let dir = dir_for(root_dir, abs_path);
    let json = serde_json::to_vec(seats).map_err(std::io::Error::other)?;
    std::fs::create_dir_all(&dir)?;
    write_staged(&dir, millis, SEATS_EXT, &json)
}

/// Record `content` as [`record_in`] does, then the seats beside whichever
/// snapshot now holds that content: the one just written, or the newest
/// existing one when the save changed nothing (`record_in` skips those).
pub fn record_with_seats_in(
    root_dir: &Path,
    abs_path: &Path,
    content: &[u8],
    millis: u64,
    seats: &crate::provenance::Provenance,
) -> std::io::Result<()> {
    record_in(root_dir, abs_path, content, millis)?;
    // Nothing attributed writes nothing (see `record_seats_in`), so skip the
    // listing and the snapshot read that would find its holder.
    if seats.attributed() == 0 {
        return Ok(());
    }
    let Some(holder) = entries_in(root_dir, abs_path)
        .into_iter()
        .find(|snap| std::fs::read(&snap.file).is_ok_and(|bytes| bytes == content))
    else {
        return Ok(());
    };
    record_seats_in(root_dir, abs_path, holder.millis, seats)
}

/// The seats recorded beside the snapshot taken at `millis`, if any (#349).
/// A sidecar that does not parse reads as nothing: a line is unknown, never
/// guessed from a record that cannot be trusted.
pub fn seats_for(
    root_dir: &Path,
    abs_path: &Path,
    millis: u64,
) -> Option<crate::provenance::Provenance> {
    let bytes = std::fs::read(seats_path(&dir_for(root_dir, abs_path), millis)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// The sidecar path for the snapshot taken at `millis`, for removal with it.
fn seats_path(dir: &Path, millis: u64) -> PathBuf {
    dir.join(format!("{millis}.{SEATS_EXT}"))
}

/// Snapshots of `abs_path` under `root_dir`, newest first.
pub fn entries_in(root_dir: &Path, abs_path: &Path) -> Vec<Snapshot> {
    let dir = dir_for(root_dir, abs_path);
    let Ok(read) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<Snapshot> = read
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let file = e.path();
            if file.extension().and_then(|x| x.to_str()) != Some(SNAP_EXT) {
                return None;
            }
            let millis: u64 = file.file_stem()?.to_str()?.parse().ok()?;
            Some(Snapshot { millis, file })
        })
        .collect();
    out.sort_by_key(|s| std::cmp::Reverse(s.millis));
    out
}

/// Delete the oldest snapshots of `abs_path` beyond the cap, and any staging
/// file abandoned there (see `sweep_abandoned_staging`).
fn prune_in(root_dir: &Path, abs_path: &Path) {
    let dir = dir_for(root_dir, abs_path);
    let all = entries_in(root_dir, abs_path);
    for old in all.into_iter().skip(MAX_SNAPSHOTS_PER_FILE) {
        let _ = std::fs::remove_file(&old.file);
        let _ = std::fs::remove_file(seats_path(&dir, old.millis));
    }
    sweep_abandoned_staging(&dir, std::time::SystemTime::now());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_snapshot_is_staged_under_a_name_the_listing_ignores() {
        // The race in #492: `record_in` must never expose an entry whose
        // bytes are not all on disk. The staging name is what makes that
        // hold, so pin both halves: a leftover staging file (a crash between
        // write and rename) is not a snapshot, and a completed record leaves
        // only the final name behind.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let f = Path::new("/work/a.rs");
        record_in(root, f, b"v1", 1000).unwrap();
        let dir = dir_for(root, f);
        std::fs::write(staging_path(&dir, 2000), b"half").unwrap();
        let snaps = entries_in(root, f);
        assert_eq!(snaps.len(), 1, "the staging file is not listed: {snaps:?}");
        assert_eq!(snaps[0].millis, 1000);
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&String::from("1000.snap")), "{names:?}");
        assert!(
            !names.iter().any(|n| n.starts_with("1000.snap.")),
            "a completed record leaves no staging file: {names:?}"
        );
    }

    #[test]
    fn each_writer_stages_under_its_own_name() {
        // Two saves of one file in the same millisecond must not share a
        // staging file: the name carries the pid and a per-process counter,
        // and still ends in the extension the listing and the sweep key on.
        let dir = Path::new("/store");
        let a = staging_path(dir, 1000);
        let b = staging_path(dir, 1000);
        assert_ne!(a, b, "two writers, two staging files");
        for p in [&a, &b] {
            assert_eq!(p.extension().and_then(|x| x.to_str()), Some(STAGING_EXT));
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            assert!(
                name.starts_with(&format!("1000.{SNAP_EXT}.{}-", std::process::id())),
                "{name}"
            );
        }
    }

    #[test]
    fn an_abandoned_staging_file_is_swept_but_a_live_one_is_left() {
        // A crash between write and rename leaves a staging file. The next
        // record sweeps it once it is older than the abandonment window; a
        // fresh one (another writer mid-write) survives the same sweep.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let f = Path::new("/work/a.rs");
        record_in(root, f, b"v1", 1000).unwrap();
        let dir = dir_for(root, f);
        let stale = staging_path(&dir, 900);
        std::fs::write(&stale, b"half").unwrap();
        let long_ago = std::time::SystemTime::now() - STAGING_ABANDONED_AFTER * 10;
        std::fs::File::options()
            .write(true)
            .open(&stale)
            .unwrap()
            .set_modified(long_ago)
            .unwrap();
        let fresh = staging_path(&dir, 950);
        std::fs::write(&fresh, b"half").unwrap();
        record_in(root, f, b"v2", 120_000).unwrap();
        assert!(!stale.exists(), "the abandoned staging file was swept");
        assert!(fresh.exists(), "the live one was left alone");
        assert_eq!(entries_in(root, f).len(), 2, "and the snapshots are intact");
    }

    #[cfg(not(windows))]
    #[test]
    fn a_slow_writer_keeps_its_staging_file_however_old_it_looks() {
        // Age alone must not decide a sweep: a writer paused between its
        // write and its rename still holds the lock on its staging file, and
        // the sweep skips it. Once the writer lets go, the same file goes.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let f = Path::new("/work/a.rs");
        record_in(root, f, b"v1", 1000).unwrap();
        let dir = dir_for(root, f);
        let paused = staging_path(&dir, 900);
        let holder = std::fs::File::create(&paused).unwrap();
        holder.lock().unwrap();
        holder
            .set_modified(std::time::SystemTime::now() - STAGING_ABANDONED_AFTER * 10)
            .unwrap();
        record_in(root, f, b"v2", 120_000).unwrap();
        assert!(
            paused.exists(),
            "a locked staging file is not swept, whatever its age"
        );
        drop(holder);
        record_in(root, f, b"v3", 240_000).unwrap();
        assert!(
            !paused.exists(),
            "released, it is swept like any abandoned one"
        );
    }

    #[test]
    fn a_record_that_cannot_land_leaves_no_staging_file_behind() {
        // The cleanup branch of the staged write: a rename that fails (the
        // final name is taken by a directory, which `rename` refuses on
        // every platform) surfaces as the error `record_in` returns, and
        // the staging file is removed rather than left to look like history
        // to anyone listing the directory by hand.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let f = Path::new("/work/a.rs");
        let dir = dir_for(root, f);
        std::fs::create_dir_all(dir.join("1000.snap")).unwrap();
        assert!(
            record_in(root, f, b"v1", 1000).is_err(),
            "a snapshot that cannot land is reported"
        );
        let leftovers: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(STAGING_EXT))
            .collect();
        assert!(
            leftovers.is_empty(),
            "and its staging file is gone: {leftovers:?}"
        );
    }

    #[test]
    fn records_and_lists_snapshots_newest_first() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let f = Path::new("/work/a.rs");
        record_in(root, f, b"v1", 1000).unwrap();
        record_in(root, f, b"v2", 60_000).unwrap();
        record_in(root, f, b"v3", 120_000).unwrap();
        let e = entries_in(root, f);
        assert_eq!(e.len(), 3);
        assert_eq!(e[0].millis, 120_000, "newest first");
        assert_eq!(e[2].millis, 1000);
        assert_eq!(std::fs::read_to_string(&e[0].file).unwrap(), "v3");
    }

    #[test]
    fn identical_consecutive_saves_are_deduplicated() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let f = Path::new("/work/a.rs");
        record_in(root, f, b"same", 1000).unwrap();
        record_in(root, f, b"same", 60_000).unwrap();
        assert_eq!(
            entries_in(root, f).len(),
            1,
            "an unchanged save must not add a snapshot"
        );
        // A real change still records.
        record_in(root, f, b"different", 120_000).unwrap();
        assert_eq!(entries_in(root, f).len(), 2);
    }

    #[test]
    fn distinct_files_never_share_a_key() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        record_in(root, Path::new("/work/a.rs"), b"a", 1).unwrap();
        record_in(root, Path::new("/work/b.rs"), b"b", 1).unwrap();
        assert_eq!(entries_in(root, Path::new("/work/a.rs")).len(), 1);
        assert_eq!(entries_in(root, Path::new("/work/b.rs")).len(), 1);
        assert_eq!(
            std::fs::read_to_string(&entries_in(root, Path::new("/work/a.rs"))[0].file).unwrap(),
            "a"
        );
    }

    #[test]
    fn old_snapshots_are_pruned_beyond_the_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let f = Path::new("/work/a.rs");
        for i in 0..(MAX_SNAPSHOTS_PER_FILE as u64 + 10) {
            // Vary content so nothing dedups.
            record_in(root, f, format!("v{i}").as_bytes(), (i + 1) * 60_000).unwrap();
        }
        let e = entries_in(root, f);
        assert_eq!(e.len(), MAX_SNAPSHOTS_PER_FILE, "capped");
        assert_eq!(
            e[0].millis,
            (MAX_SNAPSHOTS_PER_FILE as u64 + 10) * 60_000,
            "the newest survives"
        );
        assert!(
            e.last().unwrap().millis > 10 * 60_000,
            "the oldest snapshots were pruned, not the newest"
        );
    }

    #[test]
    fn entries_for_an_unknown_file_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(entries_in(tmp.path(), Path::new("/nope.rs")).is_empty());
    }

    /// Auto save fires every second, so without a merge window the 50-snap cap
    /// becomes a keystroke log and real history evaporates in minutes. VS Code
    /// replaces the newest entry inside a 10s window instead of appending.
    #[test]
    fn saves_within_the_merge_window_replace_the_newest_snapshot() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let f = Path::new("/work/a.rs");
        record_in(root, f, b"v1", 1_000).unwrap();
        record_in(root, f, b"v2", 60_000).unwrap();
        // 5s after v2: inside the window, so v3 replaces v2 rather than piling on.
        record_in(root, f, b"v3", 65_000).unwrap();
        let e = entries_in(root, f);
        assert_eq!(e.len(), 2, "the within-window save must merge, not append");
        assert_eq!(e[0].millis, 65_000);
        assert_eq!(std::fs::read(&e[0].file).unwrap(), b"v3");
        assert_eq!(
            std::fs::read(&e[1].file).unwrap(),
            b"v1",
            "history older than the window survives the churn"
        );
    }

    /// The store must hold raw bytes: a UTF-16 file is first-class in the
    /// editor, so its history must round-trip byte-exactly too.
    #[test]
    fn snapshots_hold_raw_bytes_so_non_utf8_files_have_history_too() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let f = Path::new("/work/u16.txt");
        let utf16 = b"\xff\xfeh\x00i\x00";
        record_in(root, f, utf16, 1_000).unwrap();
        let e = entries_in(root, f);
        assert_eq!(std::fs::read(&e[0].file).unwrap(), utf16);
    }

    /// The directory key is pinned to FNV-1a 64: `DefaultHasher`'s algorithm is
    /// unspecified across Rust releases, so keying on it orphans every stored
    /// snapshot at a toolchain bump.
    #[test]
    fn the_store_key_is_a_stable_hash_not_default_hasher() {
        assert_eq!(key_for(Path::new("/work/a.rs")), "04a67ef4ed166070");
    }

    /// A store written by a pre-FNV build (keyed by `DefaultHasher`) is
    /// migrated by renaming its directory the first time the file is touched.
    #[test]
    fn a_default_hasher_keyed_store_is_migrated_to_the_stable_key() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let f = Path::new("/work/a.rs");
        let old_dir = root.join(legacy_key_for(f));
        std::fs::create_dir_all(&old_dir).unwrap();
        std::fs::write(old_dir.join("1000.snap"), b"old").unwrap();
        let e = entries_in(root, f);
        assert_eq!(e.len(), 1, "pre-FNV snapshots must be found via migration");
        assert!(
            root.join(key_for(f)).is_dir() && !old_dir.exists(),
            "the legacy directory must be renamed to the stable key"
        );
    }

    #[test]
    fn to_timeline_entries_tags_local_rows_and_dates_them() {
        let snaps = vec![
            Snapshot {
                millis: 9000,
                file: PathBuf::from("/x/9000.snap"),
            },
            Snapshot {
                millis: 3000,
                file: PathBuf::from("/x/3000.snap"),
            },
        ];
        let rows = to_timeline_entries(&snaps, 10_000);
        assert_eq!(rows[0].short_hash, "local:9000");
        assert_eq!(rows[0].author, "You");
        assert_eq!(rows[0].summary, "Local snapshot");
        assert_eq!(rows[0].age_secs, 1, "(10000-9000)/1000 = 1s");
        assert_eq!(rows[1].age_secs, 7);
        assert_eq!(snapshot_millis("local:9000"), Some(9000));
        assert_eq!(
            snapshot_millis("abc123"),
            None,
            "a git hash is not a snapshot"
        );
    }

    #[test]
    fn merged_timeline_interleaves_commits_and_snapshots_newest_first() {
        use crate::git::FileHistoryEntry;
        let commits = vec![
            FileHistoryEntry {
                short_hash: "c1".into(),
                summary: "new".into(),
                author: "a".into(),
                age_secs: 2,
            },
            FileHistoryEntry {
                short_hash: "c2".into(),
                summary: "old".into(),
                author: "a".into(),
                age_secs: 100,
            },
        ];
        // A snapshot 5s old should sort between the two commits.
        let snaps = vec![Snapshot {
            millis: 5000,
            file: PathBuf::from("/x/5000.snap"),
        }];
        let merged = merged_timeline(commits, &snaps, 10_000);
        let hashes: Vec<&str> = merged.iter().map(|e| e.short_hash.as_str()).collect();
        assert_eq!(hashes, vec!["c1", "local:5000", "c2"], "sorted by age");
    }

    /// Who typed each line is recorded beside the snapshot it describes and
    /// read back by the snapshot's timestamp (#349); a snapshot with no
    /// record reads as nothing, never as a guess.
    #[test]
    fn seats_are_recorded_beside_their_snapshot_and_read_back() {
        use crate::provenance::{Provenance, Seat};
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let f = tmp.path().join("a.txt");
        record_in(root, &f, b"one\ntwo\n", 1_000).unwrap();
        let mut seats = Provenance::new();
        seats.record(0..1, Seat::Navigator);
        seats.record(1..2, Seat::Peer(String::from("ada")));
        record_seats_in(root, &f, 1_000, &seats).unwrap();
        let back = seats_for(root, &f, 1_000).expect("the record is read back");
        assert_eq!(back.seat(0), Some(&Seat::Navigator));
        assert_eq!(back.seat(1), Some(&Seat::Peer(String::from("ada"))));
        assert_eq!(back.seat(2), None);
        assert!(
            seats_for(root, &f, 2_000).is_none(),
            "a timestamp with no record reads as nothing"
        );
    }

    /// A snapshot superseded inside the merge window takes its sidecar with
    /// it (#349): the seats described bytes that no longer have a snapshot.
    #[test]
    fn a_superseded_snapshot_takes_its_seats_with_it() {
        use crate::provenance::{Provenance, Seat};
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let f = tmp.path().join("a.txt");
        let mut seats = Provenance::new();
        seats.record(0..1, Seat::Navigator);
        record_in(root, &f, b"v1\n", 1_000).unwrap();
        record_seats_in(root, &f, 1_000, &seats).unwrap();
        assert!(
            seats_for(root, &f, 1_000).is_some(),
            "control: the sidecar was written"
        );
        record_in(root, &f, b"v2\n", 5_000).unwrap();
        assert!(
            seats_for(root, &f, 1_000).is_none(),
            "the superseded snapshot's seats are gone"
        );
    }

    /// Pruning past the cap removes the evicted snapshot's sidecar and keeps
    /// a survivor's (#349).
    #[test]
    fn a_pruned_snapshot_takes_its_seats_with_it() {
        use crate::provenance::{Provenance, Seat};
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let f = tmp.path().join("a.txt");
        let mut seats = Provenance::new();
        seats.record(0..1, Seat::Me);
        // A minute apart, so none merge; each with a sidecar.
        for n in 0..=MAX_SNAPSHOTS_PER_FILE as u64 {
            let millis = 60_000 * (n + 1);
            record_in(root, &f, format!("v{n}\n").as_bytes(), millis).unwrap();
            record_seats_in(root, &f, millis, &seats).unwrap();
        }
        assert!(
            seats_for(root, &f, 60_000).is_none(),
            "the evicted first snapshot's seats are gone"
        );
        let newest = 60_000 * (MAX_SNAPSHOTS_PER_FILE as u64 + 1);
        assert!(
            seats_for(root, &f, newest).is_some(),
            "the newest keeps its seats"
        );
    }

    /// A save that changes nothing still attaches its seats to the snapshot
    /// that holds those bytes, and an unattributed map writes no sidecar.
    #[test]
    fn seats_attach_to_the_snapshot_holding_the_bytes_and_an_empty_map_writes_nothing() {
        use crate::provenance::{Provenance, Seat};
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let f = tmp.path().join("a.txt");
        record_in(root, &f, b"v1\n", 1_000).unwrap();
        let mut seats = Provenance::new();
        seats.record(0..1, Seat::Generated);
        // Identical content much later: `record_in` skips, the seats attach
        // to the existing holder, and no snapshot appears at the new time.
        record_with_seats_in(root, &f, b"v1\n", 60_000, &seats).unwrap();
        assert!(
            seats_for(root, &f, 1_000).is_some(),
            "attached to the holder"
        );
        assert!(
            seats_for(root, &f, 60_000).is_none(),
            "nothing at the skipped time"
        );
        let g = tmp.path().join("b.txt");
        record_with_seats_in(root, &g, b"plain\n", 1_000, &Provenance::new()).unwrap();
        let dir = dir_for(root, &g);
        let sidecars = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some(SEATS_EXT))
            .count();
        assert_eq!(sidecars, 0, "an unattributed map writes no sidecar");
    }
}
