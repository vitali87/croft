//! Quick Open's file index: every file under the workspace roots, walked in
//! parallel under the ignore rules, with the folders croft never indexes
//! (dependency caches, build output, VCS stores, macOS's protected folders)
//! left out. Quick Open, the `@` file mentions, the file watcher, CodeQL and
//! test discovery build on it; nothing here draws (the picker is
//! `widgets::file_finder`).

use ignore::{WalkBuilder, WalkState};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug)]
pub struct FileEntry {
    pub path: PathBuf,
    pub rel: String,
    pub rel_lower: String,
    /// Byte offset of the filename within `rel` (rendering's dir/file
    /// split). `rel_lower` gets its own offset below: lowercasing can
    /// change a component's byte length (e.g. `İ` → `i̇`), so one offset
    /// cannot index both strings.
    pub filename_start: usize,
    /// Byte offset of the filename within `rel_lower` — the scoring
    /// string. Slicing `rel_lower` with the `rel` offset panicked on a
    /// non-boundary whenever an earlier component's lowercase changed
    /// byte length (#148 review).
    pub filename_start_lower: usize,
}

/// Directory names that are vendored / generated / VCS-internal and
/// will never be a useful Cmd+P target. Including these in the index
/// would drown signal in noise (every `.git/objects/*` blob,
/// `node_modules/**`, `target/debug/incremental/*`, etc.) and balloon
/// the walk by orders of magnitude. The match is exact on the path
/// component name - a real workspace file literally named `target.md`
/// is still indexed.
const NOISE_DIR_NAMES: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".bzr",
    "node_modules",
    "target",
    "__pycache__",
    ".venv",
    "venv",
    ".tox",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    ".next",
    ".nuxt",
    ".cache",
    "dist",
    "build",
    ".gradle",
    ".idea",
    ".vscode-test",
    ".DS_Store",
    // Package-manager, language-toolchain, and agent caches that live in
    // $HOME. Never edit targets, and several (.cargo, .claude, .npm) are
    // written to continuously, so watching them turns a home-dir workspace
    // into a perpetual FS-event storm. Measured on a Linux remote opened at
    // /root: ~7000 inotify watches and 39% of CPU stuck in readlink
    // path-walks re-scanning these trees (~200% CPU total). They dominate
    // both the watch count and the churn, so excluding them is the fix.
    ".cargo",
    ".rustup",
    ".npm",
    ".nvm",
    ".bun",
    ".pnpm-store",
    ".deno",
    ".gem",
    ".pyenv",
    ".rbenv",
    ".m2",
    ".local",
    ".docker",
    ".claude",
    ".codex",
    // The same class of $HOME toolchain cache for the .NET, Dart and Go
    // ecosystems, plus oh-my-zsh's plugin tree. Measured on a Linux
    // remote opened at /root: .nuget 1127 dirs, .pub-cache 1271,
    // go/pkg/mod 1357 (below, by path) — together the top consumers of a
    // readlink storm that held ~210% CPU for 19 minutes without
    // finishing the walk.
    ".nuget",
    ".pub-cache",
    ".dart-tool",
    ".dotnet",
    ".oh-my-zsh",
];

/// Noise trees whose LEADING component is too generic to blacklist by
/// name: a workspace really can hold a `go/` source directory, so only
/// the module/checksum caches GOPATH parks in `$HOME` are pruned. Matched
/// as a consecutive component run anywhere in the path, so both the cache
/// root and everything under it are covered.
const NOISE_PATH_RUNS: &[&[&str]] = &[&["go", "pkg", "mod"], &["go", "pkg", "sumdb"]];

/// Number of `Normal` components in `path` — its depth, ignoring the root
/// prefix and any `.` / `..` the caller left in.
fn normal_component_count(path: &Path) -> usize {
    path.components()
        .filter(|c| matches!(c, std::path::Component::Normal(_)))
        .count()
}

