//! Config sync (#262): the user's croft configuration follows them to a
//! remote over the SSH connection they already have.
//!
//! Connect to a fresh box today and you get default keybindings, no
//! snippets, no triggers — the binary follows you but the configuration
//! does not. This module names *what* may travel; [`crate::remote`] does
//! the travelling.
//!
//! # Why an allow-list, not a directory walk
//!
//! Every syncable file is named explicitly. Walking `config_dir()` and
//! excluding known-bad names would be shorter and is the wrong shape: a
//! file added later travels by default, and the failure is silent and
//! security-relevant. The same reasoning already governs
//! [`crate::config_layers::WORKSPACE_ALLOWED_KEYS`] and the remote install
//! stamp, whose deny-list ancestor once read a 98 GB `target.noindex`.
//!
//! Adding an entry here is a deliberate review decision: it means "this
//! file is safe to place on every machine the user connects to".

use std::path::PathBuf;

/// A file that may travel to a remote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Syncable {
    /// The name it lands under in the remote's `~/.config/croft/`.
    pub name: &'static str,
    /// The local file under `~/.config/croft/` it comes from. The same as
    /// `name` for a file that travels whole; `config.json` for the synced
    /// settings layer, which travels as a projection (see [`local_source`]).
    pub source: &'static str,
    /// Whether a running croft applies this file when it changes on disk:
    /// `reload_config_for_path` has an arm for it, reached both by croft's
    /// own save and by [`ConfigWatch`], which notices a file that ARRIVES by
    /// sync (or any other writer) within [`ConfigWatch::INTERVAL`].
    pub hot_reloads: bool,
}

/// Files that travel, in the order the OUTPUT channel lists them.
///
/// `config.json` itself never travels. It carries MCP consent
/// (`mcp_consented`), trust-on-first-use tool fingerprints
/// (`mcp_tool_fingerprints`), and `disabled_extensions` in the same
/// document as the appearance settings a user would want synced: sent as a
/// file, consent granted on the laptop would silently become consent on
/// every box they connect to, and the push would clobber consent granted on
/// the remote. What travels instead is its projection onto the keys a
/// workspace layer may set ([`crate::config_layers::synced_projection`]),
/// landing as the remote's `config.synced.json`, a layer of its own that
/// the remote's loader filters by the same allowlist on the way in.
pub const SYNCABLE: &[Syncable] = &[
    Syncable {
        name: crate::config_layers::SYNCED_CONFIG_NAME,
        source: "config.json",
        hot_reloads: true,
    },
    Syncable {
        name: "keybindings.json",
        source: "keybindings.json",
        hot_reloads: true,
    },
    Syncable {
        name: "snippets.json",
        source: "snippets.json",
        hot_reloads: true,
    },
    Syncable {
        name: "triggers.json",
        source: "triggers.json",
        hot_reloads: true,
    },
    Syncable {
        name: "matchers.json",
        source: "matchers.json",
        hot_reloads: true,
    },
    Syncable {
        name: "macros.json",
        source: "macros.json",
        hot_reloads: true,
    },
];

/// Names that must never travel, with the reason, so a future edit to
/// [`SYNCABLE`] has to argue with something rather than merely compile.
///
/// Checked by a test rather than at runtime: the allow-list is already
/// deny-by-default, and this exists to make the *intent* explicit and to
/// fail loudly if someone adds one of these to it. That makes it test-only
/// by construction, not dead code awaiting a caller.
#[cfg(test)]
pub const NEVER_SYNC: &[(&str, &str)] = &[
    (
        "config.json",
        "carries MCP consent and trust-on-first-use fingerprints; see #262",
    ),
    (
        "config.local.json",
        "the machine-local layer is machine-local by definition",
    ),
    (
        "history",
        "local history snapshots are per-machine working state",
    ),
    (
        "command_history.json",
        "command history is per-machine working state",
    ),
];

