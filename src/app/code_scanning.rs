//! SARIF results viewer (list, open, preview, baselines, export, fixes and step marks),
//! GitHub code scanning (pick, download, follow branch, dismiss) and SARIF logs shown as
//! problems.

use super::*;

/// GET `url`'s body, up to 64 MiB, for a SARIF location's source.
pub(super) fn fetch_sarif_source(url: &str) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let resp = crate::net::builder(url)
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .get(url)
        .set("User-Agent", concat!("croft/", env!("CARGO_PKG_VERSION")))
        .call()
        .map_err(|e| match e {
            ureq::Error::Status(404, _) => String::from("not found (a private repository?)"),
            ureq::Error::Status(code, _) => format!("HTTP {code}"),
            other => other.to_string(),
        })?;
    let mut bytes = Vec::new();
    resp.into_reader()
        .take(64 << 20)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    Ok(bytes)
}

impl App {
    /// Publish every open SARIF log's results as diagnostics (#577), and
    /// withdraw what a closed log published. Recomputed only when the set of
    /// open logs, or one of them on disk, changes; returns whether anything
    /// did.
    pub fn sync_sarif_diagnostics(&mut self) -> bool {
        // Logs added to a viewer with `o` are read again when they change
        // on disk (#577), at most once a second.
        if self
            .sarif_added_polled
            .is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(1))
        {
            self.sarif_added_polled = Some(std::time::Instant::now());
            let mut refreshed = 0;
            for tab in self.editor.editors.iter_mut() {
                if let Some(view) = tab.sarif.as_mut() {
                    refreshed += view.refresh_added_logs().len();
                }
            }
            if refreshed > 0 {
                // Their results are published again below.
                self.sarif_diag_signature.clear();
                self.status = format!(
                    "Reloaded {refreshed} changed SARIF log{}",
                    if refreshed == 1 { "" } else { "s" }
                );
            }
        }
        let inactive: Vec<&crate::widgets::editor::Editor> = self
            .editor_layout
            .inactive_groups()
            .into_iter()
            .flat_map(|g| g.editors.iter())
            .collect();
        let mut signature: Vec<(PathBuf, Option<(std::time::SystemTime, u64)>)> = self
            .editor
            .iter_tabs()
            .chain(inactive)
            .filter(|t| t.sarif.is_some())
            .filter_map(|t| Some((t.path.clone()?, t.disk_stamp())))
            .collect();
        signature.sort();
        signature.dedup_by(|a, b| a.0 == b.0);
        if signature == self.sarif_diag_signature {
            return false;
        }
        self.sarif_diag_signature = signature.clone();
        // Withdraw everything published before, then publish afresh.
        let mut touched: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
        for (target, key) in std::mem::take(&mut self.sarif_published) {
            if let Some(by_source) = self.lsp_diagnostics.get_mut(&target) {
                by_source.remove(&key);
                if by_source.is_empty() {
                    self.lsp_diagnostics.remove(&target);
                }
            }
            touched.insert(target);
        }
        let root = self.workspace_root().to_path_buf();
        let mut file_lines: std::collections::HashMap<PathBuf, Vec<String>> =
            std::collections::HashMap::new();
        let mut line_of = |p: &std::path::Path, n: usize| -> Option<String> {
            file_lines
                .entry(p.to_path_buf())
                .or_insert_with(|| {
                    std::fs::read_to_string(p)
                        .map(|t| t.lines().map(str::to_string).collect())
                        .unwrap_or_default()
                })
                .get(n)
                .cloned()
        };
        let mut published: std::collections::HashMap<
            (PathBuf, String),
            Vec<crate::lsp::manager::Diagnostic>,
        > = std::collections::HashMap::new();
        let views: Vec<&crate::sarif::view::SarifView> = self
            .editor
            .iter_tabs()
            .chain(
                self.editor_layout
                    .inactive_groups()
                    .into_iter()
                    .flat_map(|g| g.editors.iter()),
            )
            .filter_map(|t| t.sarif.as_ref())
            .collect();
        let mut seen_logs = std::collections::HashSet::new();
        for view in views {
            for (index, loaded) in view.logs.iter().enumerate() {
                // What the baseline reported is history, not today's code.
                if view.baseline == Some(index) || !seen_logs.insert(loaded.path.clone()) {
                    continue;
                }
                let mut roots = vec![root.clone()];
                if let Some(dir) = loaded.path.parent() {
                    roots.push(dir.to_path_buf());
                }
                let resolver = crate::sarif::resolve::Resolver {
                    roots,
                    learned: crate::sarif::resolve::saved_prefixes(self.workspace_root()),
                    open: self.open_file_paths(),
                    ..Default::default()
                };
                let log_index = view.logs.iter().position(|l| l.path == loaded.path);
                for (run_i, run) in loaded.log.runs.iter().enumerate() {
                    let key = crate::sarif::diagnostics::source_key(run);
                    for (result_i, result) in run.results.iter().flatten().enumerate() {
                        // A fixed result no longer squiggles.
                        if view.entries.iter().any(|e| {
                            Some(e.log) == log_index
                                && e.run == run_i
                                && e.result == result_i
                                && e.fixed
                        }) {
                            continue;
                        }
                        if let Some((target, d)) = crate::sarif::diagnostics::diagnostic_for(
                            run,
                            result,
                            &resolver,
                            &mut line_of,
                        ) {
                            published.entry((target, key.clone())).or_default().push(d);
                        }
                    }
                }
            }
        }
        for ((target, key), diags) in published {
            self.lsp_diagnostics
                .entry(target.clone())
                .or_default()
                .insert(key.clone(), diags);
            self.sarif_published.push((target.clone(), key));
            touched.insert(target);
        }
        for path in touched {
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
        self.rebuild_problems();
        self.refresh_problems_badge();
        true
    }

