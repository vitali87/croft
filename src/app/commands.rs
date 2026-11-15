//! `run_command`, the Command Palette, the Extensions view, extension commands (MCP
//! tools, viewers) and Show Memory Usage.

use super::*;

impl App {
    /// Pull the one-shot remote-extension-index refresh if it has finished.
    /// Rebuilds the Extensions panel exactly once, when the verified cache
    /// actually changed, then drops the receiver so later calls are no-ops.
    pub fn drain_ext_index_refresh(&mut self) -> bool {
        let Some(rx) = self.ext_index_refresh.as_ref() else {
            return false;
        };
        match rx.try_recv() {
            Ok(updated) => {
                self.ext_index_refresh = None;
                if updated {
                    self.refresh_extensions();
                }
                // A user-triggered refresh reports its outcome; the silent
                // startup refresh does not (it must not clobber other status).
                if std::mem::take(&mut self.ext_index_manual_refresh) {
                    self.status = String::from(if updated {
                        "Extensions index refreshed"
                    } else {
                        "Extensions index already up to date"
                    });
                }
                updated
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.ext_index_refresh = None;
                false
            }
        }
    }

    /// Manually re-pull the signed remote extension index (the panel's ⟳
    /// button). Re-arms the background refresh; the cached entries keep showing
    /// meanwhile, and [`Self::drain_ext_index_refresh`] rebuilds the panel and
    /// reports the outcome when it lands.
    pub(super) fn refresh_extension_index(&mut self) {
        self.ext_index_manual_refresh = true;
        self.ext_index_refresh = Some(crate::mcp::registry_index::spawn_refresh());
        self.status = String::from("Refreshing extensions index\u{2026}");
    }

    /// Rebuild the Extensions panel's row list from the bundled + user manifests,
    /// stamping each with its live enabled state from prefs. Called whenever the
    /// view is shown or a toggle flips, so the list always reflects the manifests
    /// on disk and the current disable set.
    pub(super) fn refresh_extensions(&mut self) {
        // A runner toggled on or off changes which folder Testing follows.
        self.test_root_follow = None;
        let mut vscode: Vec<String> = self
            .roots
            .iter()
            .flat_map(crate::vscode_extensions::workspace_recommendations)
            .collect();
        vscode.extend(self.vscode_listed.iter().cloned());
        let compared = crate::vscode_extensions::compare(vscode);
        let items = build_extension_items(&self.disabled_extensions, &compared);
        self.extensions.set_items(items);
        // The gates follow the manifests on disk too, so CodeQL added or
        // removed by hand (#1842) comes and goes with its row here.
        let had_codeql = self.installed_extensions.contains("codeql");
        let installed = crate::mcp::catalog::installed_ids(&self.extensions_dir());
        let changed = installed != self.installed_extensions;
        self.installed_extensions = installed;
        if self.installed_extensions.contains("codeql") != had_codeql {
            self.codeql_availability_changed();
        }
        if changed {
            self.reload_lsp_extensions();
        }
    }

    /// Hand the language servers the installed extensions and the disable
    /// set as they are now (#1842): one added, removed or toggled serves its
    /// files, or stops serving them, without a relaunch.
    pub(super) fn reload_lsp_extensions(&mut self) {
        if self.lsp.is_none() {
            return;
        }
        let sources = crate::lsp::manifest::read_extension_sources(&self.extensions_dir());
        let sources: Vec<&str> = sources.iter().map(String::as_str).collect();
        let gone = match self.lsp.as_mut() {
            Some(lsp) => lsp.reload_extensions(&sources, &self.disabled_extensions),
            None => return,
        };
        if gone.is_empty() {
            return;
        }
        // The worker withdraws what a server it stops reported only on the
        // documents it held open; what it reported on files never opened
        // would stay in PROBLEMS with nothing left to clear it.
        for by_source in self.lsp_diagnostics.values_mut() {
            by_source
                .retain(|key, _| !gone.contains(&crate::lsp::manager::diagnostics_source(key)));
        }
        self.lsp_diagnostics
            .retain(|_, by_source| !by_source.is_empty());
        self.rebuild_problems();
    }

    /// Extensions: Compare with VS Code (#352). Lists what VS Code has
    /// installed (one `code --list-extensions`, which is why it is a command
    /// and not a refresh), merges it with the workspace's recommendations,
    /// and shows the panel with the FROM VS CODE group filled in.
    pub(super) fn compare_extensions_with_vscode(&mut self) {
        self.vscode_listed = crate::vscode_extensions::installed_via_code();
        self.refresh_extensions();
        self.set_sidebar_view(SidebarView::Extensions);
        let n = self
            .extensions
            .items()
            .iter()
            .filter(|it| it.vscode)
            .count();
        self.status = if self.vscode_listed.is_empty() {
            format!(
                "{n} VS Code extension{} from this workspace's recommendations; `code` was not found on PATH, so the installed list is not included",
                if n == 1 { "" } else { "s" }
            )
        } else {
            format!(
                "{n} VS Code extension{} compared under FROM VS CODE",
                if n == 1 { "" } else { "s" }
            )
        };
    }

