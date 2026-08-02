//! Durable, context-rich shell command history (the atuin model, embedded):
//! every command a shell-integrated pane finishes is recorded with its
//! working directory, exit code, duration and timestamp, in a JSONL file
//! under the croft config dir. The Ctrl+Shift+R popup searches it across
//! sessions and restarts — newest first, deduplicated, filterable to the
//! current directory or to failures only.
//!
//! The file is append-only in the hot path (one `serde_json` line per
//! finished command); it is compacted back to the newest [`HISTORY_MAX`]
//! entries when it grows past twice that, so a long-lived install never
//! rereads an unbounded file.

use std::io::Write;
use std::path::{Path, PathBuf};

/// Newest entries kept in memory and, after compaction, on disk.
pub const HISTORY_MAX: usize = 10_000;

/// Collapse every way OSC 7 spells "this machine" — empty (no integration,
/// legacy entries), "localhost" (shim fallback), or the machine's own
/// hostname — into "", so the Dir scope's host pairing only separates REAL
/// remote hosts.
fn canon_host(h: &str) -> &str {
    if h.is_empty() || h.eq_ignore_ascii_case("localhost") || is_local_hostname(h) {
        ""
    } else {
        h
    }
}

/// Whether an OSC 7 reporting host means "this machine" (empty, the RFC
/// 8089 "localhost", or the machine's own hostname). Shared with every
/// consumer that acts on a shell-reported path on the LOCAL filesystem.
pub fn is_local_host(h: &str) -> bool {
    canon_host(h).is_empty()
}

/// Whether `h` names this machine (case-insensitive, with or without the
/// mDNS-style domain the shells sometimes include).
fn is_local_hostname(h: &str) -> bool {
    static LOCAL: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let local = LOCAL.get_or_init(|| {
        let mut buf = [0u8; 256];
        // SAFETY: gethostname writes at most buf.len() bytes and
        // NUL-terminates within it on every supported platform.
        let ok = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } == 0;
        if !ok {
            return String::new();
        }
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        String::from_utf8_lossy(&buf[..end]).to_string()
    });
    if local.is_empty() {
        return false;
    }
    let short = |s: &str| s.split('.').next().unwrap_or(s).to_ascii_lowercase();
    short(h) == short(local)
}

/// One finished shell command.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HistoryEntry {
    pub cmd: String,
    /// The pane's OSC 7 working directory at finish time; empty when the
    /// shell never reported one.
    #[serde(default)]
    pub cwd: String,
    /// The OSC 7 reporting hostname at finish time; empty for the local
    /// machine. Without it, a command run at /home/dev/api over an in-pane
    /// ssh session is indistinguishable from one run at the same local
    /// path, and the Dir scope conflates them.
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub exit: Option<i32>,
    #[serde(default)]
    pub dur_ms: u64,
    /// Seconds since the Unix epoch.
    #[serde(default)]
    pub ts: u64,
}

/// Which slice of history the popup searches; Ctrl+R cycles it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum HistoryScope {
    #[default]
    All,
    /// Only commands finished in the given directory.
    Dir,
    /// Only commands that exited non-zero (or died without a code).
    Failed,
}

impl HistoryScope {
    pub fn next(self) -> Self {
        match self {
            Self::All => Self::Dir,
            Self::Dir => Self::Failed,
            Self::Failed => Self::All,
        }
    }
}

/// The real store path, `~/.config/croft/command_history.jsonl`. The App
/// caches a loaded store in a field so tests can point it at a tempdir.
pub fn history_path() -> PathBuf {
    crate::prefs::config_dir().join("command_history.jsonl")
}

pub struct CommandHistory {
    path: PathBuf,
    pub entries: Vec<HistoryEntry>,
    /// Lines on disk (loaded + appended); drives compaction.
    disk_lines: usize,
}

impl CommandHistory {
    /// Load the store, tolerantly: an unparsable line is skipped, a missing
    /// file is an empty history. Only the newest [`HISTORY_MAX`] survive.
    pub fn load(path: &Path) -> Self {
        let raw = std::fs::read_to_string(path).unwrap_or_default();
        let mut entries: Vec<HistoryEntry> = raw
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        let disk_lines = entries.len();
        if entries.len() > HISTORY_MAX {
            let drop = entries.len() - HISTORY_MAX;
            entries.drain(..drop);
        }
        Self {
            path: path.to_path_buf(),
            entries,
            disk_lines,
        }
    }

