//! Language servers: sync, messages and progress, decorations, go-to and peek,
//! hierarchies, rename, code actions, formatting, linked editing, smart select,
//! highlights, symbol pickers, code lens, outline, breadcrumbs, sticky scroll, restart.

use super::*;

/// What to do with one on-type formatting reply (#254). `NotOurs` leaves the
/// armed request alone — a superseded reply must not disarm the newer one
/// still in flight — while `Stale` and `Apply` both consume it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum OnTypeReply {
    NotOurs,
    Stale,
    Apply,
}

/// What a breadcrumb crumb stands for (#1608).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum CrumbTarget {
    Folder(PathBuf),
    File(PathBuf),
    /// An index into the outline's symbols.
    Symbol(usize),
}

/// The file the OUTLINE shows and the edit seq it was synced at.
pub(super) type OutlineKey = (PathBuf, Option<u64>);

/// Where an off-thread outline parse delivers its symbols (#1150).
pub(super) type OutlineParse = std::sync::mpsc::Receiver<Vec<crate::lsp::manager::OutlineSymbol>>;

/// A place Peek References can show: path, 0-based line, UTF-16 column.
pub(super) type PeekLocation = (PathBuf, u32, u32);

/// How long a buffer must sit unedited before its code lenses are asked
/// for (#608).
pub(super) const CODE_LENS_SETTLE: std::time::Duration = std::time::Duration::from_millis(600);

/// Maximum pinned sticky-scroll header rows (VS Code's default is 5).
pub(super) const STICKY_SCROLL_MAX: usize = 5;

