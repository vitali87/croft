//! The search itself: walking the workspace in parallel, honouring .gitignore and leaving
//! out binaries and version-control stores, streaming each file's hits, and searching an
//! unsaved buffer's text the same way.

use super::*;

pub(super) const MAX_LINE_LEN: usize = 200;

pub(super) struct HitSink<'a> {
    path: PathBuf,
    matcher: &'a SearchMatcher,
    batch: &'a mut Vec<SearchHit>,
    cancel: Option<Arc<AtomicBool>>,
}

impl<'a> Sink for HitSink<'a> {
    type Error = std::io::Error;

    fn matched(
        &mut self,
        _searcher: &Searcher,
        mat: &SinkMatch<'_>,
    ) -> Result<bool, std::io::Error> {
        if let Some(c) = self.cancel.as_ref()
            && c.load(Ordering::Relaxed)
        {
            return Ok(false);
        }
        let line_no = mat.line_number().unwrap_or(0) as usize;
        let line_str = std::str::from_utf8(mat.bytes()).unwrap_or("");
        let stripped = line_str.trim_end_matches(['\r', '\n']);
        // The indent is trimmed only up to where the first match starts: a
        // match that begins in it (a query starting with whitespace) would
        // otherwise lose its highlight in the preview.
        let indent = stripped.len() - stripped.trim_start().len();
        let start = grep_matcher::Matcher::find(self.matcher, stripped.as_bytes())
            .ok()
            .flatten()
            .map_or(indent, |m| m.start().min(indent));
        let trimmed = stripped.get(start..).unwrap_or(stripped.trim_start());
        let cut = if trimmed.chars().count() > MAX_LINE_LEN {
            // Cut plainly, no trailing ellipsis marker (no ellipsis anywhere).
            trimmed.chars().take(MAX_LINE_LEN).collect()
        } else {
            trimmed.to_string()
        };
        self.batch.push(SearchHit {
            path: self.path.clone(),
            line_no,
            line_text: cut,
            // A listed line is at least one result, even one whose bytes
            // are not UTF-8 and so cannot be counted.
            matches: count_in_line(stripped, self.matcher).max(1),
        });
        Ok(true)
    }
}

/// The query as the user typed it (#1175): spaces and tabs are part of
/// what's searched, so `return ` skips `returned`. Only a line break at
/// either end, the usual leftover of a paste, is dropped.
pub(crate) fn as_typed(query: &str) -> &str {
    query.trim_matches(|c| c == '\r' || c == '\n')
}

/// Stream every match for `query` under `root` into `on_batch`, one batch
/// per file. Honours `.gitignore` and skips binaries via NUL detection.
/// Walks in parallel using `WalkBuilder::build_parallel` and uses
/// `grep-searcher` + `grep-regex` (the same engine that powers ripgrep).
/// Set `cancel` to abort an in-flight scan.
pub fn search_workspace_streaming<F>(
    root: &Path,
    query: &str,
    opts: SearchOpts,
    cancel: &Arc<AtomicBool>,
    on_batch: F,
) where
    F: FnMut(Vec<SearchHit>) + Send + Clone,
{
    search_workspace_streaming_skipping(root, query, opts, cancel, &HashSet::new(), on_batch);
}

/// Like `search_workspace_streaming` but skips any walked file whose
/// canonical path is in `skip`. Used for dirty-aware search: files open in
/// unsaved editors are searched from their in-memory buffer instead, so the
/// stale on-disk copy must not also contribute (duplicate / outdated) hits.
pub fn search_workspace_streaming_skipping<F>(
    root: &Path,
    query: &str,
    opts: SearchOpts,
    cancel: &Arc<AtomicBool>,
    skip: &HashSet<PathBuf>,
    on_batch: F,
) where
    F: FnMut(Vec<SearchHit>) + Send + Clone,
{
    search_workspace_streaming_filtered(
        root,
        query,
        opts,
        cancel,
        skip,
        &PathFilter::default(),
        on_batch,
    );
}