/// Notices croft's own config files changing on disk (#262).
///
/// The reload path used to run only when croft itself saved the file, so a
/// file that arrived by config sync, or was written by any other tool, was
/// not applied until the next launch. The editor's filesystem watcher covers
/// the workspace, not `~/.config/croft`, and a handful of known files does
/// not need one: this compares each file's modification time and length
/// every [`ConfigWatch::INTERVAL`]. A file that appears or disappears counts
/// as a change too, so deleting keybindings.json falls back to the defaults.
#[derive(Debug)]
pub struct ConfigWatch {
    files: Vec<(PathBuf, Option<Stamp>)>,
    next: std::time::Instant,
}

type Stamp = (std::time::SystemTime, u64);

fn stamp(path: &std::path::Path) -> Option<Stamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

impl ConfigWatch {
    /// How often the files are checked: a sync is noticed within this long.
    pub const INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

    /// Watch `paths`, taking their current state as already applied.
    pub fn new(paths: Vec<PathBuf>) -> Self {
        let files = paths
            .into_iter()
            .map(|p| {
                let s = stamp(&p);
                (p, s)
            })
            .collect();
        ConfigWatch {
            files,
            next: std::time::Instant::now() + Self::INTERVAL,
        }
    }

    /// The watched files that changed since they were last seen, once per
    /// [`Self::INTERVAL`]; empty between checks.
    pub fn poll(&mut self, now: std::time::Instant) -> Vec<PathBuf> {
        if now < self.next {
            return Vec::new();
        }
        self.next = now + Self::INTERVAL;
        let mut changed = Vec::new();
        for (path, seen) in &mut self.files {
            let current = stamp(path);
            if current != *seen {
                *seen = current;
                changed.push(path.clone());
            }
        }
        changed
    }

    /// Record `path` as applied in its current state, so a reload croft has
    /// just done itself (on its own save) is not repeated by the next poll.
    pub fn note(&mut self, path: &std::path::Path) {
        if let Some((_, seen)) = self.files.iter_mut().find(|(p, _)| p == path) {
            *seen = stamp(path);
        }
    }
}

/// Local paths of the syncable files that actually exist.
///
/// A missing file is skipped rather than erroring: a user with no snippets
/// is the common case, not a fault, and pushing a zero-byte file would
/// blank whatever the remote had.
pub fn local_files() -> Vec<(Syncable, PathBuf)> {
    let dir = crate::prefs::config_dir();
    let out = projection_dir();
    SYNCABLE
        .iter()
        .filter_map(|s| Some((*s, local_source(s, &dir, &out)?)))
        .collect()
}

/// Where the synced settings projection is written before it is pushed.
fn projection_dir() -> PathBuf {
    crate::app::croft_cache_dir().join("config-sync")
}

/// The local file to push for `s`, if there is one. A whole-file syncable
/// is its own source. The synced settings layer is built from `config.json`
/// in `dir`: its appearance and editor keys, and nothing else, written under
/// `out` as `s.name`. No `config.json`, an unreadable one, or one with none
/// of those keys means nothing to push, never an empty file that would
/// blank the remote's copy.
pub fn local_source(s: &Syncable, dir: &std::path::Path, out: &std::path::Path) -> Option<PathBuf> {
    let src = dir.join(s.source);
    if s.source == s.name {
        return src.is_file().then_some(src);
    }
    let text = std::fs::read_to_string(&src).ok()?;
    let doc: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&text).ok()?;
    let projected = crate::config_layers::synced_projection(&doc)?;
    let json = serde_json::to_string_pretty(&projected).ok()? + "\n";
    std::fs::create_dir_all(out).ok()?;
    let path = out.join(s.name);
    std::fs::write(&path, json).ok()?;
    Some(path)
}

/// Whether `host` is one config sync never pushes to (#262): the user's
/// `config_sync_excluded_hosts`, matched case-insensitively like the other
/// per-host lists (ssh aliases are not case-sensitive in practice).
pub fn host_excluded(host: &str, excluded: &[String]) -> bool {
    excluded.iter().any(|h| h.eq_ignore_ascii_case(host))
}