/// True when `run` appears as consecutive `Normal` components of `path`
/// ENDING deeper than `min_end` components in — that is, the run must reach
/// below the first `min_end` components. `0` matches a run anywhere.
///
/// The depth bound is what lets a run straddle a workspace root: the head of
/// the run may lie in the root's own path as long as its tail does not.
fn has_component_run(path: &Path, run: &[&str], min_end: usize) -> bool {
    let comps: Vec<&std::ffi::OsStr> = path
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s),
            _ => None,
        })
        .collect();
    comps.windows(run.len()).enumerate().any(|(i, w)| {
        i + run.len() > min_end
            && w.iter()
                .zip(run)
                .all(|(a, b)| *a == std::ffi::OsStr::new(b))
    })
}

pub fn is_noise_dir(name: &std::ffi::OsStr) -> bool {
    name.to_str().is_some_and(|n| NOISE_DIR_NAMES.contains(&n))
}

/// True when `path` contains one of the [`NOISE_PATH_RUNS`] cache trees,
/// measured RELATIVE TO `root`: the run must REACH BELOW the root.
///
/// The relative part is load-bearing. The workspace root's OWN path is not
/// part of the workspace, so matching the absolute path would prune the
/// entire tree of anyone whose checkout happens to live under a matching
/// ancestor — opening a dependency inside `~/go/pkg/mod/...` to read it
/// would index zero files.
///
/// "Reaches below the root" rather than "lies below it", because a run can
/// STRADDLE the root. A workspace opened at `~/go` — the ordinary GOPATH —
/// keeps its cache at `pkg/mod`, a run whose first component is the root's
/// own last one. Stripping the root off and matching what remained missed
/// exactly that, re-admitting the tree this rule exists to prune.
pub fn has_noise_path_run(path: &Path, root: &Path) -> bool {
    // Components at or above the root cannot make an entry noise on their
    // own; they can only supply the head of a run whose tail is below it. A
    // path outside the root has no root to be measured against, so every
    // component of it counts, as it did before roots entered this rule.
    let depth = if path.starts_with(root) {
        normal_component_count(root)
    } else {
        0
    };
    NOISE_PATH_RUNS
        .iter()
        .any(|r| has_component_run(path, r, depth))
}

/// True when `path` is noise measured RELATIVE TO `root`: it carries a
/// noise-named component, or one of the [`NOISE_PATH_RUNS`] cache trees, at
/// or below the root.
///
/// This is the reading every EVENT path wants. [`is_path_under_noise_dir`]
/// answers the same question about a path in isolation, which is right for
/// the walk (it starts at the root, so everything it reaches is already
/// below it) and wrong for a watcher event, whose path is absolute: a
/// checkout at `~/build/myapp` would have `build` matched among its own
/// ancestors and every file in it classified as noise. The root's ancestors
/// are not part of the workspace, exactly as in [`has_noise_path_run`].
///
/// A `path` that is not under `root` is measured whole. There is no root for
/// its components to be relative to, and a path outside the workspace is not
/// workspace content whichever way it is read.
pub fn is_noise_path_under_root(path: &Path, root: &Path) -> bool {
    let rel = path.strip_prefix(root).unwrap_or(path);
    is_path_under_noise_dir(rel) || has_noise_path_run(path, root)
}

/// True when `path` sits inside (or names) one of the noise dirs the
/// finder index already excludes from its walk. Callers use this to
/// avoid kicking a full index rebuild for FS events the finder would
/// have ignored anyway — e.g. cargo writing into `target/`, npm into
/// `node_modules/`, git into `.git/`. Without this gate, a sustained
/// cargo build produces a continuous index-rebuild storm that pegs
/// every core via the parallel walker.
pub fn is_path_under_noise_dir(path: &Path) -> bool {
    path.components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s),
            _ => None,
        })
        .any(is_noise_dir)
}

/// Absolute paths to prune from the walk for two reasons:
///   1. `Library/Containers` and `Library/Group Containers` trigger a
///      macOS TCC consent prompt ("App would like to access data from
///      other apps") because they live behind
///      kTCCServiceSystemPolicyAppData.
///   2. The Library/Caches, Library/Application Support, Library/Mobile
///      Documents, Library/Developer, Library/CoreSimulator,
///      Library/Mail, Library/Messages, Library/CloudStorage,
///      Library/Safari, Library/Cookies trees collectively account
///      for ~1.4M of the ~1.5M files found by a `~`-rooted walk on a
///      typical developer Mac. None of those are useful Cmd+P
///      targets, and including them blows per-keystroke match
///      latency from ~5ms to ~300ms. Library/Logs is kept in because
///      log files DO get opened (the user's lsp.log lives at
///      `~/.croft/lsp.log` but `~/Library/Logs/*` is a reasonable
///      Cmd+P target too).
fn macos_blocked_paths() -> Vec<PathBuf> {
    match std::env::var_os("HOME") {
        Some(home) => macos_blocked_paths_for(&PathBuf::from(home)),
        None => Vec::new(),
    }
}