/// A server's message as one status line, `server: first line`, so a
/// multi-line cargo error does not spill over the bar.
pub(super) fn server_message_status(server: &str, text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    format!("{server}: {line}")
}

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
        let tabs = std::iter::once(&self.editor)
            .chain(self.editor_layout.inactive_groups())
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
            if self.editor.path.as_deref() == Some(path.as_path()) {
                self.editor
                    .apply_semantic_tokens(path.clone(), data.clone(), legend.clone(), true);
            }
            for group in self.editor_layout.inactive_groups_mut() {
                if group.path.as_deref() == Some(path.as_path()) {
                    group.apply_semantic_tokens(path.clone(), data.clone(), legend.clone(), true);
                }
            }
        }
    }

    /// Whether rust-analyzer is open on the active tab's file (#1626), so
    /// its `Rust:` commands have a server to go to. Asked of the servers
    /// open on the file, not those reporting diagnostics on it: one slow
    /// diagnostic pull retires rust-analyzer from reporting while its
    /// client keeps answering.
    pub(super) fn rust_analyzer_serves_active_file(&self) -> bool {
        match (self.editor.path.as_deref(), self.lsp.as_ref()) {
            (Some(path), Some(lsp)) => {
                lsp.document_served_by(path, crate::lsp::rust_analyzer::SERVER)
            }
            _ => false,
        }
    }

    /// Send a `Rust:` command's request (#1626) for the caret in the
    /// active file; [`App::drain_lsp_rust_analyzer`] acts on the answer.
    pub(super) fn request_rust_analyzer(&mut self, request: crate::lsp::rust_analyzer::RaRequest) {
        use crate::lsp::rust_analyzer::RaRequest;
        if !self.rust_analyzer_serves_active_file() {
            self.status = String::from("rust-analyzer is not running for this file");
            return;
        }
        let Some(path) = self.editor.path.clone() else {
            return;
        };
        let (line, character) = self
            .editor
            .pos_to_utf16(self.editor.cursor_row, self.editor.cursor_col);
        let Some(lsp) = self.lsp.as_mut() else {
            return;
        };
        let id = lsp.request_rust_analyzer(path, request, line, character);
        self.rust_analyzer_request = Some((id, request));
        self.status = String::from(match request {
            RaRequest::ExpandMacro => "Expanding the macro at the caret\u{2026}",
            RaRequest::ParentModule => "Locating the parent module\u{2026}",
            RaRequest::OpenCargoToml => "Finding Cargo.toml\u{2026}",
            RaRequest::RebuildProcMacros => "Asking rust-analyzer to rebuild proc macros\u{2026}",
            RaRequest::ReloadWorkspace => "Asking rust-analyzer to reload the workspace\u{2026}",
        });
    }

    pub fn drain_lsp_rust_analyzer(&mut self) -> bool {
        let mut results = Vec::new();
        if let Some(lsp) = self.lsp.as_ref() {
            while let Some(r) = lsp.drain_rust_analyzer() {
                results.push(r);
            }
        }
        let mut changed = false;
        for result in results {
            changed |= self.apply_rust_analyzer_result(result);
        }
        changed
    }

    /// Act on a rust-analyzer answer (#1626) when it is the one awaited:
    /// open an expansion as a Rust tab, jump to a parent module or manifest
    /// (a picker when there are several), or report a workspace request.
    pub(super) fn apply_rust_analyzer_result(
        &mut self,
        result: crate::lsp::manager::RustAnalyzerResult,
    ) -> bool {
        use crate::lsp::rust_analyzer::{self as ra, RaRequest};
        if self.rust_analyzer_request != Some((result.request_id, result.request)) {
            return false;
        }
        self.rust_analyzer_request = None;
        let value = match result.outcome {
            Ok(v) => v,
            Err(e) => {
                self.status = format!("rust-analyzer: {e}");
                return true;
            }
        };
        match result.request {
            RaRequest::ExpandMacro => match ra::expansion(&value) {
                Some((name, text)) => {
                    let label = self.scratch_buffer_path(&format!("Expansion of {name}!"));
                    match self.editor.open_text_buffer(&label, &text) {
                        Ok(()) => {
                            self.editor
                                .set_language(Some(crate::highlight::LangKind::Rust));
                            self.focus_pane(Pane::Editor);
                            self.status = format!("Expanded {name}!");
                        }
                        Err(e) => self.status = format!("Could not open the expansion: {e}"),
                    }
                }
                None => self.status = String::from("No macro call at the caret"),
            },
            RaRequest::ParentModule | RaRequest::OpenCargoToml => {
                let targets = ra::locations(&value);
                let what = if result.request == RaRequest::ParentModule {
                    "parent modules"
                } else {
                    "Cargo.toml files"
                };
                match targets.len() {
                    0 => self.status = format!("No {what} found"),
                    1 => {
                        let (path, line, col) = targets.into_iter().next().expect("len == 1");
                        self.go_to_definition(path, line, col);
                    }
                    _ => self.open_location_picker(targets, what),
                }
            }
            RaRequest::RebuildProcMacros => {
                self.status =
                    String::from("rust-analyzer is rebuilding proc macros and build scripts")
            }
            RaRequest::ReloadWorkspace => {
                self.status = String::from("rust-analyzer is reloading the workspace")
            }
        }
        true
    }

    /// Drain semantic-token batches and hand each to whichever editor group
    /// currently shows that file. Returns true when an overlay changed and
    /// the next frame should redraw.
    pub fn drain_lsp_semantic_tokens(&mut self) -> bool {
        let Some(lsp) = self.lsp.as_ref() else {
            return false;
        };
        // Collect first so the immutable borrow of `lsp` ends before the
        // editors are mutated below.
        let mut updates = Vec::new();
        while let Some(u) = lsp.drain_semantic_tokens() {
            updates.push(u);
        }
        let mut changed = false;
        for u in updates {
            // Drop a batch measured against text the buffer has moved past —
            // an external reload swaps the whole document while a request is
            // still in its retry backoff. Decoding it over the new lines
            // paints the old file's colours at the wrong offsets, and the
            // `is_full` cache store below would persist that under the NEW
            // content's key, replaying it on every future open.
            if !semantic_reply_is_current(
                self.semantic_generation_seen.get(&u.path).copied(),
                u.generation,
            ) {
                continue;
            }
            let mut applied_anywhere = false;
            if self.editor.path.as_deref() == Some(u.path.as_path())
                && self.editor.edit_seq == u.seq
            {
                self.editor.apply_semantic_tokens(
                    u.path.clone(),
                    u.data.clone(),
                    u.legend.clone(),
                    u.is_full,
                );
                changed = true;
                applied_anywhere = true;
            }
            for group in self.editor_layout.inactive_groups_mut() {
                if group.path.as_deref() == Some(u.path.as_path()) && group.edit_seq == u.seq {
                    group.apply_semantic_tokens(
                        u.path.clone(),
                        u.data.clone(),
                        u.legend.clone(),
                        u.is_full,
                    );
                    changed = true;
                    applied_anywhere = true;
                }
            }
            // Persist whole-document batches so the next open of this exact
            // content paints these colours instantly instead of waiting out the
            // server's cold analysis. Only when a visible editor holds the file
            // unchanged, so the cache key matches the bytes the next open reads.
            if applied_anywhere {
                self.semantic_generation_seen
                    .insert(u.path.clone(), u.generation);
            }
            if u.is_full && applied_anywhere {
                let mut clean_text = (self.editor.path.as_deref() == Some(u.path.as_path()))
                    .then(|| self.editor.clean_cache_text())
                    .flatten();
                if clean_text.is_none() {
                    clean_text = self
                        .editor_layout
                        .inactive_groups_mut()
                        .into_iter()
                        .find(|g| g.path.as_deref() == Some(u.path.as_path()))
                        .and_then(|g| g.clean_cache_text());
                }
                if let Some(text) = clean_text {
                    crate::lsp::semantic_cache::store(&u.path, &text, &u.legend, &u.data);
                }
            }
        }
        changed
    }

    /// Route drained inlay-hint batches to whichever visible editor holds the
    /// file. A reply whose `seq` no longer matches the buffer's last-synced
    /// edit seq is dropped: its anchors were computed against older text, and
    /// the request fired for the current seq is still in flight.
    pub fn drain_lsp_inlay_hints(&mut self) -> bool {
        let Some(lsp) = self.lsp.as_ref() else {
            return false;
        };
        let mut updates = Vec::new();
        while let Some(u) = lsp.drain_inlay_hints() {
            updates.push(u);
        }
        let mut changed = false;
        for u in updates {
            changed |= self.fan_out_batch(&u.path, u.seq, |e| {
                e.apply_inlay_hints(u.path.clone(), u.hints.clone())
            });
        }
        changed
    }

    /// Hand a seq-stamped batch for `path` to every editor showing the file,
    /// the active one and each inactive group, through `apply`. A batch whose
    /// `seq` no longer matches the buffer's last-synced edit seq is dropped:
    /// its anchors were computed against older text, and the request fired
    /// for the current seq is still in flight. Returns whether any editor
    /// took it.
    pub(super) fn fan_out_batch(
        &mut self,
        path: &Path,
        seq: u64,
        mut apply: impl FnMut(&mut EditorTabs),
    ) -> bool {
        if self.lsp_last_seen.get(path) != Some(&seq) {
            return false;
        }
        let mut applied = false;
        if self.editor.path.as_deref() == Some(path) {
            apply(&mut self.editor);
            applied = true;
        }
        for group in self.editor_layout.inactive_groups_mut() {
            if group.path.as_deref() == Some(path) {
                apply(group);
                applied = true;
            }
        }
        applied
    }

    /// Drain document-link batches (#254) into every editor showing the
    /// file — the inlay fan-out, seq gate included. Links change no
    /// pixels, so this never asks for a redraw.
    pub fn drain_lsp_document_links(&mut self) {
        let Some(lsp) = self.lsp.as_ref() else {
            return;
        };
        let mut updates = Vec::new();
        while let Some(u) = lsp.drain_document_links() {
            updates.push(u);
        }
        for u in updates {
            self.fan_out_batch(&u.path, u.seq, |e| {
                e.apply_document_links(u.path.clone(), u.links.clone())
            });
        }
    }

    /// Follow a server-resolved document link (#254): `file://` targets
    /// open in the editor, web links go to the system opener — or, on a
    /// relay-capable session, to the user's own browser through the relay.
    ///
    /// Web links only past the `file://` branch, matching
    /// `open_detected_url`: the target comes from a language server and the
    /// text it decorates says nothing about where it points, so a
    /// custom-scheme URI would hand `open`/`xdg-open` an arbitrary target
    /// (app launch, protocol handler) off one disguised Ctrl+click. The
    /// refusal surfaces the real destination in the status line.
    pub(super) fn open_document_link(&mut self, target: &str) {
        // One normalisation for every branch below, so the string that is
        // checked is the string that is forwarded: surrounding whitespace is
        // dropped, a control character (which the relay's own validator
        // rejects, and which has no business in a URI) refuses outright, and
        // the scheme match is case-insensitive like `file_ref`'s.
        let target = target.trim();
        if target.chars().any(char::is_control) {
            self.status = format!("Refused to open malformed link: {target:?}");
            return;
        }
        let lower = target.to_ascii_lowercase();
        if lower.starts_with("file://") {
            if let Ok(url) = lsp_types::Url::parse(target)
                && let Ok(p) = url.to_file_path()
            {
                // Record where the user was so Back returns to it, exactly
                // like go-to-definition and terminal file links.
                let from = self.editor.path.clone().map(|path| NavLoc {
                    path,
                    row: self.editor.cursor_row,
                    col: self.editor.cursor_col,
                });
                match self.editor.open_pinned(&p) {
                    Ok(()) => {
                        if let Some(loc) = from {
                            self.nav.record(loc);
                        }
                        self.focus_pane(Pane::Editor);
                        self.status = format!("Opened {}", self.status_path(&p));
                    }
                    Err(e) => self.status = format!("Open failed: {e}"),
                }
                return;
            }
            self.status = String::from("Malformed file link");
            return;
        }
        // Deliberately NOT delegated to `open_detected_url`, despite the
        // duplicated guard below: that path first probes `diff://` and
        // `editor_file_uri`, which resolves `vscode://file/<path>` (and the
        // Cursor/Windsurf/Zed schemes) into a file open. A terminal link is
        // text the user's own program printed; a document link's target is
        // chosen by the language server, so letting it name an editor URI
        // would hand a server the ability to open arbitrary paths from a
        // click on unrelated-looking text.
        let scheme_ok = lower.starts_with("http://") || lower.starts_with("https://");
        if !scheme_ok {
            self.status = format!("Refused to open non-web link: {target}");
            return;
        }
        // Web link on a remote session: the relay can put it in front of the
        // user's own browser, exactly like a terminal link.
        if self.drop_relay_active() || is_remote_session() {
            self.open_detected_url(target);
            return;
        }
        self.status = match open_url(target) {
            Ok(()) => format!("Opened {target}"),
            Err(e) => format!("Open link failed: {e}"),
        };
    }

    /// Drain the LSP fold-span replies (#254) into every editor showing
    /// the file. Seq-gated like inlay hints; an EMPTY reply is dropped so
    /// a server that answers before indexing (or lacks real spans) never
    /// erases the indentation/region fallback — the outline's guard.
    pub fn drain_lsp_folding_ranges(&mut self) -> bool {
        let Some(lsp) = self.lsp.as_ref() else {
            return false;
        };
        let mut updates = Vec::new();
        while let Some(u) = lsp.drain_folding_ranges() {
            updates.push(u);
        }
        let mut changed = false;
        for u in updates {
            if self.lsp_last_seen.get(&u.path) != Some(&u.seq) {
                continue;
            }
            if u.ranges.is_empty() {
                continue;
            }
            if self.editor.path.as_deref() == Some(u.path.as_path()) {
                self.editor.set_lsp_folds(u.ranges.clone());
                changed = true;
            }
            for group in self.editor_layout.inactive_groups_mut() {
                if group.path.as_deref() == Some(u.path.as_path()) {
                    group.set_lsp_folds(u.ranges.clone());
                    changed = true;
                }
            }
        }
        changed
    }

    /// Drain document-color batches (#254) into every editor showing the
    /// file — the inlay-hints fan-out, seq gate included. An empty batch
    /// clears the swatches (a stylesheet that lost its last color).
    pub fn drain_lsp_document_colors(&mut self) -> bool {
        let Some(lsp) = self.lsp.as_ref() else {
            return false;
        };
        let mut updates = Vec::new();
        while let Some(u) = lsp.drain_document_colors() {
            updates.push(u);
        }
        let mut changed = false;
        for u in updates {
            changed |= self.fan_out_batch(&u.path, u.seq, |e| {
                e.apply_document_colors(u.path.clone(), u.colors.clone())
            });
        }
        changed
    }

    /// "Change Color Presentation" (#254): ask for the alternative
    /// spellings of the color value at the cursor and defer the picker on
    /// the reply.
    pub(super) fn change_color_presentation(&mut self) {
        if !self.editor_is_text() {
            return;
        }
        let (row, col) = (self.editor.cursor_row, self.editor.cursor_col);
        let Some(item) = self.editor.color_at(row, col).cloned() else {
            self.status = String::from("No color value at the cursor");
            return;
        };
        let Some(path) = self.editor.path.clone() else {
            return;
        };
        let seq = self.editor.edit_seq;
        let Some(lsp) = self.lsp.as_mut() else {
            self.status = String::from("No language server for this file");
            return;
        };
        let id = lsp.request_color_presentations(
            path.clone(),
            item.raw,
            (item.line, item.character),
            (item.end_line, item.end_character),
        );
        self.color_presentations_request = Some((id, path, seq));
        self.status = String::from("Fetching color presentations…");
    }

    /// Open the presentation picker when the reply lands (#254); gated on
    /// id + path + edit_seq like every deferred caret request.
    pub fn drain_lsp_color_presentations(&mut self) -> bool {
        let mut results = Vec::new();
        {
            let Some(lsp) = self.lsp.as_ref() else {
                return false;
            };
            while let Some(r) = lsp.drain_color_presentations() {
                results.push(r);
            }
        }
        let mut changed = false;
        for result in results {
            let Some((id, path, seq)) = self.color_presentations_request.clone() else {
                continue;
            };
            if result.request_id != id {
                continue;
            }
            self.color_presentations_request = None;
            if self.editor.path.as_deref() != Some(path.as_path()) || self.editor.edit_seq != seq {
                continue;
            }
            changed = true;
            if result.presentations.is_empty() {
                self.status = String::from("No color presentations offered here");
                continue;
            }
            let rows: Vec<crate::widgets::list_picker::ListRow> = result
                .presentations
                .iter()
                .enumerate()
                .map(|(i, (label, _))| crate::widgets::list_picker::ListRow {
                    id: i.to_string(),
                    label: label.clone(),
                })
                .collect();
            self.pending_color_presentations = result.presentations;
            self.pending_color_context = Some((path.clone(), seq));
            self.open_list_picker(
                crate::widgets::list_picker::ListPicker::new(
                    crate::widgets::list_picker::ListPurpose::ColorPresentation,
                    "Change Color Presentation",
                    rows,
                ),
                "No color presentations offered here",
            );
        }
        changed
    }

    /// Drain server work-done progress updates into the per-server map the
    /// status bar reads. Returns true when anything changed so the main loop
    /// repaints the bar. A `Some(message)` upserts the server's status; a
    /// `None` (the server's `End`) clears it.
    pub fn drain_lsp_progress(&mut self) -> bool {
        let mut updates = Vec::new();
        if let Some(lsp) = self.lsp.as_ref() {
            while let Some(u) = lsp.drain_server_update() {
                updates.push(u);
            }
        }
        let mut changed = false;
        for u in updates {
            changed |= self.apply_server_update(u);
        }
        changed | self.ask_next_lsp_question()
    }

    /// Act on one progress update or `window/*` message from a server.
    /// Returns true when the screen needs a repaint.
    pub(super) fn apply_server_update(&mut self, u: crate::lsp::manager::ServerUpdate) -> bool {
        use crate::lsp::manager::ServerEvent;
        match u.event {
            ServerEvent::Progress(Some(message)) => {
                self.lsp_progress.insert(u.server, message);
                true
            }
            ServerEvent::Progress(None) => self.lsp_progress.remove(&u.server).is_some(),
            // An error a server can't recover from on its own, such as
            // rust-analyzer's "Failed to load workspaces", used to sit in
            // OUTPUT while the status bar said all was well (#1599).
            ServerEvent::Message { text, .. } => {
                self.status = server_message_status(&u.server, &text);
                true
            }
            ServerEvent::Question {
                text,
                actions,
                reply,
                ..
            } => {
                if actions.is_empty() {
                    // Nothing to pick: a message, answered "no action" by
                    // dropping `reply`.
                    self.status = server_message_status(&u.server, &text);
                    return true;
                }
                self.lsp_questions.push_back(LspQuestion {
                    server: u.server,
                    text,
                    actions,
                    reply,
                });
                false
            }
            ServerEvent::ShowDocument {
                uri,
                external,
                take_focus,
                selection,
                reply,
            } => {
                let shown = self.show_server_document(uri, external, take_focus, selection);
                let _ = reply.send(shown);
                true
            }
        }
    }

    /// Put the oldest waiting server question in the list picker, once no
    /// other picker is open. Returns true when one was opened.
    pub(super) fn ask_next_lsp_question(&mut self) -> bool {
        use crate::widgets::list_picker::{ListPicker, ListPurpose, ListRow};
        let asking = self
            .list_picker
            .as_ref()
            .is_some_and(|p| p.purpose == ListPurpose::ServerQuestion);
        if !asking {
            // Its picker was replaced or went away without a pick.
            self.lsp_question = None;
        }
        if self.list_picker.is_some() {
            return false;
        }
        let Some(q) = self.lsp_questions.pop_front() else {
            return false;
        };
        let rows = q
            .actions
            .iter()
            .enumerate()
            .map(|(i, a)| ListRow {
                id: i.to_string(),
                label: a.title.clone(),
            })
            .collect();
        let title = server_message_status(&q.server, &q.text);
        self.status = title.clone();
        self.lsp_question = Some(q);
        self.open_list_picker(
            ListPicker::new(ListPurpose::ServerQuestion, title, rows),
            "",
        );
        true
    }

    /// `window/showDocument`: a link goes to the browser, a file opens with
    /// the server's range selected. Without `take_focus` the file still
    /// opens but keyboard focus stays in the pane that had it. Returns
    /// whether the document was shown, which is the server's answer.
    pub(super) fn show_server_document(
        &mut self,
        uri: lsp_types::Url,
        external: bool,
        take_focus: bool,
        selection: Option<lsp_types::Range>,
    ) -> bool {
        let path = (!external && uri.scheme() == "file")
            .then(|| uri.to_file_path().ok())
            .flatten();
        let Some(path) = path else {
            return self.try_open_url_for_user(uri.as_str());
        };
        let start = selection.map(|r| r.start).unwrap_or_default();
        let (focus, maximized) = (self.focus, self.terminal_maximized);
        if !self.try_go_to_definition(path.clone(), start.line, start.character) {
            return false;
        }
        if !take_focus && focus != Pane::Editor {
            self.focus_pane(focus);
            self.terminal_maximized = maximized;
        }
        let Some(end) = selection.map(|r| r.end).filter(|&end| end != start) else {
            return true;
        };
        if self.editor.path.as_deref() != Some(path.as_path()) {
            return true;
        }
        let row = (end.line as usize).min(self.editor.lines.len().saturating_sub(1));
        let col = self.editor.utf16_col_to_char_pub(row, end.character);
        let anchor = (self.editor.cursor_row, self.editor.cursor_col);
        self.editor.selection = Some(crate::widgets::editor::EditorSelection {
            anchor,
            head: (row, col),
        });
        self.editor.cursor_row = row;
        self.editor.cursor_col = col;
        self.editor.ensure_cursor_col_visible();
        true
    }

    /// The active LSP progress line for the status bar, if any server is busy
    /// (e.g. `rust-analyzer: Indexing 112/340 33%`). Joins multiple busy
    /// servers with `  ·  ` so a Rust + Python workspace shows both.
    pub(super) fn lsp_progress_status(&self) -> Option<String> {
        if self.lsp_progress.is_empty() {
            return None;
        }
        let mut entries: Vec<String> = self
            .lsp_progress
            .iter()
            .map(|(server, msg)| format!("{server}: {msg}"))
            .collect();
        entries.sort();
        Some(entries.join("  ·  "))
    }

    /// Re-pull semantic tokens when a server asked us to via
    /// `workspace/semanticTokens/refresh`. rust-analyzer answers the first
    /// request from partial syntax analysis, then sends refresh once its
    /// cargo/crate-graph analysis resolves the richer type-aware tokens; this
    /// is how that upgraded set reaches the editor without an edit. Only the
    /// visible editor(s) are re-requested (matching VS Code), and only for
    /// files the manager has open. The fresh full batch replaces the partial
    /// one via the editor's `is_full` guard.
    pub fn refresh_semantic_tokens_if_requested(&self) {
        let Some(lsp) = self.lsp.as_ref() else {
            return;
        };
        if !lsp.take_semantic_refresh() {
            return;
        }
        let mut paths: Vec<PathBuf> = Vec::new();
        if let Some(p) = self.editor.path.clone() {
            paths.push(p);
        }
        for group in self.editor_layout.inactive_groups() {
            if let Some(p) = group.path.clone() {
                paths.push(p);
            }
        }
        for p in paths {
            // Re-request at the seq the doc was last synced at, so a reply
            // arriving after an external reload is recognised as stale.
            if let Some(&seq) = self.lsp_last_seen.get(&p) {
                lsp.request_semantic_tokens(p, seq);
            }
        }
    }

    /// Re-request inlay hints when a server asked us to via
    /// `workspace/inlayHint/refresh` (rust-analyzer sends it after a config
    /// reload or when cross-file analysis changes the hints without an edit).
    /// Mirrors [`refresh_semantic_tokens_if_requested`].
    pub fn refresh_inlay_hints_if_requested(&mut self) {
        let Some(lsp) = self.lsp.as_ref() else {
            return;
        };
        if !lsp.take_inlay_refresh() {
            return;
        }
        self.request_inlay_hints_for_open_editors();
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

    /// Fire an inlay-hint request for every visible editor's file at its
    /// current edit seq. Shared by the server-driven refresh and the palette
    /// toggle's re-enable path.
    pub(super) fn request_inlay_hints_for_open_editors(&mut self) {
        let Some(lsp) = self.lsp.as_ref() else {
            return;
        };
        for (p, end) in self.inlay_hint_targets() {
            if !self.inlay_hints_enabled_for(path_scope_id(&p)) {
                continue;
            }
            if let Some(&seq) = self.lsp_last_seen.get(&p) {
                lsp.request_inlay_hints(p, end, seq);
            }
        }
    }

    /// Every visible editor's file with where its buffer ends: the range an
    /// inlay-hint request covers (#1490).
    pub(super) fn inlay_hint_targets(&self) -> Vec<(PathBuf, lsp_types::Position)> {
        let mut targets = Vec::new();
        if let Some(p) = self.editor.path.clone() {
            targets.push((p, self.editor.document_end()));
        }
        for group in self.editor_layout.inactive_groups() {
            if let Some(p) = group.path.clone() {
                targets.push((p, group.document_end()));
            }
        }
        targets
    }

    /// Editor: Toggle Inlay Hints — flips the hint display live and persists
    /// the choice. Off clears every editor's hints; on re-requests them for
    /// the visible files.
    pub(super) fn toggle_inlay_hints(&mut self) {
        self.inlay_hints_enabled = !self.inlay_hints_enabled;
        if self.inlay_hints_enabled {
            self.request_inlay_hints_for_open_editors();
            self.status = String::from("Inlay Hints: on");
        } else {
            self.editor.clear_inlay_hints();
            for group in self.editor_layout.inactive_groups_mut() {
                group.clear_inlay_hints();
            }
            self.status = String::from("Inlay Hints: off");
        }
        if !cfg!(test) {
            self.report_unsaved_setting(crate::prefs::save_inlay_hints(self.inlay_hints_enabled));
        }
    }

    pub(super) fn go_to_definition(&mut self, path: PathBuf, line: u32, col: u32) {
        self.try_go_to_definition(path, line, col);
    }

    /// `go_to_definition`, returning whether the file opened.
    pub(super) fn try_go_to_definition(&mut self, path: PathBuf, line: u32, col: u32) -> bool {
        let pinned = self.keep_preview_open_for_navigation_to(&path);
        if let Some(from) = self.editor.path.clone() {
            self.nav.record(NavLoc {
                path: from,
                row: self.editor.cursor_row,
                col: self.editor.cursor_col,
            });
        }
        // `col` is the server's UTF-16 column, not a character column.
        match self.open_at_utf16(&path, line, col) {
            Ok(()) => {
                self.status = format!("Jumped to {} line {}", self.status_path(&path), line + 1);
                true
            }
            Err(e) => {
                self.restore_preview_after_failed_navigation(pinned);
                self.status = format!("Go to definition failed: {e}");
                false
            }
        }
    }

    /// The breadcrumb segments for the active editor: the workspace-relative
    /// path folders and file name, then the enclosing symbol chain at the
    /// caret. Empty for the welcome screen, preview tabs, and buffers with no
    /// path (so the bar hides). Symbol crumbs carry a jump target; path crumbs
    /// are informational.
    pub(super) fn build_breadcrumbs(&self) -> Vec<crate::widgets::editor::Crumb> {
        use crate::widgets::editor::Crumb;
        let Some(path) = self.editor.path.as_deref() else {
            return Vec::new();
        };
        if self.editor.has_non_text_view() {
            return Vec::new();
        }
        let mut crumbs = Vec::new();
        // Strip against the OWNING root (#147): a secondary-root file
        // crumbs from its own root (folder-relative, like status_path),
        // never the primary — which yielded the full absolute chain.
        let rel = self
            .roots
            .owning_root(path)
            .and_then(|r| path.strip_prefix(r).ok())
            .unwrap_or(path);
        for comp in rel.components() {
            if let std::path::Component::Normal(os) = comp {
                crumbs.push(Crumb {
                    label: os.to_string_lossy().into_owned(),
                    target: None,
                    icon: None,
                });
            }
        }
        // The file crumb wears its Explorer icon (drawn on a `surfaces`
        // theme only); the folders before it stay plain.
        if let Some(file) = crumbs.last_mut() {
            let suffix = path
                .extension()
                .map(|e| format!(".{}", e.to_string_lossy()))
                .unwrap_or_default();
            let icon = crate::icons::for_path(&file.label, &suffix);
            file.icon = Some((icon.glyph, icon.color));
        }
        // Symbol scope chain, only when the outline belongs to this file so a
        // stale outline from the previous tab never leaks into the bar.
        if self.outline.path() == Some(path) {
            // While scrubbing (#371) the outline is the commit's, so the
            // chain follows the historical view's caret.
            let line = self
                .scrub_view
                .as_ref()
                .map_or(self.editor.cursor_row, |v| v.cursor_row) as u32;
            let symbols = self.outline.symbols();
            let col = self
                .scrub_view
                .as_ref()
                .map_or(self.editor.cursor_col, |v| v.cursor_col) as u32;
            for i in breadcrumb_symbol_chain(symbols, line, col) {
                let s = &symbols[i];
                let icon = crate::icons::for_outline_kind(s.kind);
                crumbs.push(Crumb {
                    label: s.name.clone(),
                    target: Some((s.line, s.character)),
                    icon: Some((icon.glyph, icon.color)),
                });
            }
        }
        crumbs
    }

    /// What each crumb [`build_breadcrumbs`](Self::build_breadcrumbs) makes
    /// stands for, in the same order (#1608).
    pub(super) fn breadcrumb_targets(&self) -> Vec<CrumbTarget> {
        let Some(path) = self.editor.path.as_deref() else {
            return Vec::new();
        };
        if self.editor.has_non_text_view() {
            return Vec::new();
        }
        let root = self.roots.owning_root(path);
        let rel = root.and_then(|r| path.strip_prefix(r).ok()).unwrap_or(path);
        let mut at = root.map(Path::to_path_buf).unwrap_or_default();
        let names: Vec<_> = rel
            .components()
            .filter_map(|c| match c {
                std::path::Component::Normal(os) => Some(os.to_os_string()),
                _ => None,
            })
            .collect();
        if root.is_none() {
            // An absolute path outside every root: its crumbs start at `/`.
            at = path
                .ancestors()
                .last()
                .map(Path::to_path_buf)
                .unwrap_or_default();
        }
        let mut targets = Vec::new();
        for (k, name) in names.iter().enumerate() {
            at.push(name);
            targets.push(if k + 1 == names.len() {
                CrumbTarget::File(at.clone())
            } else {
                CrumbTarget::Folder(at.clone())
            });
        }
        if self.outline.path() == Some(path) {
            let line = self
                .scrub_view
                .as_ref()
                .map_or(self.editor.cursor_row, |v| v.cursor_row) as u32;
            let col = self
                .scrub_view
                .as_ref()
                .map_or(self.editor.cursor_col, |v| v.cursor_col) as u32;
            targets.extend(
                breadcrumb_symbol_chain(self.outline.symbols(), line, col)
                    .into_iter()
                    .map(CrumbTarget::Symbol),
            );
        }
        targets
    }

    /// Open crumb `i`'s picker under it (#1608): a folder crumb lists the
    /// folder, the file crumb lists its folder with the file selected, and a
    /// symbol crumb lists the symbols beside it with it selected.
    pub(super) fn open_breadcrumb_picker(&mut self, i: usize) {
        let Some(target) = self.breadcrumb_targets().into_iter().nth(i) else {
            return;
        };
        match target {
            CrumbTarget::Folder(dir) => self.open_crumb_folder_picker(&dir, i, None),
            CrumbTarget::File(path) => {
                let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
                self.open_crumb_folder_picker(&dir, i, Some(&path));
            }
            CrumbTarget::Symbol(s) => {
                let Some(path) = self.editor.path.clone() else {
                    return;
                };
                let symbols = self.outline.symbols();
                let siblings = sibling_symbols(symbols, s);
                let selected = siblings.iter().position(|&k| k == s).unwrap_or(0);
                let items = siblings
                    .into_iter()
                    .map(|k| {
                        let sym = &symbols[k];
                        (
                            sym.name.clone(),
                            MenuAction::GoToLocation {
                                path: path.clone(),
                                line: sym.line,
                                col: sym.character,
                            },
                        )
                    })
                    .collect();
                self.show_crumb_picker(i, items, selected);
            }
        }
    }

    /// The picker of crumb `i` listing `dir`: folders first, then files,
    /// each by name; `select` is the row to start on.
    pub(super) fn open_crumb_folder_picker(&mut self, dir: &Path, i: usize, select: Option<&Path>) {
        let mut entries: Vec<(bool, String, PathBuf)> = std::fs::read_dir(dir)
            .map(|rd| {
                rd.filter_map(Result::ok)
                    .filter(|e| e.file_name() != ".git")
                    .map(|e| {
                        let path = e.path();
                        (
                            path.is_dir(),
                            e.file_name().to_string_lossy().into_owned(),
                            path,
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        entries.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase()))
        });
        if entries.is_empty() {
            self.status = format!("{} is empty", dir.display());
            return;
        }
        let selected = select
            .and_then(|p| entries.iter().position(|e| e.2 == p))
            .unwrap_or(0);
        let items = entries
            .into_iter()
            .map(|(is_dir, name, path)| {
                if is_dir {
                    (
                        format!("{name}/"),
                        MenuAction::BrowseCrumbFolder {
                            dir: path,
                            crumb: i,
                        },
                    )
                } else {
                    (name, MenuAction::OpenCrumbFile(path))
                }
            })
            .collect();
        self.show_crumb_picker(i, items, selected);
    }

    pub(super) fn show_crumb_picker(
        &mut self,
        i: usize,
        items: Vec<(String, MenuAction)>,
        selected: usize,
    ) {
        let Some(origin) = self.editor.breadcrumb_anchor(i) else {
            return;
        };
        let mut menu = ContextMenu::flat(origin, items, self.tree.root.clone());
        menu.selected = selected;
        menu.crumb = Some(i);
        self.crumb_filter = (String::new(), menu.items.clone());
        self.context_menu = Some(menu);
    }

    /// Re-derive a breadcrumb picker's rows from `crumb_filter`: the rows
    /// whose label contains the typed text (case-insensitive), keeping the
    /// highlighted row when it survives and otherwise starting from the top.
    pub(super) fn apply_crumb_filter(&mut self) {
        let Some(menu) = self.context_menu.as_mut() else {
            return;
        };
        let (text, all) = &self.crumb_filter;
        let kept = menu.items.get(menu.selected).and_then(|e| match e {
            MenuEntry::Item { label, .. } => Some(label.clone()),
            _ => None,
        });
        let needle = text.trim().to_lowercase();
        menu.items = all
            .iter()
            .filter(|e| match e {
                MenuEntry::Item { label, .. } => {
                    needle.is_empty() || label.to_lowercase().contains(&needle)
                }
                _ => needle.is_empty(),
            })
            .cloned()
            .collect();
        menu.selected = kept
            .and_then(|k| {
                menu.items
                    .iter()
                    .position(|e| matches!(e, MenuEntry::Item { label, .. } if *label == k))
            })
            .unwrap_or(0);
        menu.scroll.set(0);
    }

    /// A file picked from a breadcrumb picker opens as a Quick Open pick does.
    pub(super) fn open_crumb_file(&mut self, path: &Path) {
        match self.editor.open_preview(path) {
            Ok(()) => {
                self.sync_open_file_poll_mtime();
                self.tree.reveal_path(path);
                self.focus_pane(Pane::Editor);
                self.status = format!("Opened {}", self.status_path(path));
            }
            Err(e) => self.status = format!("Error: {e}"),
        }
    }

    /// View: Focus and Select Breadcrumbs (#1608): the last crumb's picker,
    /// from which Left / Right walk the bar.
    pub(super) fn focus_breadcrumbs(&mut self) {
        // A row too narrow for the whole chain focuses the last crumb that
        // fit on it.
        let n = self.editor.breadcrumbs.len();
        let Some(last) = (0..n)
            .rev()
            .find(|&i| self.editor.breadcrumb_anchor(i).is_some())
        else {
            self.status = String::from("No breadcrumbs to focus");
            return;
        };
        self.focus_pane(Pane::Editor);
        self.open_breadcrumb_picker(last);
    }

    /// The scope-header lines to pin as sticky scroll: the enclosing symbols of
    /// the top visible line whose header has scrolled above the viewport,
    /// outermost first, capped. Empty when the feature does not apply (welcome
    /// screen, preview tab, no outline, or nothing scrolled off). The header
    /// is the symbol's name line (its `selectionRange`), not where its range
    /// starts: a decorated Python `def` or `class` starts at `@route(...)`,
    /// which names nothing and could be pinned over the `class` line (#940).
    pub(super) fn build_sticky_lines(&self) -> Vec<u32> {
        let Some(path) = self.editor.path.as_deref() else {
            return Vec::new();
        };
        if self.editor.has_non_text_view() {
            return Vec::new();
        }
        if self.outline.path() != Some(path) {
            return Vec::new();
        }
        let top = self.editor.scroll as u32;
        let symbols = self.outline.symbols();
        breadcrumb_symbol_chain(symbols, top, 0)
            .into_iter()
            .map(|i| symbols[i].line)
            .filter(|&l| l < top)
            .take(STICKY_SCROLL_MAX)
            .collect()
    }

    /// Keep the OUTLINE in sync with the active editor: request fresh symbols
    /// when the file or its edit-seq changed (once per change), settle a
    /// loaded-empty outline for files no language server tracks, and advance
    /// follow-cursor. Returns whether the highlight moved (for redraw).
    pub fn sync_outline(&mut self) -> bool {
        let Some(path) = self.editor.path.clone() else {
            // Back on the welcome screen: drop any stale outline.
            if self.outline.path.is_some() {
                self.outline.clear();
            }
            self.outline_synced = None;
            return false;
        };
        // A tab switch while scrubbing is noticed by render, which runs
        // after this: rebuild here first, or the new file's key would be
        // paired with the previous file's historical text.
        if self.scrubber.is_some() && self.scrub_for.as_ref() != Some(&path) {
            self.rebuild_scrub_view();
        }
        // Scrubbing (#371): the outline is the file as it was at the commit
        // on screen, from its own syntax tree; no language server knows that
        // text. Parsing a big file takes far longer than a frame, so the
        // scrubber's builder parses it off the UI thread with the finished
        // view, and until that lands the previous outline stays. Leaving
        // drops the key, so the live outline is recomputed.
        let commit = self
            .scrubber
            .as_ref()
            .and_then(|s| s.commit())
            .map(|c| c.hash.clone());
        if let (Some(row), Some(commit)) = (self.scrub_view.as_ref().map(|v| v.cursor_row), commit)
        {
            let key = (path.clone(), commit.clone());
            if self.outline_scrub_key.as_ref() != Some(&key) {
                let rel = path
                    .strip_prefix(self.workspace_root())
                    .map(|p| p.to_string_lossy().into_owned());
                let symbols = rel.ok().and_then(|rel| {
                    self.scrub_outlines
                        .get(&crate::scrubber::ViewKey { hash: commit, rel })
                        .cloned()
                });
                if let Some(symbols) = symbols {
                    self.outline.set_syntax_symbols(path, symbols, false);
                    self.outline_scrub_key = Some(key);
                    self.outline_synced = None;
                } else if self.outline.path.as_ref() != Some(&path) {
                    // Another file's outline must never stand in for this
                    // one's: empty until its own lands.
                    self.outline.set_syntax_symbols(path, Vec::new(), false);
                    self.outline_scrub_key = None;
                    self.outline_synced = None;
                }
            }
            return self.outline.follow_caret(row as u32);
        }
        if self.outline_scrub_key.take().is_some() {
            self.outline_synced = None;
        }
        // `lsp_last_seen` carries the edit seq the manager last saw for an open
        // doc; absent means no language server tracks this file.
        let seq = self.lsp_last_seen.get(&path).copied();
        let key = (path.clone(), seq);
        if self.outline_synced.as_ref() != Some(&key) {
            self.outline_synced = Some(key);
            // Tree-sitter paints the outline instantly from the buffer's own
            // syntax tree, before any language server replies. The LSP request
            // below refines it with richer kinds/detail when (and if) it lands.
            let lsp_pending = seq.is_some();
            if outline_parses_inline(&self.editor.lines) {
                let syntax = self.compute_syntax_outline(&path);
                self.outline
                    .set_syntax_symbols(path.clone(), syntax, lsp_pending);
            } else if self.outline.path.as_ref() != Some(&path) {
                // A large buffer is parsed off the UI thread (#1150); until
                // that lands, another file's outline must never stand in.
                self.outline
                    .set_syntax_symbols(path.clone(), Vec::new(), true);
            }
            if lsp_pending && let (Some(lsp), Some(seq)) = (self.lsp.as_ref(), seq) {
                lsp.request_document_symbols(path, seq);
            }
        }
        let landed = self.drain_outline_parse();
        self.start_outline_parse();
        // Follow-cursor every tick; cheap (O(symbols)) and only repaints when
        // the enclosing symbol actually changes.
        self.outline.follow_caret(self.editor.cursor_row as u32) | landed
    }

    /// Parse a large buffer's syntax outline on a worker (#1150): inline, a
    /// multi-megabyte file re-parsed from scratch on every keystroke and each
    /// character took seconds to appear. Starts only when no parse is
    /// running and the buffer moved past the last one started, so a burst of
    /// typing costs one parse when it ends, not one per key.
    pub(super) fn start_outline_parse(&mut self) {
        if self.outline_parse.is_some() || self.outline_parsed_for == self.outline_synced {
            return;
        }
        let Some(key) = self.outline_synced.clone() else {
            return;
        };
        if self.editor.path.as_ref() != Some(&key.0) || outline_parses_inline(&self.editor.lines) {
            return;
        }
        let (path, lines) = (key.0.clone(), self.editor.lines.clone());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(syntax_outline_for(&path, &lines));
        });
        self.outline_parsed_for = Some(key.clone());
        self.outline_parse = Some((key, rx));
    }

    /// Apply a finished off-thread outline parse, if the buffer is still the
    /// one it parsed and the language server's outline of that same buffer
    /// has not landed first. True when the outline changed.
    pub(super) fn drain_outline_parse(&mut self) -> bool {
        let Some((key, rx)) = &self.outline_parse else {
            return false;
        };
        let symbols = match rx.try_recv() {
            Ok(symbols) => symbols,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.outline_parse = None;
                return false;
            }
        };
        let key = key.clone();
        self.outline_parse = None;
        if self.outline_synced.as_ref() != Some(&key) {
            return false;
        }
        let (path, seq) = key;
        if seq.is_some_and(|seq| self.outline_lsp_landed == Some((path.clone(), seq))) {
            return false;
        }
        self.outline
            .set_syntax_symbols(path, symbols, seq.is_some());
        true
    }

    /// Apply a `documentSymbol` reply to the OUTLINE panel, but only when its
    /// `path` is still the active editor file — a reply for a file the user
    /// navigated away from is dropped. Refreshes follow-cursor on success.
    /// Returns whether the outline changed (for redraw).
    pub(super) fn apply_outline_symbols(
        &mut self,
        path: std::path::PathBuf,
        seq: u64,
        symbols: Vec<crate::lsp::manager::OutlineSymbol>,
    ) -> bool {
        if self.editor.path.as_ref() != Some(&path) {
            return false;
        }
        // Drop a reply answering an older buffer than the newest request: its
        // ranges are shifted by the edits made since, the caret falls outside
        // them, and the breadcrumb's symbol crumb flickers off and on with
        // every keystroke (visible against a slow remote rust-analyzer while
        // tree-sitter and the late reply take turns owning the outline). Only
        // enforced when a request seq is tracked for this file; an untracked
        // file has no newer request to be stale against.
        if let Some((p, Some(newest))) = self.outline_synced.as_ref()
            && *p == path
            && seq != *newest
        {
            return false;
        }
        // An empty reply (server lacks `documentSymbol`, or answered before it
        // finished indexing) must not blank an outline tree-sitter already
        // filled. Keep the syntactic symbols rather than flashing "No symbols".
        if symbols.is_empty() && !self.outline.is_empty() {
            return false;
        }
        if !symbols.is_empty() {
            self.outline_lsp_landed = Some((path.clone(), seq));
        }
        self.outline.set_symbols(path, symbols);
        self.outline.follow_caret(self.editor.cursor_row as u32);
        true
    }

    /// Compute the active editor's outline directly from its syntax tree, so
    /// the OUTLINE panel can paint before a (possibly cold) language server
    /// answers `documentSymbol`. Empty for files whose language has no outline
    /// query; those fall back to the LSP reply.
    pub(super) fn compute_syntax_outline(
        &self,
        path: &std::path::Path,
    ) -> Vec<crate::lsp::manager::OutlineSymbol> {
        syntax_outline_for(path, &self.editor.lines)
    }

    /// Drain pending `documentSymbol` replies and offer every one to
    /// `apply_outline_symbols`, which is the sole judge of what applies.
    ///
    /// Deliberately no pre-selection. The drain used to collapse a tick's
    /// replies to whichever ARRIVED last and discard the rest, which lost the
    /// active file's reply whenever a slow one for the file just left landed
    /// after it — the guard then rejected the survivor on path, and
    /// `sync_outline` never re-requests because its `(path, seq)` key has not
    /// changed. Picking the highest seq instead would be just as wrong: `seq`
    /// is a per-`Editor` counter that RESTARTS at zero when a file lands in a
    /// fresh editor group (a split, or close-and-reopen), so the outstanding
    /// request can carry a lower number than a reply still in flight from the
    /// old group. Offering all of them keeps the decision in the one place
    /// that knows which request is outstanding.
    pub fn drain_lsp_document_symbols(&mut self) -> bool {
        let mut replies = Vec::new();
        if let Some(lsp) = self.lsp.as_ref() {
            while let Some(r) = lsp.drain_document_symbols() {
                replies.push((r.path, r.seq, r.symbols));
            }
        }
        let mut applied = false;
        for (path, seq, symbols) in replies {
            applied |= self.apply_outline_symbols(path, seq, symbols);
        }
        applied
    }

    pub fn drain_lsp_definition(&mut self) -> bool {
        let mut target = None;
        let mut peek = None;
        let mut empty_reply = false;
        {
            let Some(lsp) = self.lsp.as_ref() else {
                return false;
            };
            let mut peek_replied = false;
            let mut replied = false;
            while let Some(result) = lsp.drain_definition() {
                if Some(result.request_id) == self.peek_definition_request_id {
                    peek_replied = true;
                    peek = result.target;
                    continue;
                }
                if Some(result.request_id) != self.definition_request_id {
                    continue;
                }
                replied = true;
                target = result.target;
            }
            if peek_replied && peek.is_none() {
                // The reply arrived empty: without this the "Peeking
                // definition" status hung forever with no popup.
                self.peek_definition_request_id = None;
                self.status = String::from("No definition found");
            }
            if replied {
                self.definition_request_id = None;
                // An empty F12 reply said nothing (#1302): a slow server, a
                // missing one and "nothing here" all looked alike.
                if target.is_none() {
                    self.status = String::from("No definition found");
                    empty_reply = true;
                }
            }
        }
        let mut changed = empty_reply;
        if let Some((path, line, col)) = peek {
            self.peek_definition_request_id = None;
            self.open_peek_popup(path, line, col);
            changed = true;
        }
        if let Some((path, line, col)) = target {
            self.go_to_definition(path, line, col);
            changed = true;
        }
        changed
    }

    /// Build and show the Peek Definition popup: a `path:line` header and a
    /// numbered excerpt of the target region with the definition line marked.
    /// The open buffer supplies the text when the target file is already open
    /// (unsaved edits visible); disk otherwise.
    pub(super) fn open_peek_popup(&mut self, path: PathBuf, line: u32, col: u32) {
        self.open_peek_popup_titled(path, line, col, None);
    }

    /// [`open_peek_popup`](Self::open_peek_popup) with an optional first
    /// line above the location, e.g. "Reference 2 of 7" (#616).
    pub(super) fn open_peek_popup_titled(
        &mut self,
        path: PathBuf,
        line: u32,
        col: u32,
        title: Option<String>,
    ) {
        // Any open buffer wins over disk — the ACTIVE group's tabs plus
        // every inactive split leaf's — so unsaved edits show wherever the
        // target file happens to be open.
        let open_buffer = self
            .editor
            .editors
            .iter()
            .chain(
                self.editor_layout
                    .inactive_leaf_tabs()
                    .into_iter()
                    .flat_map(|tabs| tabs.editors.iter()),
            )
            .find(|e| e.path.as_deref() == Some(path.as_path()))
            .map(|e| e.lines.clone());
        let lines: Vec<String> = match open_buffer {
            Some(lines) => lines,
            None => match std::fs::read_to_string(&path) {
                Ok(text) => text.lines().map(|s| s.to_string()).collect(),
                Err(e) => {
                    self.status = format!("Peek failed: {e}");
                    return;
                }
            },
        };
        let target = line as usize;
        let (start, excerpt) = peek_excerpt(&lines, target, 4, 12);
        let mut body: Vec<String> = title.into_iter().collect();
        body.push(format!("{}:{}", self.status_path(&path), target + 1));
        let num_w = (start + excerpt.len()).to_string().len();
        for (i, text) in excerpt.iter().enumerate() {
            let n = start + i;
            let marker = if n == target { '\u{25b6}' } else { ' ' };
            body.push(format!("{marker}{:>num_w$}  {text}", n + 1));
        }
        body.push(String::from("Enter jumps \u{00b7} Esc closes"));
        let anchor = self.editor.cursor_screen_pos().unwrap_or((
            self.editor.last_full_area.x + 1,
            self.editor.last_full_area.y + 1,
        ));
        self.peek_popup = Some(crate::widgets::hover_popup::HoverPopup::new(
            body.join("\n"),
            anchor,
        ));
        self.peek_target = Some((path, line, col));
        self.status = String::new();
    }

    pub(super) fn close_peek_popup(&mut self) {
        self.peek_popup = None;
        self.peek_target = None;
        self.peek_refs = None;
    }

    /// Show the reference `peek_refs` points at in the peek popup (#616),
    /// titled with its place in the list.
    pub(super) fn show_peeked_reference(&mut self) {
        let Some((targets, idx)) = self.peek_refs.clone() else {
            return;
        };
        let Some((path, line, col)) = targets.get(idx).cloned() else {
            return;
        };
        let title = format!(
            "Reference {} of {}  \u{2191}\u{2193} steps",
            idx + 1,
            targets.len()
        );
        self.open_peek_popup_titled(path, line, col, Some(title));
        // Re-opening goes through the same path as a fresh peek, which
        // leaves the reference list alone; restore it in case it was reset.
        self.peek_refs = Some((targets, idx));
    }

    /// Step Peek References by `delta`, wrapping at either end.
    pub(super) fn step_peeked_reference(&mut self, delta: isize) {
        if let Some((targets, idx)) = self.peek_refs.as_mut() {
            let n = targets.len() as isize;
            *idx = (*idx as isize + delta).rem_euclid(n) as usize;
        }
        self.show_peeked_reference();
    }

    /// Peek References (Alt+Shift+F12, #616): the references of the symbol
    /// at the caret in the peek popup rather than a picker.
    pub(super) fn peek_references_at_cursor(&mut self) {
        self.request_references_at_cursor();
        self.references_want_peek = self.references_request_id.is_some();
    }

    pub fn drain_lsp_declaration(&mut self) -> bool {
        let Some(lsp) = self.lsp.as_ref() else {
            return false;
        };
        let mut reply = None;
        while let Some(result) = lsp.drain_declaration() {
            if Some(result.request_id) == self.declaration_request_id {
                reply = Some((result.target, result.unsupported));
            }
        }
        let Some((target, unsupported)) = reply else {
            return false;
        };
        self.declaration_request_id = None;
        self.finish_jump(target, unsupported, "Go to Declaration", "declaration");
        true
    }

    pub fn drain_lsp_type_definition(&mut self) -> bool {
        let Some(lsp) = self.lsp.as_ref() else {
            return false;
        };
        let mut reply = None;
        while let Some(result) = lsp.drain_type_definition() {
            if Some(result.request_id) == self.type_definition_request_id {
                reply = Some((result.target, result.unsupported));
            }
        }
        let Some((target, unsupported)) = reply else {
            return false;
        };
        self.type_definition_request_id = None;
        self.finish_jump(
            target,
            unsupported,
            "Go to Type Definition",
            "type definition",
        );
        true
    }

    /// Act on the reply to a go-to request: jump to its target the way Go to
    /// Definition does (open the file, move the caret, record nav history),
    /// or say so rather than silently doing nothing, both when the language
    /// server has no `feature` provider (`unsupported`, e.g. TypeScript's
    /// vtsls has no declarations) and when it found no `noun` at this
    /// position.
    pub(super) fn finish_jump(
        &mut self,
        target: Option<(PathBuf, u32, u32)>,
        unsupported: bool,
        feature: &str,
        noun: &str,
    ) {
        match target {
            Some((path, line, col)) => self.go_to_definition(path, line, col),
            None if unsupported => {
                self.status = format!("{feature}: not supported by this file's language server");
            }
            None => self.status = format!("No {noun} found"),
        }
    }

    pub fn drain_lsp_implementation(&mut self) -> bool {
        let mut targets: Vec<(PathBuf, u32, u32)> = Vec::new();
        let mut unsupported = false;
        let mut responded = false;
        {
            let Some(lsp) = self.lsp.as_ref() else {
                return false;
            };
            while let Some(result) = lsp.drain_implementation() {
                if Some(result.request_id) != self.implementation_request_id {
                    continue;
                }
                targets = result.targets;
                unsupported = result.unsupported;
                responded = true;
            }
        }
        if !responded {
            return false;
        }
        if unsupported {
            self.status =
                String::from("Go to Implementations: not supported by this file's language server");
            return true;
        }
        match targets.len() {
            0 => self.status = String::from("No implementations found"),
            // One implementor: behave exactly like Go to Definition.
            1 => {
                let (path, line, col) = targets.into_iter().next().expect("len == 1");
                self.go_to_definition(path, line, col);
            }
            // Many implementors: offer a picker rather than silently choosing
            // the first. This 1:many case is the whole point of the feature.
            _ => self.open_location_picker(targets, "implementations"),
        }
        true
    }

    pub fn drain_lsp_references(&mut self) -> bool {
        let mut targets: Vec<(PathBuf, u32, u32)> = Vec::new();
        let mut unsupported = false;
        let mut responded = false;
        {
            let Some(lsp) = self.lsp.as_ref() else {
                return false;
            };
            while let Some(result) = lsp.drain_references() {
                if Some(result.request_id) != self.references_request_id {
                    continue;
                }
                targets = result.targets;
                unsupported = result.unsupported;
                responded = true;
            }
        }
        if !responded {
            return false;
        }
        let peek = std::mem::take(&mut self.references_want_peek);
        if unsupported {
            self.status =
                String::from("Go to References: not supported by this file's language server");
            return true;
        }
        if peek && !targets.is_empty() {
            self.peek_refs = Some((targets, 0));
            self.show_peeked_reference();
            return true;
        }
        match targets.len() {
            0 => self.status = String::from("No references found"),
            // A symbol used exactly once: jump straight there like Go to
            // Definition rather than pop a one-row picker.
            1 => {
                let (path, line, col) = targets.into_iter().next().expect("len == 1");
                self.go_to_definition(path, line, col);
            }
            // The common case: a symbol used in many places. Offer every use as
            // a picker, exactly like Go to Implementations.
            _ => self.open_location_picker(targets, "references"),
        }
        true
    }

    /// Apply a call-hierarchy reply: a picker of callers/callees labelled
    /// `name — path:line`, jumping on select. Invoking the command again at a
    /// picked site walks the next level of the hierarchy.
    pub fn drain_lsp_call_hierarchy(&mut self) -> bool {
        let mut result = None;
        {
            let Some(lsp) = self.lsp.as_ref() else {
                return false;
            };
            while let Some(r) = lsp.drain_call_hierarchy() {
                if Some(r.request_id) == self.call_hierarchy_request_id {
                    result = Some(r);
                }
            }
        }
        let Some(result) = result else {
            return false;
        };
        let noun = result.kind.noun();
        if result.unsupported {
            let what = match result.kind {
                crate::lsp::manager::HierarchyKind::Supertypes
                | crate::lsp::manager::HierarchyKind::Subtypes => "Type hierarchy",
                _ => "Call hierarchy",
            };
            self.status = format!("{what}: not supported by this file's language server");
            return true;
        }
        if result.sites.is_empty() {
            self.status = format!("No {noun}");
            return true;
        }
        let root = self.tree.root.clone();
        let items: Vec<(String, MenuAction)> = result
            .sites
            .into_iter()
            .map(|s| {
                let label = format!(
                    "{} — {}:{}",
                    s.name,
                    s.path.strip_prefix(&root).unwrap_or(&s.path).display(),
                    s.line + 1
                );
                (
                    label,
                    MenuAction::GoToLocation {
                        path: s.path,
                        line: s.line,
                        col: s.character,
                    },
                )
            })
            .collect();
        self.present_call_hierarchy_menu(items, noun);
        true
    }

    /// Show a call-hierarchy reply as a picker at the caret — unless another
    /// menu is already open: rust-analyzer can answer seconds later, and a
    /// late reply must not replace what the user is looking at (or yank
    /// focus to the editor under them).
    pub(super) fn present_call_hierarchy_menu(
        &mut self,
        items: Vec<(String, MenuAction)>,
        noun: &str,
    ) {
        if self.context_menu.is_some() {
            self.status = String::from("Call hierarchy ready, but another menu is open");
            return;
        }
        self.status = format!("{} {noun}", items.len());
        let origin = self.editor.cursor_screen_pos().unwrap_or((
            self.editor.last_full_area.x + 1,
            self.editor.last_full_area.y + 1,
        ));
        // A late server reply is not the user asking to leave the sidebar.
        // The `context_menu` guard cannot cover this: it is set on the NEXT
        // line, so auto-hide would already have fired.
        self.without_auto_hide(|app| app.focus_pane(Pane::Editor));
        let root = self.tree.root.clone();
        self.context_menu = Some(ContextMenu::flat(origin, items, root));
    }

    /// Request the callers (`incoming`) or callees of the symbol at the
    /// cursor. Cmd+K H / Cmd+K Shift+H and the right-click menu rows.
    pub(super) fn request_call_hierarchy_at_cursor(&mut self, incoming: bool) {
        let (line, character) = self
            .editor
            .pos_to_utf16(self.editor.cursor_row, self.editor.cursor_col);
        let Some(path) = self.editor.path.clone() else {
            return;
        };
        let Some(lsp) = self.lsp.as_mut() else {
            self.status = String::from("Call hierarchy: no language server for this file");
            return;
        };
        let id = lsp.request_call_hierarchy(path, line, character, incoming);
        self.call_hierarchy_request_id = Some(id);
    }

    /// Request the supertypes or subtypes of the type at the cursor (#613).
    /// Cmd+K Shift+U / Cmd+K Shift+D and the right-click menu rows.
    pub(super) fn request_type_hierarchy_at_cursor(&mut self, supertypes: bool) {
        let (line, character) = self
            .editor
            .pos_to_utf16(self.editor.cursor_row, self.editor.cursor_col);
        let Some(path) = self.editor.path.clone() else {
            return;
        };
        let Some(lsp) = self.lsp.as_mut() else {
            self.status = String::from("Type hierarchy: no language server for this file");
            return;
        };
        let id = lsp.request_type_hierarchy(path, line, character, supertypes);
        self.call_hierarchy_request_id = Some(id);
    }

    /// Whether the current file's language server implements call hierarchy;
    /// drives the right-click menu rows, like the other capability gates.
    pub(super) fn editor_language_supports_call_hierarchy(&self) -> bool {
        self.editor_language_supports(crate::lsp::LspManager::language_supports_call_hierarchy)
    }

    /// The language the current editor file's name or extension maps to, if
    /// any.
    pub(super) fn editor_language(&self) -> Option<crate::lsp::Language> {
        self.editor
            .path
            .as_deref()
            .and_then(crate::lsp::Language::from_path)
    }

    /// Whether the current editor file's language server reports support
    /// through `supports`, one of the `language_supports_*` getters on
    /// [`crate::lsp::LspManager`]. Unknown (server not yet spawned) or no
    /// server counts as unsupported, so a gated row only appears once support
    /// is confirmed.
    pub(super) fn editor_language_supports(
        &self,
        supports: fn(&crate::lsp::LspManager, crate::lsp::Language) -> Option<bool>,
    ) -> bool {
        let Some(lang) = self.editor_language() else {
            return false;
        };
        self.lsp
            .as_ref()
            .and_then(|lsp| supports(lsp, lang))
            .unwrap_or(false)
    }

    /// On-type formatting (#254), run once per frame: when enabled and the
    /// last edit was a keystroke in the server's advertised trigger set,
    /// fire `textDocument/onTypeFormatting` at the caret. The typed record
    /// is consumed here — examined exactly once, on the frame right after
    /// the keystroke — so a later tab switch or settings toggle can never
    /// replay a stale trigger. Never fires unadvertised: the trigger set
    /// comes from the capability the server declared, and `None` (no server
    /// probed, or no provider) means stay inert.
    pub fn tick_on_type_formatting(&mut self) {
        let taken = self.editor.last_typed.take();
        // A keystroke and a tab switch can arrive in the same input burst,
        // draining before this tick runs — the record is then armed on a tab
        // that is no longer active, and nothing else ever clears it. Sweep
        // every editor in every group (the active one is already drained by
        // the `take` above) so a trigger can only ever fire from the tab
        // that was active at its tick.
        let groups =
            std::iter::once(&mut self.editor).chain(self.editor_layout.inactive_groups_mut());
        for ed in groups.flat_map(|g| g.editors.iter_mut()) {
            ed.last_typed = None;
        }
        let Some((ch, seq)) = taken else {
            return;
        };
        if !self.format_on_type_for(editor_scope_id(&self.editor)) || !self.editor_is_text() {
            return;
        }
        if seq != self.editor.edit_seq {
            return;
        }
        let Some(path) = self.editor.path.clone() else {
            return;
        };
        let Some(lang) = crate::lsp::Language::from_path(&path) else {
            return;
        };
        let triggers = self
            .lsp
            .as_ref()
            .and_then(|lsp| lsp.language_on_type_triggers(lang));
        if !triggers.is_some_and(|t| t.contains(ch)) {
            return;
        }
        let (line, character) = self
            .editor
            .pos_to_utf16(self.editor.cursor_row, self.editor.cursor_col);
        let (tab_size, insert_spaces) = self.editor.indent_preference();
        let Some(lsp) = self.lsp.as_mut() else {
            return;
        };
        let id = lsp.request_on_type_formatting(
            path.clone(),
            line,
            character,
            ch,
            tab_size,
            insert_spaces,
        );
        self.on_type_request = Some((id, seq, path));
    }

    /// What to do with one on-type formatting reply. Extracted from the
    /// drain loop so the three-way decision is testable without a live
    /// server: the channel's sender lives inside the LSP worker, so a test
    /// cannot inject a `FormatResult` through it.
    pub(super) fn on_type_reply_disposition(
        &self,
        result: &crate::lsp::manager::FormatResult,
    ) -> OnTypeReply {
        let Some((id, seq, ref requested)) = self.on_type_request else {
            return OnTypeReply::NotOurs;
        };
        // A superseded reply must NOT clear the armed request: a newer one
        // is in flight and its own reply still has to apply.
        if result.request_id != id {
            return OnTypeReply::NotOurs;
        }
        // `edit_seq` is per-tab, so the counter comparison only means
        // "buffer unchanged" when the active tab is still the one the
        // request was computed against — require the path to match too.
        // Content-unchanged is also the whole test on purpose: caret-only
        // motion (arrow keys, auto-pair type-over) leaves the counter
        // alone, and that is correct — the server's edits are document
        // edits computed against text that is still byte-identical, so
        // they apply cleanly wherever the caret has wandered.
        if self.editor.edit_seq != seq
            || requested != &result.path
            || self.editor.path.as_deref() != Some(result.path.as_path())
        {
            return OnTypeReply::Stale;
        }
        OnTypeReply::Apply
    }

    /// Apply an on-type formatting reply — quietly, and only while the
    /// buffer has not moved since the request (the edits were computed
    /// against exactly that text; continued typing makes them stale).
    pub fn drain_lsp_on_type_format(&mut self) -> bool {
        let mut results = Vec::new();
        {
            let Some(lsp) = self.lsp.as_ref() else {
                return false;
            };
            while let Some(r) = lsp.drain_on_type_formatting() {
                results.push(r);
            }
        }
        let mut changed = false;
        for result in results {
            match self.on_type_reply_disposition(&result) {
                OnTypeReply::NotOurs => continue,
                OnTypeReply::Stale => {
                    self.on_type_request = None;
                    continue;
                }
                OnTypeReply::Apply => self.on_type_request = None,
            }
            if let Some(edits) = result.edits.filter(|e| !e.is_empty())
                && self.apply_on_type_edits(&result.path, &edits)
            {
                changed = true;
            }
        }
        changed
    }

    /// Apply an on-type formatting reply for `path`, the active tab's file
    /// (the reply's disposition checked that), keeping the caret beside the
    /// text that was just typed.
    pub(crate) fn apply_on_type_edits(
        &mut self,
        path: &std::path::Path,
        edits: &[crate::widgets::editor::TextSpanEdit],
    ) -> bool {
        if self.editor.path.as_deref() == Some(path) {
            return self.editor.apply_span_edits_moving_caret(edits) > 0;
        }
        let applied = self.editor.apply_rename_to_open_tab(path, edits).is_some();
        self.editor.clamp_cursor();
        applied
    }

    /// Linked editing (#254): replay the last keystroke across the
    /// sibling tag ranges, keep the set alive only while the caret
    /// stays inside one, and (debounced on caret idle) ask the server
    /// for the set at a new caret position. Never fires a request for a
    /// language the LSP layer isn't tracking; a server without the
    /// provider simply never answers, and the feature stays inert.
    pub fn tick_linked_editing(&mut self) -> bool {
        const LINKED_IDLE: std::time::Duration = std::time::Duration::from_millis(250);
        let mirrored = self.editor.mirror_linked_edit();
        let eligible = self.editor_is_text() && self.lsp.is_some();
        let Some(path) = self.editor.path.clone().filter(|_| eligible) else {
            self.linked_observed = None;
            self.linked_request = None;
            self.linked_asked = false;
            self.editor.clear_linked_ranges();
            return mirrored;
        };
        let (row, col) = (self.editor.cursor_row, self.editor.cursor_col);
        // While the caret stays inside the live set, keep it — the whole
        // point is typing inside a range. (The mirror above re-synced
        // positions for this frame's keystroke.)
        if self.editor.has_linked_ranges() {
            if self.editor.linked_ranges_contain(row, col) {
                return mirrored;
            }
            self.editor.clear_linked_ranges();
        }
        let cur = (path, row, col, self.editor.edit_seq);
        if self.linked_observed.as_ref() != Some(&cur) {
            self.linked_observed = Some(cur);
            self.linked_observed_at = std::time::Instant::now();
            self.linked_request = None;
            self.linked_asked = false;
            return mirrored;
        }
        if self.linked_asked || self.linked_observed_at.elapsed() < LINKED_IDLE {
            return mirrored;
        }
        let (uline, uchar) = self.editor.cursor_position_utf16();
        let Some(lsp) = self.lsp.as_mut() else {
            return mirrored;
        };
        let id = lsp.request_linked_editing(cur.0.clone(), uline, uchar);
        self.linked_request = Some((id, cur.0, cur.3));
        self.linked_asked = true;
        mirrored
    }

    /// Install a linkedEditingRange reply, gated on the in-flight id,
    /// the path, and the edit seq the spans were computed against.
    pub fn drain_lsp_linked_editing(&mut self) -> bool {
        let mut results = Vec::new();
        {
            let Some(lsp) = self.lsp.as_ref() else {
                return false;
            };
            while let Some(r) = lsp.drain_linked_editing() {
                results.push(r);
            }
        }
        let mut changed = false;
        for result in results {
            let Some((id, path, seq)) = self.linked_request.as_ref() else {
                continue;
            };
            if result.request_id != *id
                || self.editor.path.as_deref() != Some(path.as_path())
                || self.editor.edit_seq != *seq
            {
                continue;
            }
            self.linked_request = None;
            self.editor.set_linked_ranges(&result.ranges);
            changed = true;
        }
        changed
    }

    /// Expand Selection (#254): grow every cursor to the next
    /// semantically meaningful range. Cached chain steps apply
    /// instantly; otherwise the LSP `selectionRange` chains are fetched
    /// (one request, all cursors), with tree-sitter node ancestry as the
    /// serverless fallback — so the gesture works with no LSP at all.
    pub(super) fn expand_selection(&mut self) {
        if !self.editor_is_text() {
            return;
        }
        self.editor.validate_expand_stacks();
        if self.editor.expand_selection_from_stack() {
            return;
        }
        let lang_known = self.editor_language().is_some();
        if lang_known
            && self.lsp.is_some()
            && let Some(path) = self.editor.path.clone()
        {
            let seq = self.editor.edit_seq;
            let positions = self.editor.cursor_positions_utf16();
            // Reuse the pending request only when it was fired for these
            // exact cursors; a caret move re-fires for the new spot.
            if let Some(pending) = self.selection_range_request.as_mut()
                && pending.path == path
                && pending.seq == seq
                && pending.positions == positions
            {
                pending.presses += 1;
                return;
            }
            if let Some(lsp) = self.lsp.as_mut() {
                let id = lsp.request_selection_ranges(path.clone(), positions.clone());
                self.selection_range_request = Some(PendingSelectionRanges {
                    id,
                    path,
                    seq,
                    presses: 1,
                    positions,
                });
            }
            return;
        }
        if !self.editor.expand_selection_syntax() {
            self.status = String::from("No larger range to select");
        }
    }

    /// Shrink Selection: retrace the expand stack one step. Purely
    /// local — the stack already remembers the exact ranges.
    pub(super) fn shrink_selection(&mut self) {
        if !self.editor_is_text() {
            return;
        }
        self.editor.shrink_selection_step();
    }

    /// Drain `selectionRange` replies (#254): triple-gated like
    /// occurrences (id + path + edit_seq), then either install the
    /// server's chains or resolve the queued presses via tree-sitter
    /// when the request came back unsupported/empty.
    pub(super) fn drain_lsp_selection_ranges(&mut self) -> bool {
        let Some(lsp) = self.lsp.as_ref() else {
            return false;
        };
        let mut results = Vec::new();
        while let Some(r) = lsp.drain_selection_ranges() {
            results.push(r);
        }
        let mut changed = false;
        for r in results {
            let Some(pending) = self.selection_range_request.as_ref() else {
                continue;
            };
            // Gate on id + path + edit_seq AND the cursors the request
            // was fired for: a click between request and reply leaves
            // `edit_seq` unchanged, but the chains describe the OLD
            // caret and must not apply at the new one.
            if r.request_id != pending.id
                || self.editor.path.as_deref() != Some(pending.path.as_path())
                || self.editor.edit_seq != pending.seq
                || self.editor.cursor_positions_utf16() != pending.positions
            {
                if r.request_id == pending.id {
                    self.selection_range_request = None;
                }
                continue;
            }
            let presses = pending.presses;
            self.selection_range_request = None;
            if r.unsupported || r.chains.is_empty() {
                for _ in 0..presses {
                    if !self.editor.expand_selection_syntax() {
                        break;
                    }
                }
                changed = true;
                continue;
            }
            self.editor.install_selection_chains(r.chains);
            for _ in 0..presses {
                if !self.editor.expand_selection_from_stack() {
                    break;
                }
            }
            changed = true;
        }
        changed
    }

    /// Idle-caret occurrences trigger, run once per frame: when the caret has
    /// rested on one buffer position for `OCCURRENCES_IDLE`, fire ONE
    /// `documentHighlight` request for it; the moment it moves (or the buffer
    /// edits), drop the painted set. Mirrors VS Code's word highlight, which
    /// follows the cursor with a small debounce rather than every keystroke.
    pub fn tick_occurrences(&mut self) -> bool {
        const OCCURRENCES_IDLE: std::time::Duration = std::time::Duration::from_millis(250);
        let eligible = self.editor.diff.is_none()
            && self.editor.sheet.is_none()
            && self.editor.image.is_none()
            && self.lsp.is_some();
        let Some(path) = self.editor.path.clone().filter(|_| eligible) else {
            self.occ_observed = None;
            self.occurrences_request = None;
            return self.editor.clear_occurrences();
        };
        let cur = (
            path,
            self.editor.cursor_row,
            self.editor.cursor_col,
            self.editor.edit_seq,
        );
        if self.occ_observed.as_ref() != Some(&cur) {
            self.occ_observed = Some(cur);
            self.occ_observed_at = std::time::Instant::now();
            // The old symbol's tints are stale the moment the caret leaves it
            // (an edit already cleared them via mark_buffer_changed), and an
            // in-flight reply for the old spot must never paint late.
            self.occurrences_request = None;
            return self.editor.clear_occurrences();
        }
        let already_requested = self
            .occurrences_request
            .as_ref()
            .is_some_and(|(_, p, r, c, s)| (p, r, c, s) == (&cur.0, &cur.1, &cur.2, &cur.3));
        if already_requested || self.occ_observed_at.elapsed() < OCCURRENCES_IDLE {
            return false;
        }
        let (uline, uchar) = self.editor.pos_to_utf16(cur.1, cur.2);
        let Some(lsp) = self.lsp.as_mut() else {
            return false;
        };
        let id = lsp.request_document_highlights(cur.0.clone(), uline, uchar);
        self.occurrences_request = Some((id, cur.0, cur.1, cur.2, cur.3));
        false
    }

    /// Apply a documentHighlight reply to the editor, dropping it unless it
    /// answers the in-flight request AND the editor still shows that file at
    /// that edit seq (the columns were computed against that text).
    pub fn drain_lsp_document_highlights(&mut self) -> bool {
        let mut results = Vec::new();
        {
            let Some(lsp) = self.lsp.as_ref() else {
                return false;
            };
            while let Some(r) = lsp.drain_document_highlights() {
                results.push(r);
            }
        }
        let mut changed = false;
        for result in results {
            let Some((id, path, _row, _col, seq)) = self.occurrences_request.as_ref() else {
                continue;
            };
            if result.request_id != *id
                || self.editor.path.as_deref() != Some(path.as_path())
                || self.editor.edit_seq != *seq
            {
                continue;
            }
            self.editor.apply_occurrences(result.items);
            changed = true;
        }
        changed
    }

    /// Build a multi-location picker as a `ContextMenu` anchored at the editor
    /// cursor: one row per target, labelled `relative/path:line`, each carrying a
    /// `GoToLocation` that jumps on select. Reuses the context menu's existing
    /// keyboard (arrows + Enter) and mouse handling. Shared by Go to
    /// Implementations and Go to References; `noun` is the status-line plural
    /// ("implementations" / "references").
    pub(super) fn open_location_picker(&mut self, targets: Vec<(PathBuf, u32, u32)>, noun: &str) {
        // Same guard as present_call_hierarchy_menu: the LSP can answer
        // seconds later, and the reply must not replace a menu the user
        // opened while waiting (or yank focus to the editor).
        if self.context_menu.is_some() {
            self.status = String::from("Locations ready, but another menu is open");
            return;
        }
        let root = self.tree.root.clone();
        let current = self.location_under_caret(&targets);
        let items: Vec<(String, MenuAction)> = targets
            .into_iter()
            .map(|(path, line, col)| {
                let label = format!(
                    "{}:{}",
                    path.strip_prefix(&root).unwrap_or(&path).display(),
                    line + 1
                );
                (label, MenuAction::GoToLocation { path, line, col })
            })
            .collect();
        self.status = format!("{} {noun}", items.len());
        let origin = self.editor.cursor_screen_pos().unwrap_or((
            self.editor.last_full_area.x + 1,
            self.editor.last_full_area.y + 1,
        ));
        // As in `present_call_hierarchy_menu`: the reply arrives whenever the
        // server is ready, and the `context_menu` guard is armed a line too
        // late to protect the sidebar from this focus move.
        self.without_auto_hide(|app| app.focus_pane(Pane::Editor));
        let mut menu = ContextMenu::flat(origin, items, root);
        // Start on the reference the caret is on, as VS Code's peek does.
        menu.selected = current;
        self.context_menu = Some(menu);
    }

    /// Which of `targets` the caret is on: the one in this file, on the
    /// caret's line, starting at or before the caret (the nearest such), else
    /// the first one on that line. 0 when none is (a reference list opened
    /// from elsewhere starts at the top).
    pub(super) fn location_under_caret(&self, targets: &[(PathBuf, u32, u32)]) -> usize {
        let Some(path) = self.editor.path.as_deref() else {
            return 0;
        };
        let (row, col) = (self.editor.cursor_row, self.editor.cursor_col);
        let on_line = || {
            targets
                .iter()
                .enumerate()
                .filter(move |(_, (p, line, _))| p == path && *line as usize == row)
        };
        on_line()
            .filter(|(_, (_, _, c))| *c as usize <= col)
            .max_by_key(|(_, (_, _, c))| *c)
            .or_else(|| on_line().next())
            .map_or(0, |(i, _)| i)
    }
}
