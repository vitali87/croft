//! The bottom panel and its tab strip, the OUTPUT picker, PROBLEMS and the diagnostics
//! store, the whole-project check, build matchers, and the PORTS panel.

use super::*;

/// Rows the port toast takes: its box, a title line and a button row. The
/// update card stacks above it when both are up.
pub(super) const PORT_TOAST_HEIGHT: u16 = 4;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum PortToastAction {
    ForwardAndOpen,
    ForwardOnly,
    Open,
    Dismiss,
}

/// How long the port-detection toast lingers before auto-dismissing.
pub(super) const PORT_TOAST_TTL: Duration = Duration::from_secs(12);

/// How often the socket-table poll runs (the output scrape is continuous;
/// this is the backstop for servers that announce nothing).
pub(super) const PORT_POLL_INTERVAL: Duration = Duration::from_secs(3);

/// What activating a PORTS row (Enter / double-click) should do, computed from
/// the entry and whether this is a remote (relay-active) session. Pure so it's
/// testable without spawning a browser or standing up a relay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PortOpenAction {
    /// Remote port not yet forwarded: forward it home, then open.
    Forward(u16),
    /// A locally reachable address to open in the user's browser via the relay
    /// — a remote session, so the opener must run on the local Mac, not the
    /// headless remote.
    OpenViaRelay(String),
    /// A locally reachable address to open directly (local session).
    OpenDirect(String),
    /// Nothing actionable.
    Nothing,
}

pub(super) fn plan_port_open(
    entry: &crate::widgets::ports::PortEntry,
    relay_active: bool,
) -> PortOpenAction {
    use crate::widgets::ports::PortOrigin;
    if matches!(entry.origin, PortOrigin::Remote(_)) && !entry.forwarded {
        return PortOpenAction::Forward(entry.port);
    }
    match entry.open_url() {
        Some(url) if relay_active => PortOpenAction::OpenViaRelay(url),
        Some(url) => PortOpenAction::OpenDirect(url),
        None => PortOpenAction::Nothing,
    }
}

impl App {
    /// Lints every open `.md` tab and folds the results into the same
    /// diagnostics store a real language server writes to (`lsp_diagnostics`,
    /// keyed by path then by "producer"), under the synthetic server name
    /// `"Markdown Lint"`. It needs no language server, so it works the moment
    /// a file opens and on a box where none could be installed. For a file
    /// rumdl (#851) reports on, which covers these same rules among its own,
    /// it stands down instead, so no violation is listed twice; a file rumdl
    /// does not see (one shown only in a split's other group, which is not
    /// sent to the language servers, or one rumdl stopped answering for)
    /// keeps the built-in lint. Gated by its own
    /// edit-seq cursor (`markdown_lint_last_seen`) so an unchanged buffer
    /// isn't re-linted every tick. Returns whether any editor's overlay or
    /// the PROBLEMS panel changed, for the caller's redraw decision.
    pub fn sync_markdown_lint(&mut self) -> bool {
        let mut current: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
        let mut to_lint: Vec<(PathBuf, Vec<String>, u64)> = Vec::new();
        let mut superseded: Vec<PathBuf> = Vec::new();
        // Both collections: `self.editor` is the ACTIVE group's tabs, and a
        // split's other panes live in `editor_layout`. Walking only the
        // active one left a `.md` open in an inactive pane unlinted, and the
        // `closed` sweep below then dropped its diagnostics as though the tab
        // had gone - squiggles vanishing from a pane in plain sight.
        let inactive_tabs: Vec<&crate::widgets::editor::Editor> = self
            .editor_layout
            .inactive_groups()
            .into_iter()
            .flat_map(|g| g.editors.iter())
            .collect();
        for tab in self.editor.iter_tabs().chain(inactive_tabs) {
            if tab.has_non_text_view() {
                continue;
            }
            let Some(path) = tab.path.as_ref() else {
                continue;
            };
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            // The same file can be open in several panes, each its own
            // buffer with its own `edit_seq`. Only the FIRST tab seen for a
            // path (the active group comes first) is compared: judging each
            // pane against one shared cursor made two panes with different
            // seqs take turns, re-linting and redrawing on every tick.
            if !current.insert(path.clone()) {
                continue;
            }
            if self.lsp.as_ref().is_some_and(|lsp| {
                lsp.document_reported_by(path, crate::markdown_lint::SUPERSEDED_BY)
            }) {
                superseded.push(path.clone());
                continue;
            }
            // The seq alone cannot tell two panes apart: each counts its own
            // edits, so they can agree while holding different text. The
            // lines last linted are kept too and compared when the seqs
            // match (one comparison, no allocation), so switching to the
            // other pane re-lints exactly when its text differs.
            let seq = tab.edit_seq;
            let seen = self
                .markdown_lint_last_seen
                .get(path)
                .is_some_and(|(s, lines)| *s == seq && *lines == tab.lines);
            if !seen {
                to_lint.push((path.clone(), tab.lines.clone(), seq));
            }
        }
        let mut changed = false;
        for path in superseded {
            changed |= self.withdraw_markdown_lint(&path);
        }
        for (path, lines, seq) in to_lint {
            let diags = crate::markdown_lint::lint(&lines.join("\n"));
            let by_server = self.lsp_diagnostics.entry(path.clone()).or_default();
            if diags.is_empty() {
                changed |= by_server.remove("Markdown Lint").is_some();
                if by_server.is_empty() {
                    self.lsp_diagnostics.remove(&path);
                }
            } else if by_server.get("Markdown Lint") != Some(&diags) {
                // Only a different set is a change: storing the same
                // diagnostics again must not ask for a redraw.
                by_server.insert("Markdown Lint".to_string(), diags);
                changed = true;
            }
            self.markdown_lint_last_seen
                .insert(path.clone(), (seq, lines));
            let merged = self.merged_diagnostics(&path);
            if self.editor.path.as_deref() == Some(path.as_path()) {
                self.editor.apply_diagnostics(path.clone(), merged.clone());
            }
            for group in self.editor_layout.inactive_groups_mut() {
                if group.path.as_deref() == Some(path.as_path()) {
                    group.apply_diagnostics(path.clone(), merged.clone());
                }
            }
        }
        let closed: Vec<PathBuf> = self
            .markdown_lint_last_seen
            .keys()
            .filter(|p| !current.contains(*p))
            .cloned()
            .collect();
        for p in closed {
            self.markdown_lint_last_seen.remove(&p);
            if let Some(by_server) = self.lsp_diagnostics.get_mut(&p) {
                changed |= by_server.remove("Markdown Lint").is_some();
                if by_server.is_empty() {
                    self.lsp_diagnostics.remove(&p);
                }
            }
        }
        if changed {
            self.rebuild_problems();
            self.refresh_problems_badge();
        }
        changed
    }

    /// Withdraws what the built-in Markdown lint published for `path`, a
    /// file rumdl reports on (#851), and repaints the tabs showing it. The
    /// caller rebuilds PROBLEMS when this returns true. Forgetting the seen
    /// cursor makes the lint run afresh on the file once rumdl stops
    /// reporting on it.
    pub(super) fn withdraw_markdown_lint(&mut self, path: &Path) -> bool {
        if self.markdown_lint_last_seen.remove(path).is_none() {
            return false;
        }
        let Some(by_server) = self.lsp_diagnostics.get_mut(path) else {
            return false;
        };
        if by_server.remove("Markdown Lint").is_none() {
            return false;
        }
        if by_server.is_empty() {
            self.lsp_diagnostics.remove(path);
        }
        let merged = self.merged_diagnostics(path);
        if self.editor.path.as_deref() == Some(path) {
            self.editor
                .apply_diagnostics(path.to_path_buf(), merged.clone());
        }
        for group in self.editor_layout.inactive_groups_mut() {
            if group.path.as_deref() == Some(path) {
                group.apply_diagnostics(path.to_path_buf(), merged.clone());
            }
        }
        true
    }