    pub(super) fn handle_extensions_key(&mut self, key: KeyEvent) {
        let has_mod = key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER);
        // Keep the filter caret solid right after a key, like the commit box.
        self.poke_cursor();
        match key.code {
            KeyCode::Up => self.extensions.move_up(),
            KeyCode::Down => self.extensions.move_down(),
            KeyCode::PageUp => self.extensions.scroll_up(10),
            KeyCode::PageDown => self.extensions.scroll_down(10),
            // Space / Enter flip the selected extension's toggle switch. Space is
            // reserved for the toggle (per the panel's design), so the filter box
            // takes every other printable character but never a space.
            KeyCode::Char(' ') if !has_mod => self.toggle_selected_extension(),
            KeyCode::Enter => self.toggle_selected_extension(),
            // Delete uninstalls the selected installed extension (distinct from
            // Backspace, which edits the filter). Reaches the app on both iTerm2
            // and Ghostty as a plain key, so no chord forwarder is needed.
            KeyCode::Delete => self.uninstall_selected_extension(),
            KeyCode::Backspace if !self.extensions.filter_is_empty() => {
                self.extensions.pop_filter_char()
            }
            KeyCode::Char(c) if !has_mod => self.extensions.push_filter_char(c),
            // Esc clears an active filter first; a second Esc leaves the view.
            KeyCode::Esc if !self.extensions.clear_filter() => {
                self.set_sidebar_view(SidebarView::Explorer)
            }
            _ => {}
        }
    }

    /// Flip the enabled state of the Extensions panel's selected row: update the
    /// in-memory disabled set, persist it, rebuild the list (so the row's state
    /// label and dim styling refresh), and apply any immediate feature gating
    /// (e.g. closing a now-disabled viewer). Language servers follow too: a
    /// disabled one stops and an enabled or added one starts (#1842).
    pub(super) fn toggle_selected_extension(&mut self) {
        let Some(id) = self.extensions.selected_id().map(str::to_string) else {
            return;
        };
        // A FROM VS CODE row with nothing to install is an answer, not a
        // control: say what croft has for it and stop.
        if let Some(item) = self.extensions.selected_item()
            && item.vscode
            && item.install.is_none()
        {
            self.status = format!("{}: {}", item.id, item.description);
            return;
        }
        // AVAILABLE catalog rows: the toggle "adds" (installs) rather than
        // enables. Materialize the manifest into the user extensions dir, then
        // refresh so it moves into INSTALLED (enabled by default). A FROM VS
        // CODE row installs its croft equivalent, not its own (VS Code) id.
        if self.extensions.selected_available() {
            let id = self
                .extensions
                .selected_install_target()
                .map(str::to_string)
                .unwrap_or(id);
            match crate::mcp::catalog::install_in(&self.extensions_dir(), &id) {
                Ok(_) => {
                    // Adding is asking for it: a disabled state left from
                    // when it was built in (CodeQL, #1842) must not hide it.
                    let saved = if self.disabled_extensions.remove(&id) {
                        crate::prefs::save_disabled_extensions_in(
                            &self.config_dir,
                            &self.disabled_extensions,
                        )
                    } else {
                        Ok(())
                    };
                    self.refresh_extensions();
                    self.status = if id == "codeql" {
                        String::from(
                            "Added 'codeql' — open it from the QL icon in the activity bar",
                        )
                    } else {
                        format!(
                            "Added '{id}' — invoke it from the Command Palette (its server installs on first use)"
                        )
                    };
                    // After the confirmation, so a failed save is what shows.
                    self.report_unsaved_setting(saved);
                }
                Err(e) => self.status = format!("Could not add '{id}': {e}"),
            }
            return;
        }
        let now_enabled = self.disabled_extensions.contains(&id);
        self.status = format!(
            "{} extension '{id}'",
            if now_enabled { "Enabled" } else { "Disabled" }
        );
        self.set_extension_enabled(&id, now_enabled);
    }

    /// Uninstall the selected INSTALLED extension: delete its manifest from the
    /// user extensions dir so it drops back to AVAILABLE on refresh. Restricted
    /// to catalog-sourced ids — croft only removes what it added, never a
    /// hand-dropped user manifest (which it can only disable). Reversible by
    /// re-adding from the catalog with one keystroke, so no confirmation modal.
    pub(super) fn uninstall_selected_extension(&mut self) {
        if !self.extensions.selected_removable() {
            return;
        }
        let Some(id) = self.extensions.selected_id().map(str::to_string) else {
            return;
        };
        if !crate::mcp::catalog::is_removable(&id) {
            self.status =
                format!("'{id}' is your own extension — croft won't delete it; disable it instead");
            return;
        }
        // Confirm before removing: uninstall is destructive and must never fire
        // from a single Delete keypress (or trash click) without asking. Enter in
        // the popup performs it; Esc keeps the extension.
        use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
        self.open_input_prompt(InputPrompt::confirm(
            InputPurpose::ExtensionUninstall { id: id.clone() },
            format!("Uninstall '{id}'?  It returns to Available."),
            "Enter or Y to uninstall · Esc or N to keep",
        ));
    }

    /// Perform the confirmed uninstall (the popup's Enter path). Removes the
    /// catalog/index-installed manifest and refreshes the panel.
    pub(super) fn perform_extension_uninstall(&mut self, id: &str) {
        // Read before the manifest goes: a re-add of a newer version must
        // approve its tools afresh, like its consent below.
        let command_ids = crate::mcp::registry::command_ids_of_in_dir(&self.config_dir, id);
        match crate::mcp::catalog::uninstall_in(&self.extensions_dir(), id) {
            Ok(()) => {
                let mut saved =
                    crate::prefs::forget_mcp_tool_fingerprints_in(&self.config_dir, &command_ids);
                // Drop any stale disabled-state for the removed id so a later
                // re-add starts enabled, matching a fresh install.
                if self.disabled_extensions.remove(id) {
                    saved = saved.and(crate::prefs::save_disabled_extensions_in(
                        &self.config_dir,
                        &self.disabled_extensions,
                    ));
                }
                // And its consent: a re-add is a program the user has not
                // approved this time, so the first-run gate asks again.
                saved = saved.and(self.forget_extension_consent(id));
                self.refresh_extensions();
                self.status = format!("Uninstalled '{id}' — it's back under AVAILABLE to re-add");
                self.report_unsaved_setting(saved);
            }
            Err(e) => self.status = format!("Could not uninstall '{id}': {e}"),
        }
    }

    /// Write a first-run consent for `ext_id` to prefs, after the caller has
    /// put it in the session set both gates read. The session grant holds
    /// either way; a prefs write that fails is said in the status rather
    /// than swallowed, since the user would otherwise be asked again next
    /// launch with no idea why.
    pub(super) fn persist_extension_consent(&mut self, ext_id: &str) {
        if let Err(e) = crate::prefs::save_mcp_consent_in(&self.config_dir, ext_id) {
            self.status = format!("Allowed {ext_id}, but the consent could not be saved: {e}");
        }
    }

    /// Forget `id`'s first-run consent in the session set and in prefs,
    /// returning whether prefs took it.
    pub(super) fn forget_extension_consent(&mut self, id: &str) -> anyhow::Result<()> {
        self.consented_extensions.remove(id);
        crate::prefs::forget_mcp_consent_in(&self.config_dir, id)
    }

    /// The user's added extensions, under the app's config dir.
    pub(super) fn extensions_dir(&self) -> PathBuf {
        self.config_dir.join("extensions")
    }

    /// Whether the extension `id` is installed and not in the disabled set.
    /// The single gate the shipped features consult before activating.
    pub(super) fn is_extension_enabled(&self, id: &str) -> bool {
        self.installed_extensions.contains(id) && !self.disabled_extensions.contains(id)
    }

    /// Developer: Show Memory Usage (#694): what each subsystem holds, as a
    /// tab. Running it again refreshes the same tab.
    pub(super) fn show_memory_usage(&mut self) {
        use memory_report::{BufferMem, ChannelMem, MemoryReport, ServerMem, TerminalMem};
        let live = self.terminals.iter().map(|t| (t, false));
        let closed = self.closed_terminals.iter().map(|c| (&c.term, true));
        let terminals = live
            .chain(closed)
            .map(|(t, closed)| {
                let (scrollback_lines, screen_lines, columns) = t.grid_extent();
                let rewind_bytes = t.rewind().lock().map(|r| r.bytes()).unwrap_or(0);
                TerminalMem {
                    name: t.label().to_string(),
                    closed,
                    rewind_bytes,
                    scrollback_lines,
                    screen_lines,
                    columns,
                }
            })
            .collect();
        let root = self.roots.primary().to_path_buf();
        let buffers = std::iter::once(&self.editor)
            .chain(self.editor_layout.inactive_groups())
            .flat_map(|g| g.editors.iter())
            .map(|e| {
                let (steps, bytes) = e.undo_history_size();
                let name = e.path.as_deref().map_or_else(
                    || String::from("untitled"),
                    |p| p.strip_prefix(&root).unwrap_or(p).display().to_string(),
                );
                BufferMem { name, steps, bytes }
            })
            .collect();
        let channels = crate::output::channel_sizes()
            .into_iter()
            .map(|(name, lines, bytes)| ChannelMem { name, lines, bytes })
            .collect();
        let mut servers: std::collections::BTreeMap<&str, ServerMem> = Default::default();
        for by_server in self.lsp_diagnostics.values() {
            // A server's pushed and pulled entries are one file (#1102).
            let mut counted = std::collections::HashSet::new();
            for (key, diags) in by_server {
                let server = diagnostics_source(key);
                let entry = servers.entry(server).or_insert_with(|| ServerMem {
                    name: server.to_string(),
                    files: 0,
                    diagnostics: 0,
                });
                if counted.insert(server) {
                    entry.files += 1;
                }
                entry.diagnostics += diags.len();
            }
        }
        let report = MemoryReport {
            rss_kb: perf_hud::read_rss_kb(),
            rewind_budget: crate::rewind::budget().total(),
            cell_bytes: std::mem::size_of::<alacritty_terminal::term::cell::Cell>(),
            terminals,
            buffers,
            channels,
            servers: servers.into_values().collect(),
        }
        .format();
        match self
            .editor
            .open_text_buffer(Path::new("Memory Usage"), &report)
        {
            Ok(()) => {
                // Rendered rather than raw (#694): the preview ignores typing,
                // so keys meant for another tab can't land in the report.
                // Highlighting only; no language server hears about the tab.
                // The fresh tab carries the default theme until the next
                // frame's sync, and the preview bakes its colours in now.
                self.editor.theme = self.theme;
                self.editor
                    .set_language(Some(crate::highlight::LangKind::Markdown));
                if self.editor.markdown_preview.is_none() {
                    self.editor.toggle_markdown_preview();
                }
                self.focus_pane(Pane::Editor);
            }
            Err(e) => self.status = format!("Could not open the report: {e}"),
        }
    }

    /// Enable or disable an extension by id, persist the choice, and keep the
    /// UI consistent with it: a disabled CodeQL takes its activity-bar icon
    /// and its open side bar with it.
    pub(super) fn set_extension_enabled(&mut self, id: &str, enabled: bool) {
        if enabled {
            self.disabled_extensions.remove(id);
        } else {
            self.disabled_extensions.insert(id.to_string());
        }
        let mut saved =
            crate::prefs::save_disabled_extensions_in(&self.config_dir, &self.disabled_extensions);
        // Turning an extension off forgets its tools' fingerprints, so
        // turning it back on re-approves a tool whose definition changed.
        if !enabled {
            let ids = crate::mcp::registry::command_ids_of_in_dir(&self.config_dir, id);
            saved = saved.and(crate::prefs::forget_mcp_tool_fingerprints_in(
                &self.config_dir,
                &ids,
            ));
        }
        self.report_unsaved_setting(saved);
        self.refresh_extensions();
        self.reload_lsp_extensions();
        if id == "codeql" {
            self.codeql_availability_changed();
        }
    }

    /// Spinner glyph for an in-flight extension command (frame 0 when idle).
    pub(super) fn mcp_spinner_glyph(&self) -> &'static str {
        match self.mcp_started {
            Some(t) => update_spinner_frame(t.elapsed().as_millis()),
            None => UPDATE_SPINNER_FRAMES[0],
        }
    }

    /// Monotonic phase so the loop redraws each frame while an extension command
    /// runs (`0` when idle, so it never wakes the renderer otherwise).
    pub(super) fn mcp_spinner_phase(&self) -> u128 {
        match self.mcp_started {
            Some(t) => t.elapsed().as_millis() / UPDATE_SPINNER_FRAME_MS,
            None => 0,
        }
    }

    pub fn consume_command_palette_image_clear(&mut self) -> bool {
        self.overlays.command_palette_clear.consume()
    }

    /// Open the Command Palette (Cmd/Ctrl+Shift+P). The palette is built from
    /// the static command registry, so there is nothing to index — it opens
    /// instantly regardless of workspace size.
    pub(super) fn open_command_palette(&mut self) {
        if self.command_palette.is_some() {
            return;
        }
        let mut palette = crate::widgets::command_palette::CommandPalette::new();
        // Inject the commands contributed by enabled MCP sidecar extensions so
        // they're discoverable alongside the built-ins (eager registration; the
        // server is only spawned when one is actually invoked).
        let ext_commands: Vec<crate::widgets::command_palette::ExtensionCommand> =
            crate::mcp::registry::contributed_commands()
                .into_iter()
                .chain(crate::mcp::registry::contributed_viewer_commands_in_dir(
                    &self.config_dir,
                ))
                .map(|c| crate::widgets::command_palette::ExtensionCommand {
                    ext_id: c.ext_id,
                    id: c.id,
                    title: c.title,
                })
                .collect();
        palette.set_extension_commands(ext_commands);
        // VS Code offers Pin Editor on an unpinned tab and Unpin Editor on
        // a pinned one (#852), never the one that would do nothing.
        let mut hidden = vec![if self.editor.is_pinned(self.editor.active_index()) {
            crate::widgets::command_palette::Command::PinEditor
        } else {
            crate::widgets::command_palette::Command::UnpinEditor
        }];
        // rust-analyzer's commands (#1626) only where it serves the file.
        if !self.rust_analyzer_serves_active_file() {
            use crate::widgets::command_palette::Command as C;
            hidden.extend([
                C::RustExpandMacro,
                C::RustParentModule,
                C::RustOpenCargoToml,
                C::RustRebuildProcMacros,
                C::RustReloadWorkspace,
            ]);
        }
        // CodeQL's commands (#1842) only once it is added and enabled.
        if !self.is_extension_enabled("codeql") {
            hidden.extend(
                crate::widgets::command_palette::ALL_COMMANDS
                    .iter()
                    .copied()
                    .filter(|c| c.is_codeql()),
            );
        }
        palette.set_hidden(hidden);
        // The chords as keybindings.json leaves them (#1482): a user's own
        // chord, and no built-in chord a user binding gave to another command.
        palette.hint_overrides = crate::widgets::command_palette::ALL_COMMANDS
            .iter()
            .filter_map(|&cmd| match self.keymap.chord_for(cmd) {
                Some(user) => Some((cmd, crate::keymap::display_chord(&user))),
                None => self
                    .keymap
                    .shadows_builtin(cmd)
                    .then(|| (cmd, String::new())),
            })
            .collect();
        let count = palette.results.len();
        self.command_palette = Some(palette);
        self.overlays.command_palette_clear.request();
        self.status = format!("Command Palette: {count} commands — Esc to close");
    }

    pub(super) fn close_command_palette(&mut self) {
        if self.command_palette.take().is_some() {
            self.overlays.command_palette_clear.request();
            self.overlays.activity.mark_dirty();
            self.overlays.welcome.mark_dirty();
            self.overlays.hero.mark_dirty();
            self.invalidate_editor_image_layouts();
            self.status.clear();
        }
    }

    pub(super) fn handle_command_palette_key(&mut self, key: KeyEvent) {
        let Some(palette) = self.command_palette.as_mut() else {
            return;
        };
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => self.close_command_palette(),
            (KeyCode::Enter, _) => self.run_command_palette_selection(),
            (KeyCode::Up, _) => palette.select_prev(),
            (KeyCode::Down, _) => palette.select_next(),
            (KeyCode::PageUp, _) => {
                for _ in 0..10 {
                    palette.select_prev();
                }
            }
            (KeyCode::PageDown, _) => {
                for _ in 0..10 {
                    palette.select_next();
                }
            }
            (KeyCode::Left, _) => palette.move_cursor_left(),
            (KeyCode::Right, _) => palette.move_cursor_right(),
            (KeyCode::Home, _) => palette.move_cursor_home(),
            (KeyCode::End, _) => palette.move_cursor_end(),
            (KeyCode::Backspace, _) => palette.pop_char(),
            (KeyCode::Delete, _) => palette.delete_char(),
            (KeyCode::Char(c), m) if !m.intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER) => {
                palette.push_char(c);
            }
            _ => {}
        }
    }

    /// Run the highlighted command and dismiss the palette: Enter's action,
    /// shared with a mouse click on a result row.
    pub(super) fn run_command_palette_selection(&mut self) {
        let Some(palette) = self.command_palette.as_ref() else {
            return;
        };
        let item = palette.selected_item();
        self.close_command_palette();
        if let Some(item) = item {
            self.run_palette_item(item);
        }
    }

    pub(super) fn handle_command_palette_mouse(&mut self, m: MouseEvent) {
        let Some(palette) = self.command_palette.as_mut() else {
            return;
        };
        let inside = rect_contains(palette.last_rect, m.column, m.row);
        match m.kind {
            MouseEventKind::ScrollDown => {
                for _ in 0..3 {
                    palette.select_next();
                }
            }
            MouseEventKind::ScrollUp => {
                for _ in 0..3 {
                    palette.select_prev();
                }
            }
            MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Down(MouseButton::Right)
                if !inside =>
            {
                self.close_command_palette();
            }
            // Click a row to run it, the way VS Code's quick-pick behaves.
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(idx) = palette.row_index_at(m.row) {
                    palette.selected = idx;
                    self.run_command_palette_selection();
                }
            }
            _ => {}
        }
    }

    pub(super) fn render_command_palette(&mut self, frame: &mut ratatui::Frame) {
        let theme = self.theme;
        let center = self.quick_input_position == QuickInputPosition::Center;
        // Inline images are baked pixels the scrim cannot dim, so the
        // backdrop stays lit where the terminal draws them.
        let backdrop = !self.inline_images_enabled();
        let Some(palette) = self.command_palette.as_mut() else {
            return;
        };
        let area = frame.area();
        crate::widgets::command_palette::render_command_palette(
            palette,
            area,
            frame.buffer_mut(),
            theme,
            center,
            backdrop,
        );
    }

    /// Open `path` in an extension-contributed viewer (#465): a new terminal
    /// pane running the tool on the file, focused. `{file}` in the declared
    /// args is the file's path. A provisioned tool that is not installed yet
    /// has its managed install kicked off and the user is told to run the
    /// command again once it lands, the way a sidecar's first run behaves.
    pub fn open_in_viewer(
        &mut self,
        viewer: &crate::mcp::registry::ContributedViewer,
        path: &Path,
    ) {
        // The pane spawn takes `String` args, so a name that is not valid
        // UTF-8 could only be handed over mangled, and the tool would open a
        // different path. Refuse with the reason rather than open the wrong
        // file.
        let Some(file) = path.to_str().map(str::to_owned) else {
            self.status = format!(
                "{}: the file's name is not valid UTF-8, so it cannot be passed to the viewer",
                viewer.label
            );
            return;
        };
        // A viewer spawns a program named by a manifest, exactly what the
        // sidecar consent gate exists for, and manifests can arrive from the
        // signed remote index, not only the bundled catalog. The first run of
        // an extension's viewer shows the exact command line and spawns
        // nothing until the user allows it; allowing resumes this open.
        if !self.consented_extensions.contains(&viewer.ext_id) {
            use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
            let spawn_line = std::iter::once(viewer.command.clone())
                .chain(viewer.args.iter().map(|a| a.replace("{file}", &file)))
                .collect::<Vec<_>>()
                .join(" ");
            self.open_input_prompt(InputPrompt::confirm(
                InputPurpose::ViewerConsent {
                    key: viewer.key(),
                    path: path.to_path_buf(),
                },
                format!("Allow {} to run:  {}", viewer.ext_id, spawn_line),
                "Enter or Y to allow · Esc or N to cancel",
            ));
            return;
        }
        let install_name = viewer.install_name();
        let program = match viewer.provision.as_ref() {
            None => viewer.command.clone(),
            Some(provision) => {
                match crate::lsp::install::provisioned_command(&install_name, provision) {
                    Some((command, _extra_paths)) => command,
                    None => {
                        let name: &'static str = crate::lsp::install::static_name(&install_name);
                        let config = crate::lsp::config::ServerConfig {
                            name,
                            command: viewer.command.clone(),
                            args: viewer.args.clone(),
                            language: crate::lsp::config::Language("viewer"),
                            initialization_options: None,
                            provision: Some(provision.clone()),
                        };
                        crate::lsp::install::resolve_managed(&config, provision, true);
                        self.status =
                            format!("Installing {}; run it again once it lands", viewer.label);
                        return;
                    }
                }
            }
        };
        let args: Vec<String> = viewer
            .args
            .iter()
            .map(|a| a.replace("{file}", &file))
            .collect();
        let cwd = path
            .parent()
            .filter(|p| p.is_dir())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.workspace_root().to_path_buf());
        match PtyTerminal::new_running(&program, &args, &cwd) {
            Ok(term) => {
                let slot = (self.active_terminal + 1).min(self.terminals.len());
                self.terminals.insert(slot, term);
                self.active_terminal = slot;
                if !self.show_terminal {
                    self.show_terminal = true;
                }
                self.focus_pane(Pane::Terminal);
                self.status = format!("{}: {}", viewer.label, self.status_path(path));
            }
            Err(e) => {
                self.status = format!("{} failed to start: {e}", viewer.label);
            }
        }
    }

    /// A palette `viewer:<extension id>/<viewer id>` row: open the active
    /// editor file in that viewer, if the file is a kind it handles.
    pub(super) fn run_viewer_command(&mut self, viewer_id: &str) {
        let Some(viewer) = crate::mcp::registry::viewer_by_id_in_dir(&self.config_dir, viewer_id)
        else {
            self.status = format!("Viewer '{viewer_id}' is unavailable");
            return;
        };
        self.open_active_file_in_viewer(&viewer);
    }

    /// The palette row's work once its viewer is known: the active editor
    /// file must exist and be a kind the viewer handles, or the status line
    /// says which it is not.
    pub(super) fn open_active_file_in_viewer(
        &mut self,
        viewer: &crate::mcp::registry::ContributedViewer,
    ) {
        let Some(path) = self.editor.path.clone() else {
            self.status = format!("{}: open a file first", viewer.label);
            return;
        };
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if !viewer.extensions.contains(&ext) {
            self.status = format!(
                "{}: the active file is not one of .{}",
                viewer.label,
                viewer.extensions.join(" / .")
            );
            return;
        }
        self.open_in_viewer(viewer, &path);
    }

    /// Dispatch a chosen palette row: a built-in command runs inline; an
    /// extension-contributed command routes through the MCP sidecar path.
    pub(super) fn run_palette_item(&mut self, item: crate::widgets::command_palette::PaletteItem) {
        use crate::widgets::command_palette::PaletteItem;
        match item {
            PaletteItem::Builtin(cmd) => self.run_command(cmd),
            PaletteItem::Extension(ec) => self.run_extension_command(&ec.id),
        }
    }

    /// Invoke an MCP sidecar command by id. Gated by first-run consent (a popup
    /// showing the exact spawn command); once consented, collects the declared
    /// argument (if any) via an input popup, then spawns the server off-thread.
    pub(super) fn run_extension_command(&mut self, command_id: &str) {
        use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
        // Viewer rows (#465) run a terminal program on the active file; no
        // sidecar, so none of the MCP consent or argument flow below applies.
        if let Some(viewer_id) =
            command_id.strip_prefix(crate::mcp::registry::VIEWER_COMMAND_PREFIX)
        {
            self.run_viewer_command(viewer_id);
            return;
        }
        let Some(resolved) =
            crate::mcp::registry::resolve_command_in_dir(&self.config_dir, command_id)
        else {
            self.status = format!("Extension command '{command_id}' is unavailable");
            return;
        };
        // First-run consent gate: never spawn a sidecar until the user has
        // approved this extension, shown the exact command line croft will run.
        // One set answers this gate and the viewer gate alike: it is loaded
        // from prefs at startup and fed by every grant since, so the two
        // cannot disagree, whatever became of a prefs write.
        if !self.consented_extensions.contains(&resolved.ext_id) {
            let spawn_line = std::iter::once(resolved.server.command.clone())
                .chain(resolved.server.args.iter().cloned())
                .collect::<Vec<_>>()
                .join(" ");
            self.open_input_prompt(InputPrompt::confirm(
                InputPurpose::McpConsent {
                    command_id: command_id.to_string(),
                },
                format!("Allow {} to run:  {}", resolved.ext_id, spawn_line),
                "Enter or Y to allow · Esc or N to cancel",
            ));
            return;
        }
        // Collect the declared argument, if any, then run.
        if let (Some(_), Some(label)) = (resolved.arg.as_ref(), resolved.prompt.as_ref()) {
            self.open_input_prompt(InputPrompt::new(
                InputPurpose::McpArg {
                    command_id: command_id.to_string(),
                },
                resolved.title.clone(),
                label.clone(),
            ));
            return;
        }
        self.spawn_mcp_command(resolved, None);
    }

    /// Spawn the MCP server off the UI thread and run the command's tool; the
    /// result is delivered to [`poll_mcp`](Self::poll_mcp) over a channel and
    /// rendered into a markdown scratch buffer. Refuses to start a second
    /// command while one is in flight.
    pub(super) fn spawn_mcp_command(
        &mut self,
        resolved: crate::mcp::registry::ResolvedCommand,
        arg: Option<String>,
    ) {
        if self.mcp_rx.is_some() {
            self.status = String::from("An extension command is already running");
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        self.mcp_rx = Some(rx);
        // On first use the server isn't provisioned yet; the worker installs it
        // (block-and-wait) before running, which can take a moment. Say so, so
        // the user sees install progress rather than a silent "Running".
        let installing = resolved.server.provision.as_ref().is_some_and(|p| {
            crate::lsp::install::provisioned_command(&resolved.server.id, p).is_none()
        });
        let label = if installing {
            format!(
                "Installing {} (first run, this can take a moment)",
                resolved.title
            )
        } else {
            format!("Running {}", resolved.title)
        };
        // Drive the status-bar spinner badge while the worker runs.
        self.mcp_started = Some(std::time::Instant::now());
        self.mcp_busy_label = Some(label.clone());
        self.status = label.clone();
        let cwd = self.workspace_root().to_path_buf();
        let config_dir = self.config_dir.clone();
        let version = env!("CARGO_PKG_VERSION").to_string();
        let _ = std::thread::Builder::new()
            .name("mcp-cmd".into())
            .spawn(move || {
                let _ = tx.send(run_mcp_command_blocking(
                    &resolved,
                    arg,
                    &cwd,
                    &config_dir,
                    &version,
                ));
            });
    }

    /// Label for a scratch buffer (MCP output, Git Output): anchored under the
    /// workspace root with a filesystem-safe name, so File: Save As… offers to
    /// keep it in the open project rather than in croft's working directory.
    /// Cmd+S never writes it (#951).
    pub(super) fn scratch_buffer_path(&self, name: &str) -> PathBuf {
        let safe = name.replace(['/', '\\'], "-");
        self.workspace_root().join(safe)
    }

    /// Drain a completed MCP command: render its text into a scratch buffer on
    /// success, or surface the error in the status line. Returns whether a
    /// redraw is needed. Called each frame from the main loop.
    pub fn poll_mcp(&mut self) -> bool {
        let Some(rx) = self.mcp_rx.as_ref() else {
            return false;
        };
        let outcome = match rx.try_recv() {
            Ok(o) => o,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.mcp_rx = None;
                self.mcp_started = None;
                self.mcp_busy_label = None;
                return false;
            }
        };
        self.mcp_rx = None;
        self.mcp_started = None;
        self.mcp_busy_label = None;
        match outcome.body {
            Ok(text) => {
                let label = self.scratch_buffer_path(&format!("{}.md", outcome.title));
                if self.editor.open_text_buffer(&label, &text).is_ok() {
                    // An MCP result arrives asynchronously; the user may be
                    // mid-scroll in the Explorer and did not ask for focus to
                    // move, let alone for the sidebar to close (#260).
                    self.without_auto_hide(|app| app.focus_pane(Pane::Editor));
                    self.status = format!(
                        "{}: opened in a new tab — Cmd+S saves it into the project",
                        outcome.title
                    );
                }
            }
            Err(e) => {
                self.status = format!("{}: {e}", outcome.title);
            }
        }
        true
    }

    /// Execute a Command Palette entry. Editor text commands act on the active
    /// editor buffer; view / navigation commands defer to the same methods
    /// their dedicated chords use, so the palette can never drift from the
    /// keyboard surface.
    pub(crate) fn run_command(&mut self, cmd: crate::widgets::command_palette::Command) {
        use crate::widgets::command_palette::Command as Cmd;
        use crate::widgets::editor::CaseTransform;
        // The tab a tab command acts on (#1427): the clicked tab when a
        // tab-strip mouse binding runs it directly, as VS Code's tab commands
        // act on the clicked tab, else the active tab. Taken here so a
        // command this one runs in turn never sees it.
        let tab_target = self
            .tab_click_target
            .take()
            .unwrap_or_else(|| self.editor.active_index());
        // The palette closes before a recording can show what it ran.
        self.screencast.name_command(cmd.title());
        // A keybinding can still name a CodeQL command the palette hides.
        if cmd.is_codeql()
            && let Some(why) = self.codeql_unavailable()
        {
            self.status = String::from(why);
            return;
        }
        match cmd {
            // A preview's placeholder lines are not the file (#916): an edit
            // to them leaves the tab dirty with nothing a save can write.
            Cmd::MoveLineUp | Cmd::MoveLineDown | Cmd::CopyLineUp | Cmd::CopyLineDown
                if self.editor.has_non_text_view() =>
            {
                self.status = format!("{} needs a text file", cmd.title());
            }
            Cmd::MoveLineUp => self.editor.move_lines_up(),
            Cmd::MoveLineDown => self.editor.move_lines_down(),
            Cmd::CopyLineUp => self.editor.duplicate_lines_up(),
            Cmd::CopyLineDown => self.editor.duplicate_lines_down(),
            Cmd::ToggleLineComment => {
                if !self.editor.toggle_line_comment() && !self.editor.has_line_comment_syntax() {
                    self.status = String::from("Toggle Line Comment: no comment for this language");
                }
            }
            Cmd::ToggleBlockComment => {
                if !self.editor.toggle_block_comment() {
                    self.status =
                        String::from("Toggle Block Comment: no block comment for this language");
                }
            }
            Cmd::JoinLines => self.editor.join_lines(),
            Cmd::InsertLineBelow => {
                self.editor.open_line_below();
                self.status = String::from("Opened line below");
            }
            Cmd::InsertLineAbove => {
                self.editor.open_line_above();
                self.status = String::from("Opened line above");
            }
            Cmd::DeleteLine => self.delete_current_line(),
            Cmd::KillToEndOfLine => self.kill_to_end_of_line(),
            Cmd::TransformUpper => self.editor.transform_selection_case(CaseTransform::Upper),
            Cmd::TransformLower => self.editor.transform_selection_case(CaseTransform::Lower),
            Cmd::TransformTitle => self.editor.transform_selection_case(CaseTransform::Title),
            Cmd::TransformSnake => self.editor.transform_selection_case(CaseTransform::Snake),
            Cmd::TransformCamel => self.editor.transform_selection_case(CaseTransform::Camel),
            Cmd::TransformPascal => self.editor.transform_selection_case(CaseTransform::Pascal),
            Cmd::TransformKebab => self.editor.transform_selection_case(CaseTransform::Kebab),
            Cmd::SortLinesAscending => self.editor.sort_lines(true),
            Cmd::SortLinesDescending => self.editor.sort_lines(false),
            Cmd::IncrementNumber => self.bump_number_under_cursor(1),
            Cmd::DecrementNumber => self.bump_number_under_cursor(-1),
            Cmd::TrimTrailingWhitespace => {
                if self.editor.trim_trailing_whitespace() {
                    self.status = String::from("Trimmed trailing whitespace");
                }
            }
            Cmd::EmmetExpandAbbreviation => self.expand_emmet_abbreviation(),
            Cmd::EmmetWrapWithAbbreviation => self.open_emmet_prompt(true),
            Cmd::EmmetUpdateTag => self.open_emmet_prompt(false),
            Cmd::EmmetRemoveTag => self.run_emmet_edit(EmmetEdit::RemoveTag),
            Cmd::EmmetMatchTag => self.run_emmet_edit(EmmetEdit::MatchTag),
            Cmd::EmmetBalanceOutward => self.run_emmet_edit(EmmetEdit::BalanceOutward),
            Cmd::EmmetBalanceInward => self.run_emmet_edit(EmmetEdit::BalanceInward),
            Cmd::ExpandSelection => self.expand_selection(),
            Cmd::ShrinkSelection => self.shrink_selection(),
            Cmd::SwitchHeaderSource => self.switch_header_source(),
            Cmd::ToggleWordWrap => {
                self.editor.toggle_wrap();
                self.status = if self.editor.wrap_enabled() {
                    String::from("Word wrap: on")
                } else {
                    String::from("Word wrap: off")
                };
            }
            Cmd::AddCursorAbove => self.editor.add_cursor_above(),
            Cmd::AddCursorBelow => self.editor.add_cursor_below(),
            Cmd::ColumnSelectUp => self.editor.column_select(-1, 0),
            Cmd::ColumnSelectDown => self.editor.column_select(1, 0),
            Cmd::ColumnSelectLeft => self.editor.column_select(0, -1),
            Cmd::ColumnSelectRight => self.editor.column_select(0, 1),
            Cmd::ColumnSelectPageUp => {
                let page = self.editor.column_select_page();
                self.editor.column_select(-page, 0);
            }
            Cmd::ColumnSelectPageDown => {
                let page = self.editor.column_select_page();
                self.editor.column_select(page, 0);
            }
            Cmd::ToggleColumnSelection => self.toggle_column_selection_mode(),
            Cmd::AddSelectionToNextMatch => self.start_select_next_occurrence(),
            Cmd::SelectAllOccurrences => self.select_all_occurrences(),
            Cmd::AddCursorsToLineEnds => self.add_cursors_to_line_ends(),
            Cmd::ExpandLineSelection => self.expand_line_selection(),
            Cmd::JumpToBracket => {
                if !self.editor.jump_to_matching_bracket() {
                    self.status = String::from("Go to Bracket: no matching bracket");
                }
            }
            Cmd::SelectToBracket => {
                if !self.editor.select_to_matching_bracket() {
                    self.status = String::from("Select to Bracket: no matching bracket");
                }
            }
            Cmd::TransposeCharacters => self.editor.transpose_chars(),
            Cmd::IndentationToSpaces => {
                self.editor.indentation_to_spaces();
                self.status = String::from("Converted indentation to spaces");
            }
            Cmd::IndentationToTabs => {
                self.editor.indentation_to_tabs();
                self.status = String::from("Converted indentation to tabs");
            }
            Cmd::FormatDocument => self.start_format_document(),
            Cmd::FormatSelection => self.format_selection(),
            Cmd::ChangeColorPresentation => self.change_color_presentation(),
            Cmd::ToggleFormatOnType => self.toggle_format_on_type(),
            Cmd::ToggleFormatOnSave => self.toggle_format_on_save(),
            Cmd::ToggleAutoSave => self.toggle_auto_save(),
            Cmd::ToggleAutoSaveOnFocusChange => self.toggle_auto_save_on_focus_change(),
            Cmd::ToggleInlineBlame => self.toggle_inline_blame(),
            Cmd::ToggleLiveRun => self.toggle_live_run(),
            Cmd::ToggleProvenance => self.toggle_provenance(),
            Cmd::DiffToggleGroupBySeat => self.diff_toggle_group_by_seat(),
            Cmd::ToggleIndentGuides => self.toggle_indent_guides(),
            Cmd::ToggleBracketColors => self.toggle_bracket_colors(),
            Cmd::ToggleRenderWhitespace => self.toggle_render_whitespace(),
            Cmd::CycleLineNumbers => self.cycle_line_numbers(),
            Cmd::ToggleInlineValues => self.toggle_inline_values(),
            Cmd::ToggleInlayHints => self.toggle_inlay_hints(),
            Cmd::ToggleMarkdownPreview => self.toggle_markdown_preview(),
            Cmd::OpenMarkdownPreviewToSide => self.open_markdown_preview_to_side(),
            // Cmd+Enter's run from any terminal (#843): the same confirm popup.
            Cmd::RunCodeBlockAtCursor => {
                if !self.run_fence_at_cursor(true) {
                    self.status = String::from(
                        "Run code block: open a Markdown file's source (not its preview) and put the caret in a runnable fence",
                    );
                }
            }
            Cmd::ToggleTerminalTimestamps => self.toggle_terminal_timestamps(),
            Cmd::ToggleLogHighlight => self.toggle_log_highlight(),
            Cmd::CollapseTerminalPane => self.collapse_active_terminal_pane(),
            Cmd::RestoreTerminalPanes => self.restore_all_terminal_panes(),
            Cmd::ToggleSecretRedaction => self.toggle_secret_redaction(),
            Cmd::RevealRedactedSecrets => self.reveal_redacted_secrets(),
            Cmd::SearchFromTerminal => self.search_from_last_terminal_command(),
            Cmd::RestoreSnapshot => self.restore_history_snapshot(),
            Cmd::QuickFix => self.start_code_action(),
            Cmd::ReplaceInFile => self.open_editor_replace(),
            Cmd::ToggleFindInSelection => self.toggle_editor_find_in_selection(),
            // The accept/navigate commands serve both merge flows: the
            // three-way merge editor when its view is up (#253), the
            // in-buffer marker flow otherwise.
            Cmd::MergeAcceptCurrent => {
                if self.editor.merge.is_some() {
                    self.merge_apply(crate::merge_editor::ConflictState::Current);
                } else {
                    self.resolve_merge_at_cursor(crate::merge::Resolution::Current)
                }
            }
            Cmd::MergeAcceptIncoming => {
                if self.editor.merge.is_some() {
                    self.merge_apply(crate::merge_editor::ConflictState::Incoming);
                } else {
                    self.resolve_merge_at_cursor(crate::merge::Resolution::Incoming)
                }
            }
            Cmd::MergeAcceptBoth => {
                if self.editor.merge.is_some() {
                    self.merge_apply(crate::merge_editor::ConflictState::Both);
                } else {
                    self.resolve_merge_at_cursor(crate::merge::Resolution::Both)
                }
            }
            Cmd::MergeAcceptBothReverse => {
                if self.editor.merge.is_some() {
                    self.merge_apply(crate::merge_editor::ConflictState::BothReverse);
                } else {
                    self.status = String::from("Incoming-first combination needs the merge editor");
                }
            }
            Cmd::MergeIgnore => {
                if self.editor.merge.is_some() {
                    self.merge_apply(crate::merge_editor::ConflictState::Base);
                } else {
                    self.status = String::from("Ignore (keep base) needs the merge editor");
                }
            }
            Cmd::MergeAcceptAllCurrent => {
                if self.editor.merge.is_some() {
                    self.merge_accept_all(crate::merge_editor::ConflictState::Current);
                } else {
                    self.resolve_all_merge(crate::merge::Resolution::Current)
                }
            }
            Cmd::MergeAcceptAllIncoming => {
                if self.editor.merge.is_some() {
                    self.merge_accept_all(crate::merge_editor::ConflictState::Incoming);
                } else {
                    self.resolve_all_merge(crate::merge::Resolution::Incoming)
                }
            }
            Cmd::MergeNextConflict => {
                if self.editor.merge.is_some() {
                    self.merge_jump(false);
                } else {
                    self.jump_conflict(false)
                }
            }
            Cmd::GoToNextChange => self.jump_editor_change(true),
            Cmd::GoToNextProblem => self.go_to_problem(true),
            Cmd::GoToPrevProblem => self.go_to_problem(false),
            Cmd::GoToPrevChange => self.jump_editor_change(false),
            Cmd::MergePrevConflict => {
                if self.editor.merge.is_some() {
                    self.merge_jump(true);
                } else {
                    self.jump_conflict(true)
                }
            }
            Cmd::MergeComplete => self.complete_merge(),
            Cmd::MergeOpenEditor => match self.editor.path.clone() {
                Some(p) => {
                    self.open_merge_editor_for(&p);
                }
                None => self.status = String::from("No file in the active tab"),
            },
            Cmd::MergeToggleBase => match self.editor.merge.as_mut() {
                Some(mv) => {
                    mv.show_base = !mv.show_base;
                    self.status = if self.editor.merge.as_ref().is_some_and(|m| m.show_base) {
                        String::from("Base pane shown")
                    } else {
                        String::from("Base pane hidden")
                    };
                }
                None => self.status = String::from("Show Base needs the merge editor"),
            },
            Cmd::DebugAddWatch => self.open_add_watch_prompt(),
            Cmd::PeekDefinition => self.peek_definition_at_cursor(),
            Cmd::PeekReferences => self.peek_references_at_cursor(),
            // The caret-driven LSP actions (#1212), the same calls the F-key
            // chords make. A palette run or a rebound chord on a non-text
            // view says why nothing happened instead of staying silent.
            Cmd::GoToDefinition
            | Cmd::GoToReferences
            | Cmd::GoToDeclaration
            | Cmd::GoToTypeDefinition
            | Cmd::GoToImplementations
            | Cmd::RenameSymbol
            | Cmd::TriggerSuggest
            | Cmd::TriggerParameterHints
            | Cmd::ShowHover
                if self.editor.has_non_text_view() =>
            {
                self.status = format!("{} needs a text file", cmd.title());
            }
            Cmd::GoToDefinition => self.request_definition_at_cursor(),
            Cmd::GoToReferences => self.request_references_at_cursor(),
            Cmd::GoToDeclaration => self.request_declaration_at_cursor(),
            Cmd::GoToTypeDefinition => self.request_type_definition_at_cursor(),
            Cmd::GoToImplementations => self.request_implementation_at_cursor(),
            Cmd::RenameSymbol => self.start_rename_symbol(),
            Cmd::TriggerSuggest => self.trigger_completion(),
            Cmd::TriggerParameterHints => self.trigger_signature_help(),
            Cmd::ShowHover => self.show_hover_at_caret(),
            // Position-carrying commands (#259). They read the click the
            // dispatcher set, and do nothing from the keyboard: invoked from
            // the palette there is no click to act on, and guessing the
            // caret instead would make one command mean two things.
            // All three arms refuse on a non-text view for the reason
            // `trigger_definition_at` does: `buffer_pos_at` maps cells using
            // only the text layout, so on a diff/sheet/hex/image/archive tab
            // it happily returns a position into the stub text side. Acting
            // on it moves the cursor somewhere meaningless and — for the
            // definition arm — fires an LSP request against it. The built-in
            // returns silently; a BOUND command says so instead, because the
            // user asked for it explicitly and silence reads as a broken
            // binding rather than a deliberate refusal.
            Cmd::MouseAddCursorAtClick => match self.last_mouse_pos {
                Some((col, row)) => {
                    if self.editor.has_non_text_view() {
                        self.status = String::from("No cursors in this view");
                    } else if self.editor.add_caret_at_screen(col, row) {
                        // Same as the built-in Alt+click: reset the blink
                        // phase, or a new caret can land mid-blink-off and
                        // read as "nothing happened" for up to a full period.
                        self.poke_cursor();
                    } else {
                        self.status = String::from("No place for a cursor there");
                    }
                }
                None => {
                    self.status = String::from("Mouse: Add Cursor at Click needs a mouse binding")
                }
            },
            Cmd::MouseGoToDefinitionAtClick => match self.last_mouse_pos {
                Some(_) if self.editor.has_non_text_view() => {
                    self.status = String::from("No definitions in this view");
                }
                // The outer `buffer_pos_at` exists ONLY to produce a status
                // when the click resolves to no buffer position: the built-in
                // fails silently there, which is right for a gesture the user
                // did not aim, but a command they bound deliberately should
                // say why nothing happened. `trigger_definition_at` repeats
                // the lookup internally — do not "simplify" this away, or the
                // bound command goes mute on a miss.
                Some((col, row)) => match self.editor.buffer_pos_at(col, row) {
                    // Delegate rather than re-implement: `trigger_definition_at`
                    // follows a server-resolved document link BEFORE asking for
                    // a definition (#254), and a bound gesture meant to replace
                    // the built-in must keep that precedence. Duplicating the
                    // check here would let the two drift on the next change.
                    Some(_) => self.trigger_definition_at(col, row),
                    None => self.status = String::from("Nothing to define there"),
                },
                None => {
                    self.status =
                        String::from("Mouse: Go to Definition at Click needs a mouse binding")
                }
            },
            Cmd::MouseOpenLinkAtClick => match self.last_mouse_pos {
                Some((col, row)) => {
                    // `editor` is the DEFAULT `when` region, so the most
                    // natural binding for this command lands there. Both
                    // terminal helpers hit-test the TERMINAL's `last_inner`
                    // and return false anywhere else, so consulting only them
                    // made the command a permanent no-op in its own default
                    // region -- reporting "No link there" over a link that is.
                    // The refusal belongs to the EDITOR lookup only. Its
                    // reason — `buffer_pos_at` maps cells from the TEXT
                    // layout, so a diff/sheet/hex/log view still yields a
                    // position and we would read links from a buffer the user
                    // is not looking at — says nothing about the terminal
                    // helpers below, which read `self.terminal()` alone. The
                    // built-in this command replaces guards neither, so a
                    // guard spanning both made the bound command diverge from
                    // it in the opposite direction to the one being fixed.
                    // Bound once: the refusal and the status it explains must
                    // answer the SAME question, or the status describes a
                    // lookup that never ran.
                    let editor_blocked = self.editor.has_non_text_view();
                    let followed = !editor_blocked
                        && self
                            .editor
                            .buffer_pos_at(col, row)
                            .and_then(|(line, c)| {
                                self.editor.document_link_at(line, c).map(str::to_string)
                            })
                            .inspect(|target| self.open_document_link(target))
                            .is_some();
                    if !followed
                        && !self.terminal_url_click(col, row)
                        && !self.terminal_file_click(col, row)
                    {
                        // Name the view only when the EDITOR lookup is the
                        // one that missed. A click in a terminal pane never
                        // consulted the editor — `editor_blocked` short-
                        // circuited it above — so reporting the editor's view
                        // there names something that was never searched.
                        // "a non-text view is up" and "the editor lookup is
                        // the one that missed" are different questions.
                        let in_terminal = self.terminal_at_pos(col, row).is_some();
                        self.status = String::from(if editor_blocked && !in_terminal {
                            "No links in this view"
                        } else {
                            "No link there"
                        });
                    }
                }
                None => {
                    self.status = String::from("Mouse: Open Link at Click needs a mouse binding")
                }
            },
            Cmd::ClearBuildDiagnostics => self.clear_build_diagnostics(),
            Cmd::DebugClearWatch => {
                let n = self.watch_exprs.len();
                self.watch_exprs.clear();
                self.watch_vals.clear();
                self.watch_prev.clear();
                self.status = format!(
                    "Removed {n} watch expression{}",
                    if n == 1 { "" } else { "s" }
                );
            }
            Cmd::StageHunk => self.stage_hunk_at_caret(),
            Cmd::UnstageHunk => self.unstage_hunk_at_caret(),
            Cmd::RevertHunk => self.request_revert_hunk_at_caret(),
            Cmd::GitViewStash => {
                self.open_stash_picker(crate::widgets::list_picker::ListPurpose::StashView)
            }
            Cmd::GitUndoLastCommit => {
                self.dispatch_scm_action(crate::widgets::scm_menu::ScmAction::UndoLastCommit)
            }
            Cmd::RustExpandMacro => {
                self.request_rust_analyzer(crate::lsp::rust_analyzer::RaRequest::ExpandMacro)
            }
            Cmd::RustParentModule => {
                self.request_rust_analyzer(crate::lsp::rust_analyzer::RaRequest::ParentModule)
            }
            Cmd::RustOpenCargoToml => {
                self.request_rust_analyzer(crate::lsp::rust_analyzer::RaRequest::OpenCargoToml)
            }
            Cmd::RustRebuildProcMacros => {
                self.request_rust_analyzer(crate::lsp::rust_analyzer::RaRequest::RebuildProcMacros)
            }
            Cmd::RustReloadWorkspace => {
                self.request_rust_analyzer(crate::lsp::rust_analyzer::RaRequest::ReloadWorkspace)
            }
            Cmd::GitCopyPermalink => self.share_link(ShareLink::CopyPermalink),
            Cmd::GitCopyHeadLink => self.share_link(ShareLink::CopyHeadLink),
            Cmd::GitOpenOnRemote => self.share_link(ShareLink::OpenOnRemote),
            Cmd::GitOpenLineCommit => self.open_commit_for_line(),
            Cmd::GitCopyLineCommitHash => {
                if let Some((hash, short)) = self.caret_line_commit() {
                    self.run_commit_action(CommitAction::CopyHash, &hash, &short);
                }
            }
            Cmd::ToggleFold => {
                let row = self.editor.cursor_row;
                self.editor.toggle_fold(row);
            }
            Cmd::FoldAll => self.editor.fold_all(),
            Cmd::UnfoldAll => self.editor.unfold_all(),
            Cmd::FoldAllComments => self
                .editor
                .fold_all_of_kind(crate::lsp::manager::FoldRangeKind::Comment),
            Cmd::FoldAllRegions => self
                .editor
                .fold_all_of_kind(crate::lsp::manager::FoldRangeKind::Region),
            Cmd::UnfoldAllRegions => self
                .editor
                .unfold_all_of_kind(crate::lsp::manager::FoldRangeKind::Region),
            Cmd::FoldLevel1 => self.editor.fold_level(1),
            Cmd::FoldLevel2 => self.editor.fold_level(2),
            Cmd::FoldLevel3 => self.editor.fold_level(3),
            Cmd::FoldLevel4 => self.editor.fold_level(4),
            Cmd::FoldLevel5 => self.editor.fold_level(5),
            Cmd::FoldLevel6 => self.editor.fold_level(6),
            Cmd::FoldLevel7 => self.editor.fold_level(7),
            Cmd::FoldRecursively => {
                let row = self.editor.cursor_row;
                self.editor.fold_recursively(row);
            }
            Cmd::UnfoldRecursively => {
                let row = self.editor.cursor_row;
                self.editor.unfold_recursively(row);
            }
            Cmd::TrimFinalNewlines => {
                if self.editor.trim_final_newlines() {
                    self.status = String::from("Trimmed final newlines");
                }
            }
            Cmd::ToggleOverviewRuler => {
                self.editor.overview_ruler = !self.editor.overview_ruler;
                self.status = if self.editor.overview_ruler {
                    String::from("Overview ruler on")
                } else {
                    String::from("Overview ruler off")
                };
            }
            Cmd::ToggleBookmark => self.toggle_bookmark(),
            Cmd::NextBookmark => self.goto_bookmark(true),
            Cmd::PreviousBookmark => self.goto_bookmark(false),
            Cmd::ClearBookmarks => self.clear_bookmarks(),
            Cmd::ViewBookmarks => self.open_bookmark_picker(),
            Cmd::SaveFile => self.save(),
            Cmd::SaveAs => self.open_save_as_prompt(),
            Cmd::SaveWithSudo => self.save_with_sudo(),
            Cmd::RevertFile => self.revert_file(),
            Cmd::SaveAll => self.save_all(),
            Cmd::NewUntitledFile => self.new_untitled_file(),
            Cmd::NewFile => {
                let target = self.palette_create_target_dir();
                self.open_create_prompt(CreateKind::File, target);
            }
            Cmd::NewFolder => {
                let target = self.palette_create_target_dir();
                self.open_create_prompt(CreateKind::Folder, target);
            }
            Cmd::Undo => {
                self.status = if self.editor.undo() {
                    String::from("Undo")
                } else {
                    String::from("Nothing to undo")
                };
            }
            Cmd::Redo => {
                self.status = if self.editor.redo() {
                    String::from("Redo")
                } else {
                    String::from("Nothing to redo")
                };
            }
            // The keyboard route on Linux, where Ctrl+A is line start and a
            // terminal without Super forwarding never delivers Cmd+A (#852).
            Cmd::SelectAll => {
                self.focus_pane(Pane::Editor);
                self.editor.select_all();
                self.status = format!(
                    "Selected {} chars",
                    self.editor.selection_text().chars().count()
                );
            }
            Cmd::CloseEditor => self.request_close_tab(tab_target),
            Cmd::ReopenClosedEditor => self.reopen_closed_tab(),
            // The Explorer's Cmd+Z (#843), which has no `Ctrl` form.
            Cmd::ZoxideJump => self.open_zoxide_jump(),
            // The tab menu's rows (#852), on the active tab and through the
            // same methods its clicks and the Cmd+K chords use.
            Cmd::CloseOtherEditors => self.close_other_tabs(tab_target),
            Cmd::CloseEditorsToTheRight => self.close_tabs_to_right(tab_target),
            Cmd::CloseSavedEditors => self.close_saved_tabs(),
            Cmd::CloseAllEditors => self.close_all_tabs(),
            Cmd::PinEditor => self.set_active_tab_pinned(true),
            Cmd::UnpinEditor => self.set_active_tab_pinned(false),
            Cmd::KeepEditor => self.keep_tab_open(tab_target),
            Cmd::MoveEditorLeftInGroup => self.move_active_tab(false),
            Cmd::MoveEditorRightInGroup => self.move_active_tab(true),
            Cmd::MoveEditorToNextGroup => self.move_active_editor_to_adjacent_group(true),
            Cmd::MoveEditorToPreviousGroup => self.move_active_editor_to_adjacent_group(false),
            Cmd::SplitEditor => self.split_editor(),
            // The chords' own moves (#843): Cmd+Opt+Left / Right have no
            // `Ctrl` form off macOS.
            Cmd::FocusLeftEditorGroup => self.focus_editor_group(true),
            Cmd::FocusRightEditorGroup => self.focus_editor_group(false),
            Cmd::QuickOpen => self.open_file_finder(),
            Cmd::TakeTheTour => self.start_demo(),
            Cmd::SponsorCroft => self.open_sponsor_page(),
            Cmd::SarifNextResult => self.step_sarif_result(true),
            Cmd::SarifPreviousResult => self.step_sarif_result(false),
            Cmd::SarifOpenCodeScanning => self.open_code_scanning_picker(),
            Cmd::SarifLoadCodeScanning => match self.code_scan_branch() {
                Some(branch) => self.start_code_scan_lookup(branch, true),
                None => self.status = String::from("Code scanning: not in a git repository"),
            },
            Cmd::GoToSymbol => self.open_go_to_symbol(),
            Cmd::GoToWorkspaceSymbol => self.open_workspace_symbols(""),
            Cmd::NavigateBack => self.nav_back(),
            Cmd::NavigateForward => self.nav_forward(),
            Cmd::GoToLastEditLocation => self.goto_last_edit_location(),
            Cmd::ToggleVimMode => self.toggle_vim_mode(),
            Cmd::MacroStartStopRecording if self.macro_replaying => {}
            Cmd::MacroStartStopRecording => {
                if self.macro_recording.is_some() {
                    self.stop_macro_recording();
                } else {
                    self.start_macro_recording(None);
                }
            }
            Cmd::MacroReplayLast if self.macro_replaying => {}
            Cmd::MacroReplayLast => self.replay_last_macro(1),
            // Guarded like its siblings: `macro_replay_times` is a bindable
            // id, so a replayed key reaches it. A prompt opening mid-replay
            // swallows the macro's remaining keys into the count field, and
            // `input_prompt` is checked ABOVE the macro tap, so a later
            // Enter would submit the count and re-enter replay.
            Cmd::MacroReplayTimes if self.macro_replaying => {}
            Cmd::MacroReplayTimes => {
                use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
                self.open_input_prompt(InputPrompt::new(
                    InputPurpose::MacroReplayCount,
                    "Replay Macro N Times",
                    "count",
                ));
            }
            Cmd::ShowExplorer => self.set_sidebar_view(SidebarView::Explorer),
            Cmd::FocusBreadcrumbs => self.focus_breadcrumbs(),
            Cmd::ShowSearch => self.set_sidebar_view(SidebarView::Search),
            Cmd::ReplaceInFiles => self.open_replace_in_files(),
            // Run from the palette the side bar may be hidden, so it is
            // revealed first: a toggle flipped out of sight is a toggle the
            // user cannot confirm.
            Cmd::SearchToggleMatchCase => {
                self.set_sidebar_view(SidebarView::Search);
                self.toggle_search_opt(crate::widgets::search::SearchToggle::CaseSensitive);
            }
            Cmd::SearchToggleWholeWord => {
                self.set_sidebar_view(SidebarView::Search);
                self.toggle_search_opt(crate::widgets::search::SearchToggle::WholeWord);
            }
            Cmd::SearchToggleRegex => {
                self.set_sidebar_view(SidebarView::Search);
                self.toggle_search_opt(crate::widgets::search::SearchToggle::UseRegex);
            }
            Cmd::SearchTogglePreserveCase => {
                self.set_sidebar_view(SidebarView::Search);
                self.toggle_search_opt(crate::widgets::search::SearchToggle::PreserveCase);
            }
            Cmd::SearchToggleDetails => {
                self.set_sidebar_view(SidebarView::Search);
                self.search.toggle_details();
            }
            Cmd::ShowSourceControl => self.set_sidebar_view(SidebarView::SourceControl),
            Cmd::AddWorkspaceFolder => self.open_add_folder_picker(),
            Cmd::SaveWorkspaceAs => {
                use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
                let suggestion = {
                    let name = self
                        .workspace_root()
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| String::from("workspace"));
                    self.workspace_root()
                        .join(format!("{name}.code-workspace"))
                        .display()
                        .to_string()
                };
                self.open_input_prompt(
                    InputPrompt::new(
                        InputPurpose::SaveWorkspaceAs,
                        String::from("Save Workspace As"),
                        "Enter to save · Esc to cancel",
                    )
                    .with_value(&suggestion),
                );
            }
            Cmd::OpenWorkspaceFromFile => match self.editor.path.clone() {
                Some(p) if crate::workspace::is_workspace_file(&p) => self.open_workspace_file(&p),
                _ => {
                    self.status = String::from("Open a .code-workspace file in the editor first");
                }
            },
            Cmd::ReopenAsHex => match self.editor.path.clone() {
                // A dirty buffer's edits exist only in memory; the hex
                // view reads DISK, so switching would silently show (and
                // strand) stale bytes.
                Some(_) if self.editor.dirty => {
                    self.status = String::from("Save or revert the buffer before reopening as hex");
                }
                Some(p) => {
                    // The explicit hex choice supersedes a lingering
                    // Reopen-as-Text override, and a Reopen-as-Preview
                    // log view's (#1537), which a reload would restore.
                    self.editor.force_text = false;
                    self.editor.force_log = false;
                    if let Err(e) = self.editor.open_hex(&p) {
                        self.status = format!("Reopen as hex: {e}");
                    }
                }
                None => self.status = String::from("No file in the active tab"),
            },
            Cmd::NotebookRunAll => {
                let text = self.editor.lines.join("\n");
                if self
                    .editor
                    .markdown_preview
                    .as_ref()
                    .is_some_and(|m| m.notebook)
                {
                    self.run_notebook_cells(crate::notebook_kernel::code_cells(&text));
                } else {
                    self.status = String::from("Run All works on a notebook's rendered view");
                }
            }
            Cmd::NotebookInterrupt => {
                self.notebook_kernel_request(crate::notebook_kernel::Request::Interrupt)
            }
            Cmd::NotebookRestart => {
                self.notebook_kernel_request(crate::notebook_kernel::Request::Restart)
            }
            Cmd::NotebookSelectKernel => self.select_notebook_kernel(),
            Cmd::RestartLanguageServers => self.restart_language_servers(),
            Cmd::RestartLanguageServersForFile => self.restart_language_servers_for_file(),
            Cmd::TestingRunWithCoverage => self.run_all_tests_with_coverage(),
            Cmd::CoverageClear => self.clear_coverage(),
            Cmd::TestingInstallCoverageTool => self.install_coverage_tool(),
            Cmd::TestingShowCoverageReport => self.show_coverage_report(),
            Cmd::DeveloperShowMemoryUsage => self.show_memory_usage(),
            Cmd::ToggleScreencastMode => self.toggle_screencast_mode(),
            Cmd::RunTestAtCursorWithCoverage => self.run_test_at_cursor_with_coverage(),
            Cmd::TestingToggleWatchAll => {
                self.toggle_test_watch(crate::testing::watch::WatchScope::All)
            }
            Cmd::TestingGoToFirstFailure => self.go_to_first_failed_test(),
            Cmd::TestingCancelRun => self.cancel_test_run(),
            Cmd::ReopenAsText => match self.editor.path.clone() {
                // Merge editor (#253): back to the in-buffer marker flow.
                // The Result buffer is deliberately discarded — it was
                // never saved, and the marker file on disk is the source
                // of truth the old flow works on.
                Some(p) if self.editor.merge.is_some() => {
                    self.editor.merge = None;
                    self.editor.dirty = false;
                    self.editor.force_text = true;
                    match self.editor.open(&p) {
                        Ok(()) => {
                            let n = self.editor.conflicts().len();
                            self.status = format!(
                                "Marker flow: {n} conflict{} — F7 jumps, Cmd+. accepts",
                                if n == 1 { "" } else { "s" }
                            );
                        }
                        Err(e) => self.status = format!("Reopen as text: {e}"),
                    }
                }
                // A rendered notebook/markdown preview counts (#180): the
                // command lands in the raw source.
                Some(_)
                    if !self.editor.has_non_text_view()
                        && self.editor.markdown_preview.is_none() =>
                {
                    self.status = String::from("The active tab is already a text view");
                }
                // A dirty hex tab holds unwritten byte overwrites; the
                // text route would silently drop them (#173).
                Some(_) if self.editor.dirty => {
                    self.status =
                        String::from("Save or revert the pending edits before reopening as text");
                }
                Some(p) => {
                    // Per-tab override (#175): skip every preview route —
                    // an SVG lands in its XML source, a hex tab in the
                    // text heuristic (whose binary verdict routes back to
                    // hex, reported honestly). Sticks across FS-sync
                    // reloads; cleared by opening a different file.
                    self.editor.force_text = true;
                    // A Markdown preview is a view of the buffer that a
                    // same-path reload keeps (#1605), not a route `open`
                    // takes, so it is put down here.
                    self.editor.markdown_preview = None;
                    match self.editor.open(&p) {
                        Ok(()) if self.editor.hex.is_some() => {
                            self.status =
                                String::from("Still binary by content - staying in the hex view");
                        }
                        Ok(()) => {}
                        Err(e) => self.status = format!("Reopen as text: {e}"),
                    }
                }
                None => self.status = String::from("No file in the active tab"),
            },
            Cmd::HexFindNext => self.hex_find_next(),
            Cmd::SheetInsertRowBelow
            | Cmd::SheetDeleteRow
            | Cmd::SheetInsertColRight
            | Cmd::SheetDeleteCol => self.sheet_structure_op(cmd),
            Cmd::MediaOpenExternal => match self.editor.path.clone() {
                Some(p)
                    if self
                        .editor
                        .markdown_preview
                        .as_ref()
                        .is_some_and(|md| md.media) =>
                {
                    // A remote session has no display to hand the file
                    // to (#202 review): the opener would run on the
                    // headless host and claim success.
                    if is_remote_session() {
                        self.status = format!(
                            "Running remotely: copy {} to your machine to play it",
                            p.display()
                        );
                    } else {
                        // The OS opener takes a filesystem path directly.
                        self.status = match open_url(&p.display().to_string()) {
                            Ok(()) => format!("Handed {} to the system player", p.display()),
                            Err(e) => format!("System player failed: {e}"),
                        };
                    }
                }
                _ => {
                    self.status = String::from("The active tab is not a media info card");
                }
            },
            Cmd::ReopenAsPreview => match self.editor.path.clone() {
                // Markdown's preview is a toggle on the open tab, not a
                // routing decision `open` makes (#1363): turn it on, as
                // Markdown: Toggle Preview does, keeping the caret, undo
                // history and any unsaved edits.
                Some(_)
                    if self.editor.language() == Some(crate::highlight::LangKind::Markdown)
                        && !self.editor.has_non_text_view() =>
                {
                    if self.editor.markdown_preview.is_none() {
                        self.toggle_markdown_preview();
                    } else {
                        self.status = String::from("Markdown Preview");
                    }
                }
                Some(_) if self.editor.dirty => {
                    self.status =
                        String::from("Save or revert the buffer before reopening the preview");
                }
                Some(p) => {
                    // Undo a Reopen-as-Text: normal routing decides the
                    // view again (SVG raster, image, sheet, hex).
                    self.editor.force_text = false;
                    self.editor.force_log = false;
                    match self.editor.open(&p) {
                        // Plain text with no grammar (#1537): a `.txt` or
                        // `.out` capture, or a `.log`, gets the rendered
                        // log view's colours instead of a refusal. The
                        // log view reads raw bytes as UTF-8, so a UTF-16 or
                        // legacy-encoded buffer keeps its decoded text.
                        Ok(())
                            if !self.editor.has_non_text_view()
                                && self.editor.markdown_preview.is_none()
                                && self.editor.language().is_none()
                                && self.editor.encoding == encoding_rs::UTF_8 =>
                        {
                            self.editor.force_log = true;
                            if let Err(e) = self.editor.open(&p) {
                                self.status = format!("Reopen as preview: {e}");
                            } else if self.editor.log.is_none() {
                                self.editor.force_log = false;
                                self.status =
                                    String::from("No preview for this file type; showing the text");
                            }
                        }
                        Ok(())
                            if !self.editor.has_non_text_view()
                                && self.editor.markdown_preview.is_none() =>
                        {
                            self.status =
                                String::from("No preview for this file type; showing the text");
                        }
                        Ok(()) => {}
                        Err(e) => self.status = format!("Reopen as preview: {e}"),
                    }
                }
                None => self.status = String::from("No file in the active tab"),
            },
            Cmd::RemoveWorkspaceFolder => {
                // Operates on the Explorer's selected row when it is a
                // SECONDARY root section; anything else gets the hint.
                let target = self
                    .tree
                    .nodes
                    .get(self.tree.selected)
                    .filter(|n| n.depth == 0 && n.path != self.tree.root)
                    .map(|n| n.path.clone());
                match target {
                    Some(p) => self.remove_workspace_folder(p),
                    None => {
                        self.status =
                            String::from("Select a secondary root folder in the Explorer first");
                    }
                }
            }
            Cmd::ShowRunDebug => self.set_sidebar_view(SidebarView::RunDebug),
            Cmd::ShowRemote => self.set_sidebar_view(SidebarView::Remote),
            Cmd::ShowExtensions => self.set_sidebar_view(SidebarView::Extensions),
            Cmd::CompareExtensionsWithVscode => self.compare_extensions_with_vscode(),
            Cmd::ShowTesting => self.open_testing_view(),
            Cmd::CodeqlRunQuery => self.run_codeql_query(),
            Cmd::CodeqlRemoveDatabase => {
                if let Some(i) = self.current_codeql_database() {
                    self.confirm_remove_codeql_database(i);
                }
            }
            Cmd::CodeqlRenameDatabase => {
                if let Some(i) = self.current_codeql_database() {
                    self.prompt_rename_codeql_database(i);
                }
            }
            Cmd::CodeqlSortDatabases => self.sort_codeql_databases(),
            Cmd::CodeqlRevealDatabase => {
                if let Some(i) = self.current_codeql_database() {
                    self.reveal_codeql_database(i);
                }
            }
            Cmd::CodeqlUpgradeDatabase => {
                if let Some(i) = self.current_codeql_database() {
                    self.upgrade_codeql_database(i);
                }
            }
            Cmd::CodeqlClearCache | Cmd::CodeqlTrimCache | Cmd::CodeqlTrimCacheToOverlay => {
                use crate::codeql_query::DbJob;
                let job = match cmd {
                    Cmd::CodeqlClearCache => DbJob::ClearCache,
                    Cmd::CodeqlTrimCache => DbJob::TrimCache,
                    _ => DbJob::TrimCacheToOverlay,
                };
                if let Some(i) = self.current_codeql_database() {
                    self.start_codeql_db_job(i, job);
                }
            }
            Cmd::CodeqlAddDatabaseSource => {
                if let Some(i) = self.current_codeql_database() {
                    self.add_codeql_database_source(i);
                }
            }
            Cmd::CodeqlDeleteUnusedDatabases => self.confirm_delete_unused_codeql_databases(),
            Cmd::CodeqlRemoveHistory => {
                if let Some(i) = self.current_codeql_history() {
                    self.confirm_remove_codeql_history(i);
                }
            }
            Cmd::CodeqlRenameHistory => {
                if let Some(i) = self.current_codeql_history() {
                    self.prompt_rename_codeql_history(i);
                }
            }
            Cmd::CodeqlSortHistory => self.sort_codeql_history(),
            Cmd::CodeqlViewQuery => {
                if let Some(i) = self.current_codeql_history() {
                    self.view_codeql_history_query(i);
                }
            }
            Cmd::CodeqlCreateQuery => self.prompt_create_codeql_query(),
            Cmd::CodeqlRunQueryOnDatabases => self.run_codeql_query_on_databases(),
            Cmd::CodeqlRunPack => match self.codeql.selected_pack() {
                Some(p) => self.run_codeql_pack(p),
                None => {
                    self.status = String::from(
                        "Select a query pack or one of its queries in the CodeQL side bar first",
                    );
                }
            },
            Cmd::CodeqlCancelQueue => self.cancel_codeql_queue(),
            Cmd::CodeqlCopyVersion => self.copy_codeql_version(),
            Cmd::CodeqlDownloadCli => self.download_codeql_cli(),
            Cmd::CodeqlCheckCliUpdates => self.check_codeql_cli_updates(),
            Cmd::CodeqlInstallPackDependencies => match self.codeql.selected_pack() {
                Some(p) => self.install_codeql_pack_dependencies(p),
                None => {
                    self.status = String::from(
                        "Select a query pack or one of its queries in the CodeQL side bar first",
                    );
                }
            },
            Cmd::CodeqlDownloadPacks => self.prompt_download_codeql_packs(),
            Cmd::CodeqlRunPublishedPack => self.prompt_run_codeql_published_pack(),
            Cmd::CodeqlQuickQuery => self.open_codeql_quick_query(),
            Cmd::CodeqlCompareResults => {
                if let Some(i) = self.current_codeql_history() {
                    self.compare_codeql_results(i);
                }
            }
            Cmd::CodeqlShowEvaluatorLog => {
                if let Some(i) = self.current_codeql_history() {
                    self.show_codeql_evaluator_log(i);
                }
            }
            Cmd::CodeqlShowQueryLog => {
                if let Some(i) = self.current_codeql_history() {
                    self.show_codeql_query_log(i);
                }
            }
            Cmd::CodeqlShowEvaluatorLogSummary => {
                if let Some(i) = self.current_codeql_history() {
                    self.show_codeql_evaluator_log_summary(i);
                }
            }
            Cmd::CodeqlShowEvaluatorLogViewer => {
                if let Some(i) = self.current_codeql_history() {
                    self.show_codeql_evaluator_log_viewer(i);
                }
            }
            Cmd::CodeqlComparePerformance => {
                if let Some(i) = self.current_codeql_history() {
                    self.compare_codeql_performance(i);
                }
            }
            Cmd::CodeqlPreviewQueryHelp => self.preview_codeql_query_help(),
            Cmd::CodeqlExportResults => self.prompt_export_codeql_results(),
            Cmd::CodeqlRunQuerySuite => self.run_codeql_query_suite(),
            Cmd::CodeqlSetUpController => self.prompt_codeql_controller(),
            Cmd::CodeqlAddVariantRepo => self.prompt_add_codeql_variant_repo(),
            Cmd::CodeqlAddVariantList => self.prompt_add_codeql_variant_list(),
            Cmd::CodeqlAddVariantOwner => self.prompt_add_codeql_variant_owner(),
            Cmd::CodeqlVariantCodeSearch => self.prompt_codeql_variant_code_search(),
            Cmd::CodeqlOpenVariantConfig => self.open_codeql_variant_config(),
            Cmd::CodeqlOpenVariantOnGithub => self.open_codeql_variant_on_github(),
            Cmd::CodeqlRunVariantAnalysis => self.run_codeql_variant_analysis(),
            Cmd::CodeqlExportVariantResults => {
                self.prompt_export_codeql_variant_results(self.codeql.selected_variant_run())
            }
            Cmd::CodeqlCopyVariantRepoList => {
                self.copy_codeql_variant_repo_list(self.codeql.selected_variant_run())
            }
            Cmd::CodeqlViewVariantLogs => {
                self.view_codeql_variant_logs(self.codeql.selected_variant_run())
            }
            Cmd::CodeqlOpenVariantResults => {
                self.open_codeql_variant_results(self.codeql.selected_variant_run())
            }
            Cmd::CodeqlRunTests => self.run_codeql_tests(),
            Cmd::CodeqlDebugQuery => self.debug_codeql_query(),
            Cmd::CodeqlDebugSelection => self.quick_eval_codeql(false),
            Cmd::CodeqlResultsUp => self.walk_codeql_results(-1, 0),
            Cmd::CodeqlResultsDown => self.walk_codeql_results(1, 0),
            Cmd::CodeqlResultsLeft => self.walk_codeql_results(0, -1),
            Cmd::CodeqlResultsRight => self.walk_codeql_results(0, 1),
            Cmd::SheetSortByColumn => self.sort_sheet_by_column(),
            Cmd::CodeqlQuickEval => self.quick_eval_codeql(false),
            Cmd::CodeqlQuickEvalCount => self.quick_eval_codeql(true),
            Cmd::CodeqlViewAst => self.view_codeql_ast(),
            Cmd::CodeqlOpenModelEditor => self.open_codeql_model_editor(),
            Cmd::CodeqlModelEndpoint => {
                self.model_codeql_endpoint(self.codeql.selected_model_endpoint())
            }
            Cmd::CodeqlOpenReferencedFile => self.open_codeql_referenced_file(),
            Cmd::CodeqlViewAlertsCsv => self.view_codeql_alerts(true),
            Cmd::CodeqlViewAlertsSarif => self.view_codeql_alerts(false),
            Cmd::CodeqlViewResultsCsv => self.view_codeql_raw_results(),
            Cmd::CodeqlShowResultSet => self.show_codeql_result_sets(),
            Cmd::CodeqlViewCfg => self.view_codeql_cfg(),
            Cmd::CodeqlRunAllQueries => self.run_all_codeql_queries(),
            Cmd::CodeqlRunSelectedQueries => self.run_selected_codeql_queries(),
            Cmd::CodeqlCancelRunningQuery => self.cancel_codeql_run(),
            Cmd::CodeqlAcceptTestOutput => self.accept_codeql_test_output(),
            Cmd::CodeqlFocusSideBar => {
                // Show it and take the keys, as VS Code's "Focus" does.
                self.open_codeql_view();
                if self.sidebar_view == SidebarView::CodeQL {
                    self.focus = Pane::Tree;
                }
            }
            Cmd::CodeqlOpenResultsDirectory => self.open_codeql_results_directory(),
            Cmd::ShowCodeQL => self.open_codeql_view(),
            Cmd::RunTestAtCursor => self.run_test_at_cursor(),
            Cmd::TestingRunAll => {
                if !self.testing_busy_says_so() {
                    self.run_all_tests();
                }
            }
            Cmd::TestingRunFile => self.run_tests_in_current_file(),
            Cmd::TestingRerunFailed => self.rerun_failed_tests(),
            Cmd::TestingRerunLast => self.rerun_last_test_run(),
            Cmd::DebugTestAtCursor => self.debug_test_at_cursor(),
            Cmd::ToggleSideBar => self.toggle_side_bar(),
            Cmd::ToggleAutoHideSideBar => self.toggle_sidebar_auto_hide(),
            Cmd::ToggleSecondarySideBar => self.toggle_secondary_side_bar(),
            Cmd::ToggleZenMode => self.toggle_zen_mode(),
            Cmd::ToggleCenteredLayout => self.toggle_centered_layout(),
            Cmd::ToggleTerminal => self.toggle_terminal(),
            // Each tab names the command that shows it, so the palette reads
            // the tab from there rather than keeping a second mapping.
            Cmd::FocusTerminal
            | Cmd::ShowProblems
            | Cmd::ShowOutput
            | Cmd::ShowPorts
            | Cmd::ShowCaptures => {
                if let Some(tab) = BottomPanelTab::ALL
                    .into_iter()
                    .find(|tab| tab.show_command() == cmd)
                {
                    self.set_bottom_panel_tab(tab);
                }
            }
            Cmd::OutputSelectChannel => self.open_output_channel_picker(),
            // The Customize Layout popup's rows (#852), through the same
            // setters its clicks use.
            Cmd::ToggleActivityBar => self.toggle_activity_bar(),
            Cmd::ToggleStatusBar => self.toggle_status_bar(),
            Cmd::ToggleSideBarPosition => {
                self.set_side_bar_position(match self.side_bar_position {
                    SideBarPosition::Left => SideBarPosition::Right,
                    SideBarPosition::Right => SideBarPosition::Left,
                })
            }
            Cmd::SetPanelAlignment => self.open_panel_alignment_picker(),
            Cmd::SetQuickInputPosition => self.open_quick_input_position_picker(),
            Cmd::CustomizeLayout => self.open_customize_layout_menu(),
            Cmd::ToggleMinimap => self.toggle_minimap(),
            Cmd::ProblemsToggleProjectAuto => {
                // auto -> on -> off -> auto. Cycling rather than a boolean
                // because `auto` is a distinct answer from both, not a
                // default the other two override.
                let next = match Self::project_scope_mode(&self.problems_project_scope) {
                    "auto" => "on",
                    "on" => "off",
                    _ => "auto",
                };
                self.problems_project_scope = String::from(next);
                // `on` and `off` are answers on their own. `auto` is the
                // only mode whose effect depends on a count, and counting is
                // a whole-workspace walk - which this repo runs on a
                // background thread, never on a keypress. The early stop
                // bounds the walk only ABOVE the cap; a root under it is
                // walked to completion, which is the common case.
                let effect = match next {
                    "on" => " (runs on its own)",
                    "off" => " (never runs on its own)",
                    // Deliberately not the answer: reporting it here would
                    // cost the walk. The check itself reports when it
                    // declines, at the point it actually declines.
                    _ => " (runs on its own unless the workspace is very large)",
                };
                self.status = format!("Whole-project check: {next}{effect}");
                if !cfg!(test) {
                    self.report_unsaved_setting(crate::prefs::save_problems_project_scope(next));
                }
            }
            Cmd::ProblemsCheckProject => {
                // An explicit ask always runs: the cap governs UNASKED runs
                // (#256 criterion 5), and refusing what the user just
                // requested would be answering a question nobody asked.
                let root = self.workspace_root().to_path_buf();
                match Self::project_check_command(&root) {
                    None => {
                        self.status = String::from(
                            "No whole-project checker for this workspace (expected tsconfig.json)",
                        );
                    }
                    Some(ProjectCheck::Empty) => {
                        self.status =
                            String::from("Whole-project check: tsconfig.json includes no files");
                    }
                    Some(_) if self.project_check_running => {
                        self.status = String::from("A whole-project check is already running");
                    }
                    Some(ProjectCheck::Run(runs)) => {
                        // Off the main loop: tsc on a large project takes tens
                        // of seconds, and running it inline froze input and
                        // redraw for the duration - the "Checking..." status
                        // below never even painted. Same shape as
                        // `record_history_snapshot_of`: spawn, send, drain on
                        // the tick.
                        let tx = self.project_check_tx.clone();
                        let root_for_thread = root.clone();
                        let mode = self.problems_project_scope.clone();
                        let hint_tx = self.project_hint_tx.clone();
                        // NOT set here: this fires for every explicit check,
                        // so a run under `on`, `off`, or an under-cap `auto`
                        // would consume the one-shot and a later oversized
                        // `auto` run would stay silent forever. The flag is
                        // set where the hint is RECEIVED, which is the only
                        // point that knows one was warranted.
                        let hinted = self.project_check_hinted;
                        std::thread::spawn(move || {
                            // The cap's one-time hint (#256 criterion 5).
                            // Computed HERE, on the thread already spawned
                            // for the compile, because it is a whole-
                            // workspace walk - never on the UI thread. The
                            // explicit run still proceeds either way; the
                            // cap governs UNASKED runs, and this tells the
                            // user once that automatic ones will not start.
                            if !hinted
                                && !Self::project_check_auto_enabled(
                                    &root_for_thread,
                                    &mode,
                                    Self::PROJECT_CHECK_AUTO_MAX_FILES,
                                )
                                && Self::project_scope_mode(&mode) == "auto"
                            {
                                let _ = hint_tx.send(());
                            }
                            // .output(), never .status(): the checker must not
                            // inherit croft's TTY, or its diagnostics spray
                            // over the UI. One run per referenced project,
                            // merged: both streams concatenated, and the
                            // last failing status kept, so one failing
                            // project fails the sweep.
                            let mut done: Option<std::io::Result<std::process::Output>> = None;
                            for (program, args) in runs {
                                let out = std::process::Command::new(&program)
                                    .args(&args)
                                    .current_dir(&root_for_thread)
                                    .output();
                                done = Some(match (done, out) {
                                    (Some(Ok(mut acc)), Ok(o)) => {
                                        acc.stdout.extend(o.stdout);
                                        acc.stderr.extend(o.stderr);
                                        if !o.status.success() {
                                            acc.status = o.status;
                                        }
                                        Ok(acc)
                                    }
                                    (Some(Err(e)), _) | (_, Err(e)) => Err(e),
                                    (None, Ok(o)) => Ok(o),
                                });
                                if matches!(done, Some(Err(_))) {
                                    break;
                                }
                            }
                            if let Some(done) = done {
                                let _ = tx.send((root_for_thread, done));
                            }
                        });
                        self.project_check_running = true;
                        self.status = String::from("Checking the whole project...");
                    }
                }
            }
            Cmd::ProblemsToggleScope => {
                self.problems.scope = self.problems.scope.next();
                // Rebuild rather than filter in the panel: the badge counts
                // the same list, so both follow the scope from one place.
                self.rebuild_problems();
                self.refresh_problems_badge();
                self.status = format!("Problems: {}", self.problems.scope.label());
                if !cfg!(test) {
                    self.report_unsaved_setting(crate::prefs::save_problems_scope(
                        self.problems.scope.to_config(),
                    ));
                }
            }
            Cmd::DiffToggleIgnoreWhitespace => self.diff_cycle_whitespace_mode(),
            Cmd::RunSelectedText => self.run_selected_text_in_terminal(),
            // Cmd+] / Cmd+[ (#843), which have no `Ctrl` form. The chords
            // work in the terminal pane; from the palette the commands bring
            // it up and focus it first, as VS Code's do.
            Cmd::FocusNextTerminal | Cmd::FocusPreviousTerminal => {
                self.show_terminal = true;
                self.focus_pane(Pane::Terminal);
                if cmd == Cmd::FocusNextTerminal {
                    self.cycle_terminal();
                } else {
                    self.cycle_terminal_back();
                }
            }
            Cmd::TerminalClear
            | Cmd::TerminalRename
            | Cmd::TerminalKill
            | Cmd::TerminalSplit
            | Cmd::TerminalNewWithProfile
            | Cmd::TerminalQuickSelect
            | Cmd::TerminalCopyMode
            | Cmd::TerminalCommandHistory
            | Cmd::TerminalOpenScrollback => self.run_terminal_command(cmd),
            Cmd::NewTerminal => match self.split_terminal() {
                Ok(()) => {
                    self.terminal_status(format!(
                        "Split terminal: {} active",
                        self.live_terminal_count()
                    ));
                }
                Err(e) => {
                    self.status = format!("Split terminal failed: {e}");
                }
            },
            Cmd::StartDebugging => self.debug_start_or_continue(),
            Cmd::SelectDebugConfig => self.open_debug_config_picker(),
            Cmd::AddDebugConfig => self.open_add_debug_config(),
            Cmd::StopDebugging => self.debug_stop_by_user(),
            Cmd::PauseDebugging => self.debug_pause(),
            Cmd::SwitchDebugSession => self.switch_debug_session(),
            Cmd::RestartDebugging => self.debug_restart(),
            Cmd::ToggleBreakpoint => self.debug_toggle_breakpoint(),
            Cmd::EditLogpoint => self.debug_edit_logpoint(),
            Cmd::ShowIncomingCalls => self.request_call_hierarchy_at_cursor(true),
            Cmd::ShowSupertypes => self.request_type_hierarchy_at_cursor(true),
            Cmd::ToggleCodeLens => self.toggle_code_lens(),
            Cmd::OpenSearchEditor => self.open_search_editor(),
            Cmd::RebaseAbort => self.abort_rebase_todo(),
            Cmd::GitContinueOperation => self.continue_git_operation(),
            Cmd::GitSkipOperation => self.skip_git_operation(),
            Cmd::GitAbortOperation => self.abort_git_operation(),
            Cmd::GitCreateTag => {
                self.dispatch_scm_action(crate::widgets::scm_menu::ScmAction::CreateTag)
            }
            Cmd::GitPushTags => {
                self.dispatch_scm_action(crate::widgets::scm_menu::ScmAction::PushTags)
            }
            Cmd::GitDeleteRemoteTag => {
                self.dispatch_scm_action(crate::widgets::scm_menu::ScmAction::DeleteRemoteTag)
            }
            Cmd::GitCommit => self.dispatch_scm_action(crate::widgets::scm_menu::ScmAction::Commit),
            Cmd::GitPush => self.dispatch_scm_action(crate::widgets::scm_menu::ScmAction::Push),
            Cmd::GitPull => self.dispatch_scm_action(crate::widgets::scm_menu::ScmAction::Pull),
            Cmd::GitSync => self.dispatch_scm_action(crate::widgets::scm_menu::ScmAction::Sync),
            Cmd::GitCheckoutTo => {
                self.dispatch_scm_action(crate::widgets::scm_menu::ScmAction::CheckoutTo)
            }
            Cmd::GitStash => self.dispatch_scm_action(crate::widgets::scm_menu::ScmAction::Stash),
            Cmd::ToggleTerminalSuggestions => self.toggle_terminal_suggestions(),
            Cmd::ToggleScreenReader => self.toggle_screen_reader(),
            Cmd::OpenKeyboardShortcuts => self.open_keyboard_shortcuts(),
            Cmd::SwitchProfile => self.open_profiles(),
            Cmd::ToggleInlineSuggestions => self.toggle_inline_suggestions(),
            Cmd::RerunSearchEditor => self.rerun_search_editor(),
            Cmd::DebugAddHitCountBreakpoint => self.debug_edit_hit_condition(),
            Cmd::DebugAddFunctionBreakpoint => self.debug_add_function_breakpoint(),
            Cmd::DebugRemoveFunctionBreakpoints => self.debug_clear_function_breakpoints(),
            Cmd::DebugRunToCursor => self.debug_run_to_cursor(),
            Cmd::DebugBreakOnValueChange => self.debug_break_on_value_change(),
            Cmd::DebugRemoveDataBreakpoints => self.debug_clear_data_breakpoints(),
            Cmd::RunCodeLens => self.run_code_lens_at_cursor(),
            Cmd::ShowSubtypes => self.request_type_hierarchy_at_cursor(false),
            Cmd::ShowOutgoingCalls => self.request_call_hierarchy_at_cursor(false),
            Cmd::EditBreakpointCondition => self.debug_edit_condition(),
            Cmd::StepOver => self.debug_step("next"),
            Cmd::ContinueDebugging => self.debug_continue(),
            Cmd::StepInto => self.debug_step("stepIn"),
            Cmd::StepOut => self.debug_step("stepOut"),
            Cmd::StepIntoTarget => self.debug_step_into_target(),
            Cmd::ToggleRaisedExceptions => self.debug_toggle_raised_exceptions(),
            Cmd::AttachPythonProcess => self.open_attach_python_picker(),
            Cmd::ColorTheme => self.open_theme_picker(),
            Cmd::SessionParticipants => self.open_participants_picker(),
            Cmd::SessionDetach => self.detach_session_client(),
            Cmd::CollabCancelStream => self.collab_cancel_stream(),
            Cmd::AskNavigatorAboutCapture => self.ask_navigator_about_capture(),
            Cmd::OpenWorkspaceOnSshHost => self.open_workspace_on_ssh_host(),
            Cmd::ScrubHistory => self.scrub_history(),
            Cmd::DebugInstallDelve => self.install_delve(),
            Cmd::RemoteSyncConfigNow => {
                self.open_input_prompt(crate::widgets::input_prompt::InputPrompt::new(
                    crate::widgets::input_prompt::InputPurpose::SyncConfigHost,
                    String::from("Sync config to host"),
                    String::from("an ssh host alias"),
                ))
            }
            Cmd::StopAutoApprove => self.stop_auto_approve(),
            Cmd::ScrubOpenHere => self.scrub_open_here(),
            Cmd::ScrubDiffToWorkingTree => self.scrub_diff_to_working_tree(),
            Cmd::OpenAsSymbolTab => self.open_symbol_tab(),
            Cmd::LoadReviewThreads => self.load_review_threads(),
            Cmd::ReviewAddComment => self.open_review_comment_prompt(),
            Cmd::ReviewSubmit => self.open_review_submit(),
            Cmd::ReviewToggleResolved => self.toggle_review_thread_resolved(),
            Cmd::ExportCommentsToPr => self.open_export_comments(),
            Cmd::NoteAdd => self.open_sticky_note_prompt(),
            Cmd::NoteDelete => self.delete_sticky_note_here(),
            Cmd::ReviewDiscardPending => {
                let n = self.review_pending.len();
                self.review_pending.clear();
                self.review_pending_pr = None;
                self.status = format!("Discarded {n} pending comment(s)");
            }
            Cmd::ToggleSessionRecording => self.toggle_session_recording(),
            Cmd::FleetRun => {
                self.open_input_prompt(crate::widgets::input_prompt::InputPrompt::new(
                    crate::widgets::input_prompt::InputPurpose::FleetCommand,
                    String::from("Fleet run"),
                    String::from("hosts: command   (or  *: command  for every host)"),
                ))
            }
            Cmd::NewWorktreeLane => self.open_new_lane_prompt(),
            Cmd::ReviewPullRequest => {
                // The open PRs first, fetched off the UI thread; the typed
                // prompt stays one row away (and is the fallback when gh
                // cannot list).
                self.status = String::from("Listing open pull requests…");
                self.spawn_pr_gh(PrGhJob::List, crate::pr_review::list_args());
            }
            Cmd::DiffWorktreeLane => self.diff_worktree_lane(),
            Cmd::CloseWorktreeLane => self.close_worktree_lane(),
            Cmd::MarkAgentFileReviewed => {
                match self.editor.path.clone() {
                    None => self.status = String::from("No file open"),
                    Some(path) if !self.agent_ledger.is_unreviewed(&path) => {
                        self.status =
                            String::from("No agent change waiting for review on this file");
                    }
                    Some(path) => {
                        // Every lane holding it: a file two agents both
                        // touched is reviewed once, for both.
                        let agents: Vec<String> = self
                            .agent_ledger
                            .agents()
                            .into_iter()
                            .map(String::from)
                            .collect();
                        let mut n = 0;
                        for agent in &agents {
                            if self.mark_agent_file_reviewed(agent, &path) {
                                n += 1;
                            }
                        }
                        let name = path
                            .file_name()
                            .map(|f| f.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        self.status = format!(
                            "{name} marked reviewed in {n} agent lane{}",
                            if n == 1 { "" } else { "s" }
                        );
                    }
                }
            }
            Cmd::ShowAgentLane => {
                let rows = self.agent_lane_rows();
                self.status = if rows.is_empty() {
                    String::from("No agent has changed anything yet")
                } else if self.agent_ledger.may_be_incomplete() {
                    format!(
                        "{} \u{b7} at least: the file watcher overflowed",
                        rows.join(" \u{b7} ")
                    )
                } else {
                    rows.join(" \u{b7} ")
                };
            }
            Cmd::OpenAgentLaneSection => self.show_agent_lane_section(),
            Cmd::PickAgentLaneFile => self.pick_agent_lane_file(),
            Cmd::DiffAgentFileSinceReview => {
                match self.editor.path.clone() {
                    None => self.status = String::from("No file open"),
                    Some(path) => {
                        // The first lane holding it. A file two agents both
                        // touched has one baseline per lane, and diffing
                        // against an arbitrary second one would silently
                        // answer a different question than the row shows.
                        let agent = self
                            .agent_ledger
                            .agents()
                            .into_iter()
                            .map(String::from)
                            .find(|a| self.agent_ledger.lane(a).iter().any(|f| f.path == path));
                        match agent {
                            None => {
                                self.status = String::from("No agent has changed this file");
                            }
                            Some(agent) => self.diff_agent_lane_row(&agent, &path),
                        }
                    }
                }
            }
            Cmd::MarkAgentLaneReviewed => {
                let agents: Vec<String> = self
                    .agent_ledger
                    .agents()
                    .into_iter()
                    .map(String::from)
                    .collect();
                match agents.len() {
                    0 => self.status = String::from("No agent has changed anything yet"),
                    _ => {
                        let mut total = 0;
                        for agent in &agents {
                            total += self.mark_agent_lane_reviewed(agent);
                        }
                        // Each lane set its own status; the summary is the
                        // one the user is left with.
                        self.status = match total {
                            0 => String::from("Nothing waiting for review"),
                            1 => String::from("1 file marked reviewed"),
                            n => format!("{n} files marked reviewed"),
                        };
                    }
                }
            }
            Cmd::FixProblemWithNavigator => match self.navigator_fix_candidate() {
                Some((path, item)) => self.fix_problem_with_navigator(path, item, Vec::new()),
                None => self.status = String::from("No diagnostic at the caret to fix"),
            },
            Cmd::SendHttpRequest => self.send_http_request_under_caret(),
            Cmd::CopyHttpRequestAsCurl => self.copy_http_request_as_curl(),
            Cmd::SwitchHttpEnvironment => self.open_http_environment_picker(),
            Cmd::AskNavigator => {
                let (range, selection) = match self.editor.selection_rows() {
                    Some(rows) => (rows, self.editor.selection_text()),
                    None => {
                        let row = self.editor.cursor_row;
                        ((row, row), String::new())
                    }
                };
                self.open_ask_navigator(range, selection);
            }
            Cmd::YieldToNavigator => self.yield_to_navigator(),
            Cmd::ToggleNavigator => self.toggle_navigator(),
            Cmd::ClearNavigatorNotes => self.clear_navigator_notes(),
            Cmd::ToggleProactiveNavigator => self.toggle_proactive_navigator(),
            Cmd::NextComment => self.cycle_comment_box(),
            Cmd::IgnoreComment => self.ignore_focused_comment(),
            Cmd::RunTask => self.open_run_task_picker(),
            Cmd::RunBuildTask => self.run_build_task(),
            Cmd::RerunLastTask => self.rerun_last_task(),
            Cmd::KeyboardShortcuts => self.open_shortcuts_modal(),
            Cmd::UpdateCroft => self.update_croft(),
            Cmd::OpenSettings => self.open_settings_view(),
            Cmd::OpenSettingsEditor => self.open_settings_editor(),
            Cmd::OpenSettingsJson => self
                .open_config_file_in_editor(crate::prefs::config_path(), ConfigFileSeed::Settings),
            Cmd::OpenWorkspaceSettingsJson => self.open_workspace_settings(false),
            Cmd::OpenWorkspaceSettingsLocalJson => self.open_workspace_settings(true),
            Cmd::OpenKeybindingsJson => self.open_config_file_in_editor(
                crate::keymap::keybindings_path(),
                ConfigFileSeed::Keybindings,
            ),
            Cmd::ConfigureSnippets => self.open_config_file_in_editor(
                crate::snippets::snippets_path(),
                ConfigFileSeed::Snippets,
            ),
            Cmd::OpenAgentsJson => self
                .open_config_file_in_editor(crate::agents::agents_path(), ConfigFileSeed::Agents),
            Cmd::OpenTriggersJson => self.open_config_file_in_editor(
                crate::triggers::triggers_path(),
                ConfigFileSeed::Triggers,
            ),
            Cmd::OpenMatchersJson => self.open_config_file_in_editor(
                crate::problem_matchers::matchers_path(),
                ConfigFileSeed::Matchers,
            ),
        }
    }
}

