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

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

/// Cap per file so a hot save loop can't fill the disk; oldest pruned first.
const MAX_SNAPSHOTS_PER_FILE: usize = 50;

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
    let file = root_dir
        .join(key_for(abs_path))
        .join(format!("{millis}.{SNAP_EXT}"));
    file.is_file().then_some(file)
}

/// Stable directory name for `abs_path`: a hex hash of its string form, so two
/// files never collide and no path-to-key index is needed.
fn key_for(abs_path: &Path) -> String {
    let mut h = DefaultHasher::new();
    abs_path.to_string_lossy().hash(&mut h);
    format!("{:016x}", h.finish())
}

/// Record `content` as a new snapshot of `abs_path` under `root_dir` (the
/// history root; separated out so tests can point it at a tempdir). A no-op
/// when `content` matches the newest snapshot, so idle saves don't pile up.
/// Prunes to [`MAX_SNAPSHOTS_PER_FILE`]. Best-effort: any IO error is
/// returned but a caller may ignore it (history is a convenience, not a
/// guarantee).
pub fn record_in(
    root_dir: &Path,
    abs_path: &Path,
    content: &str,
    millis: u64,
) -> std::io::Result<()> {
    let dir = root_dir.join(key_for(abs_path));
    // Skip when the newest snapshot already holds this exact content.
    if let Some(latest) = entries_in(root_dir, abs_path).first()
        && std::fs::read_to_string(&latest.file).is_ok_and(|prev| prev == content)
    {
        return Ok(());
    }
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(format!("{millis}.{SNAP_EXT}")), content)?;
    prune_in(root_dir, abs_path);
    Ok(())
}

/// Snapshots of `abs_path` under `root_dir`, newest first.
pub fn entries_in(root_dir: &Path, abs_path: &Path) -> Vec<Snapshot> {
    let dir = root_dir.join(key_for(abs_path));
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

/// Delete the oldest snapshots of `abs_path` beyond the cap.
fn prune_in(root_dir: &Path, abs_path: &Path) {
    let all = entries_in(root_dir, abs_path);
    for old in all.into_iter().skip(MAX_SNAPSHOTS_PER_FILE) {
        let _ = std::fs::remove_file(&old.file);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_and_lists_snapshots_newest_first() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let f = Path::new("/work/a.rs");
        record_in(root, f, "v1", 1000).unwrap();
        record_in(root, f, "v2", 2000).unwrap();
        record_in(root, f, "v3", 3000).unwrap();
        let e = entries_in(root, f);
        assert_eq!(e.len(), 3);
        assert_eq!(e[0].millis, 3000, "newest first");
        assert_eq!(e[2].millis, 1000);
        assert_eq!(std::fs::read_to_string(&e[0].file).unwrap(), "v3");
    }

    #[test]
    fn identical_consecutive_saves_are_deduplicated() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let f = Path::new("/work/a.rs");
        record_in(root, f, "same", 1000).unwrap();
        record_in(root, f, "same", 2000).unwrap();
        assert_eq!(
            entries_in(root, f).len(),
            1,
            "an unchanged save must not add a snapshot"
        );
        // A real change still records.
        record_in(root, f, "different", 3000).unwrap();
        assert_eq!(entries_in(root, f).len(), 2);
    }

    #[test]
    fn distinct_files_never_share_a_key() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        record_in(root, Path::new("/work/a.rs"), "a", 1).unwrap();
        record_in(root, Path::new("/work/b.rs"), "b", 1).unwrap();
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
            record_in(root, f, &format!("v{i}"), i + 1).unwrap();
        }
        let e = entries_in(root, f);
        assert_eq!(e.len(), MAX_SNAPSHOTS_PER_FILE, "capped");
        assert_eq!(
            e[0].millis,
            MAX_SNAPSHOTS_PER_FILE as u64 + 10,
            "the newest survives"
        );
        assert!(
            e.last().unwrap().millis > 10,
            "the oldest snapshots were pruned, not the newest"
        );
    }

    #[test]
    fn entries_for_an_unknown_file_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(entries_in(tmp.path(), Path::new("/nope.rs")).is_empty());
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
}