    /// Flatten the per-server diagnostics stored for `path` into one list for
    /// the editor. Servers are layered, not merged across (each owns its own
    /// findings), so this is a simple concatenation of every server's set.
    pub(super) fn merged_diagnostics(&self, path: &Path) -> Vec<crate::lsp::manager::Diagnostic> {
        self.lsp_diagnostics
            .get(path)
            .map(|by_server| {
                diagnostics_by_source(by_server)
                    .map(|(_, d)| d.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The producer id for the whole-project check (#256).
    ///
    /// `build_diag_files_by_pane` keys a producer's contribution so its next
    /// run replaces its previous one. Pane uids are allocated from 1 upward
    /// by an `AtomicU64`, so `u64::MAX` cannot collide with a real pane. The
    /// sweep needs its OWN id rather than borrowing one: sharing would make a
    /// pane's next command wipe the sweep's rows, and the sweep wipe the
    /// pane's.
    pub(super) const PROJECT_CHECK_ID: u64 = u64::MAX;

    /// The first line with something to say, escapes stripped (#256).
    ///
    /// A checker's failure output is not a tidy single line: npx opens with
    /// a blank line and an ANSI-coloured banner, so both a leading-blank
    /// skip and the stripping are needed for the message to carry a reason
    /// rather than an escape sequence.
    pub(super) fn first_meaningful_line(text: &str) -> Option<String> {
        text.lines()
            .map(|l| crate::remote_connect::strip_ansi(l.as_bytes()))
            .map(|l| l.trim().to_string())
            .find(|l| !l.is_empty())
    }

    /// Take a finished project check off the channel and report it (#256).
    ///
    /// Three outcomes, deliberately distinguished. A spawn failure names the
    /// error. A run that produced no rows AND exited non-zero is an
    /// INVOCATION failure, not a clean project: `tsc` prints global errors
    /// like `error TS5023: Unknown compiler option` with no `file(line,col)`
    /// prefix, so no matcher claims them and the row count is zero - exactly
    /// what a clean project looks like. Reporting "no problems" there tells
    /// the user their code is fine when the checker never checked it.
    /// Otherwise the count is this producer's own, not the panel's.
    pub(super) fn drain_project_check(&mut self) -> bool {
        let Ok((cwd, done)) = self.project_check_rx.try_recv() else {
            return false;
        };
        self.project_check_running = false;
        match done {
            Err(e) => {
                // true, not false: the status changed, so the frame must be
                // redrawn or the message waits for an unrelated repaint.
                self.status = format!("Could not run the project checker: {e}");
                true
            }
            Ok(out) => {
                let text = String::from_utf8_lossy(&out.stdout).into_owned();
                self.ingest_project_check(Some(&cwd), &text);
                self.rebuild_problems();
                let n = self.project_check_count();
                if n == 0 && !out.status.success() {
                    // The first NON-BLANK line, from either stream. Which
                    // stream carries the reason is not predictable: npx
                    // reports a missing compiler on STDOUT (with an empty
                    // stderr and a leading blank line), while tsc's own
                    // global errors also go to stdout and a missing binary
                    // goes to stderr. Taking `.next()` showed the blank line
                    // and reported a failure with no reason at all.
                    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
                    let why = Self::first_meaningful_line(&stderr)
                        .or_else(|| Self::first_meaningful_line(&text))
                        .unwrap_or_else(|| String::from("no output"));
                    self.status = format!("The project checker failed: {why}");
                } else {
                    self.status = match n {
                        0 => String::from("Whole-project check: no problems"),
                        1 => String::from("Whole-project check: 1 problem"),
                        n => format!("Whole-project check: {n} problems"),
                    };
                }
                true
            }
        }
    }

    /// Queue a finished check on the REAL channel WITHOUT draining (#256).
    ///
    /// Test-only. The sibling helper drains immediately, which is right for
    /// testing the reporting logic but wrong for the same-tick race: the
    /// hint is applied by `sync_explorer_panels`, so the test must let that
    /// function do the draining.
    #[cfg(all(test, unix))]
    pub(crate) fn queue_project_check_result_for_test(
        &mut self,
        cwd: &Path,
        stdout: &str,
        stderr: &str,
        success: bool,
    ) {
        use std::os::unix::process::ExitStatusExt;
        let out = std::process::Output {
            status: std::process::ExitStatus::from_raw(if success { 0 } else { 256 }),
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        };
        self.project_check_running = true;
        let _ = self.project_check_tx.send((cwd.to_path_buf(), Ok(out)));
    }

    /// Whether the session's one-shot size hint has been spent (#256).
    /// Test-only reader; the field is private.
    #[cfg(all(test, unix))]
    pub(crate) fn project_check_hinted_for_test(&self) -> bool {
        self.project_check_hinted
    }

    /// Queue a size hint on the REAL channel (#256). Test-only.
    ///
    /// Does not drain: the point is to have it in flight when the result
    /// arrives, which is the race that erased it.
    #[cfg(all(test, unix))]
    pub(crate) fn send_project_hint_for_test(&mut self) {
        let _ = self.project_hint_tx.send(());
    }

    /// Push a finished check through the REAL channel and drain it (#256).
    ///
    /// Test-only, and deliberately not a shortcut: it sends down
    /// `project_check_tx` and calls `drain_project_check`, so the reporting
    /// logic under test is the shipped one. A helper that set the status
    /// itself would pass while the drain was broken.
    /// Unix-only: `ExitStatusExt::from_raw` has a different meaning on
    /// Windows, and an ungated spawning helper breaks that target's test
    /// build without failing anything here or in CI.
    #[cfg(all(test, unix))]
    pub(crate) fn finish_project_check_for_test(
        &mut self,
        cwd: &Path,
        stdout: &str,
        stderr: &str,
        success: bool,
    ) {
        use std::os::unix::process::ExitStatusExt;
        let out = std::process::Output {
            status: std::process::ExitStatus::from_raw(if success { 0 } else { 256 }),
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        };
        self.project_check_running = true;
        let _ = self.project_check_tx.send((cwd.to_path_buf(), Ok(out)));
        self.drain_project_check();
    }

    /// Scan a whole-project checker's output into PROBLEMS (#256).
    ///
    /// Routed through `apply_build_scan` rather than a parallel path, so the
    /// sweep inherits the matcher table (tsc's two output shapes are already
    /// handled), the relative-path resolution, and the replace-wholesale
    /// semantics a re-run needs. `cwd` is where the checker ran, which is
    /// what its relative paths are against; `None` means the workspace root.
    pub(crate) fn ingest_project_check(&mut self, cwd: Option<&Path>, output: &str) -> bool {
        let root = self.workspace_root().to_path_buf();
        let cwd = cwd.unwrap_or(&root).to_path_buf();
        // Count what THIS scan produced, before it is merged into a store
        // shared with every other producer.
        self.project_check_rows = self.matchers.scan_batch(output, "tsc").len();
        self.apply_build_scan(Self::PROJECT_CHECK_ID, Some(&cwd), "tsc", output)
    }

    /// How many files put a root out of the sweep's automatic reach (#256).
    ///
    /// VS Code's own workspace-diagnostics guidance draws the line around
    /// the size where a project-wide compile stops being a background cost
    /// and starts being the foreground one. The exact number matters less
    /// than having one: the setting overrides it either way.
    pub(super) const PROJECT_CHECK_AUTO_MAX_FILES: usize = 100_000;

    /// Whether `root` holds more than `limit` files worth counting (#256).
    ///
    /// Stops at `limit + 1` rather than completing the walk. Counting a
    /// giant tree to decide not to walk a giant tree would cost exactly what
    /// the cap exists to avoid, and the question is a threshold one - the
    /// total is never used.
    ///
    /// Ignored and vendored directories do not count: `node_modules` alone
    /// puts most JS projects over any sensible cap, and disabling the sweep
    /// for JS projects would disable it for the projects it is FOR.
    pub(crate) fn workspace_exceeds(root: &Path, limit: usize) -> bool {
        let mut seen = 0usize;
        for entry in ignore::WalkBuilder::new(root)
            .git_ignore(true)
            .require_git(false)
            .hidden(false)
            .filter_entry(|e| {
                e.depth() == 0
                    || !e.file_type().is_some_and(|t| t.is_dir())
                    || !crate::widgets::file_finder::is_noise_dir(e.file_name())
            })
            .build()
        {
            let Ok(entry) = entry else { continue };
            if entry.file_type().is_some_and(|t| t.is_file()) {
                seen += 1;
                if seen > limit {
                    return true;
                }
            }
        }
        false
    }

    /// Whether the sweep may run WITHOUT the user asking, for `mode` (#256).
    ///
    /// `on` and `off` are answers, not hints: a cap that only ever declined
    /// would make `on` useless on the monorepos that most want it, and a cap
    /// that could not be turned off would surprise someone who knows their
    /// tree. `auto` asks the size question. Anything else reads as `auto`
    /// rather than as `on`, so a typo cannot start a compile on a 100k-file
    /// root - the failure the cap exists to prevent.
    pub(crate) fn project_check_auto_enabled(root: &Path, mode: &str, limit: usize) -> bool {
        match Self::project_scope_mode(mode) {
            "on" => true,
            "off" => false,
            _ => !Self::workspace_exceeds(root, limit),
        }
    }

    /// Normalise a `problems_project_scope` value (#256).
    ///
    /// Mirrors `ProblemScope::from_config`: a hand-edited config saying
    /// `"Off"` or `" off "` means off, and reading it as `auto` would give
    /// the user a size-dependent maybe when they asked for never. Anything
    /// unrecognised still becomes `auto`, so a typo cannot turn the sweep
    /// ON. Returns a `&'static str` so the cycle and the predicate agree by
    /// construction rather than by both remembering to normalise.
    pub(crate) fn project_scope_mode(raw: &str) -> &'static str {
        match raw.trim().to_ascii_lowercase().as_str() {
            "on" | "always" => "on",
            "off" | "never" => "off",
            _ => "auto",
        }
    }

    /// The whole-project check command for this workspace (#256).
    ///
    /// `tsc --noEmit` when a `tsconfig.json` sits at the root, which is the
    /// path VS Code users already take: vtsls exposes project-wide checking
    /// poorly over LSP, so the pragmatic route is the compiler itself. `None`
    /// when nothing is recognised - the command then says so rather than
    /// running a guess.
    ///
    /// The PROJECT's compiler first, `npx --no-install` second. Bare `npx
    /// tsc` on a project without TypeScript installed does not fail: it
    /// fetches `tsc` from the registry, which is an unrelated Unix
    /// terminal-control tool, and runs THAT against the user's source. The
    /// `--no-install` flag turns a missing compiler into an error, which is
    /// the honest outcome.
    ///
    /// A solution-style root tsconfig (`"files": []` plus `references`, the
    /// layout Vite's TypeScript templates and most monorepos use) compiles
    /// nothing on its own: plain `tsc --noEmit` exits 0 having checked no
    /// file, which read as a clean project (#1291). Each referenced project
    /// is checked instead, `tsc -p <ref> --noEmit`, which writes nothing (no
    /// `.tsbuildinfo`, unlike `tsc -b`); nested solution configs are followed
    /// to their leaves, and a root that also owns files is checked too. A
    /// root that names no files and no references is `ProjectCheck::Empty`,
    /// never run and never "clean". Because nothing is emitted, a project
    /// importing another referenced project whose declarations were never
    /// built gets tsc's TS6305 error: reported as a problem, not hidden.
    pub(super) fn project_check_command(root: &Path) -> Option<ProjectCheck> {
        let text = std::fs::read_to_string(root.join("tsconfig.json")).ok()?;
        let mut runs: Vec<Vec<String>> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let root_config = root.join("tsconfig.json");
        seen.insert(std::fs::canonicalize(&root_config).unwrap_or(root_config));
        Self::collect_tsc_runs(root, None, &text, &mut seen, &mut runs);
        if runs.is_empty() {
            return Some(ProjectCheck::Empty);
        }
        let local = root.join("node_modules").join(".bin").join("tsc");
        let (program, prefix): (std::ffi::OsString, &[&str]) = if local.is_file() {
            (local.into_os_string(), &[])
        } else {
            (std::ffi::OsString::from("npx"), &["--no-install", "tsc"])
        };
        Some(ProjectCheck::Run(
            runs.into_iter()
                .map(|args| {
                    let mut all: Vec<String> = prefix.iter().map(|a| a.to_string()).collect();
                    all.extend(args);
                    (program.clone(), all)
                })
                .collect(),
        ))
    }

    /// The `tsc` runs that check the tsconfig at `rel` (`None` = the root
    /// `tsconfig.json`), whose contents are `text` (#1291).
    ///
    /// A config that owns files gets its own `--noEmit` run, even when it
    /// also has `references`: tsc does not check references during a plain
    /// project check, and skipping the root would hide its own files'
    /// errors. Each reference is then followed, so a referenced
    /// solution-style config is checked through its leaves rather than run
    /// on its own and compiling nothing. A reference that can't be read
    /// is still run, so tsc reports it. `seen` stops a reference cycle.
    pub(super) fn collect_tsc_runs(
        root: &Path,
        rel: Option<&str>,
        text: &str,
        seen: &mut std::collections::HashSet<PathBuf>,
        runs: &mut Vec<Vec<String>>,
    ) {
        let config: serde_json::Value =
            serde_json::from_str(&crate::jsonc::strip_jsonc(text)).unwrap_or_default();
        let names_no_files = config
            .get("files")
            .and_then(|f| f.as_array())
            .is_some_and(|f| f.is_empty())
            && config.get("include").is_none()
            // An `extends` base may supply the files.
            && config.get("extends").is_none();
        if !names_no_files {
            runs.push(match rel {
                None => vec![String::from("--noEmit")],
                Some(rel) => vec![
                    String::from("-p"),
                    rel.to_string(),
                    String::from("--noEmit"),
                ],
            });
        }
        // References resolve against the directory of the config naming them.
        let base = rel.map(|rel| {
            let p = Path::new(rel);
            if root.join(p).is_dir() {
                p.to_path_buf()
            } else {
                p.parent().map(Path::to_path_buf).unwrap_or_default()
            }
        });
        let references = config
            .get("references")
            .and_then(|r| r.as_array())
            .into_iter()
            .flatten()
            .filter_map(|r| r.get("path")?.as_str());
        for reference in references {
            let child = match &base {
                None => reference.to_string(),
                Some(base) => base.join(reference).to_string_lossy().into_owned(),
            };
            let on_disk = root.join(&child);
            let file = if on_disk.is_dir() {
                on_disk.join("tsconfig.json")
            } else {
                on_disk
            };
            let key = std::fs::canonicalize(&file).unwrap_or(file.clone());
            if !seen.insert(key) {
                continue;
            }
            match std::fs::read_to_string(&file) {
                Ok(text) => Self::collect_tsc_runs(root, Some(&child), &text, seen, runs),
                Err(_) => runs.push(vec![String::from("-p"), child, String::from("--noEmit")]),
            }
        }
    }

    /// How many rows the last project check contributed (#256).
    ///
    /// NOT `problems.total_count()`: that is the panel's projection, which
    /// mixes in every language server's diagnostics and every pane's build
    /// output, and which the Open Files scope filters down - so under that
    /// scope a sweep whose findings are all in unopened files would report
    /// "no problems", defeating the feature. Count this producer's own
    /// contribution instead.
    ///
    /// Recorded at install time rather than summed from the store: a file's
    /// entry in `build_diagnostics` holds EVERY producer's items for that
    /// file, so summing it double-counts a file the sweep and a pane both
    /// report. Filtering on the item's `source` would not help either: a
    /// pane running tsc produces the same source string as this sweep.
    /// (The rows do carry an owner since #532, so an owner-filtered count
    /// would work - but this field is already exact and cheaper.)
    pub(super) fn project_check_count(&self) -> usize {
        self.project_check_rows
    }

    /// Scan one finished command's output through the build matchers and
    /// install the results (#119). The pane's PREVIOUS contribution is
    /// replaced wholesale — a rebuild that fixed everything clears its old
    /// entries because the fresh scan is empty. Relative tool paths resolve
    /// against the command's cwd (falling back to the workspace root);
    /// resolution is lexical, no filesystem probe, so a path the tool
    /// printed oddly still gets a row even if navigation later misses. The
    /// exception is the loose go and lint rows (#1594), kept only for a
    /// file that exists, so other `name:1:2: text` output is not a problem.
    pub(crate) fn apply_build_scan(
        &mut self,
        pane: u64,
        cwd: Option<&Path>,
        cmd: &str,
        output: &str,
    ) -> bool {
        // A watch matcher already published this pane's batch mid-run: the
        // watcher's exit must not overwrite the last cycle with a rescan of
        // its whole history. One skip only — the pane's next command scans.
        if self.watch_published_panes.remove(&pane) {
            return false;
        }
        // A task-declared problemMatcher is exclusive for its pane
        // (VS Code's model); otherwise user matchers claiming the command
        // run alongside the built-in table.
        let diags = match self.task_matcher_by_pane.get(&pane) {
            Some(m) => m.scan_batch(output),
            None => self.matchers.scan_batch(output, cmd),
        };
        let tabstop = crate::build_matchers::tabstop_from_command(cmd);
        self.install_build_diags(pane, cwd, diags, tabstop)
    }

    /// Install `diags` as pane `pane`'s PROBLEMS contribution (#119/#252).
    /// The gcc/clang columns are converted here (`build_column_unit`, with
    /// `tabstop` for a tab), so a watch task's batches get the same
    /// conversion as a finished command's scan, and each row gets it once.
    pub(super) fn install_build_diags(
        &mut self,
        pane: u64,
        cwd: Option<&Path>,
        mut diags: Vec<crate::build_matchers::BuildDiag>,
        tabstop: usize,
    ) -> bool {
        crate::build_matchers::convert_columns(
            &mut diags,
            cwd.unwrap_or(self.workspace_root()),
            self.build_column_unit,
            tabstop,
        );
        let had_old = self
            .build_diag_files_by_pane
            .get(&pane)
            .is_some_and(|v| !v.is_empty());
        if diags.is_empty() && !had_old {
            return false;
        }
        if let Some(files) = self.build_diag_files_by_pane.remove(&pane) {
            for f in files {
                // Filter, never remove: another producer may hold rows on
                // this same file, and dropping the entry took them too
                // (#532). The entry goes only once nothing is left in it.
                if let Some(rows) = self.build_diagnostics.get_mut(&f) {
                    rows.retain(|(owner, _)| *owner != pane);
                    if rows.is_empty() {
                        self.build_diagnostics.remove(&f);
                    }
                }
            }
        }
        let base = cwd.unwrap_or(self.workspace_root()).to_path_buf();
        let mut touched = Vec::new();
        for d in diags {
            let path = {
                let p = Path::new(&d.file);
                if p.is_absolute() {
                    p.to_path_buf()
                } else {
                    base.join(p)
                }
            };
            if d.needs_file && !path.is_file() {
                continue;
            }
            let entry = self.build_diagnostics.entry(path.clone()).or_default();
            // Unconditionally: recording only when the entry was empty meant
            // the SECOND producer to reach a file never listed it, so its
            // next run had nothing to clear (#532). `touched` is this
            // producer's own file list, not a set of first-touchers.
            if !touched.contains(&path) {
                touched.push(path);
            }
            entry.push((
                pane,
                crate::widgets::problems::ProblemItem {
                    line: d.line,
                    col: d.col,
                    // Build tools report characters (rustc counts code
                    // points), not the UTF-16 units an LSP position carries.
                    col_utf16: false,
                    severity: d.severity,
                    message: d.message,
                    source: d.source.to_string(),
                    code: None,
                    related: Vec::new(),
                },
            ));
        }
        self.build_diag_files_by_pane.insert(pane, touched);
        self.rebuild_problems();
        true
    }

    /// Re-read matchers.json (user + workspace) after a save, surfacing
    /// compile warnings once in OUTPUT · Matchers (#252).
    pub(super) fn reload_matchers(&mut self) {
        // Same isolation guard as App::new: the developer's real
        // matchers.json must never steer app tests — a test that saves a
        // path equal to the real matchers_path() would otherwise reload
        // the runner's actual config here.
        let set = if cfg!(test) {
            crate::problem_matchers::MatcherSet::default()
        } else {
            crate::problem_matchers::MatcherSet::load(
                &crate::problem_matchers::matchers_path(),
                Some(&crate::problem_matchers::workspace_matchers_path(
                    self.workspace_root(),
                )),
            )
        };
        for w in &set.warnings {
            crate::output::push("Matchers", crate::output::OutputLevel::Warn, w);
        }
        let warned = set.warnings.len();
        self.watch_set = std::sync::Arc::new(set.watch_set());
        self.matchers = std::sync::Arc::new(set);
        self.status = if warned == 0 {
            format!(
                "Problem matchers reloaded ({} active)",
                self.matchers.matchers.len()
            )
        } else {
            format!(
                "Problem matchers reloaded ({} active, {warned} warning{} — see OUTPUT · Matchers)",
                self.matchers.matchers.len(),
                if warned == 1 { "" } else { "s" }
            )
        };
    }

    pub(super) fn clear_build_diagnostics(&mut self) {
        let n: usize = self.build_diagnostics.values().map(Vec::len).sum();
        self.build_diagnostics.clear();
        self.build_diag_files_by_pane.clear();
        self.rebuild_problems();
        self.status = format!(
            "Cleared {n} build diagnostic{}",
            if n == 1 { "" } else { "s" }
        );
    }

    /// Rebuild the PROBLEMS panel's grouped projection from the per-file
    /// diagnostics store. Returns whether the projection changed (so the caller
    /// can fold it into its redraw decision). Files are grouped, ordered by
    /// path; each group's diagnostics are ordered by position.
    pub(super) fn rebuild_problems(&mut self) -> bool {
        use crate::widgets::problems::{ProblemGroup, ProblemItem, ProblemRelated};
        let mut paths: Vec<&PathBuf> = self
            .lsp_diagnostics
            .keys()
            .chain(self.build_diagnostics.keys())
            .collect();
        paths.sort();
        paths.dedup();
        // Scope (#256): Open Files keeps only paths with a live buffer, so the
        // panel and the status-bar badge (which counts the same list) agree.
        let scoped = self.problems.scope == crate::widgets::problems::ProblemScope::OpenFiles;
        let open = if scoped {
            self.open_buffer_paths()
        } else {
            std::collections::BTreeSet::new()
        };
        let scoped_out = |p: &PathBuf| scoped && !open.contains(p);
        let mut groups = Vec::new();
        for path in paths {
            if scoped_out(path) {
                continue;
            }
            let mut items: Vec<ProblemItem> = Vec::new();
            if let Some(by_server) = self.lsp_diagnostics.get(path) {
                // Hints (unused names, unreachable code) stay in the editor
                // and its hover, out of the list and its count, as in VS
                // Code (#1522).
                let shown = diagnostics_by_source(by_server)
                    .filter(|(_, d)| d.severity != crate::lsp::manager::DiagnosticSeverity::Hint);
                for (source, d) in shown {
                    let related = d.related().into_iter().map(|r| ProblemRelated {
                        rel_path: r
                            .path
                            .strip_prefix(self.workspace_root())
                            .unwrap_or(&r.path)
                            .to_string_lossy()
                            .into_owned(),
                        path: r.path,
                        line: r.line,
                        col: r.character,
                        message: r.message,
                    });
                    items.push(ProblemItem {
                        line: d.start_line,
                        col: d.start_char,
                        col_utf16: true,
                        severity: d.severity,
                        message: d.message.clone(),
                        source: source.to_string(),
                        code: d.code.clone(),
                        related: related.collect(),
                    });
                }
            }
            if let Some(build) = self.build_diagnostics.get(path) {
                // A task matcher can emit `hint` too; same rule (#1522).
                items.extend(
                    build
                        .iter()
                        .map(|(_, item)| item)
                        .filter(|i| i.severity != crate::lsp::manager::DiagnosticSeverity::Hint)
                        .cloned(),
                );
            }
            if items.is_empty() {
                continue;
            }
            items.sort_by_key(|i| (i.line, i.col));
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let rel_dir = path
                .strip_prefix(self.workspace_root())
                .unwrap_or(path)
                .parent()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            groups.push(ProblemGroup {
                path: path.clone(),
                name,
                rel_dir,
                items,
            });
        }
        self.problems.set_groups(groups)
    }

    /// Drain server-pushed diagnostics into the per-file store, then refresh
    /// the underline overlay on whichever editor group shows an affected (or
    /// just-switched-to) file. Returns true when an overlay changed and the
    /// next frame should redraw.
    pub fn drain_lsp_diagnostics(&mut self) -> bool {
        // Collect first so the immutable borrow of `lsp` ends before the store
        // and the editors are mutated below.
        let mut updates = Vec::new();
        if let Some(lsp) = self.lsp.as_ref() {
            while let Some(u) = lsp.drain_diagnostics() {
                updates.push(u);
            }
        }
        self.apply_diagnostics_updates(updates)
    }

    /// Store a batch of servers' diagnostics and repaint what they touch:
    /// the body of [`Self::drain_lsp_diagnostics`], apart from the drain.
    pub(super) fn apply_diagnostics_updates(
        &mut self,
        updates: Vec<crate::lsp::manager::DiagnosticsUpdate>,
    ) -> bool {
        let mut touched: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
        let mut changed = false;
        for u in updates {
            // While a proposal is being checked (#347), what the servers say
            // about its file is about the proposal, not the buffer.
            if let Some(check) = self.approval_check.as_mut()
                && check.path == u.path
            {
                check
                    .by_server
                    .insert(diagnostics_store_key(&u), u.diagnostics);
                changed = true;
                continue;
            }
            touched.insert(u.path.clone());
            store_diagnostics_update(&mut self.lsp_diagnostics, u);
        }
        // The active editor re-decodes when its file was touched this tick OR
        // when it now shows a file whose stored diagnostics it hasn't applied
        // yet (a tab switch: diagnostics are pushed, never re-requested).
        if let Some(path) = self.editor.path.clone() {
            let stale = self.editor.diagnostics_path() != Some(path.as_path());
            if stale || touched.contains(&path) {
                let merged = self.merged_diagnostics(&path);
                self.editor.apply_diagnostics(path, merged);
                changed = true;
            }
        }
        // Same for every inactive group, computed without overlapping borrows.
        let mut split_targets: Vec<PathBuf> = self
            .editor_layout
            .inactive_groups_mut()
            .into_iter()
            .filter_map(|s| {
                let p = s.path.clone()?;
                let stale = s.diagnostics_path() != Some(p.as_path());
                (stale || touched.contains(&p)).then_some(p)
            })
            .collect();
        split_targets.dedup();
        for path in split_targets {
            let merged = self.merged_diagnostics(&path);
            for group in self.editor_layout.inactive_groups_mut() {
                if group.path.as_deref() == Some(path.as_path()) {
                    group.apply_diagnostics(path.clone(), merged.clone());
                    changed = true;
                }
            }
        }
        // The PROBLEMS panel aggregates every file's diagnostics, including
        // files no editor group has open, so rebuild it whenever any server
        // republished this tick (not only when an editor overlay changed).
        if !touched.is_empty() {
            changed |= self.rebuild_problems();
        }
        changed
    }

    pub(super) fn toggle_terminal(&mut self) {
        self.show_terminal = !self.show_terminal;
        // Hiding the terminal must also clear the maximize flag, otherwise
        // the next time the user reopens the terminal it would still hide
        // the editor — which is surprising because Ctrl+J is the user's
        // mental "show my normal layout again" key.
        if !self.show_terminal {
            self.terminal_maximized = false;
        }
        // If we just hid the terminal while it was focused, fall back to editor.
        if !self.show_terminal && self.focus == Pane::Terminal {
            self.focus_pane(Pane::Editor);
        }
        // If we just showed the terminal, optionally jump focus to it for quick use.
        if self.show_terminal {
            self.focus_pane(Pane::Terminal);
        }
    }

    /// `Ctrl+Shift+J`: flip the maximize-terminal flag. When turning ON,
    /// also ensure the terminal pane is visible and focused so the user
    /// can start typing immediately. When turning OFF, re-arm the welcome
    /// wordmark so it re-paints in the restored editor area.
    pub(super) fn toggle_terminal_maximize(&mut self) {
        self.terminal_maximized = !self.terminal_maximized;
        if self.terminal_maximized {
            if !self.show_terminal {
                self.show_terminal = true;
            }
            self.focus_pane(Pane::Terminal);
            self.overlays.welcome.request_clear_if_displayed();
        } else {
            self.overlays.welcome.mark_dirty();
        }
    }

    /// Paint the bottom panel group's tab strip (PROBLEMS / TERMINAL) across
    /// `strip` (its top row) and record each tab's hit rect. The active tab is
    /// bright/bold; the inactive one is dim, mirroring VS Code's panel tabs. A
    /// non-zero problem count rides the PROBLEMS label as a badge.
    pub(super) fn paint_panel_tabs(&mut self, frame: &mut ratatui::Frame, strip: Rect) {
        use ratatui::widgets::Block;
        // Every target on this row is cleared before anything can return: a
        // row that is not painted must not keep the last frame's, whose cells
        // now belong to whatever is drawn there (the status bar, in a one-row
        // window).
        self.problems_tab_rect = Rect::default();
        self.output_tab_rect = Rect::default();
        self.ports_tab_rect = Rect::default();
        self.captures_tab_rect = Rect::default();
        self.terminal_tab_rect = Rect::default();
        self.problems_badge_cell = None;
        self.panel_maximize_button = Rect::default();
        self.terminal_focus_button = Rect::default();
        // The labels go through `set_stringn`, which indexes the buffer
        // directly, so a strip with no row inside the frame paints nothing.
        let strip = strip.intersection(frame.area());
        if strip.is_empty() {
            return;
        }
        // Croft Loch's title row is a chrome band over the canvas the views
        // paint on, with muted inactive titles.
        let surfaces = self.theme.surfaces();
        let tones = crate::widgets::surface::tones(self.theme);
        let strip_bg = surfaces.map_or(self.theme.editor_bg(), |s| s.chrome());
        frame.render_widget(Block::default().style(Style::default().bg(strip_bg)), strip);
        let brand = self.theme.gradient();
        // Croft Loch's three steps: the active title in the strong ink (its
        // accent underline says the rest, so no bold), a hovered one lifted
        // to body text, the others muted.
        let active_fg = match tones {
            Some(t) => t.strong(),
            None if brand => crate::gradient::rgb_color(crate::gradient::PANEL_TITLE_FG),
            None => self.theme.ui(Color::White),
        };
        let active_mods = if tones.is_some() {
            Modifier::empty()
        } else {
            Modifier::BOLD
        };
        let hover_fg = tones.map_or(active_fg, |t| t.text());
        let inactive_fg =
            surfaces.map_or(self.theme.ui(Color::Rgb(0x80, 0x88, 0x98)), |s| s.muted());
        let right = strip.x + strip.width;
        let count = self.problems.total_count();
        let pointer = self.pointer_cell;
        let orange = crate::gradient::rgb_color(crate::gradient::GRAD_TR);
        let badge_fg = self.theme.ui(Color::Rgb(0x1a, 0x12, 0x0a));
        let active = self.bottom_panel_tab;

        // In images mode the count rides as an orange circle (the same raster
        // badge as the activity bar), emitted post-frame; recompute its anchor
        // each paint. Glyph-fallback terminals keep the powerline-cap pill.
        // Croft Loch draws neither: its count is a tinted chip in either mode,
        // red while there are errors, amber for warnings, neutral otherwise.
        let images = self.overlays.activity.has_images() && tones.is_none();
        let badge_tint = tones.map(|t| {
            let hue = if self.problems.error_count() > 0 {
                (0xe5, 0x53, 0x4b)
            } else if self.problems.warning_count() > 0 {
                (0xe2, 0xa0, 0x3a)
            } else {
                t.muted
            };
            chip_tint(t, hue, false)
        });

        // VS Code panel-group order: PROBLEMS, OUTPUT, TERMINAL, PORTS. PROBLEMS
        // alone carries the orange count badge after its label.
        // Each label is translated and padded to its click target.
        // Croft Loch drops the first title's leading pad, so its word lands
        // on the terminal's first text column below it. The tab still starts
        // a column in: the panel's edge column belongs to the splitter.
        let flush_first = tones.is_some();
        let tabs = BottomPanelTab::ALL.map(|tab| {
            let pad = if flush_first && tab == BottomPanelTab::ALL[0] {
                ""
            } else {
                " "
            };
            (tab, format!("{pad}{} ", crate::i18n::tr(tab.label())))
        });

        // Lay each tab out left to right, recording its hit rect (which, for
        // PROBLEMS, spans the label and the pill together).
        let mut placed: Vec<(BottomPanelTab, String, Rect, u16)> = Vec::with_capacity(tabs.len());
        let mut x = strip.x + 1;
        for (tab, label) in tabs {
            if x >= right {
                break;
            }
            let badge_w = if tab == BottomPanelTab::Problems && count > 0 {
                if images {
                    crate::iterm2_inline::count_badge_cells_w(count)
                } else {
                    format!("{count}").chars().count() as u16 + 2
                }
            } else {
                0
            };
            let w = unicode_width::UnicodeWidthStr::width(label.as_str()) as u16 + badge_w;
            let rect = Rect {
                x,
                y: strip.y,
                width: w.min(right.saturating_sub(x)),
                height: 1,
            };
            match tab {
                BottomPanelTab::Problems => self.problems_tab_rect = rect,
                BottomPanelTab::Output => self.output_tab_rect = rect,
                BottomPanelTab::Ports => self.ports_tab_rect = rect,
                BottomPanelTab::Captures => self.captures_tab_rect = rect,
                BottomPanelTab::Terminal => self.terminal_tab_rect = rect,
            }
            placed.push((tab, label, rect, badge_w));
            x = x.saturating_add(w + 1);
        }

        let buf = frame.buffer_mut();
        for (tab, label, rect, badge_w) in &placed {
            let is_active = *tab == active;
            // Active tab: brand fg + bold (and underlined below). Hover brightens
            // an inactive tab; the rest stay dim.
            let style = if is_active {
                Style::default()
                    .fg(active_fg)
                    .bg(strip_bg)
                    .add_modifier(active_mods)
            } else if crate::widgets::hover::row_hover_bg(*rect, pointer, self.theme).is_some() {
                Style::default().fg(hover_fg).bg(strip_bg)
            } else {
                Style::default().fg(inactive_fg).bg(strip_bg)
            };
            if rect.x < right {
                buf.set_stringn(
                    rect.x,
                    rect.y,
                    label,
                    right.saturating_sub(rect.x) as usize,
                    style,
                );
            }
            // Orange count badge after PROBLEMS. In images mode it's a raster
            // circle (matching the activity-bar badges) emitted post-frame, so
            // the cells stay blank strip_bg and we just record the anchor.
            // Otherwise the powerline-cap pill is drawn as text.
            if *badge_w > 0 {
                let bx = rect.x + unicode_width::UnicodeWidthStr::width(label.as_str()) as u16;
                if let Some((fill, ink)) = badge_tint {
                    if bx < right {
                        buf.set_stringn(
                            bx,
                            rect.y,
                            format!(" {count} "),
                            right.saturating_sub(bx) as usize,
                            Style::default().fg(ink).bg(fill),
                        );
                    }
                } else if images {
                    if bx < right {
                        self.problems_badge_cell = Some((bx, rect.y));
                    }
                } else {
                    let badge_text = format!("{count}");
                    let cap_style = Style::default().fg(orange).bg(strip_bg);
                    if bx < right {
                        buf.set_string(bx, rect.y, "\u{e0b6}", cap_style);
                    }
                    let inner_start = bx + 1;
                    if inner_start < right {
                        buf.set_stringn(
                            inner_start,
                            rect.y,
                            &badge_text,
                            right.saturating_sub(inner_start) as usize,
                            Style::default()
                                .fg(badge_fg)
                                .bg(orange)
                                .add_modifier(Modifier::BOLD),
                        );
                    }
                    let cap_r = inner_start + badge_text.chars().count() as u16;
                    if cap_r < right {
                        buf.set_string(cap_r, rect.y, "\u{e0b4}", cap_style);
                    }
                }
            }
            // The active-tab underline (VS Code's indicator) hugs the word only,
            // stopping before the padding spaces and the pill.
            if is_active {
                let leading = label.chars().take_while(|c| *c == ' ').count() as u16;
                let word_len = unicode_width::UnicodeWidthStr::width(label.trim()) as u16;
                let start = rect.x + leading;
                for ux in start..(start + word_len).min(right) {
                    let cell = &mut buf[(ux, rect.y)];
                    cell.modifier.insert(Modifier::UNDERLINED);
                    // VS Code's `panelTitle.activeBorder`: the accent.
                    if surfaces.is_some() {
                        cell.underline_color = self.theme.accent();
                    }
                }
            }
        }
        self.paint_panel_actions(buf, strip, placed.last().map_or(strip.x, |p| p.2.right()));
    }

    /// Croft Loch's top edge for a view sharing the panel (PROBLEMS, OUTPUT,
    /// PORTS, CAPTURES): the hairline under the tab strip, or the accent lip
    /// while the panel has the keyboard, as each terminal pane draws its own.
    /// A no-op on a boxed theme.
    pub(super) fn paint_panel_view_top(&self, frame: &mut ratatui::Frame, content: Rect) {
        crate::widgets::surface::paint_pane_top(
            frame.buffer_mut(),
            content,
            self.theme,
            self.focus == Pane::Terminal,
        );
    }

    /// The panel's own controls at the right end of its tab row (#1980), right
    /// to left: grow / restore the panel (`Ctrl+Shift+J`), then terminal focus
    /// mode (`Cmd+K M`). Both act on every pane at once, so they sit above the
    /// panes rather than in each pane's header. Neither is painted over a tab:
    /// on a row too narrow for both, focus mode goes first.
    pub(super) fn paint_panel_actions(
        &mut self,
        buf: &mut ratatui::buffer::Buffer,
        strip: Rect,
        tabs_end: u16,
    ) {
        let (theme, pointer) = (self.theme, self.pointer_cell);
        // One blank column at the edge, as the pane header buttons keep.
        let mut x = strip.right().saturating_sub(1);
        let mut place = |label: &str| -> Rect {
            let w = label.chars().count() as u16;
            // And one between the controls and the last tab.
            let Some(bx) = x.checked_sub(w).filter(|bx| *bx > tabs_end) else {
                return Rect::default();
            };
            x = bx;
            let rect = Rect::new(bx, strip.y, w, 1);
            let hovered = pointer.is_some_and(|(px, py)| rect_contains(rect, px, py));
            let style = crate::widgets::header_pill::action_style(theme, hovered);
            buf.set_string(bx, strip.y, label, style);
            rect
        };
        self.panel_maximize_button = place(if self.terminal_maximized {
            PANEL_RESTORE_LABEL
        } else {
            PANEL_MAXIMIZE_LABEL
        });
        // With one terminal there is nothing to focus against.
        if self.bottom_panel_tab == BottomPanelTab::Terminal
            && self.terminals.len() > 1
            && !self.panel_maximize_button.is_empty()
        {
            self.terminal_focus_button = place(if self.terminal_pane_maximized {
                TERMINAL_RESTORE_LABEL
            } else {
                TERMINAL_MAX_LABEL
            });
        }
    }

    /// Switch the bottom panel group's active tab, ensuring the panel is
    /// visible and the panel pane is focused so the new view is usable at once.
    pub(super) fn set_bottom_panel_tab(&mut self, tab: BottomPanelTab) {
        self.bottom_panel_tab = tab;
        if !self.show_terminal {
            self.show_terminal = true;
        }
        self.focus_pane(Pane::Terminal);
    }

    /// The OUTPUT tab on the Messages channel (#1609): every status message
    /// croft has shown, in full, newest last.
    pub(super) fn show_status_messages(&mut self) {
        self.output.sync();
        self.output.select_by_name(crate::output::CHANNEL_MESSAGES);
        self.set_bottom_panel_tab(BottomPanelTab::Output);
    }

    /// Output: Select Channel… (#852): the OUTPUT toolbar's channel dropdown
    /// as a keyboard picker, opened on the channel shown now. Choosing one
    /// shows it in the OUTPUT tab.
    pub(super) fn open_output_channel_picker(&mut self) {
        use crate::widgets::list_picker::{ListPicker, ListPurpose, ListRow};
        // The panel only syncs while painted; a tab never shown yet would
        // otherwise open the picker on the wrong channel.
        self.output.sync();
        let current = self.output.selected_name();
        let rows: Vec<ListRow> = crate::output::channel_names()
            .into_iter()
            .map(|name| ListRow {
                id: name.clone(),
                label: name,
            })
            .collect();
        let selected = rows
            .iter()
            .position(|r| Some(&r.id) == current.as_ref())
            .unwrap_or(0);
        let mut picker = ListPicker::new(ListPurpose::OutputChannel, "Output Channels", rows);
        picker.selected = selected;
        self.open_list_picker(picker, "No output channels yet");
    }

    /// Paint the transient port-detection toast in the bottom-right corner and
    /// record its clickable action-button rects. Click-only: it never captures
    /// keystrokes, so it can't interfere with terminal typing.
    pub(super) fn render_port_toast(&mut self, frame: &mut ratatui::Frame) {
        let Some(toast) = self.port_toast.as_ref() else {
            return;
        };
        let port = toast.port;
        let origin_remote = toast.origin_remote;
        let proc = toast.process.clone().unwrap_or_default();

        let accent = self.theme.accent();
        let btn_bg = self.theme.button();
        let actions: Vec<(String, PortToastAction)> = if origin_remote {
            vec![
                (
                    " Forward & Open ".to_string(),
                    PortToastAction::ForwardAndOpen,
                ),
                (" Forward ".to_string(), PortToastAction::ForwardOnly),
                (" \u{2715} ".to_string(), PortToastAction::Dismiss),
            ]
        } else {
            vec![
                (" Open ".to_string(), PortToastAction::Open),
                (" \u{2715} ".to_string(), PortToastAction::Dismiss),
            ]
        };
        let buttons_w: u16 = actions
            .iter()
            .map(|(l, _)| l.chars().count() as u16 + 1)
            .sum::<u16>()
            + 1;
        let title_line = format!("\u{25cf} port {port}  {proc}");
        let inner_w = buttons_w.max(title_line.chars().count() as u16);

        let area = frame.area();
        let width = (inner_w + 4).min(area.width.saturating_sub(2));
        let height = PORT_TOAST_HEIGHT;
        let status_h: u16 = if self.status_bar_visible { 1 } else { 0 };
        if area.width < width + 2 || area.height < height + status_h + 1 {
            return;
        }
        let rect = Rect {
            x: area.x + area.width - width - 2,
            y: area.y + area.height - height - status_h - 1,
            width,
            height,
        };
        let tones = crate::widgets::surface::tones(self.theme);
        let block = ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .border_style(Style::default().fg(accent));
        // A `surfaces` theme's toast is a raised card: no title on its edge
        // (the body line says what was found, the lip runs unbroken).
        let block = match tones {
            Some(t) => block.style(Style::default().bg(t.raised())),
            None => block
                .style(Style::default().bg(self.theme.editor_bg()))
                .title(ratatui::text::Span::styled(
                    " PORT DETECTED ",
                    Style::default().fg(accent).add_modifier(Modifier::BOLD),
                )),
        };
        frame.render_widget(ratatui::widgets::Clear, rect);
        frame.render_widget(block, rect);
        let inner = Rect {
            x: rect.x + 2,
            y: rect.y + 1,
            width: rect.width.saturating_sub(4),
            height: rect.height.saturating_sub(2),
        };

        let pointer = self.pointer_cell;
        let buf = frame.buffer_mut();
        if let Some(t) = tones {
            crate::widgets::surface::paint_overlay_frame(buf, rect, self.theme);
            // The live dot in the accent, the port strong, the process muted.
            let spans = vec![
                Span::styled("\u{25cf} ", Style::default().fg(t.accent())),
                Span::styled(format!("port {port}"), Style::default().fg(t.strong())),
                Span::styled(format!("  {proc}"), Style::default().fg(t.muted())),
            ];
            buf.set_line(inner.x, inner.y, &Line::from(spans), inner.width);
        } else {
            buf.set_stringn(
                inner.x,
                inner.y,
                &title_line,
                inner.width as usize,
                Style::default().fg(self.theme.ui(Color::Rgb(0xCC, 0xCC, 0xCC))),
            );
        }
        let mut bx = inner.x;
        let by = inner.y + 1;
        let mut rects: Vec<(Rect, PortToastAction)> = Vec::new();
        for (i, (label, action)) in actions.iter().enumerate() {
            let w = label.chars().count() as u16;
            if bx + w > inner.x + inner.width {
                break;
            }
            let hit = Rect::new(bx, by, w, 1);
            // The first action is the primary one, in the button fill; the
            // others are chips a lift off the card, and the dismiss cross
            // a bare muted mark until the pointer is on it.
            let style = match tones {
                Some(t) => {
                    let lit = crate::widgets::hover::contains(hit, pointer);
                    let lift = |c: Color| match c {
                        Color::Rgb(r, g, b) => {
                            rgb_color(crate::gradient::lerp_rgb((r, g, b), t.lift, 0.12))
                        }
                        other => other,
                    };
                    let (fill, fg) = match (i, *action) {
                        (0, _) => (btn_bg, t.strong()),
                        (_, PortToastAction::Dismiss) if !lit => (t.raised(), t.muted()),
                        _ => (t.keycap_on(t.raised()), t.text()),
                    };
                    let fill = if lit { lift(fill) } else { fill };
                    Style::default().fg(fg).bg(fill)
                }
                None => Style::default()
                    .fg(Color::White)
                    .bg(btn_bg)
                    .add_modifier(Modifier::BOLD),
            };
            buf.set_stringn(bx, by, label, w as usize, style);
            rects.push((
                Rect {
                    x: bx,
                    y: by,
                    width: w,
                    height: 1,
                },
                *action,
            ));
            bx += w + 1;
        }
        if let Some(t) = self.port_toast.as_mut() {
            t.buttons = rects;
        }
    }

    /// Keyboard handling for the PROBLEMS view while it owns the panel pane:
    /// arrow / page keys scroll the list, Escape hands focus back to the editor.
    /// Keys while the PROBLEMS tab owns the panel pane. Plain typing goes
    /// to the free-text filter (VS Code's Problems filter box), so Esc
    /// clears the filter first and only hands the pane back once it is
    /// already empty, mirroring the Remote panel's filter behaviour.
    pub(super) fn handle_problems_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up => self.problems.scroll_up(1),
            KeyCode::Down => self.problems.scroll_down(1),
            KeyCode::PageUp => self.problems.scroll_up(10),
            KeyCode::PageDown => self.problems.scroll_down(10),
            KeyCode::Backspace if !self.problems.text_filter.is_empty() => {
                self.problems.pop_filter_char();
            }
            KeyCode::Char(c)
                if !key.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::SUPER | KeyModifiers::ALT,
                ) =>
            {
                self.problems.push_filter_char(c);
            }
            KeyCode::Esc if !self.problems.clear_text_filter() => {
                self.focus_pane(Pane::Editor);
            }
            _ => {}
        }
    }

    pub(super) fn handle_output_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up => self.output.scroll_up(1),
            KeyCode::Down => self.output.scroll_down(1),
            KeyCode::PageUp => self.output.scroll_up(10),
            KeyCode::PageDown => self.output.scroll_down(10),
            KeyCode::Esc => self.focus_pane(Pane::Editor),
            _ => {}
        }
    }

    /// Keys while the PORTS tab owns the panel pane. `⏎` opens (forwarding first
    /// on a remote port), `f` forwards without opening, `c` copies the address,
    /// `x` stops watching (and tears the tunnel down).
    pub(super) fn handle_ports_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up => self.ports.move_selection(-1),
            KeyCode::Down => self.ports.move_selection(1),
            KeyCode::Esc => self.focus_pane(Pane::Editor),
            KeyCode::Enter => self.ports_open_selected(),
            KeyCode::Char('f') => self.ports_forward_selected(),
            KeyCode::Char('c') => self.ports_copy_selected(),
            KeyCode::Char('x') => self.ports_stop_selected(),
            _ => {}
        }
    }

    /// Origin to stamp on a newly detected port: a remote session's ports are
    /// forwardable (and carry the host label), a local session's are already
    /// reachable.
    pub(super) fn port_origin(&self) -> crate::widgets::ports::PortOrigin {
        use crate::widgets::ports::PortOrigin;
        if self.is_remote {
            PortOrigin::Remote(remote_host_label())
        } else {
            PortOrigin::Local
        }
    }

    /// Drain freshly scraped ports from every terminal and, on a low cadence,
    /// poll the socket table. Output-scrape hits are browser-facing
    /// announcements, so a new one raises the toast; socket-poll hits populate
    /// the PORTS tab silently. Returns whether anything changed (for redraw).
    pub(super) fn drain_ports_and_poll(&mut self) -> bool {
        let mut changed = false;
        if self
            .port_toast
            .as_ref()
            .is_some_and(|t| t.shown_at.elapsed() > PORT_TOAST_TTL)
        {
            self.port_toast = None;
            changed = true;
        }
        let origin = self.port_origin();
        let scraped: Vec<crate::port_detect::PortHit> = self
            .terminals
            .iter()
            .flat_map(|t| t.drain_ports())
            .collect();
        for hit in scraped {
            let port = hit.port;
            let process = hit.process.clone();
            if self
                .ports
                .upsert(port, hit.url, hit.process, origin.clone())
            {
                changed = true;
            }
            // Toast on the scrape confirmation, not on first insert: the silent
            // socket poll may have added the port first (common on a remote
            // session), which must not swallow the toast.
            if self.ports.note_scrape_toast(port) {
                self.show_port_toast(port, process);
                changed = true;
            }
        }
        // Latest background-poll snapshot (a complete listener set): add new
        // ports, then reconcile so ports that stopped listening go not-live and
        // eventually drop. Only the newest queued snapshot matters.
        if let Some(poll) = self.port_poll_rx.try_iter().last() {
            for hit in poll.hits {
                if self
                    .ports
                    .upsert(hit.port, hit.url, hit.process, origin.clone())
                {
                    changed = true;
                }
            }
            // Reconcile only against the unscoped listener set. `None` means
            // neither `lsof` nor `ss` ran, which is no evidence at all — far
            // better to leave every row alone than to retire live ports and
            // cancel their tunnels on a failed probe.
            if let Some(live) = poll.listening {
                let outcome = self.ports.reconcile_live(&live);
                if outcome.changed {
                    changed = true;
                }
                // A dropped forward's tunnel is no longer needed — cancel it.
                for (local, remote) in outcome.torn_down {
                    self.request_remote_unforward(local, remote);
                }
            }
        }
        // On cadence, kick off a fresh poll off-thread — `lsof`/`ps` block for
        // tens of ms and must never run on the event loop. One in flight max.
        if self.last_port_poll.elapsed() >= PORT_POLL_INTERVAL
            && !self
                .port_poll_inflight
                .load(std::sync::atomic::Ordering::Relaxed)
        {
            self.last_port_poll = std::time::Instant::now();
            let pids: Vec<i32> = self
                .terminals
                .iter()
                .filter_map(|t| t.pid().map(|p| p as i32))
                .collect();
            if !pids.is_empty() {
                let tx = self.port_poll_tx.clone();
                let inflight = self.port_poll_inflight.clone();
                inflight.store(true, std::sync::atomic::Ordering::Relaxed);
                std::thread::spawn(move || {
                    // Two questions, two scans. The subtree scan says what to
                    // SURFACE (a system daemon is not the user's dev server);
                    // the unscoped one says what is still UP, which the subtree
                    // cannot answer for a port nobody in it owns.
                    let mut hits: Vec<crate::port_detect::PortHit> = Vec::new();
                    for pid in pids {
                        for hit in crate::port_detect::poll_listeners(pid) {
                            if !hits.iter().any(|h| h.port == hit.port) {
                                hits.push(hit);
                            }
                        }
                    }
                    let listening = crate::port_detect::poll_all_listening();
                    let _ = tx.send(PortPoll { hits, listening });
                    inflight.store(false, std::sync::atomic::Ordering::Relaxed);
                });
            }
        }
        changed
    }

    pub(super) fn show_port_toast(&mut self, port: u16, process: Option<String>) {
        self.port_toast = Some(PortToast {
            port,
            process,
            origin_remote: self.is_remote,
            shown_at: std::time::Instant::now(),
            buttons: Vec::new(),
        });
    }

    /// `⏎` on a port: open it. A local (or already-forwarded) port opens
    /// straight away; a remote port that isn't forwarded yet is forwarded first,
    /// then opened.
    pub(super) fn ports_open_selected(&mut self) {
        let Some(entry) = self.ports.selected() else {
            return;
        };
        match plan_port_open(entry, self.drop_relay_active()) {
            PortOpenAction::Forward(port) => self.request_remote_forward(port, true),
            PortOpenAction::OpenViaRelay(url) => {
                self.request_remote_url_open(url.clone());
                self.status = format!("Opening {url} in your local browser");
            }
            PortOpenAction::OpenDirect(url) => match open_url(&url) {
                Ok(()) => self.status = format!("Opened {url}"),
                Err(e) => self.status = format!("Open failed: {e}"),
            },
            PortOpenAction::Nothing => {}
        }
    }

    pub(super) fn ports_forward_selected(&mut self) {
        use crate::widgets::ports::PortOrigin;
        let Some(entry) = self.ports.selected() else {
            return;
        };
        if !matches!(entry.origin, PortOrigin::Remote(_)) || entry.forwarded {
            self.status = String::from("Port is already reachable locally");
            return;
        }
        let port = entry.port;
        self.request_remote_forward(port, false);
    }

    pub(super) fn ports_copy_selected(&mut self) {
        let Some(entry) = self.ports.selected() else {
            return;
        };
        let addr = entry
            .open_url()
            .unwrap_or_else(|| format!("localhost:{}", entry.port));
        if copy_to_clipboard(&addr) {
            self.status = format!("Copied {addr}");
        } else {
            self.status = String::from("Copy failed");
        }
    }

    pub(super) fn ports_stop_selected(&mut self) {
        use crate::widgets::ports::PortOrigin;
        let Some(entry) = self.ports.selected() else {
            return;
        };
        let port = entry.port;
        let forwarded = matches!(
            (&entry.origin, entry.forwarded, entry.local_port),
            (PortOrigin::Remote(_), true, Some(_))
        );
        // Two different gestures share `x`. Stopping a forward leaves the
        // server running and the row in place, so the port must stay
        // detectable; dismissing the row is the one that suppresses it.
        if forwarded {
            if let Some((local, remote)) = self.ports.stop_forwarding(port) {
                self.request_remote_unforward(local, remote);
            }
            self.status = format!("Stopped forwarding port {port} (the server keeps running)");
        } else {
            self.ports.remove(port);
            self.status = format!("Dismissed port {port} from the list (the server keeps running)");
        }
    }
}