/// Collect the bundled and user-installed extension manifests, project them to
/// the panel's row list, and stamp each with its enabled state from `disabled`.
/// The bundled set ships in the binary; user extensions are read fresh from
/// `~/.config/croft/extensions/` each call so a newly dropped-in extension shows
/// up without a relaunch.
pub(super) fn build_extension_items(
    disabled: &std::collections::BTreeSet<String>,
    vscode: &[crate::vscode_extensions::Comparison],
) -> Vec<crate::widgets::extensions::ExtensionItem> {
    use crate::lsp::manifest;
    let user = manifest::read_extension_sources(&manifest::user_extensions_dir());
    let mut sources: Vec<&str> = manifest::BUNDLED_MANIFESTS.to_vec();
    sources.extend(user.iter().map(String::as_str));
    let summaries = manifest::summaries(&sources);
    let mut items = crate::widgets::extensions::items_from_summaries(summaries, disabled);
    // Flag installed catalog entries as removable: croft wrote their manifest
    // (so uninstall is reversible), while a hand-dropped user manifest is the
    // user's own file and is never deleted. Drives the per-row trash button.
    for it in &mut items {
        it.removable = !it.builtin && !it.available && crate::mcp::catalog::is_removable(&it.id);
    }
    // Append the curated MCP catalog's not-yet-installed entries under AVAILABLE.
    items.extend(crate::widgets::extensions::items_from_available(
        crate::mcp::catalog::available(),
    ));
    // And what VS Code has that croft covers, or does not (#352).
    items.extend(crate::widgets::extensions::items_from_vscode(vscode));
    items
}

