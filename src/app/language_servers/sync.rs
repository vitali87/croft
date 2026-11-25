//! Keeping the language servers in step with the open files: each tab's document opened,
//! changed and closed on its servers, the notice when shellcheck is missing, re-opening the
//! files a just-installed server covers, filing each server's diagnostics, the
//! workspace-diagnostics refresh, and Language Servers: Restart All and Restart for This
//! File.

use super::*;

impl App {
    /// Per-tick LSP doc sync. Diffs every text-buffer tab's edit_seq
    /// against `lsp_last_seen` and forwards did_open / did_change /
    /// did_close to the LspManager. Building the document text only
    /// happens when a tab's edit_seq has actually moved, so the cost
    /// scales with edits, not with frames. The text keeps the file's final
    /// newline (#911), or every formatter adds one the save doubles.
    pub fn sync_lsp(&mut self) {
        let Some(lsp) = self.lsp.as_ref() else {
            return;
        };
        // A language server that exited was restarted (#694). It knows no
        // documents, so forget which ones were sent: every open tab goes out
        // again as a fresh open below, as it does for a new manager.
        if lsp.take_servers_restarted() {
            self.lsp_last_seen.clear();
            self.lsp_progress.clear();
        }
        let mut current: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
        // `(is_open, path, text, seq, viewport)`. The viewport `(start, end)`
        // lines are captured only for opens, so the first paint can fire a fast
        // `semanticTokens/range` over what is on screen ahead of the
        // whole-document request.
        type DocSync = (bool, PathBuf, String, u64, Option<(u32, u32)>);
        let mut to_send: Vec<DocSync> = Vec::new();
        // Semantic-token batches recovered from the on-disk cache for files
        // being opened this tick. Applied after the tab loop (which borrows the
        // editor immutably) so the correct colours paint on the first frame,
        // before the server finishes its ~1.8s cold analysis. See
        // `crate::lsp::semantic_cache`.
        let mut cache_hits: Vec<(PathBuf, std::sync::Arc<Vec<String>>, Vec<u32>)> = Vec::new();
        // A file and its symbol tabs (#369) share one group, each tab with
        // its own `edit_seq`. One tab speaks for the path: the active tab
        // when it holds that file (replies are applied where the seq
        // matches), else the first. Judging each against the one
        // `lsp_last_seen` entry made them take turns resending the whole
        // text on every tick. Every split group's tabs count, the active
        // group's first: a file held only in another group stays open with
        // its unsaved text, so a rename computed for it fits the buffer it
        // lands in, not the disk (#915).
        let tabs = self
            .editor_layout
            .all_groups(&self.editor)
            .flat_map(|group| {
                let active = group.active_index();
                let active_path = group.path.as_ref().filter(|_| !group.has_non_text_view());
                group
                    .iter_tabs()
                    .enumerate()
                    .filter(move |&(i, tab)| i == active || tab.path.as_ref() != active_path)
                    .map(|(_, tab)| tab)
            });
        for tab in tabs {
            if tab.has_non_text_view() {
                continue;
            }
            let Some(path) = tab.path.as_ref() else {
                continue;
            };
            // `files_associations` count (#1574): a `.tpl` associated with
            // HTML goes to the HTML server though its extension has none.
            if crate::lsp::manager::path_to_language(path).is_none() {
                continue;
            }
            if !current.insert(path.clone()) {
                continue;
            }
            // The server holds a proposal's text for this file while it is
            // checked (#347); the buffer goes back when the check ends.
            if self
                .approval_check
                .as_ref()
                .is_some_and(|c| &c.path == path)
            {
                continue;
            }
            let seq = tab.edit_seq;
            let prev = self.lsp_last_seen.get(path).copied();
            match prev {
                None => {
                    // A small look-ahead past the visible rows keeps colour
                    // stable through the first nudge of the scroll wheel
                    // before the full-document reply lands.
                    let start = tab.scroll as u32;
                    let end = (tab.scroll + tab.page_size() + 16) as u32;
                    let text = tab.document_text();
                    // Paint the last-known semantic colours instantly from the
                    // disk cache, so an import-heavy file does not flash grey
                    // then snap to the server's class/namespace colours seconds
                    // later. A hit is identical to what the server will send, so
                    // the live reply is a no-op repaint.
                    if let Some((legend, data)) = crate::lsp::semantic_cache::load(path, &text) {
                        cache_hits.push((path.clone(), legend, data));
                    }
                    to_send.push((true, path.clone(), text, seq, Some((start, end))));
                }
                Some(p) if p != seq => {
                    to_send.push((false, path.clone(), tab.document_text(), seq, None))
                }
                _ => {}
            }
        }
        let lsp = match self.lsp.as_ref() {
            Some(l) => l,
            None => return,
        };
        // Only when bash-language-server is registered: a disabled
        // `lsp-bash` lints nothing, with or without shellcheck.
        let opened_shell = to_send
            .iter()
            .any(|(is_open, path, ..)| *is_open && is_shell_script(path))
            && lsp.server_registered(crate::lsp::Language::BASH, BASH_SERVER);
        for (is_open, path, text, seq, viewport) in to_send {
            let doc_end = crate::lsp::manager::document_end(&text);
            let viewport =
                viewport.map(|(start, end)| (start, crate::lsp::manager::rows_end(&text, end)));
            if is_open {
                lsp.open_doc(path.clone(), text);
                // Paint the visible lines first: ty answers a viewport range
                // query in tens of ms even on a cold workspace, so on-screen
                // code colours immediately while the full request fills in the
                // rest. The editor refuses to let this range batch overwrite
                // the full one when it arrives.
                if let Some((start, end)) = viewport {
                    lsp.request_semantic_tokens_range(path.clone(), start, end, seq);
                }
            } else {
                lsp.change_doc(path.clone(), text);
            }
            // Refresh the whole document's semantic tokens whenever it is
            // opened or changed. Gated by the same `seq` diff as did_change
            // above, so this fires once per edit-batch, not per keystroke.
            lsp.request_semantic_tokens(path.clone(), seq);
            // Same cadence for inlay hints; the reply carries `seq` so a
            // stale batch (computed against older text) is dropped on drain.
            if self.inlay_hints_enabled_for(path_scope_id(&path)) {
                lsp.request_inlay_hints(path.clone(), doc_end, seq);
            }
            // Document links ride the same cadence (#254); silent when no
            // server advertises a documentLinkProvider, so Ctrl+click
            // keeps meaning Go to Definition there.
            lsp.request_document_links(path.clone(), seq);
            // Fold spans ride the same cadence (#254); the worker drops
            // the request silently when no server advertises the
            // provider, which is what leaves the editor's indentation /
            // region fallback in charge.
            lsp.request_folding_ranges(path.clone(), seq);
            // Document colors ride the same cadence (#254); the worker is
            // silent when no server advertises a colorProvider, which is
            // what keeps the swatches from ever appearing unadvertised.
            lsp.request_document_colors(path.clone(), seq);
            self.lsp_last_seen.insert(path, seq);
        }
        let closed: Vec<PathBuf> = self
            .lsp_last_seen
            .keys()
            .filter(|p| !current.contains(*p))
            .cloned()
            .collect();
        let mut dropped_any = false;
        for p in closed {
            lsp.close_doc(p.clone());
            self.lsp_last_seen.remove(&p);
            // Drop the closed file's diagnostics so the store doesn't grow
            // unbounded across a long session of opening and closing files.
            dropped_any |= self.lsp_diagnostics.remove(&p).is_some();
            // Reopening asks for its lenses again anyway.
            self.code_lens_requested.remove(&p);
        }
        // bash-language-server lints only through shellcheck and says so
        // only in its log (#1511): tell the user once, when a shell script
        // first reaches it, rather than leave PROBLEMS empty and silent.
        if opened_shell && !self.shell_lint_notice_shown {
            self.shell_lint_notice_shown = true;
            if !tool_on_path("shellcheck", self.tool_search_path.as_deref()) {
                self.status = String::from(SHELLCHECK_MISSING);
            }
        }
        // A closed file's problems must leave the PROBLEMS panel too.
        // Under Open Files scope the panel also depends on WHICH buffers are
        // open, so a tab opening or closing changes the answer even when no
        // diagnostic moved — rebuild when that set shifts (#256).
        let open_now: std::collections::BTreeSet<PathBuf> = self.open_buffer_paths();
        let open_changed = open_now != self.problems_open_set;
        if open_changed {
            self.problems_open_set = open_now;
        }
        let scope_depends_on_open_set =
            self.problems.scope == crate::widgets::problems::ProblemScope::OpenFiles;
        if dropped_any || (open_changed && scope_depends_on_open_set) {
            self.rebuild_problems();
            self.refresh_problems_badge();
        }
        // Paint cached semantic colours onto the visible editor(s) now that the
        // immutable tab borrow above has ended. `is_full = true` so a later
        // server `range` reply can't blank the off-screen colours, and the
        // identical `full` reply repaints to the same pixels (no snap).
        for (path, legend, data) in cache_hits {
            for group in self.editor_layout.all_groups_mut(&mut self.editor) {
                if group.path.as_deref() == Some(path.as_path()) {
                    group.apply_semantic_tokens(path.clone(), data.clone(), legend.clone(), true);
                }
            }
        }
    }

