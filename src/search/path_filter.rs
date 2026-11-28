//! The files to include and to exclude: their comma-separated globs, compiled as VS Code
//! reads them (a bare pattern matches at any depth, a folder's name admits what is in
//! it), and the test a path must pass.

use super::*;

/// Compiled `files to include` / `files to exclude` globs, mirroring VS
/// Code's two Search-sidebar glob inputs. Both inputs take a comma-separated
/// list of globs. A bare pattern with no path separator (e.g. `*.rs`) is
/// treated as match-at-any-depth (`**/*.rs`), exactly like VS Code, so users
/// don't have to remember to prefix `**/`.
#[derive(Clone, Debug, Default)]
pub struct PathFilter {
    include: Option<GlobSet>,
    /// The include globs that name a folder (#958): those whose last part
    /// has no wildcard, such as `tests` or `src/widgets`. They also admit
    /// what is inside the folder; `*.rs` does not admit a folder named
    /// `generated.rs`.
    include_dirs: Option<GlobSet>,
    exclude: Option<GlobSet>,
}

pub(super) fn compile_glob(pat: &str) -> Option<Glob> {
    // A leading `/` anchors the pattern to the root, as in .gitignore: the
    // form a VS Code exclude setting's bare `build` arrives in (#1479). Its
    // `*` stays within one folder, so `/*.log` is the root's logs only.
    if let Some(anchored) = pat.strip_prefix('/') {
        return globset::GlobBuilder::new(anchored)
            .literal_separator(true)
            .build()
            .ok();
    }
    // VS Code convention: a pattern without a path separator matches at
    // any depth, so `*.rs` finds Rust files in every subdirectory, and a
    // leading `./` anchors the pattern to the workspace root.
    let expanded = if let Some(anchored) = pat.strip_prefix("./") {
        anchored.to_string()
    } else if pat.contains('/') {
        pat.to_string()
    } else {
        format!("**/{pat}")
    };
    Glob::new(&expanded).ok()
}

/// The entries of a glob list that don't compile, for the panel to name.
pub fn invalid_globs(raw: &str) -> Vec<&str> {
    split_globs(raw)
        .filter(|p| compile_glob(p).is_none())
        .collect()
}

/// Build a `GlobSet` from a comma-separated glob list, returning `None` when
/// the trimmed input is empty (meaning "no filter"). Invalid individual globs
/// are skipped so one typo doesn't disable the rest. When none compiles, an
/// include list (`fail_closed`) yields an empty set that matches nothing, so
/// a broken filter never widens a search or Replace All to every file.
pub(super) fn build_glob_set(raw: &str, fail_closed: bool) -> Option<GlobSet> {
    build_glob_set_of(raw, fail_closed, |_| true)
}

/// [`build_glob_set`] over the entries `keep` accepts.
pub(super) fn build_glob_set_of(
    raw: &str,
    fail_closed: bool,
    keep: impl Fn(&str) -> bool,
) -> Option<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    let mut any = false;
    let mut typed = false;
    for pat in split_globs(raw).filter(|p| keep(p)) {
        typed = true;
        if let Some(g) = compile_glob(pat) {
            builder.add(g);
            any = true;
        }
    }
    if !any && !(typed && fail_closed) {
        return None;
    }
    builder.build().ok()
}

/// Whether a glob names a folder rather than a kind of file: its last part
/// is a plain name, as in `tests`, `src/widgets` or `./src`, not a pattern
/// such as `*.rs` (#958).
pub(super) fn names_a_folder(pat: &str) -> bool {
    let last = pat.trim_end_matches('/').rsplit('/').next().unwrap_or(pat);
    !last.is_empty() && !last.contains(['*', '?', '[', '{'])
}

/// True when `rel` or one of the folders above it matches `set`: a glob that
/// names a folder stands for everything inside it.
pub(super) fn matches_self_or_parent(set: &GlobSet, rel: &Path) -> bool {
    rel.ancestors()
        .any(|p| !p.as_os_str().is_empty() && set.is_match(p))
}

impl PathFilter {
    /// Compile a filter from the raw include/exclude input strings. An empty
    /// (or all-whitespace) string disables that side of the filter.
    pub fn new(include: &str, exclude: &str) -> Self {
        Self {
            include: build_glob_set(include, true),
            include_dirs: build_glob_set_of(include, false, names_a_folder),
            exclude: build_glob_set(exclude, false),
        }
    }

    /// True when no include and no exclude globs are active, so the walker can
    /// skip the per-file relative-path work entirely.
    pub fn is_empty(&self) -> bool {
        self.include.is_none() && self.exclude.is_none()
    }

    /// Decide whether `path` (under `root`) should be searched. Matching is
    /// done against the path relative to `root` (falling back to the full path
    /// when it isn't under `root`). Include wins first: if an include set is
    /// present, the path must match it; then any exclude match drops the path.
    /// On both sides a glob that names a folder covers what is inside it, as
    /// in VS Code, so include `tests` or `src/widgets` searches those folders
    /// (#958) and exclude `dist` hides everything under it (#1345). A folder
    /// is only looked for inside the workspace: a file outside it (an open
    /// buffer elsewhere) is not let in by a folder above the workspace.
    pub fn allows(&self, root: &Path, path: &Path) -> bool {
        let inside = path.strip_prefix(root).ok();
        let rel = inside.unwrap_or(path);
        if let Some(inc) = &self.include
            && !inc.is_match(rel)
            && !inside.is_some_and(|rel| {
                self.include_dirs
                    .as_ref()
                    .is_some_and(|dirs| matches_self_or_parent(dirs, rel))
            })
        {
            return false;
        }
        if let Some(exc) = &self.exclude
            && matches_self_or_parent(exc, rel)
        {
            return false;
        }
        true
    }

    /// True when `path` (under `root`) or a folder above it matches an
    /// exclude glob. An excluded folder hides everything inside it, as VS
    /// Code's exclude settings do (#1345).
    pub fn excludes(&self, root: &Path, path: &Path) -> bool {
        let rel = path.strip_prefix(root).unwrap_or(path);
        self.exclude
            .as_ref()
            .is_some_and(|exc| matches_self_or_parent(exc, rel))
    }

    /// An exclude-only filter from a list of globs (the project's exclude
    /// settings, #1345).
    pub fn excluding(globs: &[String]) -> Self {
        Self::new("", &globs.join(", "))
    }
}