/// Resolve the program to spawn for a sidecar server: PATH-first / pinned
/// managed install (reusing the LSP provisioning), or `None`+kick the install
/// when not yet present. Returns the command plus extra PATH dirs it needs.
pub(super) fn resolve_mcp_program(
    server: &crate::mcp::registry::ServerSpawn,
) -> Result<(String, Vec<PathBuf>), String> {
    let Some(provision) = &server.provision else {
        // PATH-only server (no croft-managed provisioning declared).
        return Ok((server.command.clone(), Vec::new()));
    };
    if let Some(found) = crate::lsp::install::provisioned_command(&server.id, provision) {
        return Ok(found);
    }
    // Not installed: kick the one-shot managed install (leaking the id to
    // 'static once, only on this first-install path — `resolve_managed` keys its
    // install-started gate on it), and ask the user to retry once it lands.
    let name: &'static str = Box::leak(server.id.clone().into_boxed_str());
    let config = crate::lsp::config::ServerConfig {
        name,
        command: server.command.clone(),
        args: server.args.clone(),
        language: crate::lsp::config::Language("mcp"),
        initialization_options: None,
        provision: Some(provision.clone()),
    };
    crate::lsp::install::resolve_managed(&config, provision, true);
    // First run: the managed install runs in the background. Block here (on the
    // worker thread — the UI stays responsive) until it lands, then resolve, so a
    // single invocation installs AND runs the command instead of erroring with
    // "run again" and dropping the user's argument. spawn_mcp_command shows a
    // first-run status meanwhile.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
    loop {
        std::thread::sleep(std::time::Duration::from_millis(500));
        if let Some(found) = crate::lsp::install::provisioned_command(&server.id, provision) {
            return Ok(found);
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "Installing {} timed out — check your network and try again",
                server.id
            ));
        }
    }
}