    /// SARIF viewer keys (#577). While the filter box has focus, printable
    /// keys edit the query; otherwise they drive the list.
    pub(super) fn handle_sarif_key(&mut self, key: KeyEvent) {
        use crate::sarif::semantics::{BaselineState, Level, SuppressionState};
        use crate::sarif::view::{Column, Tab};
        let Some(view) = self.editor.sarif.as_mut() else {
            return;
        };
        if view.editing_query {
            match key.code {
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    let mut q = view.query_text.clone();
                    q.push(c);
                    view.set_query(&q);
                }
                KeyCode::Backspace => {
                    let mut q = view.query_text.clone();
                    q.pop();
                    view.set_query(&q);
                }
                KeyCode::Enter => view.editing_query = false,
                KeyCode::Esc => {
                    view.set_query("");
                    view.editing_query = false;
                }
                KeyCode::Up => view.move_selection(-1),
                KeyCode::Down => view.move_selection(1),
                _ => {}
            }
            return;
        }
        let page = (view.rows_visible as isize).max(1);
        let toggle = |set: &mut std::collections::HashSet<Level>, l: Level| {
            if !set.remove(&l) {
                set.insert(l);
            }
        };
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => view.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => view.move_selection(-1),
            KeyCode::PageDown => view.move_selection(page),
            KeyCode::PageUp => view.move_selection(-page),
            KeyCode::Home => view.select_first(),
            KeyCode::End => view.select_last(),
            KeyCode::Left | KeyCode::Char('h') => view.set_fold(true),
            KeyCode::Right | KeyCode::Char('l') => view.set_fold(false),
            KeyCode::Char('/') => view.editing_query = true,
            KeyCode::Esc => view.set_query(""),
            KeyCode::Tab | KeyCode::BackTab => {
                let order = [Tab::Locations, Tab::Rules, Tab::Logs, Tab::Run];
                let i = order.iter().position(|t| *t == view.tab).unwrap_or(0);
                let step = if key.code == KeyCode::BackTab { 2 } else { 1 };
                view.set_tab(order[(i + step) % order.len()]);
            }
            KeyCode::Char('s') => {
                let order = [
                    Column::Line,
                    Column::Level,
                    Column::Rule,
                    Column::File,
                    Column::Message,
                ];
                let i = order.iter().position(|c| *c == view.sort.0).unwrap_or(0);
                view.sort_by(order[(i + 1) % order.len()]);
                view.sort.1 = true;
            }
            KeyCode::Char('S') => {
                let col = view.sort.0;
                view.sort_by(col);
            }
            KeyCode::Char('c') => view.fold_all(true),
            KeyCode::Char('e') => view.fold_all(false),
            KeyCode::Char('1') => toggle(&mut view.filters.hidden_levels, Level::Error),
            KeyCode::Char('2') => toggle(&mut view.filters.hidden_levels, Level::Warning),
            KeyCode::Char('3') => toggle(&mut view.filters.hidden_levels, Level::Note),
            KeyCode::Char('4') => toggle(&mut view.filters.hidden_levels, Level::None),
            KeyCode::Char('u') => {
                let set = &mut view.filters.hidden_suppressions;
                if !set.remove(&SuppressionState::Suppressed) {
                    set.insert(SuppressionState::Suppressed);
                }
            }
            KeyCode::Char('a') => {
                let set = &mut view.filters.hidden_baselines;
                if !set.remove(&BaselineState::Absent) {
                    set.insert(BaselineState::Absent);
                }
            }
            KeyCode::Char('x') => view.clear_filters(),
            KeyCode::Char('K') => {
                self.status = if view.toggle_non_problem_kinds() {
                    String::from("Hiding pass, not-applicable and informational results")
                } else {
                    String::from("Showing results of every kind")
                };
            }
            KeyCode::Char('W') => {
                self.status = match view.close_added_logs() {
                    0 => String::from("Only the viewer's own log is open"),
                    n => format!("Closed {n} added log{}", if n == 1 { "" } else { "s" }),
                };
            }
            KeyCode::Char('C') => {
                use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
                let current = crate::sarif::view::columns_text(&view.columns);
                self.open_input_prompt(
                    InputPrompt::new(
                        InputPurpose::SarifColumns,
                        String::from("SARIF Columns"),
                        String::from(
                            "rule, level, kind, baseline, suppression, tool, rank (empty for none)",
                        ),
                    )
                    .with_value(current),
                );
            }
            KeyCode::Char('f') => self.apply_sarif_fix(0),
            KeyCode::Char('o') => {
                use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
                self.open_input_prompt(InputPrompt::new(
                    InputPurpose::SarifAddLog,
                    String::from("Open SARIF Log"),
                    String::from("path to a .sarif file to add to this view"),
                ));
            }
            KeyCode::Char('b') => {
                use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
                self.open_input_prompt(InputPrompt::new(
                    InputPurpose::SarifBaseline,
                    String::from("Compare with Baseline"),
                    String::from("path to an earlier .sarif log of the same code"),
                ));
            }
            KeyCode::Char('X') => self.start_dismiss_code_scanning_alert(),
            KeyCode::Char('E') => {
                use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
                self.open_input_prompt(
                    InputPrompt::new(
                        InputPurpose::SarifExport,
                        String::from("Export Results (CSV, or SARIF for a .sarif name)"),
                        String::from("file to write the visible results to"),
                    )
                    .with_value("sarif-results.csv"),
                );
            }
            KeyCode::Delete | KeyCode::Backspace if view.tab == Tab::Logs => {
                match view.selected_log() {
                    Some(i) if view.remove_log(i) => {
                        self.sarif_diag_signature.clear();
                        self.status = String::from("Closed that log");
                    }
                    Some(_) => {
                        self.status = String::from("The last log closes with its tab (Cmd+W)");
                    }
                    None => {}
                }
            }
            KeyCode::Char('d') => view.detail_tab = view.detail_tab.step(true),
            // Scroll the details pane half a page; the renderer clamps.
            KeyCode::Char(']') => {
                view.detail_scroll += (view.rows_visible as usize / 2).max(1);
            }
            KeyCode::Char('[') => {
                view.detail_scroll = view
                    .detail_scroll
                    .saturating_sub((view.rows_visible as usize / 2).max(1));
            }
            KeyCode::Char('D') => view.detail_tab = view.detail_tab.step(false),
            KeyCode::Char('n') | KeyCode::Char('N') => {
                let forward = key.code == KeyCode::Char('n');
                match view.step_nav(forward) {
                    Some(target) => self.open_sarif_loc(&target),
                    None => self.status = String::from("Nothing to step through on this tab"),
                }
            }
            KeyCode::Char('L') => match view.next_link() {
                Some(target) => self.open_sarif_loc(&target),
                None => self.status = String::from("This message has no location links"),
            },
            KeyCode::Char(' ') => view.toggle_preview(),
            KeyCode::Enter => {
                if view.selected_entry().is_some() {
                    self.open_selected_sarif_result();
                } else {
                    view.toggle_fold();
                }
            }
            _ => {}
        }
    }

    /// Open the selected SARIF result's primary location, keeping the viewer
    /// tab: it is pinned first, so the preview open lands in a new tab
    /// instead of replacing the list the user is working through.
    pub(super) fn open_selected_sarif_result(&mut self) {
        use crate::sarif::region::ColumnKind;
        let Some((
            crate::sarif::view::SelectedPlace {
                path,
                line,
                column,
                kind,
                uri,
                region: place,
                embedded,
                remote,
                ..
            },
            archived,
        )) = self.sarif_selected_place_and_origin()
        else {
            self.status = String::from("This result has no location to open");
            return;
        };
        // Not on this machine, but the log carries the file (#577): open a
        // read-only copy, text as text and binary in the hex viewer.
        let path = match (path, embedded) {
            (Some(p), _) => Some(p),
            (None, Some(contents)) => {
                // The copy has no buffer yet to convert in, so UTF-16
                // columns are turned into characters against its own line.
                let column = match (&contents, kind) {
                    (crate::sarif::resolve::Embedded::Text(t), ColumnKind::Utf16CodeUnits) => {
                        utf16_col_to_char_in(t.lines().nth(line as usize).unwrap_or(""), column)
                    }
                    _ => column as usize,
                };
                match self.open_sarif_embedded(&uri, &contents, line as usize, column) {
                    Err(e) => self.status = format!("Could not open the log's copy of {uri}: {e}"),
                    Ok(()) if archived => {
                        self.status = format!(
                            "Opened {uri} from the CodeQL database's source archive (read-only)"
                        );
                    }
                    Ok(()) => {}
                }
                return;
            }
            (None, None) => None,
        };
        let Some(path) = path else {
            // VS Code's Locate…: ask where the file is.
            let name = uri.rsplit('/').next().unwrap_or(&uri).to_string();
            self.sarif_locate_pending = Some((uri.clone(), line + 1, column + 1));
            // The run names the repository it scanned: the file can be
            // downloaded, once its host is trusted.
            if let Some(remote) = remote {
                if crate::sarif::resolve::trusted_hosts().contains(&remote.host) {
                    self.start_sarif_download(&uri, &remote.url);
                    return;
                }
                self.open_input_prompt(
                    crate::widgets::input_prompt::InputPrompt::new(
                        crate::widgets::input_prompt::InputPurpose::SarifTrustHost {
                            uri: uri.clone(),
                            host: remote.host.clone(),
                            url: remote.url,
                        },
                        format!(
                            "Download {name} from {}? Enter trusts {} for SARIF source",
                            remote.host, remote.host
                        ),
                        format!("or type the path to {name} on this machine"),
                    )
                    .allowing_blank(),
                );
                self.status = format!(
                    "{uri} is not on this machine; {} has it at the scanned commit",
                    remote.host
                );
                return;
            }
            self.open_input_prompt(crate::widgets::input_prompt::InputPrompt::new(
                crate::widgets::input_prompt::InputPurpose::SarifLocate { uri: uri.clone() },
                format!("Locate {name}"),
                format!("path to {name} on this machine"),
            ));
            self.status = format!("Cannot find {uri} on this machine: where is it?");
            return;
        };
        // The region read against the file as it is now: an offset-only
        // region gets its line, and a snippet that moved is followed. The
        // text is the open buffer's when the file is open (it may be
        // edited), else the disk's.
        let text = self.sarif_file_text(&path);
        let located = place
            .zip(text)
            .and_then(|((region, context, newlines), text)| {
                crate::sarif::region::locate(&region, context.as_ref(), &text, &newlines, kind)
            });
        self.editor.pin_active();
        let (opened, line, moved) = match located {
            // Columns are code points once located.
            Some((pos, moved)) => (self.open_at(&path, pos.line, pos.col), pos.line, moved),
            None => (
                match kind {
                    ColumnKind::Utf16CodeUnits => {
                        self.open_at_utf16(&path, line as u32, column as u32)
                    }
                    ColumnKind::UnicodeCodePoints => {
                        self.open_at(&path, line as usize, column as usize)
                    }
                },
                line as usize,
                false,
            ),
        };
        self.status = match opened {
            Ok(()) if moved => format!(
                "Opened {}:{} (the code moved since the scan; found by its snippet)",
                path.display(),
                line + 1
            ),
            Ok(()) => format!("Opened {}:{}", path.display(), line + 1),
            Err(e) => format!("Open failed: {e}"),
        };
    }

    /// Download `url`, the source of the SARIF location `uri`, off the UI
    /// thread; [`App::poll_sarif_download`] opens it (#577). The line and
    /// column come from the pending Locate.
    pub(super) fn start_sarif_download(&mut self, uri: &str, url: &str) {
        let (line, col) = self
            .sarif_locate_pending
            .as_ref()
            .filter(|(u, _, _)| u == uri)
            .map_or((1, 1), |(_, l, c)| (*l, *c));
        let (tx, rx) = std::sync::mpsc::channel();
        let fetch = self.sarif_fetch;
        let (uri, url) = (uri.to_string(), url.to_string());
        self.status = format!("Downloading {url}…");
        std::thread::spawn(move || {
            let body = fetch(&url);
            let _ = tx.send(SarifDownload {
                uri,
                line,
                col,
                body,
            });
        });
        self.sarif_download = Some(rx);
    }

    /// Open a finished SARIF source download read-only at its location,
    /// or say why it failed.
    pub(super) fn poll_sarif_download(&mut self) {
        use std::sync::mpsc::TryRecvError;
        let Some(rx) = self.sarif_download.as_ref() else {
            return;
        };
        let done = match rx.try_recv() {
            Ok(done) => done,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                self.sarif_download = None;
                return;
            }
        };
        self.sarif_download = None;
        match done.body {
            Ok(bytes) => {
                if self
                    .sarif_locate_pending
                    .as_ref()
                    .is_some_and(|(u, _, _)| *u == done.uri)
                {
                    self.sarif_locate_pending = None;
                }
                let contents = match String::from_utf8(bytes) {
                    Ok(t) => crate::sarif::resolve::Embedded::Text(t),
                    Err(e) => crate::sarif::resolve::Embedded::Binary(e.into_bytes()),
                };
                let (line, col) = (
                    (done.line.max(1) - 1) as usize,
                    (done.col.max(1) - 1) as usize,
                );
                if let Err(e) = self.open_sarif_embedded(&done.uri, &contents, line, col) {
                    self.status = format!("Could not open the download of {}: {e}", done.uri);
                }
            }
            Err(e) => self.status = format!("Could not download {}: {e}", done.uri),
        }
    }

    /// Where the selected SARIF result points, looked up in the workspace,
    /// the learned prefixes, the open files and the log's folder.
    /// Also whether its file was read from the source archive of the
    /// CodeQL database the log came from (#1369).
    pub(super) fn sarif_selected_place_and_origin(
        &self,
    ) -> Option<(crate::sarif::view::SelectedPlace, bool)> {
        let view = self.editor.sarif.as_ref()?;
        // A CodeQL run's SARIF says `%SRCROOT%` and leaves its meaning to
        // the database (#1369): the folder it was created from.
        let database = self.sarif_codeql_database();
        let mut base_overrides = std::collections::BTreeMap::new();
        if let Some(prefix) = database
            .as_deref()
            .and_then(crate::codeql_db::source_location_prefix)
            .filter(|p| p.is_dir())
        {
            base_overrides.insert(String::from("%SRCROOT%"), prefix);
        }
        let resolver = crate::sarif::resolve::Resolver {
            roots: vec![self.workspace_root().to_path_buf()],
            base_overrides,
            learned: crate::sarif::resolve::saved_prefixes(self.workspace_root()),
            open: self.open_file_paths(),
            ..Default::default()
        };
        let mut place = view.selected_place(resolver, &|p| p.is_file())?;
        // Not on this machine: the database keeps a copy of its source.
        if place.path.is_none()
            && place.embedded.is_none()
            && let Some(text) = database
                .as_deref()
                .and_then(|db| crate::codeql_db::archived_source(db, &place.uri))
        {
            place.embedded = Some(crate::sarif::resolve::Embedded::Text(text));
            return Some((place, true));
        }
        Some((place, false))
    }

    /// The CodeQL database whose query run wrote the SARIF log in the
    /// editor, when it is one (#1369).
    /// The log is the one holding the selected result, which may be a log
    /// added to the viewer rather than the tab's own.
    pub(super) fn sarif_codeql_database(&self) -> Option<PathBuf> {
        let view = self.editor.sarif.as_ref()?;
        let log = &view.logs.get(view.selected_entry()?.log)?.path;
        let history = crate::codeql_query::History::load(&Self::codeql_history_path());
        let entry = history.entries.iter().find(|e| {
            e.output == *log
                || e.output
                    .canonicalize()
                    .is_ok_and(|o| log.canonicalize().is_ok_and(|l| l == o))
        })?;
        Self::codeql_run_database(entry)
    }

    /// `path`'s text as the editor has it when it is open (it may be
    /// edited), else as it is on disk.
    pub(super) fn sarif_file_text(&self, path: &Path) -> Option<String> {
        self.editor
            .iter_tabs()
            .find(|t| t.path.as_deref() == Some(path) && !t.has_non_text_view())
            .map(|t| t.lines.join("\n"))
            .or_else(|| std::fs::read_to_string(path).ok())
    }

    /// Work out Space's source preview for the selected result when it is
    /// on and not yet worked out for this result (#577).
    pub(super) fn refresh_sarif_preview(&mut self) {
        use crate::sarif::resolve::Embedded;
        use crate::sarif::view::{PREVIEW_RADIUS, SourcePreview, source_window};
        let Some(entry) = self.editor.sarif.as_ref().and_then(|v| v.preview_due()) else {
            return;
        };
        let empty = |title: String| SourcePreview {
            entry,
            title,
            first: 1,
            target: 0,
            lines: Vec::new(),
        };
        let preview = match self.sarif_selected_place_and_origin() {
            None => empty(String::from("This result has no location to preview.")),
            Some((p, archived)) => {
                let (title, text) = match (&p.path, &p.embedded) {
                    (Some(path), _) => (path.display().to_string(), self.sarif_file_text(path)),
                    (None, Some(Embedded::Text(t))) if archived => (
                        format!("{} (the database's source archive)", p.uri),
                        Some(t.clone()),
                    ),
                    (None, Some(Embedded::Text(t))) => {
                        (format!("{} (the log's copy)", p.uri), Some(t.clone()))
                    }
                    (None, Some(Embedded::Binary(_))) => (
                        format!("{} is binary; Enter opens it in the hex viewer.", p.uri),
                        None,
                    ),
                    (None, None) => match &p.remote {
                        Some(r) => (
                            format!(
                                "{} is not on this machine; Enter downloads it from {}.",
                                p.uri, r.host
                            ),
                            None,
                        ),
                        None => (
                            format!("{} is not on this machine; Enter to locate it.", p.uri),
                            None,
                        ),
                    },
                };
                match text {
                    None => empty(title),
                    Some(text) => {
                        let line = p
                            .region
                            .as_ref()
                            .and_then(|(region, context, newlines)| {
                                crate::sarif::region::locate(
                                    region,
                                    context.as_ref(),
                                    &text,
                                    newlines,
                                    p.kind,
                                )
                            })
                            .map_or(p.line as usize, |(pos, _)| pos.line);
                        let (first, lines) = source_window(&text, line, PREVIEW_RADIUS);
                        let target = if lines.is_empty() {
                            0
                        } else {
                            (line + 1).min(first + lines.len() - 1)
                        };
                        SourcePreview {
                            entry,
                            title,
                            first,
                            target,
                            lines,
                        }
                    }
                }
            }
        };
        if let Some(view) = self.editor.sarif.as_mut() {
            view.preview = Some(preview);
        }
    }

    /// Every located result of the first open SARIF viewer as
    /// `(tab, entry, file, line, column)`, sorted by file then position.
    /// Paths are worked out without touching the disk (a relative URI
    /// against the workspace root, else the log's folder), so this is cheap
    /// enough to run every tick.
    pub(super) fn sarif_targets(&self) -> Vec<(usize, usize, PathBuf, i64, i64)> {
        let root = self.workspace_root().to_path_buf();
        let Some((tab, view)) = self
            .editor
            .editors
            .iter()
            .enumerate()
            .find_map(|(i, t)| t.sarif.as_ref().map(|v| (i, v)))
        else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (i, e) in view.entries.iter().enumerate() {
            if e.uri.is_empty() || e.line <= 0 {
                continue;
            }
            let Some(p) = crate::sarif::resolve::uri_to_path(&e.uri) else {
                continue;
            };
            let path = if p.is_absolute() {
                p
            } else {
                let in_root = root.join(&p);
                let log_dir = view
                    .logs
                    .get(e.log)
                    .and_then(|l| l.path.parent())
                    .map(|d| d.join(&p));
                match log_dir {
                    Some(d) if !in_root.exists() => d,
                    _ => in_root,
                }
            };
            out.push((tab, i, path, e.line, e.column.max(1)));
        }
        out.sort_by(|a, b| (&a.2, a.3, a.4).cmp(&(&b.2, b.3, b.4)));
        out
    }

    /// List recent code scanning analyses to open one (#577): the current
    /// branch's when it has any, else the repository's.
    pub(super) fn open_code_scanning_picker(&mut self) {
        use crate::sarif::github;
        use crate::widgets::list_picker::{ListPicker, ListPurpose, ListRow};
        let fetch = |app: &Self, git_ref: Option<&str>| {
            app.run_gh(&github::analyses_args(git_ref))
                .and_then(|json| github::parse_analyses(&json))
        };
        let branch = self.source_control.status.branch.clone();
        let on_branch = match &branch {
            Some(b) => match fetch(self, Some(&format!("refs/heads/{b}"))) {
                Ok(a) => a,
                Err(e) => {
                    self.status = format!("Code scanning: {e}");
                    return;
                }
            },
            None => Vec::new(),
        };
        let (analyses, title) = if on_branch.is_empty() {
            match fetch(self, None) {
                Ok(a) => (a, String::from("Open Code Scanning Analysis")),
                Err(e) => {
                    self.status = format!("Code scanning: {e}");
                    return;
                }
            }
        } else {
            let b = branch.unwrap_or_default();
            (on_branch, format!("Open Code Scanning Analysis \u{b7} {b}"))
        };
        let rows = analyses
            .iter()
            .map(|a| ListRow {
                id: a.id.to_string(),
                label: github::label(a),
            })
            .collect();
        self.open_list_picker(
            ListPicker::new(ListPurpose::CodeScanningAnalysis, title, rows),
            "This repository has no code scanning analyses",
        );
    }

    /// The branch code scanning follows (#577): its name, or `HEAD <oid>`
    /// when detached, from the git status the worker keeps current, which
    /// reads refs through git itself (packed refs included).
    pub(super) fn code_scan_branch(&self) -> Option<String> {
        let st = &self.source_control.status;
        if !st.in_repo {
            return None;
        }
        st.branch
            .clone()
            .or_else(|| st.detached_hash.as_ref().map(|h| format!("HEAD {h}")))
    }

    /// Look up `branch`'s code scanning analyses off the UI thread: its own
    /// ref's (the repository's when detached or when it has none), narrowed
    /// to the nearest scanned commit in the local history, one per tool.
    pub(super) fn start_code_scan_lookup(&mut self, branch: String, load: bool) {
        use crate::sarif::github;
        let gh = self.gh_program.clone();
        let root = self.active_workspace_root().to_path_buf();
        let git_ref = (!branch.starts_with("HEAD ")).then(|| format!("refs/heads/{branch}"));
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let gh_json = |git_ref: Option<&str>| -> Result<String, String> {
                let mut cmd = std::process::Command::new(&gh);
                cmd.args(github::analyses_args(git_ref))
                    .current_dir(&root)
                    .stdin(std::process::Stdio::null());
                let out = crate::review_ops::output_retrying_busy(&mut cmd)
                    .map_err(|e| crate::gh::spawn_error(&e, Some(root.as_path())))?;
                if crate::gh::signed_out(out.status) {
                    return Err(String::from(crate::gh::SIGNED_OUT));
                }
                Ok(String::from_utf8_lossy(&out.stdout).into_owned())
            };
            let result = (|| {
                let mut analyses = match git_ref.as_deref() {
                    Some(r) => github::parse_analyses(&gh_json(Some(r))?)?,
                    None => Vec::new(),
                };
                if analyses.is_empty() {
                    analyses = github::parse_analyses(&gh_json(None)?)?;
                }
                let history: Vec<String> = crate::git::branch_history(&root, 200)
                    .into_iter()
                    .map(|c| c.hash)
                    .collect();
                Ok(github::nearest_per_tool(&analyses, &history))
            })();
            let _ = tx.send(result);
        });
        self.code_scan_list = Some(CodeScanList { branch, load, rx });
    }

    /// Notice a checkout of another branch and act on the `code_scanning`
    /// setting, then drain finished lookups and fetches. Returns true when
    /// something on screen changed.
    pub(super) fn poll_code_scanning(&mut self) -> bool {
        use crate::sarif::github::CodeScanningMode;
        let mut changed = false;
        if self.code_scanning != CodeScanningMode::Off
            && self.code_scan_list.is_none()
            && let Some(branch) = self.code_scan_branch()
            && self.code_scan_seen.as_deref() != Some(branch.as_str())
        {
            self.code_scan_seen = Some(branch.clone());
            self.start_code_scan_lookup(branch, self.code_scanning == CodeScanningMode::On);
        }
        if let Some(list) = self.code_scan_list.as_ref() {
            let result = match list.rx.try_recv() {
                Ok(r) => Some(r),
                Err(std::sync::mpsc::TryRecvError::Empty) => None,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    Some(Err(String::from("the lookup stopped without answering")))
                }
            };
            if let Some(result) = result
                && let Some(CodeScanList { branch, load, .. }) = self.code_scan_list.take()
            {
                changed = true;
                let shown = branch.strip_prefix("HEAD ").map_or(branch.clone(), |h| {
                    format!("detached HEAD {}", h.chars().take(8).collect::<String>())
                });
                match result {
                    Ok(chosen) if chosen.is_empty() => {
                        if load {
                            self.status = format!("Code scanning: no analyses for {shown}");
                        }
                    }
                    Ok(chosen) if load => self.fetch_code_scan_analyses(chosen, &shown),
                    Ok(chosen) => {
                        self.status = format!(
                            "Code scanning has {} analys{} for {shown} (SARIF: Load Code Scanning Results for This Branch)",
                            chosen.len(),
                            if chosen.len() == 1 { "is" } else { "es" }
                        );
                    }
                    Err(e) => self.status = format!("Code scanning: {e}"),
                }
            }
        }
        if let Some(rx) = self.code_scan_fetch.as_ref()
            && let Ok(files) = rx.try_recv()
        {
            self.code_scan_fetch = None;
            changed = true;
            let mut opened = 0;
            let mut errors = Vec::new();
            for file in files {
                match file.and_then(|p| self.editor.open(&p).map_err(|e| e.to_string())) {
                    Ok(()) => opened += 1,
                    Err(e) => errors.push(e),
                }
            }
            self.sync_open_file_poll_mtime();
            self.status = match errors.first() {
                None => format!(
                    "Loaded {opened} code scanning analys{}",
                    if opened == 1 { "is" } else { "es" }
                ),
                Some(e) => format!("Loaded {opened} code scanning analyses; {e}"),
            };
        }
        changed
    }

    /// Download `chosen` as SARIF into croft's cache off the UI thread, to
    /// open when they arrive.
    pub(super) fn fetch_code_scan_analyses(
        &mut self,
        chosen: Vec<crate::sarif::github::Analysis>,
        shown: &str,
    ) {
        let gh = self.gh_program.clone();
        let root = self.active_workspace_root().to_path_buf();
        let dir = croft_cache_dir().join("code-scanning");
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let files = chosen
                .iter()
                .map(|a| {
                    let mut cmd = std::process::Command::new(&gh);
                    cmd.args(crate::sarif::github::sarif_args(a.id))
                        .current_dir(&root)
                        .stdin(std::process::Stdio::null());
                    let out = crate::review_ops::output_retrying_busy(&mut cmd)
                        .map_err(|e| crate::gh::spawn_error(&e, Some(root.as_path())))?;
                    if crate::gh::signed_out(out.status) {
                        return Err(String::from(crate::gh::SIGNED_OUT));
                    }
                    if !out.status.success() {
                        return Err(format!("analysis #{} could not be downloaded", a.id));
                    }
                    let path = dir.join(format!("analysis-{}.sarif", a.id));
                    std::fs::create_dir_all(&dir)
                        .and_then(|()| std::fs::write(&path, &out.stdout))
                        .map_err(|e| format!("{}: {e}", path.display()))?;
                    Ok(path)
                })
                .collect();
            let _ = tx.send(files);
        });
        self.code_scan_fetch = Some(rx);
        self.status = format!("Loading code scanning results for {shown}…");
    }

    /// Dismiss the selected result's code scanning alert (#577): the alert
    /// number GitHub stamped on the result, or else the open alert with the
    /// same rule, path and line; then ask why.
    pub(super) fn start_dismiss_code_scanning_alert(&mut self) {
        use crate::sarif::github::{self, DismissReason};
        use crate::widgets::list_picker::{ListPicker, ListPurpose, ListRow};
        let Some(view) = self.editor.sarif.as_ref() else {
            return;
        };
        let Some(e) = view.selected_entry().cloned() else {
            self.status = String::from("Select a result to dismiss its alert");
            return;
        };
        let Some(loaded) = view.logs.get(e.log) else {
            return;
        };
        let log_path = loaded.path.clone();
        let Some(run) = loaded.log.runs.get(e.run) else {
            return;
        };
        let Some(result) = run.results.as_ref().and_then(|r| r.get(e.result)) else {
            return;
        };
        let stamped = github::alert_number(result);
        // The location as the log wrote it: code scanning paths are
        // repository-relative, like the URIs GitHub's own logs carry.
        let path = result
            .locations
            .first()
            .and_then(|l| l.physical_location.as_ref())
            .and_then(|p| p.artifact_location.as_ref())
            .and_then(|a| crate::sarif::resolve::location_parts(run, a))
            .map(|(uri, _)| uri)
            .unwrap_or_default();
        let number = match stamped {
            Some(n) => n,
            None => {
                let alerts = match self
                    .run_gh(&github::alerts_args(None))
                    .and_then(|json| github::parse_alerts(&json))
                {
                    Ok(a) => a,
                    Err(err) => {
                        self.status = format!("Code scanning alerts: {err}");
                        return;
                    }
                };
                match github::match_alert(&alerts, &e.rule_id, &path, e.line) {
                    Some(n) => n,
                    None => {
                        self.status = String::from("No code scanning alert matches this result");
                        return;
                    }
                }
            }
        };
        self.dismiss_target = Some((log_path, e.run, e.result));
        let rows = DismissReason::ALL
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let api = r.api();
                let mut label = api[..1].to_uppercase();
                label.push_str(&api[1..]);
                ListRow {
                    id: format!("{number}:{i}"),
                    label,
                }
            })
            .collect();
        self.open_list_picker(
            ListPicker::new(
                ListPurpose::DismissAlert,
                format!("Dismiss Alert #{number}"),
                rows,
            ),
            "",
        );
    }

    /// Send the dismissal of alert `number` (#577); on success the result
    /// it came from is marked suppressed, which the default filter hides.
    pub(super) fn dismiss_code_scanning_alert(
        &mut self,
        number: u64,
        reason: usize,
        comment: &str,
    ) {
        use crate::sarif::github::{self, DismissReason};
        let reason = DismissReason::ALL
            .get(reason)
            .copied()
            .unwrap_or(DismissReason::FalsePositive);
        if let Err(e) = self.run_gh(&github::dismiss_args(number, reason, comment)) {
            self.status = format!("Alert #{number} was not dismissed: {e}");
            return;
        }
        if let (Some((log_path, run, result)), Some(view)) =
            (self.dismiss_target.take(), self.editor.sarif.as_mut())
        {
            let log = view.logs.iter().position(|l| l.path == log_path);
            for e in &mut view.entries {
                if Some(e.log) == log && e.run == run && e.result == result {
                    e.suppression = crate::sarif::semantics::SuppressionState::Suppressed;
                }
            }
        }
        self.status = format!("Dismissed alert #{number} as {}", reason.api());
    }

    /// Download code scanning analysis `id` as SARIF and open it (#577).
    pub(super) fn open_code_scanning_analysis(&mut self, id: u64) {
        let text = match self.run_gh(&crate::sarif::github::sarif_args(id)) {
            Ok(t) => t,
            Err(e) => {
                self.status = format!("Code scanning analysis #{id}: {e}");
                return;
            }
        };
        let dir = croft_cache_dir().join("code-scanning");
        let path = dir.join(format!("analysis-{id}.sarif"));
        if let Err(e) = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&path, text)) {
            self.status = format!("{}: {e}", path.display());
            return;
        }
        match self.editor.open(&path) {
            Ok(()) => {
                self.sync_open_file_poll_mtime();
                self.focus_pane(Pane::Editor);
                self.status = format!("Opened code scanning analysis #{id}");
            }
            Err(e) => self.status = format!("Code scanning analysis #{id}: {e}"),
        }
    }

    /// Write the SARIF viewer's visible results to a CSV file (#577),
    /// relative to the workspace, creating its folder.
    pub fn submit_sarif_export(&mut self, value: &str) {
        let Some(view) = self.editor.sarif.as_ref() else {
            return;
        };
        let v = value.trim();
        // A `.sarif` name gets SARIF, anything else the CSV (#577).
        let as_sarif = v.to_ascii_lowercase().ends_with(".sarif");
        let count = view.visible().len();
        let csv = if as_sarif {
            match view.export_sarif() {
                Some(text) => text,
                None => {
                    self.status = String::from("Could not reread the log to export it");
                    return;
                }
            }
        } else {
            view.export_csv()
        };
        let path = if std::path::Path::new(v).is_absolute() {
            PathBuf::from(v)
        } else {
            self.workspace_root().join(v)
        };
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&path, csv));
        self.status = match written {
            Ok(()) => format!(
                "Exported {count} result{} to {}",
                if count == 1 { "" } else { "s" },
                path.display()
            ),
            Err(e) => format!("{}: {e}", path.display()),
        };
    }

    /// Compare the open viewer against a baseline log (#577): results the
    /// tool did not mark become new, unchanged or updated, and the
    /// baseline's vanished results are listed as absent.
    pub fn submit_sarif_baseline(&mut self, value: &str) {
        let v = value.trim();
        let path = if std::path::Path::new(v).is_absolute() {
            PathBuf::from(v)
        } else {
            self.workspace_root().join(v)
        };
        let text = match std::fs::read(&path) {
            Ok(b) => String::from_utf8_lossy(&b).into_owned(),
            Err(e) => {
                self.status = format!("{}: {e}", path.display());
                return;
            }
        };
        let log = match crate::sarif::load::parse_log(&text) {
            Ok(l) => l,
            Err(e) => {
                self.status = format!("{} is not a usable baseline: {e}", path.display());
                return;
            }
        };
        let Some(view) = self.editor.sarif.as_mut() else {
            return;
        };
        self.status = if view.set_baseline(&path, log) {
            // The baseline's own results stop publishing.
            self.sarif_diag_signature.clear();
            let count = |s| view.entries.iter().filter(|e| e.baseline == s).count();
            use crate::sarif::semantics::BaselineState as B;
            format!(
                "Against {}: {} new, {} unchanged, {} updated, {} absent",
                path.display(),
                count(B::New),
                count(B::Unchanged),
                count(B::Updated),
                count(B::Absent)
            )
        } else {
            String::from("A log cannot be its own baseline")
        };
    }

    /// Merge another SARIF log into the open viewer (#577).
    pub fn submit_sarif_add_log(&mut self, value: &str) {
        let v = value.trim();
        let path = if std::path::Path::new(v).is_absolute() {
            PathBuf::from(v)
        } else {
            self.workspace_root().join(v)
        };
        let text = match std::fs::read(&path) {
            Ok(b) => String::from_utf8_lossy(&b).into_owned(),
            Err(e) => {
                self.status = format!("{}: {e}", path.display());
                return;
            }
        };
        let log = match crate::sarif::load::parse_log(&text) {
            Ok(l) => l,
            Err(e) => {
                self.status = format!("{} was not added: {e}", path.display());
                return;
            }
        };
        let Some(view) = self.editor.sarif.as_mut() else {
            return;
        };
        self.status = if view.add_log(&path, log) {
            // A new log publishes its results too.
            self.sarif_diag_signature.clear();
            format!("Added {} · {} results", path.display(), view.entries.len())
        } else {
            format!("{} is already open here", path.display())
        };
    }

    /// The Locate prompt's answer (#577): the file must carry the name the
    /// log gave it; its location teaches a prefix that is saved for this
    /// workspace, so the log's other files from the same place resolve too.
    pub fn submit_sarif_locate(&mut self, value: &str) {
        let Some((uri, line, col)) = self.sarif_locate_pending.clone() else {
            return;
        };
        let v = value.trim();
        let typed = match v.strip_prefix("~/") {
            Some(rest) => std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default()
                .join(rest),
            None => PathBuf::from(v),
        };
        let path = if typed.is_absolute() {
            typed
        } else {
            self.workspace_root().join(typed)
        };
        if !path.is_file() {
            self.status = format!("{} is not a file", path.display());
            return;
        }
        let want = uri.rsplit('/').next().unwrap_or("").to_string();
        let want = crate::sarif::resolve::uri_to_path(&want)
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or(want);
        let got = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if want != got {
            self.status = format!("File names must match: \"{want}\" and \"{got}\"");
            return;
        }
        self.sarif_locate_pending = None;
        let root = self.workspace_root().to_path_buf();
        if let Err(e) = crate::sarif::resolve::save_prefix(&root, &uri, &path) {
            self.status = format!("Located, but the mapping was not saved: {e}");
        }
        self.editor.pin_active();
        match self.open_at(
            &path,
            line.saturating_sub(1) as usize,
            col.saturating_sub(1) as usize,
        ) {
            Ok(()) => {
                // The learned prefix may change what resolves: republish.
                self.sarif_diag_signature.clear();
                self.status =
                    format!("Located {got}; other files from the same place will resolve");
            }
            Err(e) => self.status = format!("Open failed: {e}"),
        }
    }

    /// Jump to the next (or previous) SARIF result location from the
    /// cursor (#577), across files, wrapping at the ends.
    pub(super) fn step_sarif_result(&mut self, forward: bool) {
        let targets = self.sarif_targets();
        if targets.is_empty() {
            self.status = String::from("No SARIF results with a location are open");
            return;
        }
        let here = (
            self.editor.path.clone().unwrap_or_default(),
            self.editor.cursor_row as i64 + 1,
            self.editor.cursor_col as i64 + 1,
        );
        let key = |t: &(usize, usize, PathBuf, i64, i64)| (t.2.clone(), t.3, t.4);
        let pick = if forward {
            targets.iter().position(|t| key(t) > here).unwrap_or(0)
        } else {
            targets
                .iter()
                .rposition(|t| key(t) < (here.0.clone(), here.1, here.2))
                .unwrap_or(targets.len() - 1)
        };
        let (tab, entry, path, line, col) = targets[pick].clone();
        let message = self.editor.editors[tab]
            .sarif
            .as_ref()
            .and_then(|v| v.entries.get(entry))
            .map(|e| e.message.clone())
            .unwrap_or_default();
        if let Some(view) = self.editor.editors[tab].sarif.as_mut()
            && let Some(row) = view
                .rows()
                .iter()
                .position(|r| *r == crate::sarif::view::Row::Item { entry })
        {
            view.selected = row;
        }
        if let Err(e) = self.open_at(&path, (line - 1) as usize, (col - 1) as usize) {
            self.status = format!("Could not open {}: {e}", path.display());
            return;
        }
        self.status = format!("SARIF {}/{}: {message}", pick + 1, targets.len());
    }

    /// Select, in an open SARIF viewer, the result on the cursor's line
    /// (#577), the way VS Code's panel follows the editor.
    pub fn sync_sarif_selection_to_cursor(&mut self) {
        let Some(path) = self.editor.path.clone() else {
            return;
        };
        if self.editor.sarif.is_some() {
            return;
        }
        let line = self.editor.cursor_row as i64 + 1;
        let Some((tab, entry)) = self
            .sarif_targets()
            .into_iter()
            .find(|t| t.2 == path && t.3 == line)
            .map(|t| (t.0, t.1))
        else {
            return;
        };
        if let Some(view) = self.editor.editors[tab].sarif.as_mut()
            && let Some(row) = view
                .rows()
                .iter()
                .position(|r| *r == crate::sarif::view::Row::Item { entry })
        {
            view.selected = row;
        }
    }

    /// Apply fix `index` of the selected SARIF result (#577) to the open
    /// buffer of each file it changes (the file on disk when none is open),
    /// as one undoable edit that stays unsaved, then mark the result fixed.
    pub(super) fn apply_sarif_fix(&mut self, index: usize) {
        let root = self.workspace_root().to_path_buf();
        let Some((log_path, run_i, result_i, run, result, what)) =
            self.editor.sarif.as_ref().and_then(|v| {
                let e = v.selected_entry()?;
                let loaded = v.logs.get(e.log)?;
                let run = loaded.log.runs.get(e.run)?.clone();
                let result = run.results.as_ref()?.get(e.result)?.clone();
                let what = crate::sarif::fixes::fix_description(&result, index)?;
                Some((loaded.path.clone(), e.run, e.result, run, result, what))
            })
        else {
            self.status = String::from("This result offers no fix");
            return;
        };
        let mut roots = vec![root];
        if let Some(dir) = log_path.parent() {
            roots.push(dir.to_path_buf());
        }
        let resolver = crate::sarif::resolve::Resolver {
            roots,
            learned: crate::sarif::resolve::saved_prefixes(self.workspace_root()),
            open: self.open_file_paths(),
            ..Default::default()
        };
        let open_text = |p: &std::path::Path| -> Option<String> {
            self.editor
                .iter_tabs()
                .find(|t| t.path.as_deref() == Some(p) && !t.has_non_text_view())
                .map(|t| {
                    let mut s = t.lines.join("\n");
                    s.push('\n');
                    s
                })
        };
        // A file with unsaved edits, such as one an earlier fix from this
        // log went into, or saved since the log, is not what it describes.
        let changed = |p: &std::path::Path| {
            self.editor
                .iter_tabs()
                .any(|t| t.path.as_deref() == Some(p) && t.dirty)
                || crate::sarif::fixes::saved_after(p, &log_path, &run)
        };
        let edits = match crate::sarif::fixes::fix_for(
            &run,
            &result,
            index,
            &resolver,
            &mut |p| open_text(p).or_else(|| std::fs::read_to_string(p).ok()),
            &changed,
        ) {
            Ok(e) => e,
            Err(why) => {
                self.status = format!("Fix not applied: {why}");
                return;
            }
        };
        self.editor.pin_active();
        for edit in &edits {
            if let Err(e) = self.open_at(&edit.path, 0, 0) {
                self.status = format!("Fix not applied: {e}");
                return;
            }
            self.editor
                .replace_all_lines(crate::widgets::editor::split_into_lines(&edit.after));
        }
        let mut store = crate::sarif::view::FixedStore::load();
        store.mark(&log_path, run_i, result_i);
        let _ = store.save();
        for tab in self.editor.editors.iter_mut() {
            if let Some(view) = tab.sarif.as_mut() {
                for e in view.entries.iter_mut() {
                    if view.logs.get(e.log).is_some_and(|l| l.path == log_path)
                        && e.run == run_i
                        && e.result == result_i
                    {
                        e.fixed = true;
                    }
                }
            }
        }
        // Republish without the fixed result.
        self.sarif_diag_signature.clear();
        self.status = format!("Applied \"{what}\" to {} file(s), unsaved", edits.len());
    }

    /// Open the copy of `uri` a SARIF log embeds (#577), written read-only
    /// under croft's cache (named by a hash of the URI and contents, so an
    /// unchanged copy is reused): text at `line`/`column`, binary in the hex
    /// viewer. Saving it is refused by the file's permissions, since the
    /// log, not this copy, is the source.
    pub(super) fn open_sarif_embedded(
        &mut self,
        uri: &str,
        contents: &crate::sarif::resolve::Embedded,
        line: usize,
        column: usize,
    ) -> anyhow::Result<()> {
        use crate::sarif::resolve::Embedded;
        use std::hash::{Hash, Hasher};
        let bytes: &[u8] = match contents {
            Embedded::Text(t) => t.as_bytes(),
            Embedded::Binary(b) => b,
        };
        let mut h = std::collections::hash_map::DefaultHasher::new();
        uri.hash(&mut h);
        bytes.hash(&mut h);
        let name = uri
            .rsplit('/')
            .find(|s| !s.is_empty())
            .map(crate::sarif::resolve::percent_decode)
            .filter(|n| n != "." && n != ".." && !n.contains(['/', '\\']))
            .unwrap_or_else(|| String::from("artifact"));
        let dir = croft_cache_dir()
            .join("sarif-embedded")
            .join(format!("{:016x}", h.finish()));
        let path = dir.join(&name);
        if !path.is_file() {
            std::fs::create_dir_all(&dir)?;
            std::fs::write(&path, bytes)?;
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444))?;
        }
        self.editor.pin_active();
        match contents {
            Embedded::Text(_) => self.open_at(&path, line, column)?,
            Embedded::Binary(_) => {
                self.editor.open_hex(&path)?;
                self.sync_open_file_poll_mtime();
                self.focus_pane(Pane::Editor);
            }
        }
        self.status = format!("Opened the log's own copy of {uri} (read-only)");
        Ok(())
    }

    /// Set the SARIF results list's optional columns from what the user
    /// typed, remember them for every viewer, and apply them to each open
    /// one (#577).
    pub(super) fn submit_sarif_columns(&mut self, value: &str) {
        let cols = match crate::sarif::view::parse_columns(value) {
            Ok(c) => c,
            Err(why) => {
                self.status = why;
                return;
            }
        };
        for tab in self.editor.editors.iter_mut() {
            if let Some(view) = tab.sarif.as_mut() {
                view.columns = cols.clone();
            }
        }
        self.status = match crate::sarif::view::save_columns(&cols) {
            Ok(()) if cols.is_empty() => String::from("SARIF columns: position and message only"),
            Ok(()) => format!(
                "SARIF columns: {}",
                crate::sarif::view::columns_text(&cols).replace(',', ", ")
            ),
            Err(e) => format!("Columns set, but not saved: {e}"),
        };
    }

    /// Draw the selected SARIF result's analysis steps in the files they
    /// lie in (#577): each step with a location is labelled after its line
    /// ("step 2: user input"), in that file's tab and no other. Recomputed
    /// when the selection or the set of open files changes.
    pub fn sync_sarif_step_marks(&mut self) {
        let root = self.workspace_root().to_path_buf();
        let open = self.open_file_paths();
        let selected = self
            .editor
            .editors
            .iter()
            .find_map(|t| t.sarif.as_ref())
            .and_then(|v| v.selected_entry().map(|e| (e.log, e.run, e.result)));
        let signature = (selected, open.clone());
        if self.sarif_step_signature.as_ref() == Some(&signature) {
            return;
        }
        self.sarif_step_signature = Some(signature);
        let learned = crate::sarif::resolve::saved_prefixes(self.workspace_root());
        let mut by_file: std::collections::HashMap<PathBuf, Vec<(usize, String)>> =
            std::collections::HashMap::new();
        if let Some(view) = self
            .editor
            .editors
            .iter_mut()
            .find_map(|t| t.sarif.as_mut())
            && selected.is_some()
        {
            let log_dir = view
                .selected_entry()
                .and_then(|e| view.logs.get(e.log))
                .and_then(|l| l.path.parent().map(Path::to_path_buf));
            let run = view
                .selected_entry()
                .and_then(|e| view.logs.get(e.log)?.log.runs.get(e.run).cloned());
            let steps: Vec<(String, i64, String)> = view
                .details()
                .map(|d| {
                    d.threads
                        .iter()
                        .flat_map(|t| &t.steps)
                        .filter_map(|s| {
                            let loc = s.location.as_ref()?;
                            (!loc.uri.is_empty() && loc.line > 0)
                                .then(|| (loc.uri.clone(), loc.line, s.message.clone()))
                        })
                        .collect()
                })
                .unwrap_or_default();
            if let Some(run) = run {
                let mut roots = vec![root];
                roots.extend(log_dir);
                let resolver = crate::sarif::resolve::Resolver {
                    roots,
                    learned,
                    open,
                    ..Default::default()
                };
                for (n, (uri, line, message)) in steps.into_iter().enumerate() {
                    let artifact = crate::sarif::model::ArtifactLocation {
                        uri: Some(uri),
                        ..Default::default()
                    };
                    let Some(path) = resolver.resolve(&run, &artifact, &|p| p.is_file()) else {
                        continue;
                    };
                    let message: String = message.chars().take(60).collect();
                    let label = if message.is_empty() {
                        format!("step {}", n + 1)
                    } else {
                        format!("step {}: {message}", n + 1)
                    };
                    by_file
                        .entry(path)
                        .or_default()
                        .push(((line - 1) as usize, label));
                }
            }
        }
        for tab in self.editor.editors.iter_mut() {
            let marks = tab
                .path
                .as_ref()
                .and_then(|p| by_file.get(p))
                .cloned()
                .unwrap_or_default();
            let path = (!marks.is_empty()).then(|| tab.path.clone()).flatten();
            tab.set_step_marks(path, marks);
        }
    }

    /// The paths of the files open in editor tabs, for SARIF resolution.
    pub(super) fn open_file_paths(&self) -> Vec<PathBuf> {
        self.editor
            .iter_tabs()
            .filter_map(|t| t.path.clone())
            .collect()
    }

    /// Open a location from the selected result's details (a step, a frame, a
    /// related location), resolving it the way the result's own location is
    /// resolved and keeping the viewer tab.
    pub(super) fn open_sarif_loc(&mut self, target: &crate::sarif::details::LocRef) {
        use crate::sarif::region::{ColumnKind, column_kind};
        if target.uri.is_empty() {
            self.status = format!("{} has no file to open", target.label);
            return;
        }
        let root = self.workspace_root().to_path_buf();
        let mut on_github = None;
        let Some((path, kind)) = self.editor.sarif.as_ref().and_then(|v| {
            let e = v.selected_entry()?;
            let loaded = v.logs.get(e.log)?;
            let run = loaded.log.runs.get(e.run)?;
            on_github = crate::sarif::resolve::github_blob_url(run, &target.uri, target.line);
            let mut roots = vec![root.clone()];
            if let Some(dir) = loaded.path.parent() {
                roots.push(dir.to_path_buf());
            }
            let resolver = crate::sarif::resolve::Resolver {
                roots,
                learned: crate::sarif::resolve::saved_prefixes(self.workspace_root()),
                open: self.open_file_paths(),
                ..Default::default()
            };
            let artifact = crate::sarif::model::ArtifactLocation {
                uri: Some(target.uri.clone()),
                ..Default::default()
            };
            Some((
                resolver.resolve(run, &artifact, &|p| p.is_file()),
                column_kind(run),
            ))
        }) else {
            return;
        };
        let Some(path) = path else {
            // A variant analysis result lives in another repository; its
            // run says which, at which commit.
            match on_github {
                Some(url) => self.open_url_for_user(&url),
                None => self.status = format!("Cannot find {} on this machine", target.uri),
            }
            return;
        };
        let line = target.line.max(1) - 1;
        let column = target.column.max(1) - 1;
        self.editor.pin_active();
        let opened = match kind {
            ColumnKind::Utf16CodeUnits => self.open_at_utf16(&path, line as u32, column as u32),
            ColumnKind::UnicodeCodePoints => self.open_at(&path, line as usize, column as usize),
        };
        self.status = match opened {
            Ok(()) => format!("Opened {}:{}", path.display(), line + 1),
            Err(e) => format!("Open failed: {e}"),
        };
    }
}

/// The selected SARIF result (log, run, result) and the open files that
/// step marks were last drawn for (#577).
pub(super) type SarifStepSignature = (Option<(usize, usize, usize)>, Vec<PathBuf>);

/// The character index in `line` at which `units` UTF-16 code units have
/// passed; past the end it is the line's length.
pub(super) fn utf16_col_to_char_in(line: &str, units: i64) -> usize {
    let mut seen = 0i64;
    line.chars()
        .take_while(|c| {
            seen += c.len_utf16() as i64;
            seen <= units
        })
        .count()
}