/// Split `files` into what travels and the names the user excluded with
/// `config_sync_excluded_files` (#262), so the push can say what it skipped
/// rather than leave a missing file looking like a failed one.
pub fn apply_exclusions(
    files: Vec<(Syncable, PathBuf)>,
    excluded: &[String],
) -> (Vec<(Syncable, PathBuf)>, Vec<&'static str>) {
    let mut keep = Vec::new();
    let mut skipped = Vec::new();
    for (s, p) in files {
        if excluded.iter().any(|e| e == s.name) {
            skipped.push(s.name);
        } else {
            keep.push((s, p));
        }
    }
    (keep, skipped)
}

/// Every syncable file's local path, present or not: what a live re-push
/// watches, so a file created mid-session is sent too.
pub fn watched_local_paths() -> Vec<PathBuf> {
    let dir = crate::prefs::config_dir();
    SYNCABLE.iter().map(|s| dir.join(s.source)).collect()
}

/// The syncable files among `changed` that exist now, as pushes to make.
/// A deleted file is not pushed: the remote keeps its copy, the same rule
/// the push at connect time follows for a file the laptop does not have.
pub fn repush_targets(changed: &[PathBuf], dir: &std::path::Path) -> Vec<(Syncable, PathBuf)> {
    repush_targets_into(changed, dir, &projection_dir())
}

/// [`repush_targets`] with the projection written under `out`.
fn repush_targets_into(
    changed: &[PathBuf],
    dir: &std::path::Path,
    out: &std::path::Path,
) -> Vec<(Syncable, PathBuf)> {
    changed
        .iter()
        .filter(|p| p.parent() == Some(dir) && p.is_file())
        .filter_map(|p| {
            let name = p.file_name()?.to_str()?;
            let s = SYNCABLE.iter().find(|s| s.source == name)?;
            Some((*s, local_source(s, dir, out)?))
        })
        .collect()
}

/// The rsync destination for `name` on `host`, as an rsync remote spec.
///
/// `.config/croft` rather than `$XDG_CONFIG_HOME`: rsync gets no shell on
/// the remote side to expand a variable, and the remote's own
/// `config_dir()` honours XDG. A remote that sets XDG_CONFIG_HOME would
/// read from elsewhere, which is a known gap rather than a silent one.
pub fn remote_dest(host: &str, name: &str) -> String {
    format!("{host}:.config/croft/{name}")
}

/// The sha256 of `bytes`, as the hex `sha256sum` prints.
pub fn content_hash(bytes: &[u8]) -> String {
    use sha2::Digest;
    format!("{:x}", sha2::Sha256::digest(bytes))
}

/// The remote shell script that prints `sha256sum`'s `<hash>  <name>` for
/// each syncable file present in `~/.config/croft`, and nothing for one
/// that is absent. `shasum -a 256` for a box without coreutils.
pub fn remote_hash_script() -> String {
    let names: Vec<&str> = SYNCABLE.iter().map(|s| s.name).collect();
    let names = names.join(" ");
    format!(
        "cd ~/.config/croft 2>/dev/null || exit 0; for f in {names}; do [ -f \"$f\" ] || continue; \
         if command -v sha256sum >/dev/null 2>&1; then sha256sum \"$f\"; else shasum -a 256 \"$f\"; fi; done"
    )
}

/// Parse [`remote_hash_script`]'s output into file name → hash. Only
/// syncable names count: anything else on the line is not ours to trust.
pub fn parse_remote_hashes(out: &str) -> std::collections::BTreeMap<String, String> {
    out.lines()
        .filter_map(|l| {
            let (hash, name) = l.trim().split_once(char::is_whitespace)?;
            let name = name.trim().trim_start_matches('*');
            let known = SYNCABLE.iter().any(|s| s.name == name);
            (known && hash.len() == 64).then(|| (name.to_string(), hash.to_ascii_lowercase()))
        })
        .collect()
}

/// What config sync last agreed with one file on one host (#262).
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FileState {
    /// The hash last pushed there. A remote copy still at this hash is
    /// ours to replace; anything else was edited there since.
    #[serde(default)]
    pub pushed: Option<String>,
    /// "Keep the remote copy": its hash, and the local hash it was kept
    /// against. The push leaves it alone until either side changes.
    #[serde(default)]
    pub kept: Option<(String, String)>,
}