    // Read by the app tests' wait loops; the app itself only appends.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Record a finished command: append to memory and to the JSONL file,
    /// compacting the file once it doubles past the cap.
    pub fn append(&mut self, entry: HistoryEntry) {
        if entry.cmd.trim().is_empty() {
            return;
        }
        let line = match serde_json::to_string(&entry) {
            Ok(l) => l,
            Err(_) => return,
        };
        self.entries.push(entry);
        if self.entries.len() > HISTORY_MAX {
            let drop = self.entries.len() - HISTORY_MAX;
            self.entries.drain(..drop);
        }
        self.disk_lines += 1;
        if self.disk_lines > HISTORY_MAX * 2 {
            self.rewrite();
            return;
        }
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let _ = writeln!(f, "{line}");
        }
    }

    /// Rewrite the file with just the in-memory entries (compaction).
    fn rewrite(&mut self) {
        let mut out = String::new();
        for e in &self.entries {
            if let Ok(l) = serde_json::to_string(e) {
                out.push_str(&l);
                out.push('\n');
            }
        }
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&self.path, out);
        self.disk_lines = self.entries.len();
    }

    /// Search: newest first, one row per distinct command text (the newest
    /// occurrence wins, like atuin), case-insensitive substring `query`,
    /// narrowed by `scope` (`cwd` is the pane's current directory for
    /// [`HistoryScope::Dir`]).
    pub fn search(
        &self,
        query: &str,
        scope: HistoryScope,
        cwd: &str,
        host: &str,
    ) -> Vec<HistoryEntry> {
        let q = query.to_lowercase();
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for e in self.entries.iter().rev() {
            match scope {
                HistoryScope::All => {}
                HistoryScope::Dir => {
                    // Same directory means same directory on the same
                    // machine: the host pairs with the path. Local hosts
                    // normalize to one value — the shims report the local
                    // hostname (or "localhost"), while legacy entries and
                    // panes without integration carry "" — so a local pane
                    // still sees its own older history.
                    if e.cwd != cwd || canon_host(&e.host) != canon_host(host) {
                        continue;
                    }
                }
                HistoryScope::Failed => {
                    if e.exit == Some(0) {
                        continue;
                    }
                }
            }
            if !q.is_empty() && !e.cmd.to_lowercase().contains(&q) {
                continue;
            }
            if seen.insert(e.cmd.clone()) {
                out.push(e.clone());
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(cmd: &str, cwd: &str, exit: Option<i32>, ts: u64) -> HistoryEntry {
        HistoryEntry {
            cmd: cmd.to_string(),
            cwd: cwd.to_string(),
            host: String::new(),
            exit,
            dur_ms: 42,
            ts,
        }
    }

    /// A command run at /x over an in-pane ssh session must not surface in
    /// the Dir scope of a LOCAL pane sitting at /x — same path, different
    /// machine.
    #[test]
    fn dir_scope_separates_hosts_sharing_a_path() {
        let tmp = tempfile::tempdir().unwrap();
        let mut h = CommandHistory::load(&tmp.path().join("h.jsonl"));
        h.append(entry("local-build", "/x", Some(0), 1));
        let mut remote = entry("remote-build", "/x", Some(0), 2);
        remote.host = String::from("prod-box");
        h.append(remote);
        let local = h.search("", HistoryScope::Dir, "/x", "");
        assert_eq!(local.len(), 1, "only the local /x command");
        assert_eq!(local[0].cmd, "local-build");
        let remote = h.search("", HistoryScope::Dir, "/x", "prod-box");
        assert_eq!(remote.len(), 1, "only the remote /x command");
        assert_eq!(remote[0].cmd, "remote-build");

        // Every spelling of "this machine" is one host: a legacy entry
        // (host "") must surface in a pane whose shim reports "localhost",
        // and a "localhost" entry in a pane with no integration.
        let mut shimmed = entry("shimmed-build", "/x", Some(0), 3);
        shimmed.host = String::from("localhost");
        h.append(shimmed);
        let seen = h.search("", HistoryScope::Dir, "/x", "localhost");
        let cmds: Vec<&str> = seen.iter().map(|e| e.cmd.as_str()).collect();
        assert!(
            cmds.contains(&"local-build") && cmds.contains(&"shimmed-build"),
            "legacy and shim-reported local entries are one history: {cmds:?}"
        );
        assert!(!cmds.contains(&"remote-build"));

        // Schema round trip: a reload keeps the stored host, and a legacy
        // JSONL record with no "host" key deserializes to the local "".
        let re = CommandHistory::load(&h.path);
        let back = re.entries.iter().find(|e| e.cmd == "remote-build").unwrap();
        assert_eq!(back.host, "prod-box", "the host survives the reload");
        let legacy: HistoryEntry =
            serde_json::from_str(r#"{"cmd":"old","cwd":"/x","exit":0,"dur_ms":1,"ts":1}"#).unwrap();
        assert_eq!(legacy.host, "", "a pre-host record defaults to local");
    }

    #[test]
    fn append_persists_and_load_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("h.jsonl");
        let mut h = CommandHistory::load(&path);
        assert!(h.is_empty());
        h.append(entry("cargo test", "/repo", Some(0), 100));
        h.append(entry("ls -la", "/tmp", Some(0), 101));
        h.append(entry("   ", "/tmp", Some(0), 102)); // blank: dropped
        let re = CommandHistory::load(&path);
        assert_eq!(re.entries.len(), 2, "blank commands are never recorded");
        assert_eq!(re.entries[0].cmd, "cargo test");
        assert_eq!(re.entries[1].cwd, "/tmp");
        // A corrupt line is skipped, not fatal.
        std::fs::write(
            &path,
            format!(
                "not json at all\n{}\n",
                serde_json::to_string(&entry("ok", "/x", Some(1), 103)).unwrap()
            ),
        )
        .unwrap();
        let re = CommandHistory::load(&path);
        assert_eq!(re.entries.len(), 1);
        assert_eq!(re.entries[0].cmd, "ok");
    }

    #[test]
    fn search_dedups_newest_first_and_honours_scopes() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("h.jsonl");
        let mut h = CommandHistory::load(&path);
        h.append(entry("cargo test", "/repo", Some(0), 1));
        h.append(entry("cargo build", "/repo", Some(101), 2));
        h.append(entry("ls", "/tmp", Some(0), 3));
        h.append(entry("cargo test", "/repo", Some(1), 4)); // rerun, newest

        let all = h.search("", HistoryScope::All, "", "");
        assert_eq!(
            all.iter().map(|e| e.cmd.as_str()).collect::<Vec<_>>(),
            vec!["cargo test", "ls", "cargo build"],
            "newest first, duplicate command texts collapse to the newest run"
        );
        assert_eq!(all[0].exit, Some(1), "the newest occurrence wins");

        let q = h.search("CARGO", HistoryScope::All, "", "");
        assert_eq!(q.len(), 2, "query is case-insensitive substring");

        let dir = h.search("", HistoryScope::Dir, "/tmp", "");
        assert_eq!(
            dir.iter().map(|e| e.cmd.as_str()).collect::<Vec<_>>(),
            vec!["ls"]
        );

        let failed = h.search("", HistoryScope::Failed, "", "");
        assert_eq!(
            failed.iter().map(|e| e.cmd.as_str()).collect::<Vec<_>>(),
            vec!["cargo test", "cargo build"],
            "failed-only keeps non-zero exits"
        );
    }

    #[test]
    fn the_file_compacts_once_it_doubles_past_the_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("h.jsonl");
        let mut h = CommandHistory::load(&path);
        // Simulate an old, bloated file: disk_lines is what triggers
        // compaction, so seed it near the threshold instead of writing
        // 20k real lines.
        h.disk_lines = HISTORY_MAX * 2;
        for i in 0..3 {
            h.append(entry(&format!("cmd-{i}"), "/", Some(0), i as u64));
        }
        let lines = std::fs::read_to_string(&path).unwrap().lines().count();
        assert!(
            lines <= HISTORY_MAX,
            "compaction must rewrite the file down to the in-memory entries, got {lines} lines"
        );
        assert_eq!(h.disk_lines, h.entries.len());
    }
}