/// The pure half of [`macos_blocked_paths`], taking the home directory as
/// an argument so tests never repoint the process-global `$HOME` (#37):
/// the env var is resolved at call time by every concurrent reader (the
/// PDF rasteriser's cache dir, the session paths), and a substituted value
/// leaking across test threads made unrelated tests fail on paths inside
/// the fake home.
fn macos_blocked_paths_for(home: &Path) -> Vec<PathBuf> {
    let lib = home.join("Library");
    [
        "Containers",
        "Group Containers",
        "Caches",
        "Application Support",
        "Mobile Documents",
        "CloudStorage",
        "Developer",
        "CoreSimulator",
        "Mail",
        "Messages",
        "Safari",
        "Cookies",
        "Metadata",
        "Suggestions",
        "Biome",
        "Calendars",
        "Reminders",
        "PersonalizationPortrait",
        "HomeKit",
        "Photos",
        "Daemon Containers",
        "WebKit",
    ]
    .iter()
    .map(|leaf| lib.join(leaf))
    .collect()
}

pub fn build_file_index(root: &Path) -> Vec<FileEntry> {
    build_file_index_with_blocked(root, macos_blocked_paths())
}

/// The Quick Open index over EVERY workspace root (#147). One root is the
/// plain per-root index; with more, each root's entries carry a
/// `<root-name>/` prefix on `rel` — the VS Code multi-root label rule — so
/// same-named files in different roots stay distinguishable and ranking
/// never mixes two projects' bare paths. `filename_start` shifts with the
/// prefix, keeping the filename-tier scoring anchored on the filename.
pub fn build_multi_file_index(roots: &[PathBuf]) -> Vec<FileEntry> {
    if roots.len() == 1 {
        return build_file_index(&roots[0]);
    }
    let labels = crate::workspace::root_display_labels(roots);
    let mut out = Vec::new();
    for (root, label) in roots.iter().zip(&labels) {
        let prefix = format!("{label}/");
        for mut e in build_file_index(root) {
            e.rel = format!("{prefix}{}", e.rel);
            // Recompute BOTH offsets from the final strings: hand-shifting
            // by prefix.len() poisoned rel_lower's offset whenever the
            // label's lowercase changed byte length (#148 review).
            e.rel_lower = e.rel.to_lowercase();
            e.filename_start = e.rel.rfind('/').map(|i| i + 1).unwrap_or(0);
            e.filename_start_lower = e.rel_lower.rfind('/').map(|i| i + 1).unwrap_or(0);
            out.push(e);
        }
    }
    out
}