/// Config sync's memory, per host (ssh alias, lowercased) and file. Kept
/// in croft's cache dir: losing it only makes the next push ask about a
/// remote copy that differs, never overwrite one.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SyncState {
    #[serde(default)]
    pub hosts: std::collections::BTreeMap<String, std::collections::BTreeMap<String, FileState>>,
}

impl SyncState {
    pub fn path() -> PathBuf {
        crate::app::croft_cache_dir().join("config-sync.json")
    }

    /// Unreadable or absent state is empty state, which is the careful one.
    pub fn load(path: &std::path::Path) -> SyncState {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, text)
    }

    pub fn file(&self, host: &str, name: &str) -> Option<&FileState> {
        self.hosts.get(&host.to_ascii_lowercase())?.get(name)
    }

    pub fn file_mut(&mut self, host: &str, name: &str) -> &mut FileState {
        self.hosts
            .entry(host.to_ascii_lowercase())
            .or_default()
            .entry(name.to_string())
            .or_default()
    }
}

/// What the push does with one file (#262).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Plan {
    /// Absent there, or still what croft last pushed: send it.
    Push,
    /// The remote already has exactly this content.
    UpToDate,
    /// The user chose to keep the remote copy, and neither side changed.
    Kept,
    /// Edited on the remote since the last push (or never pushed, and
    /// different): left alone until the user chooses.
    Conflict,
}