    /// Re-pull whole-project diagnostics when a server asked us to via
    /// `workspace/diagnostic/refresh` (#533).
    ///
    /// Mirrors [`refresh_semantic_tokens_if_requested`], with one difference:
    /// there is nothing to aim it at. A pull covers the whole workspace, not
    /// the visible editors, which is the point - it is what puts problems from
    /// files no editor has open into PROBLEMS. The manager filters to the
    /// servers advertising `workspaceDiagnostics`, so a workspace whose
    /// servers all PUSH never sends a request.
    pub fn refresh_workspace_diagnostics_if_requested(&self) {
        let Some(lsp) = self.lsp.as_ref() else {
            return;
        };
        if !lsp.take_diagnostic_refresh() {
            return;
        }
        lsp.request_workspace_diagnostics();
    }
}

impl App {
    /// "Language Servers: Restart All" (#1621): stop every server and start
    /// a fresh manager at the same roots, as a re-root does, without
    /// quitting croft. The next sync re-opens every tab against the new
    /// servers, which publish their diagnostics afresh.
    pub(in crate::app) fn restart_language_servers(&mut self) {
        if self.lsp_paused_for_build {
            self.status = String::from(LSP_PAUSED_FOR_BUILD);
            return;
        }
        // The old manager's Drop waits for its servers; off the UI thread.
        if let Some(old) = self.lsp.take() {
            fs_watch::offload_drop(old);
        }
        // A format-on-save in flight dies with its server; save unformatted,
        // as when a reply fails.
        if self.format_request_id.take().is_some() {
            self.complete_pending_save();
        }
        self.lsp = match crate::lsp::LspManager::new(self.workspace_root().to_path_buf()) {
            Ok(m) => {
                m.set_extra_roots(self.roots.iter().skip(1).map(Path::to_path_buf).collect());
                Some(m)
            }
            Err(e) => {
                let msg = format!("Language servers could not restart: {e}");
                crate::output::push("Language Servers", crate::output::OutputLevel::Error, &msg);
                self.status = msg;
                None
            }
        };
        self.lsp_last_seen.clear();
        self.lsp_progress.clear();
        // The new manager numbers semantic replies from 0 again.
        self.semantic_generation_seen.clear();
        self.lsp_diagnostics.clear();
        self.rebuild_problems();
        if self.lsp.is_some() {
            let msg = "Restarted all language servers";
            crate::output::push("Language Servers", crate::output::OutputLevel::Info, msg);
            self.status = String::from(msg);
        }
    }