/// [`build_file_index`] with the blocked-path set injected, so tests drive
/// the macOS exclusion rules against a fake home without mutating `$HOME`.
fn build_file_index_with_blocked(root: &Path, blocked: Vec<PathBuf>) -> Vec<FileEntry> {
    let collected: Arc<Mutex<Vec<FileEntry>>> = Arc::new(Mutex::new(Vec::new()));
    let root_buf = root.to_path_buf();
    let filter_root = root.to_path_buf();
    let walker = WalkBuilder::new(root)
        .git_ignore(true)
        .require_git(false)
        // .hidden(false) so user dot-dirs (.croft, .cursor, .config,
        // .continue, .databridge) ARE walked - the user explicitly
        // wants to Cmd+P into them. NOISE_DIR_NAMES below prunes the
        // vendored / generated dot-dirs (.git, .venv, .mypy_cache,
        // .next, .cache, ...) that would otherwise drown the index.
        .hidden(false)
        .filter_entry(move |entry| {
            let depth = entry.depth();
            if depth == 0 {
                return true;
            }
            if is_noise_dir(entry.file_name()) {
                return false;
            }
            if has_noise_path_run(entry.path(), &filter_root) {
                return false;
            }
            // Hard-skip macOS app-data containers so readdir does not
            // trip a TCC consent prompt on every launch. starts_with
            // matches both the container root and any descendant.
            for b in &blocked {
                if entry.path().starts_with(b) {
                    return false;
                }
            }
            true
        })
        .build_parallel();
    walker.run(|| {
        let collected = collected.clone();
        let root_buf = root_buf.clone();
        Box::new(move |entry| {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => return WalkState::Continue,
            };
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                return WalkState::Continue;
            }
            let path = entry.path().to_path_buf();
            let rel_path = path.strip_prefix(&root_buf).unwrap_or(&path);
            let rel = rel_path.to_string_lossy().replace('\\', "/");
            if rel.is_empty() {
                return WalkState::Continue;
            }
            let rel_lower = rel.to_lowercase();
            let filename_start = rel.rfind('/').map(|i| i + 1).unwrap_or(0);
            let filename_start_lower = rel_lower.rfind('/').map(|i| i + 1).unwrap_or(0);
            if let Ok(mut g) = collected.lock() {
                g.push(FileEntry {
                    path,
                    rel,
                    rel_lower,
                    filename_start,
                    filename_start_lower,
                });
            }
            WalkState::Continue
        })
    });
    let mut out = Arc::try_unwrap(collected)
        .map(|m| m.into_inner().unwrap_or_default())
        .unwrap_or_default();
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multi_root_index_disambiguates_same_named_roots() {
        let outer = tempfile::tempdir().unwrap();
        let work = outer.path().join("work").join("api");
        let archive = outer.path().join("archive").join("api");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(&archive).unwrap();
        std::fs::write(
            work.join("main.rs"),
            "a
",
        )
        .unwrap();
        std::fs::write(
            archive.join("main.rs"),
            "b
",
        )
        .unwrap();
        let idx = build_multi_file_index(&[work, archive]);
        let rels: Vec<&str> = idx.iter().map(|e| e.rel.as_str()).collect();
        assert!(
            rels.contains(&"api (work)/main.rs") && rels.contains(&"api (archive)/main.rs"),
            "same-named roots label with their parents: {rels:?}"
        );
    }

    #[test]
    fn multi_root_index_prefixes_rels_with_the_root_name_and_keeps_filename_scoring() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let a_root = a.path().join("alpha");
        let b_root = b.path().join("beta");
        std::fs::create_dir(&a_root).unwrap();
        std::fs::create_dir(&b_root).unwrap();
        // The SAME relative path in both roots: the multi-root prefix is
        // the only thing keeping the two rows distinguishable.
        std::fs::write(a_root.join("main.rs"), "a\n").unwrap();
        std::fs::write(b_root.join("main.rs"), "b\n").unwrap();

        let idx = build_multi_file_index(&[a_root.clone(), b_root.clone()]);
        let rels: Vec<&str> = idx.iter().map(|e| e.rel.as_str()).collect();
        assert!(rels.contains(&"alpha/main.rs"), "rels: {rels:?}");
        assert!(rels.contains(&"beta/main.rs"), "rels: {rels:?}");
        for e in &idx {
            assert_eq!(
                &e.rel[e.filename_start..],
                "main.rs",
                "filename_start must shift with the prefix so filename-tier scoring holds"
            );
            assert!(e.path.is_absolute() && e.path.ends_with("main.rs"));
        }

        // One root: the plain index, no prefixes.
        let single = build_multi_file_index(&[a_root]);
        assert_eq!(single.len(), 1);
        assert_eq!(single[0].rel, "main.rs");
    }

    #[test]
    fn build_file_index_walks_workspace_and_honours_gitignore() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("alpha.rs"), "").unwrap();
        std::fs::create_dir(tmp.path().join("sub")).unwrap();
        std::fs::write(tmp.path().join("sub").join("beta.rs"), "").unwrap();
        std::fs::write(tmp.path().join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(tmp.path().join("ignored.txt"), "").unwrap();
        let entries = build_file_index(tmp.path());
        let names: Vec<&str> = entries.iter().map(|e| e.rel.as_str()).collect();
        assert!(names.contains(&"alpha.rs"));
        assert!(names.contains(&"sub/beta.rs"));
        assert!(
            !names.contains(&"ignored.txt"),
            "gitignored files must be excluded from the Cmd+P index"
        );
    }

    #[test]
    fn build_file_index_includes_user_owned_dot_directories() {
        // Regression for the user's "Cmd+P does not find ~/.croft/lsp.log
        // even when I type lsp.log" report. The old build_file_index
        // called .hidden(true) which silently excluded every dot-dir,
        // including the user's own ~/.croft, ~/.cursor, ~/.continue,
        // ~/.config, etc. - exactly the directories whose contents the
        // Cmd+P finder is supposed to help locate.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join(".croft")).unwrap();
        std::fs::write(tmp.path().join(".croft").join("lsp.log"), b"").unwrap();
        std::fs::create_dir(tmp.path().join(".config")).unwrap();
        std::fs::write(tmp.path().join(".config").join("init.toml"), b"").unwrap();
        let entries = build_file_index(tmp.path());
        let names: Vec<&str> = entries.iter().map(|e| e.rel.as_str()).collect();
        assert!(
            names.contains(&".croft/lsp.log"),
            "Cmd+P index must include files under user dot-dirs like .croft/; got {names:?}"
        );
        assert!(
            names.contains(&".config/init.toml"),
            "Cmd+P index must include files under .config/ and similar user dot-dirs; got {names:?}"
        );
    }

    #[test]
    fn build_file_index_skips_macos_app_data_containers() {
        // Regression for the user's "iTerm.app would like to access
        // data from other apps" TCC prompt that fired repeatedly
        // after the 0.1.148 background-index landed. macOS gates
        // ~/Library/Containers/<bundle-id> and ~/Library/Group
        // Containers/<group-id> behind kTCCServiceSystemPolicyAppData;
        // any process that readdir's into those paths trips a
        // consent prompt on the parent terminal app. None of those
        // files are useful Cmd+P targets, so prune the two directory
        // trees outright. Test simulates a workspace rooted at a fake
        // $HOME so the path-shape match fires deterministically.
        let tmp = tempfile::tempdir().unwrap();
        let fake_home = tmp.path();
        let containers = fake_home
            .join("Library")
            .join("Containers")
            .join("com.example.app")
            .join("Data");
        std::fs::create_dir_all(&containers).unwrap();
        std::fs::write(containers.join("private.sqlite"), b"").unwrap();
        let group = fake_home
            .join("Library")
            .join("Group Containers")
            .join("group.example")
            .join("shared");
        std::fs::create_dir_all(&group).unwrap();
        std::fs::write(group.join("shared.db"), b"").unwrap();
        // A normal Library subdir the user DOES want indexed - Logs
        // is the canonical lsp.log location regression target.
        let logs = fake_home.join("Library").join("Logs");
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(logs.join("app.log"), b"").unwrap();
        // The blocked set is injected instead of repointing `$HOME` (#37):
        // concurrent tests resolve the env var at call time, and a
        // substituted value made unrelated readers (the PDF rasteriser's
        // cache dir) fail on paths inside the fake home.
        let entries = build_file_index_with_blocked(fake_home, macos_blocked_paths_for(fake_home));
        let names: Vec<&str> = entries.iter().map(|e| e.rel.as_str()).collect();
        assert!(
            names.contains(&"Library/Logs/app.log"),
            "Library/Logs/app.log must still be indexed; got {names:?}"
        );
        assert!(
            !names.iter().any(|n| n.starts_with("Library/Containers/")),
            "Library/Containers/** must be excluded to avoid macOS AppData TCC prompts; got {names:?}"
        );
        assert!(
            !names
                .iter()
                .any(|n| n.starts_with("Library/Group Containers/")),
            "Library/Group Containers/** must be excluded for the same TCC reason; got {names:?}"
        );
    }

    #[test]
    fn build_file_index_skips_vendored_noise_directories() {
        // Including hidden dirs would otherwise drown the index in
        // .git/objects/* and node_modules/* — paths the user never
        // wants to open via Cmd+P. Make sure the noise-dir filter
        // keeps those out even though .hidden(false) is now in
        // effect.
        let tmp = tempfile::tempdir().unwrap();
        for noise in [
            ".git",
            "node_modules",
            "target",
            "__pycache__",
            ".venv",
            ".tox",
            ".mypy_cache",
        ] {
            let dir = tmp.path().join(noise);
            std::fs::create_dir(&dir).unwrap();
            std::fs::write(dir.join("blob"), b"").unwrap();
        }
        std::fs::write(tmp.path().join("real.txt"), b"").unwrap();
        let entries = build_file_index(tmp.path());
        let names: Vec<&str> = entries.iter().map(|e| e.rel.as_str()).collect();
        assert!(
            names.contains(&"real.txt"),
            "the regular workspace file must still be indexed; got {names:?}"
        );
        for noise in [
            ".git/blob",
            "node_modules/blob",
            "target/blob",
            "__pycache__/blob",
            ".venv/blob",
            ".tox/blob",
            ".mypy_cache/blob",
        ] {
            assert!(
                !names.contains(&noise),
                "{noise} must be filtered out so vendored / generated dirs do not drown the Cmd+P index; got {names:?}"
            );
        }
    }

    #[test]
    fn is_path_under_noise_dir_flags_cargo_and_npm_output_paths() {
        // Root-cause guard: a sample run on a live croft pegged ~150%
        // CPU showed 50+ workers in `ignore::WalkBuilder::build_parallel`
        // because every cargo write into `target/` triggered a Cmd+P
        // index rebuild — even though the rebuild itself skips target/.
        // The gate lives in this helper; if it ever stops recognising
        // cargo/npm/git output paths, the storm comes back.
        assert!(is_path_under_noise_dir(Path::new(
            "/Users/v/proj/target/debug/build/croft-abcd"
        )));
        assert!(is_path_under_noise_dir(Path::new(
            "/Users/v/proj/node_modules/foo/bar.js"
        )));
        assert!(is_path_under_noise_dir(Path::new(
            "/Users/v/proj/.git/objects/ff/abcd"
        )));
    }

    #[test]
    fn is_path_under_noise_dir_flags_home_tool_caches() {
        // Root-cause guard: a Linux remote opened at /root pegged ~200% CPU
        // because .cargo/.npm/.nvm/.bun/.claude under $HOME were watched and
        // re-walked on every cache write. If these stop being recognised,
        // the home-dir FS-event storm comes back.
        for p in [
            "/root/.cargo/registry/src/index/foo-1.0/src/lib.rs",
            "/root/.claude/projects/x/session.jsonl",
            "/root/.npm/_cacache/index-v5/aa/bb",
            "/root/.nvm/versions/node/v20/lib/x.js",
            "/root/.bun/install/cache/pkg/index.js",
            "/root/.local/share/pipx/venvs/tool/bin/x",
        ] {
            assert!(is_path_under_noise_dir(Path::new(p)), "{p}");
        }
    }

    /// The root-relative reading, which is the one every EVENT path wants:
    /// the workspace root's own ancestors are not part of the workspace.
    #[test]
    fn noise_is_measured_from_the_workspace_root_not_the_filesystem_root() {
        let root = Path::new("/home/dev/build/myapp");
        assert!(
            !is_noise_path_under_root(&root.join("src/a.rs"), root),
            "a source file in a workspace under build/ is not noise"
        );
        assert!(
            !is_noise_path_under_root(root, root),
            "nor is the root itself"
        );
        // Noise at or below the root still counts, by name and by run.
        for rel in ["target/debug/x", "node_modules/p/i.js", "go/pkg/mod/x"] {
            assert!(
                is_noise_path_under_root(&root.join(rel), root),
                "{rel} is noise below the root"
            );
        }
        // A path under NEITHER root is measured whole: there is no root for
        // its components to be relative to.
        assert!(is_noise_path_under_root(
            Path::new("/elsewhere/target/x"),
            root
        ));
        // The contrast that was the bug: read absolutely, the workspace's
        // own source file is noise because an ANCESTOR is named `build`.
        assert!(is_path_under_noise_dir(&root.join("src/a.rs")));
    }

    #[test]
    fn is_path_under_noise_dir_does_not_flag_workspace_source_paths() {
        assert!(!is_path_under_noise_dir(Path::new(
            "/Users/v/proj/src/app.rs"
        )));
        assert!(!is_path_under_noise_dir(Path::new(
            "/Users/v/proj/README.md"
        )));
        // A file literally named "target.md" must NOT be classified as
        // noise — the gate matches whole path components, not substrings.
        assert!(!is_path_under_noise_dir(Path::new(
            "/Users/v/proj/docs/target.md"
        )));
    }

    #[test]
    fn noise_rules_flag_dotnet_dart_and_go_module_caches() {
        // Root-cause guard for the second home-dir readlink storm: these
        // trees were absent from the ignore set, so opening /root walked
        // and watched all of them. Measured there: .nuget 1127 dirs,
        // .pub-cache 1271, go/pkg/mod 1357.
        // Name-matched, from a $HOME workspace root.
        for p in [
            "/root/.nuget/packages/microsoft.codeanalysis.common/5.0.0/lib/x.dll",
            "/root/.pub-cache/hosted/pub.dev/args-2.4.2/lib/args.dart",
            "/root/.dart-tool/pub/bin/x",
            "/root/.dotnet/sdk/8.0.100/x.dll",
            "/root/.oh-my-zsh/plugins/git/git.plugin.zsh",
        ] {
            assert!(is_path_under_noise_dir(Path::new(p)), "{p}");
        }
        // Run-matched, measured relative to the workspace root.
        for p in [
            "/root/go/pkg/mod/golang.org/x/tools@v0.28.0/go/analysis/x.go",
            "/root/go/pkg/sumdb/sum.golang.org/lookup/x",
        ] {
            assert!(has_noise_path_run(Path::new(p), Path::new("/root")), "{p}");
        }
    }

    #[test]
    fn build_file_index_indexes_a_workspace_opened_inside_the_module_cache() {
        // The root's own path is not part of the workspace: opening a
        // checked-out dependency to read it must still index its files.
        // Matching the absolute path pruned every entry at depth >= 1.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("go/pkg/mod/github.com/x/y@v1.0.0");
        std::fs::create_dir_all(root.join("internal")).unwrap();
        std::fs::write(root.join("main.go"), "x").unwrap();
        std::fs::write(root.join("internal/a.go"), "x").unwrap();
        let idx = build_file_index(&root);
        let mut rels: Vec<&str> = idx.iter().map(|e| e.rel.as_str()).collect();
        rels.sort_unstable();
        assert_eq!(rels, vec!["internal/a.go", "main.go"]);
    }

    #[test]
    fn build_file_index_indexes_a_workspace_under_a_noise_named_ancestor() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("build/myapp");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), "x").unwrap();
        let idx = build_file_index(&root);
        let rels: Vec<&str> = idx.iter().map(|e| e.rel.as_str()).collect();
        assert_eq!(rels, vec!["src/main.rs"]);
    }

    #[test]
    fn noise_rules_keep_a_real_go_source_tree() {
        // `go` alone is a legitimate source directory name, so only the
        // GOPATH cache runs are pruned. A project's own go/ must survive,
        // and so must a go/pkg that is not the module cache.
        let root = Path::new("/Users/v/proj");
        for p in [
            "/Users/v/proj/go/main.go",
            "/Users/v/proj/go/pkg/handler/handler.go",
            "/Users/v/proj/src/mod/thing.rs",
            "/Users/v/proj/pkg/mod/thing.go",
        ] {
            assert!(!has_noise_path_run(Path::new(p), root), "{p}");
            assert!(!is_path_under_noise_dir(Path::new(p)), "{p}");
        }
        // And the root's OWN path never makes its contents noise.
        assert!(!has_noise_path_run(
            Path::new("/home/u/go/pkg/mod/x@v1/main.go"),
            Path::new("/home/u/go/pkg/mod/x@v1")
        ));
    }

    /// A cache run can STRADDLE the root, and both halves of the rule have
    /// to hold at once.
    #[test]
    fn a_gopath_root_still_prunes_the_module_cache_below_it() {
        // A workspace opened at `~/go` - the ordinary GOPATH - keeps its
        // cache at `pkg/mod`, a run whose first component IS the root's own
        // last one. Stripping the root and matching only what was left
        // missed it, re-admitting the tree this rule exists to prune.
        let gopath = Path::new("/home/u/go");
        for p in [
            "/home/u/go/pkg/mod/x@v1/a.go",
            "/home/u/go/pkg/sumdb/lookup/y",
        ] {
            assert!(has_noise_path_run(Path::new(p), gopath), "{p}");
        }
        // The sibling source tree under that same root is not noise.
        assert!(!has_noise_path_run(
            Path::new("/home/u/go/src/app/main.go"),
            gopath
        ));
        // And the other half, unchanged: opening a dependency INSIDE the
        // cache indexes it, because there the whole run is the root's own
        // path rather than anything below it.
        assert!(!has_noise_path_run(
            Path::new("/home/u/go/pkg/mod/x@v1/a.go"),
            Path::new("/home/u/go/pkg/mod/x@v1")
        ));
        // The boundary that decides `>` from `>=`: the run ends exactly at
        // the root's last component, so the cache IS the root and its
        // contents are what the user asked to open.
        assert!(!has_noise_path_run(
            Path::new("/home/u/go/pkg/mod/x/a.go"),
            Path::new("/home/u/go/pkg/mod")
        ));
        // One component shallower, the run's tail lands below the root and
        // the same path is noise again.
        assert!(has_noise_path_run(
            Path::new("/home/u/go/pkg/mod/x/a.go"),
            Path::new("/home/u/go/pkg")
        ));
    }

    #[test]
    fn has_component_run_matches_only_consecutive_whole_components() {
        assert!(has_component_run(
            Path::new("/a/go/pkg/mod/b"),
            &["go", "pkg", "mod"],
            0
        ));
        // Non-consecutive: go/pkg/OTHER/mod must not match.
        assert!(!has_component_run(
            Path::new("/a/go/pkg/other/mod"),
            &["go", "pkg", "mod"],
            0
        ));
        // Substring of a component must not match.
        assert!(!has_component_run(
            Path::new("/a/cargo/pkg/mod"),
            &["go", "pkg", "mod"],
            0
        ));
        // The depth bound: the same run is ignored when it ends at or above
        // `min_end`, which is how a run inside the ROOT'S own path is kept
        // from making the workspace's contents noise.
        assert!(!has_component_run(
            Path::new("/a/go/pkg/mod/b"),
            &["go", "pkg", "mod"],
            4
        ));
        assert!(has_component_run(
            Path::new("/a/go/pkg/mod/b"),
            &["go", "pkg", "mod"],
            3
        ));
    }

    #[test]
    fn build_file_index_skips_the_go_module_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("go/pkg/mod/golang.org/x")).unwrap();
        std::fs::write(root.join("go/pkg/mod/golang.org/x/cached.go"), "x").unwrap();
        std::fs::create_dir_all(root.join("go/cmd")).unwrap();
        std::fs::write(root.join("go/cmd/main.go"), "x").unwrap();
        let idx = build_file_index(root);
        let rels: Vec<&str> = idx.iter().map(|e| e.rel.as_str()).collect();
        assert!(rels.contains(&"go/cmd/main.go"), "{rels:?}");
        assert!(
            !rels.iter().any(|r| r.contains("pkg/mod")),
            "module cache leaked into the index: {rels:?}"
        );
    }

    /// The degenerate root: the walk STARTS at the GOPATH, so the cache run
    /// straddles the root instead of lying below it.
    ///
    /// `build_file_index_skips_the_go_module_cache` cannot see this: it roots
    /// at `tmp` with the cache at `tmp/go/pkg/mod`, where the whole run is
    /// below the root, so it passed both before and after the straddle fix.
    #[test]
    fn build_file_index_skips_the_module_cache_of_a_gopath_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("go");
        std::fs::create_dir_all(root.join("pkg/mod/golang.org/x")).unwrap();
        std::fs::write(root.join("pkg/mod/golang.org/x/cached.go"), "x").unwrap();
        std::fs::create_dir_all(root.join("cmd")).unwrap();
        std::fs::write(root.join("cmd/main.go"), "x").unwrap();
        let idx = build_file_index(&root);
        let rels: Vec<&str> = idx.iter().map(|e| e.rel.as_str()).collect();
        assert!(rels.contains(&"cmd/main.go"), "{rels:?}");
        assert!(
            !rels.iter().any(|r| r.contains("pkg/mod")),
            "a GOPATH root's own module cache leaked into the index: {rels:?}"
        );
    }
}