/// Decide one file's fate from its local hash, the remote's (if the file
/// exists there), and what was last agreed. Local is the source of truth,
/// but never over an edit made on the remote.
pub fn plan(local: &str, remote: Option<&str>, state: Option<&FileState>) -> Plan {
    let Some(remote) = remote else {
        return Plan::Push;
    };
    if remote == local {
        return Plan::UpToDate;
    }
    let state = state.cloned().unwrap_or_default();
    if state
        .kept
        .as_ref()
        .is_some_and(|(r, l)| r == remote && l == local)
    {
        return Plan::Kept;
    }
    if state.pushed.as_deref() == Some(remote) {
        return Plan::Push;
    }
    Plan::Conflict
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_push_never_overwrites_a_copy_edited_on_the_remote() {
        let pushed = |h: &str| FileState {
            pushed: Some(h.into()),
            kept: None,
        };
        // A fresh box, and one still holding what croft pushed.
        assert_eq!(plan("L2", None, None), Plan::Push);
        assert_eq!(plan("L2", Some("L1"), Some(&pushed("L1"))), Plan::Push);
        assert_eq!(plan("L2", Some("L2"), None), Plan::UpToDate);
        // Edited there since the last push, or different and never pushed.
        assert_eq!(plan("L2", Some("R"), Some(&pushed("L1"))), Plan::Conflict);
        assert_eq!(plan("L2", Some("R"), None), Plan::Conflict);
        // Kept, until either side moves.
        let kept = FileState {
            pushed: Some("L1".into()),
            kept: Some(("R".into(), "L2".into())),
        };
        assert_eq!(plan("L2", Some("R"), Some(&kept)), Plan::Kept);
        assert_eq!(plan("L3", Some("R"), Some(&kept)), Plan::Conflict);
        assert_eq!(plan("L2", Some("R2"), Some(&kept)), Plan::Conflict);
    }

    #[test]
    fn remote_hashes_parse_for_syncable_names_only() {
        let h = "a".repeat(64);
        let out = format!("{h}  keybindings.json\n{h} *snippets.json\n{h}  config.json\nnoise\n");
        let got = parse_remote_hashes(&out);
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(got.get("snippets.json"), Some(&h));
        assert!(!got.contains_key("config.json"));
        // The script itself, run by a real shell against a config dir.
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join(".config/croft");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("keybindings.json"), "{}\n").unwrap();
        std::fs::write(dir.join("config.json"), "{}\n").unwrap();
        let out = std::process::Command::new("sh")
            .args(["-c", &remote_hash_script()])
            .env("HOME", home.path())
            .output()
            .unwrap();
        assert!(out.status.success());
        let got = parse_remote_hashes(&String::from_utf8_lossy(&out.stdout));
        assert_eq!(
            got.into_iter().collect::<Vec<_>>(),
            [(String::from("keybindings.json"), content_hash(b"{}\n"))]
        );
        assert_eq!(content_hash(b"").len(), 64);
    }

    #[test]
    fn sync_state_round_trips_per_host_case_insensitively() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.json");
        let mut st = SyncState::default();
        st.file_mut("DevBox", "keybindings.json").pushed = Some("x".into());
        st.save(&path).unwrap();
        let back = SyncState::load(&path);
        assert_eq!(
            back.file("devbox", "keybindings.json")
                .and_then(|f| f.pushed.as_deref()),
            Some("x")
        );
        assert_eq!(
            SyncState::load(&dir.path().join("none")),
            SyncState::default()
        );
    }

    #[test]
    fn the_trust_carrying_config_never_becomes_syncable() {
        // The whole point of the module. `config.json` holds mcp_consented
        // and mcp_tool_fingerprints, so syncing it as a file would grant on
        // every remote what the user granted once locally.
        for (never, why) in NEVER_SYNC {
            assert!(
                !SYNCABLE.iter().any(|s| s.name == *never),
                "{never} must never sync: {why}"
            );
        }
    }

    #[test]
    fn every_syncable_name_is_a_bare_file_name() {
        // A name with a separator would let an entry escape the config dir
        // on either end — `../` locally, or an absolute path remotely.
        for s in SYNCABLE {
            assert!(
                !s.name.contains('/') && !s.name.contains('\\') && !s.name.contains(".."),
                "{} must be a bare file name, not a path",
                s.name
            );
            assert!(!s.name.is_empty(), "an empty name would sync the dir");
        }
    }

    #[test]
    fn the_destination_stays_inside_the_remote_config_dir() {
        assert_eq!(
            remote_dest("box", "keybindings.json"),
            "box:.config/croft/keybindings.json"
        );
        // Every syncable name lands under the config dir and nowhere else.
        for s in SYNCABLE {
            let dest = remote_dest("h", s.name);
            assert!(
                dest.starts_with("h:.config/croft/"),
                "{dest} escaped the remote config dir"
            );
        }
    }

    #[test]
    fn a_missing_file_is_skipped_rather_than_pushed_empty() {
        // `local_files` filters on `is_file`, so a user with no snippets
        // pushes nothing for it instead of blanking the remote's copy.
        let listed = local_files();
        for (_, path) in &listed {
            assert!(
                path.is_file(),
                "{} was listed but is not a file",
                path.display()
            );
        }
    }

    /// #262: a local edit mid-session pushes that file; a deleted file, a
    /// file outside the config dir and one not on the allow-list do not.
    #[test]
    fn a_live_repush_sends_only_changed_syncable_files_that_exist() {
        let dir = tempfile::tempdir().unwrap();
        let keys = dir.path().join("keybindings.json");
        let gone = dir.path().join("snippets.json");
        let other = dir.path().join("config.json");
        std::fs::write(&keys, "[]").unwrap();
        std::fs::write(&other, "{}").unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let stray = elsewhere.path().join("keybindings.json");
        std::fs::write(&stray, "[]").unwrap();
        let got = repush_targets(&[keys.clone(), gone, other, stray], dir.path());
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].0.name, &got[0].1), ("keybindings.json", &keys));
    }

    #[test]
    fn every_syncable_file_reloads_live() {
        // `reload_config_for_path` has an arm for each, and `ConfigWatch`
        // reaches it for a file that arrives by sync (#262).
        assert!(SYNCABLE.iter().all(|s| s.hot_reloads), "{SYNCABLE:?}");
    }

    #[test]
    fn the_watch_reports_a_changed_created_or_deleted_file_once() {
        let dir = tempfile::tempdir().unwrap();
        let kept = dir.path().join("keybindings.json");
        let fresh = dir.path().join("snippets.json");
        std::fs::write(&kept, "[]").unwrap();
        let mut watch = ConfigWatch::new(vec![kept.clone(), fresh.clone()]);
        let t0 = std::time::Instant::now();
        let later = |n: u32| t0 + ConfigWatch::INTERVAL * n;
        assert!(watch.poll(later(1)).is_empty(), "nothing changed yet");
        // A different length is a change even within the mtime's resolution.
        std::fs::write(&kept, r#"[{"key": "ctrl+k"}]"#).unwrap();
        std::fs::write(&fresh, "{}").unwrap();
        // Between checks nothing is reported.
        assert!(watch.poll(later(1)).is_empty());
        assert_eq!(watch.poll(later(2)), vec![kept.clone(), fresh.clone()]);
        assert!(watch.poll(later(3)).is_empty(), "reported once");
        std::fs::remove_file(&fresh).unwrap();
        assert_eq!(watch.poll(later(4)), vec![fresh]);
    }

    #[test]
    fn a_change_croft_noted_itself_is_not_reported_again() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("keybindings.json");
        std::fs::write(&file, "[]").unwrap();
        let mut watch = ConfigWatch::new(vec![file.clone()]);
        std::fs::write(&file, "[ ]").unwrap();
        watch.note(&file);
        let later = std::time::Instant::now() + ConfigWatch::INTERVAL * 2;
        assert!(watch.poll(later).is_empty());
    }

    /// #262: the per-host opt-out, case-insensitive like the other lists.
    #[test]
    fn an_excluded_host_is_matched_case_insensitively() {
        let excluded = vec![String::from("Shared-Box")];
        assert!(host_excluded("shared-box", &excluded));
        assert!(!host_excluded("dev", &excluded));
        assert!(!host_excluded("shared-box", &[]));
    }

    /// #262: the per-file escape keeps the named file home and reports it.
    #[test]
    fn excluded_files_stay_home_and_are_named() {
        let files: Vec<(Syncable, PathBuf)> = SYNCABLE
            .iter()
            .map(|s| (*s, PathBuf::from(s.name)))
            .collect();
        let (keep, skipped) = apply_exclusions(files, &[String::from("keybindings.json")]);
        assert_eq!(skipped, vec!["keybindings.json"]);
        assert!(keep.iter().all(|(s, _)| s.name != "keybindings.json"));
        assert_eq!(
            keep.len(),
            SYNCABLE.len() - 1,
            "only the excluded one stays"
        );
    }

    /// #262: the theme travels, the trust does not. A changed `config.json`
    /// re-pushes as `config.synced.json` holding only its appearance and
    /// editor keys; `config.json` itself is never the file sent.
    #[test]
    fn config_json_travels_as_a_projection_without_trust() {
        let dir = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.json");
        std::fs::write(
            &config,
            r#"{ "theme": "light", "mcp_consented": ["srv"], "mcp_tool_fingerprints": {"srv": "f"} }"#,
        )
        .unwrap();
        let got = repush_targets_into(std::slice::from_ref(&config), dir.path(), out.path());
        assert_eq!(got.len(), 1);
        let (s, path) = &got[0];
        assert_eq!(s.name, crate::config_layers::SYNCED_CONFIG_NAME);
        assert_ne!(path, &config, "config.json itself never travels");
        let sent: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(sent, serde_json::json!({ "theme": "light" }));

        std::fs::write(&config, r#"{ "mcp_consented": ["srv"] }"#).unwrap();
        assert!(
            repush_targets_into(std::slice::from_ref(&config), dir.path(), out.path()).is_empty(),
            "nothing shareable, nothing pushed"
        );
        std::fs::write(&config, "not json").unwrap();
        assert!(
            repush_targets_into(std::slice::from_ref(&config), dir.path(), out.path()).is_empty(),
            "an unreadable config pushes nothing rather than an empty layer"
        );
    }

    /// A live re-push watches `config.json`, the synced layer's source.
    #[test]
    fn the_watch_covers_the_projections_source() {
        let watched = watched_local_paths();
        assert!(
            watched.iter().any(|p| p.ends_with("config.json")),
            "{watched:?}"
        );
        assert!(
            !watched
                .iter()
                .any(|p| p.ends_with(crate::config_layers::SYNCED_CONFIG_NAME)),
            "the synced layer is written by other machines, not watched for pushing"
        );
    }
}