/// Like `search_workspace_streaming_skipping` but also honours a `PathFilter`
/// (VS Code's "files to include" / "files to exclude" glob inputs). A file is
/// searched only when `filter.allows(root, path)` returns true.
pub fn search_workspace_streaming_filtered<F>(
    root: &Path,
    query: &str,
    opts: SearchOpts,
    cancel: &Arc<AtomicBool>,
    skip: &HashSet<PathBuf>,
    filter: &PathFilter,
    on_batch: F,
) where
    F: FnMut(Vec<SearchHit>) + Send + Clone,
{
    let q = as_typed(query);
    if q.is_empty() {
        return;
    }
    let Some(matcher) = build_matcher(q, opts) else {
        return;
    };
    // Dotfiles are searched (#1648): `.env`, `.github/workflows` and lint
    // configs are among the files most searched, and Quick Open already
    // lists them. `hidden(true)` skipped every dot-entry, whole `.github`
    // tree included. A repository's own store stays out, as VS Code's
    // default `files.exclude` keeps it out.
    let walker = WalkBuilder::new(root)
        .git_ignore(true)
        .hidden(false)
        .filter_entry(|entry| entry.depth() == 0 || !is_vcs_store(entry.file_name()))
        .build_parallel();
    let root = root.to_path_buf();
    walker.run(|| {
        let matcher = matcher.clone();
        let cancel = cancel.clone();
        let skip = skip.clone();
        let filter = filter.clone();
        let root = root.clone();
        let mut on_batch = on_batch.clone();
        Box::new(move |entry| {
            if cancel.load(Ordering::Relaxed) {
                return WalkState::Quit;
            }
            let entry = match entry {
                Ok(e) => e,
                Err(_) => return WalkState::Continue,
            };
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                return WalkState::Continue;
            }
            let path = entry.path();
            // Apply the include/exclude glob filter before any IO on the file.
            if !filter.is_empty() && !filter.allows(&root, path) {
                return WalkState::Continue;
            }
            // Dirty files are searched from their in-memory buffer elsewhere;
            // skip the stale disk copy. Only pay the canonicalize syscall when
            // there is actually something to skip.
            if !skip.is_empty() && path.canonicalize().is_ok_and(|canon| skip.contains(&canon)) {
                return WalkState::Continue;
            }
            let mut batch: Vec<SearchHit> = Vec::new();
            {
                let mut sink = HitSink {
                    path: path.to_path_buf(),
                    matcher: &matcher,
                    batch: &mut batch,
                    cancel: Some(cancel.clone()),
                };
                let mut searcher = build_searcher();
                let _ = searcher.search_path(&matcher, path, &mut sink);
            }
            if !batch.is_empty() {
                on_batch(batch);
            }
            WalkState::Continue
        })
    });
}

/// A version-control store (`.git`, `.hg`, `.svn`, `.bzr`, `CVS`): never
/// searched, as VS Code's default `files.exclude` keeps it out. `.git` can be
/// a worktree's gitlink file as well as a directory.
pub(super) fn is_vcs_store(name: &std::ffi::OsStr) -> bool {
    matches!(
        name.to_str(),
        Some(".git" | ".hg" | ".svn" | ".bzr" | "CVS")
    )
}

/// Aggregating wrapper around `search_workspace_streaming`. Returns every
/// match - no arbitrary cap. Honours `.gitignore`. Order is non-deterministic
/// because the walker is parallel; callers that need ordering must sort.
pub fn search_workspace(root: &Path, query: &str, opts: SearchOpts) -> Vec<SearchHit> {
    let cancel = Arc::new(AtomicBool::new(false));
    let collected: Arc<std::sync::Mutex<Vec<SearchHit>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let collected_for_cb = collected.clone();
    search_workspace_streaming(root, query, opts, &cancel, move |batch| {
        if let Ok(mut g) = collected_for_cb.lock() {
            g.extend(batch);
        }
    });
    Arc::try_unwrap(collected)
        .map(|m| m.into_inner().unwrap_or_default())
        .unwrap_or_default()
}

/// Cheap "could this line match at all?" pre-filter for bulk scans.
///
/// [`split_for_highlight`] copies the whole line (twice, when the search is
/// case-insensitive) before it can answer, which is nothing on a screenful
/// and everything when sweeping megabytes of log. This answers the same
/// question without allocating, and is deliberately CONSERVATIVE: it may say
/// `true` for a line that turns out not to match, and must never say `false`
/// for one that does, so a caller can only use it to skip work.
///
/// `whole_word` is ignored on purpose: it only ever narrows the match set, so
/// plain containment stays a superset. Regex and non-ASCII fall straight
/// through to the exact path rather than reimplementing their semantics here.
pub fn line_may_match(line: &str, needle: &str, opts: SearchOpts) -> bool {
    if needle.is_empty() || opts.use_regex {
        return true;
    }
    if !line.is_ascii() || !needle.is_ascii() {
        return true;
    }
    if opts.case_sensitive {
        return line.contains(needle);
    }
    let (hay, pat) = (line.as_bytes(), needle.as_bytes());
    if pat.len() > hay.len() {
        return false;
    }
    hay.windows(pat.len()).any(|w| w.eq_ignore_ascii_case(pat))
}

/// Per-text search helper. Wraps `grep-searcher` so the matching logic is
/// identical to the file-walking engine (same regex, line numbers, trimming).
/// Honours the three `SearchOpts` toggles. Used both by tests and by the
/// dirty-aware worker to search unsaved in-memory buffers. Regex compilation
/// failure for invalid patterns is silent: returns no matches.
pub fn collect_matches_in_text(
    path: &Path,
    content: &str,
    query: &str,
    opts: SearchOpts,
    out: &mut Vec<SearchHit>,
) {
    let q = as_typed(query);
    if q.is_empty() {
        return;
    }
    let Some(matcher) = build_matcher(q, opts) else {
        return;
    };
    let mut sink = HitSink {
        path: path.to_path_buf(),
        matcher: &matcher,
        batch: out,
        cancel: None,
    };
    let mut searcher = SearcherBuilder::new()
        .line_number(true)
        .binary_detection(BinaryDetection::quit(b'\x00'))
        .build();
    let _ = searcher.search_slice(&matcher, content.as_bytes(), &mut sink);
}