/// The trust-on-first-use gate on a contributed command's MCP tool: an
/// error naming the recovery when `fingerprint` differs from the one first
/// recorded for `command_id` under `config_dir`.
pub(super) fn mcp_tool_trust(
    config_dir: &Path,
    command_id: &str,
    tool: &str,
    fingerprint: &str,
) -> Result<(), String> {
    if crate::prefs::trust_mcp_tool_in(config_dir, command_id, fingerprint) {
        return Ok(());
    }
    Err(format!(
        "refusing to run: the '{tool}' tool definition changed since you approved it (possible rug-pull); toggle the extension off and on to re-approve"
    ))
}

/// Run a resolved MCP command to completion on a worker thread: provision +
/// spawn the server, verify the tool definition (trust-on-first-use rug-pull
/// guard), call the tool, and return its text. Pure blocking; the caller sends
/// the [`crate::mcp::McpOutcome`] back over a channel.
pub(super) fn run_mcp_command_blocking(
    resolved: &crate::mcp::registry::ResolvedCommand,
    arg: Option<String>,
    cwd: &Path,
    config_dir: &Path,
    version: &str,
) -> crate::mcp::McpOutcome {
    use serde_json::json;
    let title = resolved.title.clone();
    let body = (|| -> Result<String, String> {
        let (program, extra_paths) = resolve_mcp_program(&resolved.server)?;
        // Least-privilege env: the declared vars plus a PATH (extra dirs first,
        // then the inherited PATH) so the launcher resolves its interpreter.
        let mut env = resolved.server.env.clone();
        let mut path_parts: Vec<String> = extra_paths
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        if let Ok(sys) = std::env::var("PATH") {
            path_parts.push(sys);
        }
        env.insert("PATH".to_string(), path_parts.join(":"));

        let client = crate::mcp::client::McpClient::connect(
            &program,
            &resolved.server.args,
            cwd,
            &env,
            "croft",
            version,
        )
        .map_err(|e| e.to_string())?;

        // Trust-on-first-use: fingerprint the tool definition and refuse if it
        // changed since the user first ran this command (rug-pull / MCPoison).
        let list = client
            .call("tools/list", json!({}))
            .map_err(|e| e.to_string())?;
        // Validate the declared tool is actually advertised before calling it.
        let tools = crate::mcp::client::parse_tools(&list);
        if !tools.iter().any(|t| t.name == resolved.tool) {
            return Err(format!(
                "the server does not advertise a '{}' tool",
                resolved.tool
            ));
        }
        let fingerprint = crate::mcp::client::tool_fingerprint(&list, &resolved.tool);
        mcp_tool_trust(
            config_dir,
            &resolved.command_id,
            &resolved.tool,
            &fingerprint,
        )?;

        let arguments = match (resolved.arg.as_ref(), arg) {
            (Some(name), Some(value)) => json!({ name.as_str(): value }),
            _ => json!({}),
        };
        let result = client
            .call_tool(&resolved.tool, arguments)
            .map_err(|e| e.to_string())?;
        let text = crate::mcp::client::tool_result_text(&result);
        Ok(if text.trim().is_empty() {
            String::from("(the tool returned no text content)")
        } else {
            text
        })
    })();
    crate::mcp::McpOutcome { title, body }
}