    /// "Language Servers: Restart for This File" (#1621): restart only the
    /// servers serving the active file, so rust-analyzer's index survives a
    /// restart of ruff. The manager re-opens the tabs once they are back.
    pub(in crate::app) fn restart_language_servers_for_file(&mut self) {
        if self.lsp_paused_for_build {
            self.status = String::from(LSP_PAUSED_FOR_BUILD);
            return;
        }
        let Some(path) = self.editor.path.clone() else {
            self.status = String::from("Restart for This File needs a file open in the editor");
            return;
        };
        let Some(lsp) = self.lsp.as_ref() else {
            self.status =
                String::from("No language servers are running; Restart All starts them again");
            return;
        };
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let has_language = crate::lsp::Language::from_path(&path).is_some();
        if !has_language {
            self.status = format!("No language server serves {name}");
            return;
        }
        lsp.restart_servers_for(path.clone());
        self.status = format!("Restarting the language servers for {name}");
    }
}

/// Whether `path` is a shell script, the files bash-language-server serves.
pub(in crate::app) fn is_shell_script(path: &Path) -> bool {
    crate::lsp::Language::from_path(path) == Some(crate::lsp::Language::BASH)
}

/// Whether an executable file named `tool` is in one of `path_var`'s
/// directories, where a server croft starts would look for it.
pub(in crate::app) fn tool_on_path(tool: &str, path_var: Option<&std::ffi::OsStr>) -> bool {
    path_var.is_some_and(|paths| {
        std::env::split_paths(paths).any(|dir| {
            let candidate = dir.join(tool);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                candidate
                    .metadata()
                    .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            }
            #[cfg(not(unix))]
            {
                candidate.is_file() || candidate.with_extension("exe").is_file()
            }
        })
    })
}

/// Collapse a language to the server "family" it shares a managed server with.
/// vtsls serves the whole TypeScript/JS family but reports its config language
/// as `TypeScript`, so all four map together; every other language is its own
/// family. Used to decide which open docs to re-open after a managed install.
pub(in crate::app) fn lsp_server_family(lang: crate::lsp::Language) -> crate::lsp::Language {
    lang.server_family()
}

/// True when `path`'s language is served by one of the just-installed servers,
/// so its LSP sync state should be dropped to force a re-open + re-probe.
pub(in crate::app) fn just_installed_covers_path(
    installed: &[crate::lsp::Language],
    path: &Path,
) -> bool {
    let Some(lang) = crate::lsp::Language::from_path(path) else {
        return false;
    };
    installed
        .iter()
        .any(|&i| lsp_server_family(i) == lsp_server_family(lang))
}

/// File one server's diagnostics batch in the store. An empty batch is the
/// server saying "all clear" for its findings; a path no server reports on
/// any more leaves the store. Files never opened (a workspace pull, a
/// flycheck) and deleted ones used to keep an empty entry each for the rest
/// of the session (#694).
pub(in crate::app) fn store_diagnostics_update(
    store: &mut std::collections::HashMap<
        PathBuf,
        std::collections::HashMap<String, Vec<crate::lsp::manager::Diagnostic>>,
    >,
    u: crate::lsp::manager::DiagnosticsUpdate,
) {
    let key = diagnostics_store_key(&u);
    if u.diagnostics.is_empty() {
        if let Some(by_server) = store.get_mut(&u.path) {
            by_server.remove(&key);
            if by_server.is_empty() {
                store.remove(&u.path);
            }
        }
    } else {
        store.entry(u.path).or_default().insert(key, u.diagnostics);
    }
}
