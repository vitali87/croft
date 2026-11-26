//! The input popup and the list picker (opening, keys, clicks, drawing),
//! `submit_input_prompt` and `confirm_list_picker`, and the small inline prompt.

use super::*;
use crate::widgets::picker_rows::{PickerMouse, answer_mouse};

impl Prompt {
    /// The caret, clamped into `buffer`.
    pub(super) fn caret(&self) -> usize {
        crate::widgets::line_edit::clamp(&self.buffer, self.caret)
    }

    /// Run one edit (`insert_at`, `delete_before`, ...) at the caret and keep
    /// what it returns.
    pub(super) fn edit(&mut self, edit: impl FnOnce(&str, usize) -> (String, usize)) {
        let (text, caret) = edit(&self.buffer, self.caret());
        self.buffer = text;
        self.caret = caret;
        self.error = None;
    }

    /// Move the caret with one of `line_edit`'s walkers.
    pub(super) fn walk(&mut self, to: impl FnOnce(&str, usize) -> usize) {
        self.caret = to(&self.buffer, self.caret());
    }
}

impl App {
    /// The `> ` field line of a prompt: the buffer scrolled to keep the caret
    /// in `width` cells, and the caret drawn as a block over the char under it.
    pub(super) fn prompt_field_line(
        &self,
        p: &Prompt,
        width: usize,
        cursor_fg: Color,
    ) -> ratatui::text::Line<'static> {
        use ratatui::text::Span;
        let text_style = Style::default().fg(self.theme.ui(Color::White));
        let caret_style = Style::default().fg(cursor_fg);
        let chars: Vec<char> = p.buffer.chars().collect();
        let caret = p.caret();
        let win = crate::widgets::line_edit::window(&p.buffer, caret, width.saturating_sub(2));
        let mut spans = vec![Span::raw("> ")];
        let before: String = chars[win.start..caret].iter().collect();
        if !before.is_empty() {
            spans.push(Span::styled(before, text_style));
        }
        let mut drawn = caret;
        if caret < win.end {
            spans.push(Span::styled(
                chars[caret].to_string(),
                caret_style.add_modifier(Modifier::REVERSED),
            ));
            drawn = caret + 1;
        } else {
            spans.push(Span::styled("█", caret_style));
        }
        let tail: String = chars[drawn.min(win.end)..win.end].iter().collect();
        if !tail.is_empty() {
            spans.push(Span::styled(tail, text_style));
        }
        ratatui::text::Line::from(spans)
    }

    pub(super) fn render_prompt(&self, frame: &mut ratatui::Frame) {
        let Some(p) = &self.prompt else { return };
        let area = frame.area();
        let width = area.width.saturating_sub(8).clamp(40, 80).min(area.width);
        let height = if p.error.is_some() { 6 } else { 5 };
        let x = (area.width.saturating_sub(width)) / 2 + area.x;
        let y = (area.height.saturating_sub(height)) / 2 + area.y;
        let rect = Rect {
            x,
            y,
            width,
            height,
        };
        let rect = rect.intersection(area);
        let grad = self.popup_gradient();
        // A `surfaces` theme: the title in the strong ink on the card (no
        // chip), the caret and the prompt mark in the accent (the field has
        // the keyboard), the value in body text and the hint muted.
        let tones = crate::widgets::surface::tones(self.theme);
        let cursor_fg = match tones {
            Some(t) => t.accent(),
            None if grad => rgb_color(GRAD_TL),
            None => self.theme.ui(Color::Rgb(0x4e, 0x9a, 0xff)),
        };
        let title_bg = if grad {
            rgb_color(POPUP_SEL_BG)
        } else {
            self.theme.ui(Color::Rgb(0x1e, 0x3a, 0x6e))
        };
        let title_style = match tones {
            Some(t) => Style::default().fg(t.strong()).bg(t.raised()),
            None => Style::default()
                .fg(self.theme.ui(Color::White))
                .bg(title_bg),
        };
        let title = ratatui::text::Span::styled(
            format!(" {} ", p.label),
            title_style.add_modifier(Modifier::BOLD),
        );
        let block = ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .border_style(Style::default().fg(self.theme.ui(Color::Rgb(0x4e, 0x9a, 0xff))))
            .style(Style::default().bg(self.theme.ui(Color::Rgb(0x1e, 0x1e, 0x1e))))
            .title(title.clone());
        frame.render_widget(ratatui::widgets::Clear, rect);
        frame.render_widget(block, rect);
        // Black theme: overpaint the solid border with the gradient, then
        // re-stamp the title the gradient top edge just covered.
        if self.theme.framed_overlays() {
            crate::widgets::surface::paint_overlay_frame(frame.buffer_mut(), rect, self.theme);
            frame
                .buffer_mut()
                .set_span(rect.x + 1, rect.y, &title, title.width() as u16);
        }
        let inner = Rect {
            x: rect.x + 2,
            y: rect.y + 1,
            width: rect.width.saturating_sub(4),
            height: rect.height.saturating_sub(2),
        };
        let (top_line, hint_text) = match &p.kind {
            PromptKind::Create(_) => (
                self.prompt_field_line(p, usize::from(inner.width), cursor_fg),
                "Enter to create, Esc to cancel",
            ),
            PromptKind::Rename(_) => (
                self.prompt_field_line(p, usize::from(inner.width), cursor_fg),
                "Enter to rename, Esc to cancel",
            ),
            PromptKind::SaveAs => (
                self.prompt_field_line(p, usize::from(inner.width), cursor_fg),
                "Enter to save, Esc to cancel (missing folders are created)",
            ),
            PromptKind::RenameSymbol { .. } => (
                self.prompt_field_line(p, usize::from(inner.width), cursor_fg),
                "Enter to rename symbol, Esc to cancel",
            ),
            PromptKind::RenameTerminal(_) => (
                self.prompt_field_line(p, usize::from(inner.width), cursor_fg),
                "Enter to rename, Esc to cancel (blank clears the name)",
            ),
            PromptKind::AnnotateTerminal { existing, .. } => (
                self.prompt_field_line(p, usize::from(inner.width), cursor_fg),
                if existing.is_some() {
                    "Enter to update the note, Esc to cancel (blank deletes it)"
                } else {
                    "Enter to pin the note to the selection, Esc to cancel"
                },
            ),
            PromptKind::BreakpointCondition { .. } => (
                self.prompt_field_line(p, usize::from(inner.width), cursor_fg),
                "Enter to set condition (blank for a plain breakpoint), Esc to cancel",
            ),
            PromptKind::Logpoint { .. } => (
                self.prompt_field_line(p, usize::from(inner.width), cursor_fg),
                "Enter to set the log message, {expr} interpolates (blank for a plain breakpoint), Esc to cancel",
            ),
            PromptKind::HitCondition { .. } => (
                self.prompt_field_line(p, usize::from(inner.width), cursor_fg),
                "Enter to set the hit count, e.g. 5 (blank for a plain breakpoint), Esc to cancel",
            ),
            PromptKind::FunctionBreakpoint => (
                self.prompt_field_line(p, usize::from(inner.width), cursor_fg),
                "Enter to break on entry to this function, Esc to cancel",
            ),
            PromptKind::DataBreakpoint => (
                self.prompt_field_line(p, usize::from(inner.width), cursor_fg),
                "Enter to pause when this variable is written, Esc to cancel",
            ),
            PromptKind::NewProfile => (
                self.prompt_field_line(p, usize::from(inner.width), cursor_fg),
                "Enter to create it from your current settings, keybindings and snippets, Esc to cancel",
            ),
            PromptKind::ReviewComment => (
                self.prompt_field_line(p, usize::from(inner.width), cursor_fg),
                "Enter to add it to your pending review, Esc to cancel",
            ),
            PromptKind::ReviewSummary => (
                self.prompt_field_line(p, usize::from(inner.width), cursor_fg),
                "Enter to submit the review (the summary may be empty), Esc to cancel",
            ),
            PromptKind::StickyNote => (
                self.prompt_field_line(p, usize::from(inner.width), cursor_fg),
                "Enter to leave the note on this line (shared with everyone in the session), Esc to cancel",
            ),
        };
        frame.render_widget(
            ratatui::widgets::Paragraph::new(top_line),
            Rect {
                x: inner.x,
                y: inner.y,
                width: inner.width,
                height: 1,
            },
        );
        if let Some(t) = tones {
            // Every kind's row is `> `, the value in white, the caret: the
            // mark takes the accent, the value the body ink.
            let buf = frame.buffer_mut();
            for x in inner.x..inner.right() {
                let cell = &mut buf[(x, inner.y)];
                if x < inner.x + 2 {
                    cell.set_fg(t.accent());
                } else if cell.fg == self.theme.ui(Color::White) {
                    cell.set_fg(t.text());
                }
            }
        }
        let hint_fg = tones.map_or(Color::DarkGray, |t| t.muted());
        let hint = ratatui::widgets::Paragraph::new(ratatui::text::Line::from(hint_text))
            .style(Style::default().fg(hint_fg));
        frame.render_widget(
            hint,
            Rect {
                x: inner.x,
                y: inner.y + 2,
                width: inner.width,
                height: 1,
            },
        );
        if let Some(err) = &p.error {
            let line = ratatui::widgets::Paragraph::new(ratatui::text::Line::from(format!(
                "Error: {err}"
            )))
            .style(Style::default().fg(self.theme.ui(Color::Rgb(0xe8, 0x27, 0x4b))));
            frame.render_widget(
                line,
                Rect {
                    x: inner.x,
                    y: inner.y + 3,
                    width: inner.width,
                    height: 1,
                },
            );
        }
    }

    /// Open a single-line input modal for an SCM value (clone URL, branch
    /// rename, …).
    pub(super) fn open_input_prompt(&mut self, prompt: crate::widgets::input_prompt::InputPrompt) {
        self.close_scm_actions_menu();
        self.input_prompt = Some(prompt);
        self.overlays.popup_clear.request();
    }

    pub(super) fn close_input_prompt(&mut self) {
        if self.input_prompt.take().is_some() {
            self.overlays.popup_clear.request();
            self.overlays.activity.mark_dirty();
            self.overlays.welcome.mark_dirty();
            self.overlays.hero.mark_dirty();
            self.invalidate_editor_image_layouts();
            self.status.clear();
        }
    }

    /// Open a single-choice list picker (stashes, tags, remotes). Reports
    /// the empty case instead of opening a useless box.
    pub(super) fn open_list_picker(
        &mut self,
        picker: crate::widgets::list_picker::ListPicker,
        empty_msg: &str,
    ) {
        self.close_scm_actions_menu();
        if picker.rows.is_empty() {
            self.status = empty_msg.to_string();
            return;
        }
        self.list_picker = Some(picker);
        self.overlays.popup_clear.request();
    }

    /// The list picker closed with no row chosen (Esc, a click outside).
    pub(super) fn dismiss_list_picker(&mut self) {
        let purpose = self.list_picker.as_ref().map(|p| p.purpose);
        self.close_list_picker();
        if purpose == Some(crate::widgets::list_picker::ListPurpose::NotebookKernel) {
            self.kernel_choice_dismissed();
        }
    }

    pub(super) fn close_list_picker(&mut self) {
        // A server question closed without a pick is answered "no action".
        self.lsp_question = None;
        if self.list_picker.take().is_some() {
            self.overlays.popup_clear.request();
            self.overlays.activity.mark_dirty();
            self.overlays.welcome.mark_dirty();
            self.overlays.hero.mark_dirty();
            self.invalidate_editor_image_layouts();
            self.status.clear();
        }
    }

    /// Apply the value submitted by the input modal to its purpose's git op.
    pub(super) fn submit_input_prompt(&mut self) {
        use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
        let Some(prompt) = self.input_prompt.as_ref() else {
            return;
        };
        let Some(value) = prompt.submit_value() else {
            return; // blank: keep waiting
        };
        let purpose = prompt.purpose.clone();
        match purpose {
            InputPurpose::GoToLine => {
                self.close_input_prompt();
                match value.trim().parse::<usize>() {
                    Ok(n) if n > 0 => {
                        // The typed line is 1-based (what the status bar shows);
                        // goto_line_centered takes a 0-based row index. A
                        // selection left behind would be replaced by the next
                        // keystroke off screen, so it goes, as Ctrl+G's does.
                        // Like Quick Open's `:line`, the jump is a Go Back
                        // stop, and the editor takes the keys even when the
                        // terminal had them when the status bar was clicked.
                        if let Some(from) = self.editor.path.clone() {
                            self.nav.record(NavLoc {
                                path: from,
                                row: self.editor.cursor_row,
                                col: self.editor.cursor_col,
                            });
                        }
                        self.focus_pane(Pane::Editor);
                        self.editor.clear_selection();
                        self.editor.goto_line_centered(n - 1);
                        self.status = format!("Line {n}");
                    }
                    _ => self.status = format!("Not a line number: {}", value.trim()),
                }
            }
            InputPurpose::AddWatch => {
                self.close_input_prompt();
                self.add_watch_expression(value);
            }
            InputPurpose::StashMessage { kind } => {
                self.close_input_prompt();
                let message = (!value.is_empty()).then_some(value.as_str());
                self.perform_stash(kind, message);
            }
            InputPurpose::MacroReplayCount => {
                self.close_input_prompt();
                match value.trim().parse::<usize>() {
                    Ok(n) if n > 0 => self.replay_last_macro(n),
                    _ => self.status = format!("Not a repeat count: {}", value.trim()),
                }
            }
            InputPurpose::ArchiveExtract { member } => {
                self.close_input_prompt();
                let dest = PathBuf::from(value.trim());
                let Some((path, kind)) = self
                    .editor
                    .archive
                    .as_ref()
                    .and_then(|v| Some((self.editor.path.clone()?, v.kind)))
                else {
                    return;
                };
                match crate::archive::extract_member(&path, kind, &member, &dest) {
                    Ok(p) => self.status = format!("Extracted to {}", p.display()),
                    Err(e) => self.status = format!("Extract failed: {e}"),
                }
            }
            InputPurpose::NewWorktreeLane => {
                self.close_input_prompt();
                self.create_worktree_lane(&value);
            }
            InputPurpose::SyncConfigHost => {
                self.close_input_prompt();
                self.sync_config_to(&value);
            }
            InputPurpose::PullRequestNumber => {
                self.close_input_prompt();
                self.submit_pr_number(&value);
            }
            InputPurpose::CodeqlDatabase { source } => {
                self.close_input_prompt();
                self.submit_codeql_database(source, &value);
            }
            InputPurpose::CodeqlRemoveDatabase { path } => {
                self.close_input_prompt();
                self.perform_remove_codeql_database(&path);
            }
            InputPurpose::CodeqlDeleteUnusedDatabases { paths } => {
                self.close_input_prompt();
                self.perform_delete_unused_codeql_databases(&paths);
            }
            InputPurpose::CodeqlRenameDatabase { path } => {
                self.close_input_prompt();
                self.submit_rename_codeql_database(&path, &value);
            }
            InputPurpose::CodeqlRemoveHistory { output } => {
                self.close_input_prompt();
                self.perform_remove_codeql_history(&output);
            }
            InputPurpose::CodeqlRenameHistory { output } => {
                self.close_input_prompt();
                self.submit_rename_codeql_history(&output, &value);
            }
            InputPurpose::CodeqlCreateQuery { dir, language } => {
                self.close_input_prompt();
                self.submit_create_codeql_query(&dir, language.as_deref(), &value);
            }
            InputPurpose::CodeqlControllerRepository => {
                self.close_input_prompt();
                self.submit_codeql_controller(&value);
            }
            InputPurpose::CodeqlAddVariantRepo { list } => {
                self.close_input_prompt();
                self.submit_add_codeql_variant_repo(list.as_deref(), &value);
            }
            InputPurpose::CodeqlAddVariantList => {
                self.close_input_prompt();
                self.submit_add_codeql_variant_list(&value);
            }
            InputPurpose::CodeqlAddVariantOwner => {
                self.close_input_prompt();
                self.submit_add_codeql_variant_owner(&value);
            }
            InputPurpose::CodeqlDownloadPacks => {
                self.close_input_prompt();
                self.submit_download_codeql_packs(&value);
            }
            InputPurpose::CodeqlRunPublishedPack => {
                self.close_input_prompt();
                self.run_codeql_published_pack(&value);
            }
            InputPurpose::CodeqlExportVariantResults { index } => {
                self.close_input_prompt();
                self.start_export_codeql_variant_results(index, &value);
            }
            InputPurpose::CodeqlExportResults { output } => {
                self.close_input_prompt();
                self.submit_export_codeql_results(&output, &value);
            }
            InputPurpose::CodeqlVariantCodeSearch { list } => {
                self.close_input_prompt();
                self.start_codeql_variant_code_search(list, &value);
            }
            InputPurpose::CodeqlRenameVariantList { name } => {
                self.close_input_prompt();
                self.submit_rename_codeql_variant_list(&name, &value);
            }
            InputPurpose::CodeqlRemoveVariantList { name } => {
                self.close_input_prompt();
                self.perform_remove_codeql_variant_list(&name);
            }
            InputPurpose::SarifAddLog => {
                self.close_input_prompt();
                self.submit_sarif_add_log(&value);
            }
            InputPurpose::SarifColumns => {
                self.close_input_prompt();
                self.submit_sarif_columns(&value);
            }
            InputPurpose::SarifBaseline => {
                self.close_input_prompt();
                self.submit_sarif_baseline(&value);
            }
            InputPurpose::SarifExport => {
                self.close_input_prompt();
                self.submit_sarif_export(&value);
            }
            InputPurpose::DismissAlertComment { number, reason } => {
                self.close_input_prompt();
                self.dismiss_code_scanning_alert(number, reason, &value);
            }
            InputPurpose::SarifLocate { .. } => {
                self.close_input_prompt();
                self.submit_sarif_locate(&value);
            }
            InputPurpose::CodeqlModelKind { endpoint, choice } => {
                self.close_input_prompt();
                self.save_codeql_model(endpoint, choice, &value);
            }
            InputPurpose::SarifTrustHost { uri, host, url } => {
                self.close_input_prompt();
                if value.trim().is_empty() {
                    if let Err(e) = crate::sarif::resolve::trust_host(&host) {
                        self.status = format!("Could not save {host} as trusted: {e}");
                        return;
                    }
                    self.start_sarif_download(&uri, &url);
                } else {
                    self.submit_sarif_locate(&value);
                }
            }
            InputPurpose::SettingValue { key } => {
                self.close_input_prompt();
                self.submit_setting_value(&key, &value);
            }
            InputPurpose::FleetCommand => {
                // Closed FIRST, like every sibling arm. Leaving it open hides
                // the status line the run writes, so the user cannot see the
                // answer they just asked for.
                self.close_input_prompt();
                self.run_fleet_command(&value);
            }
            InputPurpose::EmmetWrap => {
                self.close_input_prompt();
                self.status = if self.editor.emmet_wrap(&value) {
                    format!("Emmet: wrapped with {}", value.trim())
                } else {
                    format!(
                        "Emmet: {} is not an abbreviation to wrap with",
                        value.trim()
                    )
                };
            }
            InputPurpose::EmmetUpdateTag => {
                self.close_input_prompt();
                self.status = if self.editor.emmet_update_tag(&value) {
                    format!("Emmet: tag renamed to {}", value.trim())
                } else {
                    format!(
                        "Emmet: {} is not a tag name, or no tag is around the cursor",
                        value.trim()
                    )
                };
            }
            InputPurpose::HexFind => {
                self.close_input_prompt();
                let text_side = self.editor.hex.as_ref().is_some_and(|v| v.ascii_focus);
                match crate::hex::HexView::parse_find_query(&value, text_side) {
                    Some(query) => {
                        let from = match self.editor.hex.as_mut() {
                            Some(view) => {
                                view.last_find = Some(query);
                                // The submit searches from the cursor
                                // INCLUSIVE so a match under it is
                                // found; F3 then steps past it.
                                view.cursor
                            }
                            None => return,
                        };
                        self.hex_find_run(from);
                    }
                    None => self.status = String::from("Hex find: empty query"),
                }
            }
            InputPurpose::SaveWorkspaceAs => {
                self.close_input_prompt();
                // As the other path prompts read a path (#1602): `~/` is the
                // home directory and a relative path is under the workspace.
                // Taken verbatim, `~/team.code-workspace` went into a new
                // folder named `~` under the launch directory, while the
                // status line said `~/`. A bare name gets the extension, as
                // VS Code gives it.
                let Some(mut dest) = self.typed_path_checked(&value) else {
                    self.status = String::from("Save workspace: cannot expand ~ (HOME is not set)");
                    return;
                };
                if dest.extension().is_none() {
                    dest.set_extension("code-workspace");
                }
                if !dest.exists() {
                    self.save_workspace_file(&dest);
                    return;
                }
                // The suggested path is where a team's committed file lives
                // (#1012): changing its folder list waits for a yes, and
                // one that already lists these folders is left alone.
                let folders: Vec<PathBuf> = self.roots.iter().map(Path::to_path_buf).collect();
                match crate::workspace::workspace_file_update(&dest, &folders) {
                    Ok(Some(_)) => {
                        let name = dest
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| dest.display().to_string());
                        self.open_input_prompt(InputPrompt::confirm(
                            InputPurpose::UpdateWorkspaceFile { dest },
                            format!("Change the folders listed in '{name}'?"),
                            "The rest of it stays · Enter or Y to save · Esc or N to cancel",
                        ));
                    }
                    Ok(None) => {
                        self.status =
                            format!("Workspace file already up to date: {}", dest.display());
                    }
                    Err(e) => self.status = format!("Save workspace: {e}"),
                }
            }
            InputPurpose::UpdateWorkspaceFile { dest } => {
                self.close_input_prompt();
                self.save_workspace_file(&dest);
            }
            InputPurpose::CloneUrl => {
                self.close_input_prompt();
                let parent = self
                    .tree
                    .root
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| self.tree.root.clone());
                let origin = self.tree.root.clone();
                self.spawn_git_net("clone", move || {
                    let r = crate::git::clone_into(&parent, &value);
                    Box::new(move |app: &mut App| app.finish_clone(r, &origin))
                });
            }
            InputPurpose::RenameBranch => {
                self.close_input_prompt();
                let r = crate::git::rename_branch(&self.scm_root(), &value);
                self.run_scm_op("branch -m", r, "Renamed branch", "Rename branch");
            }
            InputPurpose::CreateBranchFrom { base } => {
                self.close_input_prompt();
                let r = crate::git::create_branch_from(&self.scm_root(), &value, &base);
                self.run_scm_op("switch -c (from)", r, "Created branch", "Create branch");
            }
            InputPurpose::DebugConfigField { field } => {
                self.close_input_prompt();
                self.submit_debug_config_field(field, &value);
            }
            InputPurpose::LaunchInput => {
                // The typed text as is: a prompt's answer may mean its spaces.
                let value = self
                    .input_prompt
                    .as_ref()
                    .map_or(value, |p| p.value.clone());
                self.close_input_prompt();
                self.answer_input(value);
            }
            InputPurpose::AddRemoteName => {
                // First step: capture the name, then re-prompt for the URL.
                self.close_input_prompt();
                self.open_input_prompt(InputPrompt::new(
                    InputPurpose::AddRemoteUrl { name: value },
                    "Add Remote",
                    "Remote URL",
                ));
            }
            InputPurpose::AddRemoteUrl { name } => {
                self.close_input_prompt();
                let r = crate::git::add_remote(&self.scm_root(), &name, &value);
                self.run_scm_op("remote add", r, "Added remote", "Add remote");
            }
            InputPurpose::CreateTag => {
                // Then the message, as VS Code asks (#1582): one makes an
                // annotated tag, a blank field a lightweight one.
                self.close_input_prompt();
                self.open_input_prompt(
                    InputPrompt::new(
                        InputPurpose::CreateTagMessage {
                            name: value.clone(),
                        },
                        format!("Create Tag '{value}'"),
                        "Message (optional)",
                    )
                    .allowing_blank()
                    .with_hint(
                        "Enter with a message makes an annotated tag · blank, a lightweight one",
                    ),
                );
            }
            InputPurpose::CreateTagMessage { name } => {
                self.close_input_prompt();
                let message = (!value.is_empty()).then_some(value.as_str());
                let r = crate::git::create_tag(&self.scm_root(), &name, message);
                self.run_scm_op("tag", r, "Created tag", "Create tag");
            }
            InputPurpose::CreateTagAt { target } => {
                self.close_input_prompt();
                let r = crate::git::create_tag_at(&self.scm_root(), &value, &target);
                self.run_scm_op("tag (at)", r, "Created tag", "Create tag");
            }
            InputPurpose::DeleteRemoteTag { root, remote, name } => {
                self.close_input_prompt();
                if self.remote_tag_root_moved(&root) {
                    return;
                }
                self.spawn_scm_op(
                    "push --delete",
                    "Deleted remote tag",
                    "Delete remote tag",
                    move |root| crate::git::delete_remote_tag(root, &remote, &name),
                );
            }
            InputPurpose::ViewerConsent { key, path } => {
                // The user allowed the viewer's extension; record it and
                // resume the open that asked. The viewer is looked up first:
                // one removed while the prompt was up gets a status line and
                // no consent, since what was allowed no longer exists.
                self.close_input_prompt();
                let Some(viewer) = crate::mcp::registry::viewer_by_id_in_dir(
                    &self.config_dir,
                    &self.disabled_extensions,
                    &key,
                ) else {
                    self.status = format!("{key} is no longer available");
                    return;
                };
                // The session grant first, so the open passes the gate; the
                // prefs write after, so a failure it reports is the status
                // the user is left with rather than one the open overwrote.
                self.consented_extensions.insert(viewer.ext_id.clone());
                self.open_in_viewer(&viewer, &path);
                self.persist_extension_consent(&viewer.ext_id);
            }
            InputPurpose::McpConsent { command_id } => {
                // The user confirmed; record consent for this command's
                // extension and resume the command (which now passes the gate
                // and proceeds to its argument prompt or runs).
                self.close_input_prompt();
                // The session grant first, so the run passes the gate, and
                // the prefs write before the run here: a sidecar command's
                // run ends on its own argument prompt with no status of its
                // own, so a failure reported after it would sit under that
                // prompt and be cleared with it.
                let ext_id = crate::mcp::registry::resolve_command_in_dir(
                    &self.config_dir,
                    &self.disabled_extensions,
                    &command_id,
                )
                .map(|r| r.ext_id);
                if let Some(ext_id) = &ext_id {
                    self.consented_extensions.insert(ext_id.clone());
                    self.persist_extension_consent(ext_id);
                }
                self.run_extension_command(&command_id);
            }
            InputPurpose::McpArg { command_id } => {
                self.close_input_prompt();
                if let Some(resolved) = crate::mcp::registry::resolve_command_in_dir(
                    &self.config_dir,
                    &self.disabled_extensions,
                    &command_id,
                ) {
                    self.spawn_mcp_command(resolved, Some(value));
                }
            }
            InputPurpose::ExtensionUninstall { id } => {
                self.close_input_prompt();
                self.perform_extension_uninstall(&id);
            }
            InputPurpose::TreeDelete { paths } => {
                self.close_input_prompt();
                self.perform_delete_paths(paths);
            }
            InputPurpose::ReloadConflict { paths } => {
                self.close_input_prompt();
                if !value.eq_ignore_ascii_case("reload") {
                    self.status = String::from(
                        "Kept your unsaved edits; saving asks before overwriting the disk version",
                    );
                    return;
                }
                // Recoverable from TIMELINE: no answer, however deliberate,
                // makes typed work unrecoverable (#910).
                let kept = self.keep_unsaved_buffers_in_history(&paths);
                // Every group: the prompt lists conflicts from the other
                // splits too, and reverting only the focused one left theirs
                // unreloaded with the prompt never coming back.
                let mut reverted = self.editor.revert_paths_to_disk(&paths);
                for group in self.editor_layout.inactive_groups_mut() {
                    for p in group.revert_paths_to_disk(&paths) {
                        if !reverted.contains(&p) {
                            reverted.push(p);
                        }
                    }
                }
                self.sync_open_file_poll_mtime();
                // A failed reload (file deleted since the popup opened) keeps
                // its unsaved edits — say so instead of silently doing nothing.
                let failed = paths.len().saturating_sub(reverted.len());
                self.status = match (reverted.len(), failed) {
                    (0, _) => String::from("Reload from disk failed; unsaved edits kept"),
                    (1, 0) => format!("Reloaded {} from disk", reverted[0].display()),
                    (n, 0) => format!("Reloaded {n} files from disk"),
                    (n, f) => format!("Reloaded {n} files; {f} failed and kept their edits"),
                };
                if kept > 0 && !reverted.is_empty() {
                    self.status
                        .push_str("; the discarded edits are in Local History (TIMELINE)");
                }
            }
            InputPurpose::AskNavigator {
                file,
                range,
                selection,
            } => {
                // The prompt closes only on a SENT ask: a failure (navigator
                // gone or gone busy while the user typed) keeps the box open
                // with the draft intact for a retry, like a failed comment
                // reply keeps its draft.
                let Some(host) = &self.pair_host else {
                    self.status = String::from("Navigator is not active");
                    return;
                };
                let content = self.editor.lines.join("\n");
                let name = host.name().to_string();
                let result = host.send_ask_turn(&file, range, &selection, &value, &content);
                match result {
                    Ok(()) => {
                        self.close_input_prompt();
                        self.status = format!("Asked {name} about {file}:{}", range.0 + 1);
                        // Anchor the turn's prose commentary where it was asked.
                        self.pair_turn_origin = Some((file, range.0));
                    }
                    Err(e) => self.status = format!("Ask Navigator failed: {e}"),
                }
            }
        }
    }

    /// Act on the row chosen in the list picker.
    pub(super) fn confirm_list_picker(&mut self) {
        use crate::widgets::list_picker::ListPurpose;
        let Some(picker) = self.list_picker.as_ref() else {
            return;
        };
        let Some(row) = picker.selected_row().cloned() else {
            return;
        };
        let purpose = picker.purpose;
        let selected = picker.selected;
        let question = self.lsp_question.take();
        self.close_list_picker();
        let index = row.id.parse::<usize>().unwrap_or(0);
        match purpose {
            ListPurpose::ServerQuestion => {
                if let Some(q) = question
                    && let Some(action) = q.actions.get(index).cloned()
                {
                    self.status = format!("{}: {}", q.server, action.title);
                    let _ = q.reply.send(Some(action));
                }
            }
            ListPurpose::AgentLaneFile => {
                if let Some((agent, path)) = self.pending_lane_picks.get(index).cloned() {
                    self.diff_agent_lane_row(&agent, &path);
                }
            }
            ListPurpose::StashApply => {
                let r = crate::git::stash_apply(&self.scm_root(), index);
                self.run_scm_op("stash apply", r, "Applied stash", "Apply stash");
            }
            ListPurpose::StashPop => {
                let r = crate::git::stash_pop_at(&self.scm_root(), index);
                self.run_scm_op("stash pop", r, "Popped stash", "Pop stash");
            }
            ListPurpose::StashView => self.open_stash_patch(index),
            ListPurpose::BlockedSwitch => self.stash_and_switch_picked(row.id == "migrate"),
            // Asked first: the drop cannot be undone (#1128).
            ListPurpose::StashDrop => {
                let root = self.scm_root();
                match crate::git::stash_oid(&root, index) {
                    Ok(oid) => {
                        self.pending_stash_drop = Some(PendingStashDrop {
                            root,
                            oid,
                            label: row.label,
                        });
                    }
                    Err(err) => {
                        self.status = crate::git::failure_line("Drop stash", &err);
                    }
                }
            }
            ListPurpose::RemoveRemote => {
                let r = crate::git::remove_remote(&self.scm_root(), &row.id);
                self.run_scm_op("remote remove", r, "Removed remote", "Remove remote");
            }
            ListPurpose::DeleteTag => {
                let r = crate::git::delete_tag(&self.scm_root(), &row.id);
                self.run_scm_op("tag -d", r, "Deleted tag", "Delete tag");
            }
            ListPurpose::DeleteRemoteTag => {
                // Deleting from a remote reaches everyone who fetches it,
                // so it is confirmed first (#1582).
                let Some(root) = self.remote_tag_root.take() else {
                    return;
                };
                if self.remote_tag_root_moved(&root) {
                    return;
                }
                if let Some((remote, name)) = row.id.split_once('\t') {
                    use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
                    self.open_input_prompt(InputPrompt::confirm(
                        InputPurpose::DeleteRemoteTag {
                            root,
                            remote: remote.to_string(),
                            name: name.to_string(),
                        },
                        format!("Delete tag '{name}' from {remote}?"),
                        "Enter or Y to delete · Esc or N to keep",
                    ));
                }
            }
            ListPurpose::PushToRemote => {
                let remote = row.id.clone();
                self.spawn_scm_op("push", "Pushed", "Push", move |root| {
                    crate::git::push_to_remote(root, &remote)
                });
            }
            ListPurpose::MergeConflict => {
                let res = match row.id.as_str() {
                    "current" => crate::merge::Resolution::Current,
                    "both" => crate::merge::Resolution::Both,
                    _ => crate::merge::Resolution::Incoming,
                };
                self.resolve_merge_at_cursor(res);
            }
            ListPurpose::CodeAction => {
                // The navigator row's id is not numeric, and the shared
                // `index` above parses non-numbers to 0 — routing through
                // it would silently apply the server's FIRST action
                // instead. Branch on the id itself (#374 review).
                if row.id == "navigator" {
                    if let Some((path, item)) = self.navigator_fix_candidate() {
                        // The server's titles ride along as hints.
                        let hints = self
                            .pending_code_actions
                            .iter()
                            .map(|a| a.title.clone())
                            .collect();
                        self.fix_problem_with_navigator(path, item, hints);
                    }
                } else if let Some(item) = self.pending_code_actions.get(index).cloned() {
                    self.apply_code_action(item);
                }
                self.pending_code_actions.clear();
            }
            ListPurpose::ColorPresentation => {
                // The edits were computed against a specific buffer state;
                // an external reload while the picker was open orphans
                // them (#277 review) — refuse rather than splice at
                // stale offsets.
                let context_ok = self.pending_color_context.take().is_some_and(|(p, seq)| {
                    self.editor.path.as_deref() == Some(p.as_path()) && self.editor.edit_seq == seq
                });
                if context_ok {
                    if let Some((label, edits)) =
                        self.pending_color_presentations.get(index).cloned()
                    {
                        let applied = self.editor.apply_span_edits(&edits);
                        self.status = if applied > 0 {
                            format!("Color rewritten as {label}")
                        } else {
                            String::from("The presentation's edit no longer applies")
                        };
                    }
                } else {
                    self.status =
                        String::from("The buffer changed — re-run Change Color Presentation");
                }
                self.pending_color_presentations.clear();
            }
            ListPurpose::DismissAlert => {
                if let Some((number, reason)) = row.id.split_once(':') {
                    let number = number.parse().unwrap_or(0);
                    let reason = reason.parse().unwrap_or(0);
                    use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
                    self.open_input_prompt(InputPrompt::new(
                        InputPurpose::DismissAlertComment { number, reason },
                        format!("Dismiss Alert #{number}"),
                        String::from("comment (optional)"),
                    ));
                }
            }
            ListPurpose::CodeqlModel => self.choose_codeql_model(&row.id),
            ListPurpose::CodeqlDbSource => {
                if let Some((action, _)) = CODEQL_DB_SOURCES.get(index) {
                    self.activate_codeql(crate::widgets::codeql::Hit::Action(*action));
                }
            }
            ListPurpose::CodeqlDatabase => {
                if let Ok(i) = row.id.parse::<usize>() {
                    self.activate_codeql(crate::widgets::codeql::Hit::Action(
                        crate::widgets::codeql::Action::SelectDatabase(i),
                    ));
                }
            }
            ListPurpose::CodeqlMultiDb => self.choose_codeql_multi_db(&row.id, selected),
            ListPurpose::CodeqlResultSet => {
                let path = PathBuf::from(&row.id);
                match self.editor.open(&path) {
                    Ok(()) => self.status = format!("Result set {}", row.label),
                    Err(e) => self.status = format!("{}: {e}", path.display()),
                }
            }
            ListPurpose::CodeScanningAnalysis => {
                if let Ok(id) = row.id.parse::<u64>() {
                    self.open_code_scanning_analysis(id);
                }
            }
            ListPurpose::RunTask => {
                if let Some(task) = self.run_tasks.get(index).cloned() {
                    self.start_project_task(task);
                }
            }
            ListPurpose::ReviewPullRequest => {
                if row.id == crate::widgets::list_picker::PR_BY_NUMBER {
                    self.open_pr_number_prompt();
                } else {
                    self.start_pr_review(row.id.clone());
                }
            }
            ListPurpose::AttachProcess => {
                if let Some((rc, slot)) = row
                    .id
                    .parse::<i64>()
                    .ok()
                    .and_then(|pid| self.take_pending_attach(pid))
                {
                    self.launch_resolved_in(rc, slot);
                }
            }
            ListPurpose::DebugConfigType => {
                self.debug_config_draft = Some(crate::dap::configs::ConfigDraft::new(
                    &row.id,
                    crate::dap::configs::RequestKind::Launch,
                ));
                self.open_list_picker(
                    crate::widgets::list_picker::ListPicker::new(
                        ListPurpose::DebugConfigRequest,
                        "Add Configuration: Launch or Attach",
                        vec![
                            crate::widgets::list_picker::ListRow {
                                id: String::from("launch"),
                                label: String::from(
                                    "Launch — start the program under the debugger",
                                ),
                            },
                            crate::widgets::list_picker::ListRow {
                                id: String::from("attach"),
                                label: String::from("Attach — to a program already running"),
                            },
                        ],
                    ),
                    "",
                );
            }
            ListPurpose::LaunchInput => {
                self.answer_input(row.id.clone());
            }
            ListPurpose::StepInTarget => {
                if let Ok(target) = row.id.parse::<i64>() {
                    self.debug_step_into(Some(target));
                }
            }
            ListPurpose::DebugConfigRequest => {
                if let Some(draft) = self.debug_config_draft.as_mut() {
                    draft.request = if row.id == "attach" {
                        crate::dap::configs::RequestKind::Attach
                    } else {
                        crate::dap::configs::RequestKind::Launch
                    };
                    let first = draft.next_field(None);
                    self.prompt_debug_config_field(first, None);
                }
            }
            ListPurpose::DebugConfig => {
                if let Some(idx) = row.id.strip_prefix("compound:") {
                    // Index the row straight through rather than round-tripping
                    // via the name. `discover_compounds` dedupes by name, so a
                    // lookup would find the same element today — but it asks a
                    // weaker question than the one the row already answers, and
                    // it made the not-found arm unreachable-but-present.
                    let found = idx
                        .parse::<usize>()
                        .ok()
                        .and_then(|i| self.debug_compounds.get(i));
                    if let Some(compound) = found.cloned() {
                        self.selected_debug_config = None;
                        self.selected_debug_compound = Some(compound.name.clone());
                        self.run_debug.selected_config = Some(compound.name.clone());
                        self.launch_with_unsaved_saved(|app| app.launch_compound(&compound));
                    }
                } else if row.id == "add" {
                    self.open_add_debug_config();
                } else if row.id == "active" {
                    self.selected_debug_config = None;
                    self.selected_debug_compound = None;
                    self.run_debug.selected_config = None;
                    self.start_selected_debug();
                } else if let Some(cfg) = row
                    .id
                    .parse::<usize>()
                    .ok()
                    .and_then(|i| self.debug_configs.get(i))
                    .cloned()
                {
                    self.selected_debug_config = Some(cfg.name.clone());
                    self.selected_debug_compound = None;
                    self.run_debug.selected_config = Some(cfg.name.clone());
                    self.launch_with_unsaved_saved(|app| app.launch_debug_config(&cfg));
                }
            }
            ListPurpose::SessionParticipant => {
                if let Ok(id) = row.id.parse::<u64>() {
                    self.open_participant_actions(id);
                }
            }
            ListPurpose::SessionParticipantAction => {
                if let Some((verb, id)) = parse_participant_action(&row.id) {
                    self.run_participant_action(verb, id);
                }
            }
            ListPurpose::ExportComments => {
                if row.id == "post" {
                    self.post_exported_comments();
                } else {
                    // A comment row is information; the preview stays up
                    // until the post row or Esc.
                    self.show_export_preview();
                }
            }
            ListPurpose::ReviewVerdict => {
                use crate::review_threads::ReviewEvent;
                self.review_verdict = Some(match row.id.as_str() {
                    "APPROVE" => ReviewEvent::Approve,
                    "REQUEST_CHANGES" => ReviewEvent::RequestChanges,
                    _ => ReviewEvent::Comment,
                });
                self.prompt = Some(Prompt {
                    label: String::from("Review summary"),
                    buffer: String::new(),
                    caret: usize::MAX,
                    kind: PromptKind::ReviewSummary,
                    target_dir: self.tree.root.clone(),
                    error: None,
                });
            }
            ListPurpose::Profiles => match row.id.as_str() {
                "new" => {
                    let target_dir = self.tree.root.clone();
                    self.prompt = Some(Prompt {
                        label: String::from("New profile name"),
                        buffer: String::new(),
                        caret: usize::MAX,
                        kind: PromptKind::NewProfile,
                        target_dir,
                        error: None,
                    });
                }
                "workspace" => {
                    let active = crate::profiles::active();
                    match crate::profiles::set_workspace_default(&self.tree.root, active.as_deref())
                    {
                        Ok(()) => {
                            self.status = format!(
                                "This workspace now opens with profile {}",
                                active.unwrap_or_default()
                            )
                        }
                        Err(e) => self.status = format!("Could not save: {e}"),
                    }
                }
                "workspace-clear" => {
                    match crate::profiles::set_workspace_default(&self.tree.root, None) {
                        Ok(()) => {
                            self.status = String::from("This workspace no longer picks a profile")
                        }
                        Err(e) => self.status = format!("Could not save: {e}"),
                    }
                }
                id => {
                    if let Some(name) = id.strip_prefix("profile:") {
                        let name = (!name.is_empty()).then(|| name.to_string());
                        self.switch_profile(name);
                    }
                }
            },
            ListPurpose::KeyboardShortcuts => {
                if let Some(cmd) = row
                    .id
                    .strip_prefix("kb:")
                    .and_then(crate::widgets::command_palette::Command::from_id)
                {
                    self.recording_shortcut = Some((cmd, std::time::Instant::now()));
                    self.status = format!(
                        "Press the new shortcut for \u{201c}{}\u{201d} (a function key or a chord with Cmd/Ctrl/Alt; Esc cancels)",
                        cmd.title()
                    );
                }
            }
            ListPurpose::Bookmark => {
                // The row's own path, not one parsed back from text: a
                // file name that is not UTF-8 would not survive the trip.
                if let Some(target) = self.pending_bookmarks.get(index).cloned() {
                    self.jump_to_bookmark(target);
                }
                self.pending_bookmarks.clear();
            }
            ListPurpose::OutputChannel => {
                self.output.sync();
                self.output.select_by_name(&row.id);
                self.set_bottom_panel_tab(BottomPanelTab::Output);
            }
            ListPurpose::PanelAlignment => {
                if let Some((al, ..)) = PanelAlignment::OPTIONS
                    .iter()
                    .find(|(_, id, _)| *id == row.id)
                {
                    self.set_panel_alignment(*al);
                }
            }
            ListPurpose::NotebookKernel => self.choose_notebook_kernel(&row.id),
            ListPurpose::OpenTab => {
                if self.editor.select(index) {
                    self.sync_open_file_poll_mtime();
                }
                self.focus_pane(Pane::Editor);
                self.poke_cursor();
            }
            ListPurpose::HttpEnvironment => {
                self.status = if row.id.is_empty() {
                    String::from("HTTP: no environment")
                } else {
                    format!("HTTP: environment {}", row.id)
                };
                self.http_environment = (!row.id.is_empty()).then_some(row.id);
            }
            ListPurpose::QuickInputPosition => {
                if let Some((pos, ..)) = QuickInputPosition::OPTIONS
                    .iter()
                    .find(|(_, id, _)| *id == row.id)
                {
                    self.set_quick_input_position(*pos);
                }
            }
            ListPurpose::Settings => {
                match row.id.as_str() {
                    "toggle:format_on_save" => self.toggle_format_on_save(),
                    "toggle:auto_close_pairs" => self.toggle_auto_close_pairs(),
                    "toggle:auto_save" => self.toggle_auto_save(),
                    "toggle:auto_save_on_focus_change" => self.toggle_auto_save_on_focus_change(),
                    "toggle:inline_blame" => self.toggle_inline_blame(),
                    "toggle:indent_guides" => self.toggle_indent_guides(),
                    "toggle:bracket_colors" => self.toggle_bracket_colors(),
                    "toggle:render_whitespace" => self.toggle_render_whitespace(),
                    "toggle:line_numbers" => self.cycle_line_numbers(),
                    "toggle:vim_mode" => self.toggle_vim_mode(),
                    "toggle:inline_values" => self.toggle_inline_values(),
                    "toggle:inlay_hints" => self.toggle_inlay_hints(),
                    "toggle:copy_on_select" => self.toggle_copy_on_select(),
                    "toggle:log_highlight" => self.toggle_log_highlight(),
                    "toggle:secret_redaction" => self.toggle_secret_redaction(),
                    "toggle:format_on_type" => self.toggle_format_on_type(),
                    "toggle:code_lens" => self.toggle_code_lens(),
                    "toggle:terminal_suggestions" => self.toggle_terminal_suggestions(),
                    "toggle:screen_reader" => self.toggle_screen_reader(),
                    "cmd:keyboard_shortcuts" => {
                        self.open_keyboard_shortcuts();
                        return;
                    }
                    "cmd:color_theme" => {
                        self.open_theme_picker();
                        return;
                    }
                    "cmd:open_settings_json" => {
                        self.open_config_file_in_editor(
                            crate::prefs::config_path(),
                            ConfigFileSeed::Settings,
                        );
                        return;
                    }
                    "cmd:open_user_settings_local_json" => {
                        self.open_config_file_in_editor(
                            crate::config_layers::user_local_config_path(),
                            ConfigFileSeed::SettingsLayer,
                        );
                        return;
                    }
                    "cmd:open_workspace_settings_json" => {
                        self.open_workspace_settings(false);
                        return;
                    }
                    "cmd:open_workspace_settings_local_json" => {
                        self.open_workspace_settings(true);
                        return;
                    }
                    "cmd:open_keybindings_json" => {
                        self.open_config_file_in_editor(
                            crate::keymap::keybindings_path(),
                            ConfigFileSeed::Keybindings,
                        );
                        return;
                    }
                    "cmd:configure_snippets" => {
                        self.open_config_file_in_editor(
                            crate::snippets::snippets_path(),
                            ConfigFileSeed::Snippets,
                        );
                        return;
                    }
                    _ => return,
                }
                // A toggle stays in the hub: reopen so the new value shows and
                // the user can flip more without reinvoking the palette.
                self.open_settings_view();
            }
        }
    }

    pub(super) fn handle_input_prompt_key(&mut self, key: KeyEvent) {
        let Some(prompt) = self.input_prompt.as_mut() else {
            return;
        };
        // A yes/no question (#855) has no field: letters answer it, and
        // only unmodified ones (Shift aside), so a chord cannot.
        if prompt.confirm {
            let plain = key.modifiers.difference(KeyModifiers::SHIFT).is_empty();
            match key.code {
                KeyCode::Enter => self.submit_input_prompt(),
                KeyCode::Char('y' | 'Y') if plain => self.submit_input_prompt(),
                KeyCode::Esc => self.close_input_prompt(),
                KeyCode::Char('n' | 'N') if plain => self.close_input_prompt(),
                _ => {}
            }
            return;
        }
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => self.close_input_prompt(),
            (KeyCode::Enter, _) => self.submit_input_prompt(),
            (KeyCode::Left, _) => prompt.move_cursor_left(),
            (KeyCode::Right, _) => prompt.move_cursor_right(),
            (KeyCode::Home, _) => prompt.move_cursor_home(),
            (KeyCode::End, _) => prompt.move_cursor_end(),
            // Replacing a prefilled value took a Backspace per character,
            // typed blind (#1601): these select or clear it as the shell
            // and VS Code do, and a word goes back to a path separator.
            (KeyCode::Char('a'), KeyModifiers::CONTROL) => prompt.select_all(),
            (KeyCode::Char('a'), KeyModifiers::SUPER) => prompt.select_all(),
            (KeyCode::Char('u'), KeyModifiers::CONTROL) => prompt.clear_to_start(),
            (KeyCode::Char('k'), KeyModifiers::CONTROL) => prompt.clear_to_end(),
            (KeyCode::Char('w'), KeyModifiers::CONTROL) => prompt.delete_word_back(),
            (KeyCode::Backspace, m) if m.contains(KeyModifiers::SUPER) => prompt.clear_to_start(),
            (KeyCode::Backspace, m) if m.intersects(KeyModifiers::ALT | KeyModifiers::CONTROL) => {
                prompt.delete_word_back()
            }
            (KeyCode::Backspace, _) => prompt.pop_char(),
            (KeyCode::Delete, _) => prompt.delete_char(),
            (KeyCode::Char(c), m) if !m.intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER) => {
                prompt.push_char(c);
            }
            _ => {}
        }
    }

    pub(super) fn handle_input_prompt_mouse(&mut self, m: MouseEvent) {
        let Some(prompt) = self.input_prompt.as_ref() else {
            return;
        };
        let inside = rect_contains(prompt.last_rect, m.column, m.row);
        if matches!(
            m.kind,
            MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Down(MouseButton::Right)
        ) && !inside
        {
            self.close_input_prompt();
        }
    }

    pub(super) fn render_input_prompt(&mut self, frame: &mut ratatui::Frame) {
        let full = frame.area();
        let theme = self.theme;
        let Some(prompt) = self.input_prompt.as_mut() else {
            return;
        };
        crate::widgets::input_prompt::render_input_prompt(prompt, full, frame.buffer_mut(), theme);
    }

    pub(super) fn handle_list_picker_key(&mut self, key: KeyEvent) {
        let Some(picker) = self.list_picker.as_mut() else {
            return;
        };
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => self.dismiss_list_picker(),
            (KeyCode::Enter, _) => self.confirm_list_picker(),
            (KeyCode::Up, _) => picker.select_prev(),
            (KeyCode::Down, _) => picker.select_next(),
            (KeyCode::PageUp, _) => {
                for _ in 0..10 {
                    picker.select_prev();
                }
            }
            (KeyCode::PageDown, _) => {
                for _ in 0..10 {
                    picker.select_next();
                }
            }
            (KeyCode::Home, _) => picker.select_first(),
            (KeyCode::End, _) => picker.select_last(),
            (KeyCode::Left, _) => picker.move_cursor_left(),
            (KeyCode::Right, _) => picker.move_cursor_right(),
            (KeyCode::Backspace, _) => picker.pop_char(),
            (KeyCode::Char(c), m) if !m.intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER) => {
                picker.push_char(c);
            }
            _ => {}
        }
    }

    pub(super) fn handle_list_picker_mouse(&mut self, m: MouseEvent) {
        let Some(picker) = self.list_picker.as_mut() else {
            return;
        };
        match answer_mouse(picker, m) {
            PickerMouse::Close => self.dismiss_list_picker(),
            PickerMouse::Accept => self.confirm_list_picker(),
            PickerMouse::Stay => {}
        }
    }

    pub(super) fn render_list_picker(&mut self, frame: &mut ratatui::Frame) {
        let full = frame.area();
        let theme = self.theme;
        // Inline images are baked pixels the scrim cannot dim, so the
        // backdrop stays lit where the terminal draws them.
        let backdrop = !self.inline_images_enabled();
        let Some(picker) = self.list_picker.as_mut() else {
            return;
        };
        crate::widgets::list_picker::render_list_picker(
            picker,
            full,
            frame.buffer_mut(),
            theme,
            backdrop,
        );
    }

    pub(super) fn open_create_prompt(&mut self, kind: CreateKind, target_dir: PathBuf) {
        let label = match kind {
            CreateKind::File => format!("New File in {}", self.status_path(&target_dir)),
            CreateKind::Folder => format!("New Folder in {}", self.status_path(&target_dir)),
        };
        self.prompt = Some(Prompt {
            label,
            buffer: String::new(),
            caret: usize::MAX,
            kind: PromptKind::Create(kind),
            target_dir,
            error: None,
        });
    }

    pub(super) fn open_rename_prompt(&mut self, path: PathBuf) {
        let parent = path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.tree.root.clone());
        let current = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let label = format!("Rename {}", self.status_path(&path));
        self.prompt = Some(Prompt {
            label,
            buffer: current,
            caret: usize::MAX,
            kind: PromptKind::Rename(path),
            target_dir: parent,
            error: None,
        });
    }

    pub(super) fn handle_prompt_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.prompt = None;
                // Abandon any in-flight prepare-rename too: its verdict would
                // otherwise reopen this prompt at the caret the user just
                // walked away from (#254 item 6).
                self.prepare_rename_request = None;
                self.status = String::from("Cancelled");
            }
            KeyCode::Enter => self.commit_prompt(),
            // The caret walks and deletes the way the find bar's does
            // (#1729): Left/Right/Home/End, a word at a time with Ctrl, Cmd
            // or Alt, and Delete forward.
            KeyCode::Left | KeyCode::Right | KeyCode::Home | KeyCode::End => {
                let word = key_walks_by_word(key);
                if let Some(p) = self.prompt.as_mut() {
                    match key.code {
                        KeyCode::Left => {
                            p.walk(|t, c| crate::widgets::line_edit::caret_left(t, c, word))
                        }
                        KeyCode::Right => {
                            p.walk(|t, c| crate::widgets::line_edit::caret_right(t, c, word))
                        }
                        KeyCode::Home => p.caret = 0,
                        _ => p.caret = usize::MAX,
                    }
                }
            }
            KeyCode::Backspace => {
                let word = key_walks_by_word(key);
                if let Some(p) = self.prompt.as_mut() {
                    p.edit(|t, c| crate::widgets::line_edit::delete_before(t, c, word));
                }
            }
            KeyCode::Delete => {
                let word = key_walks_by_word(key);
                if let Some(p) = self.prompt.as_mut() {
                    p.edit(|t, c| crate::widgets::line_edit::delete_at(t, c, word));
                }
            }
            // Ctrl/Cmd+V that reaches us as a key rather than a bracketed
            // paste (bracketed paste off, or Cmd+V re-bound): read the
            // clipboard and insert at the caret, as Quick Open does.
            _ if is_clipboard_paste_key(key) => {
                if let Some(text) = self.clipboard_text()
                    && let Some(p) = self.prompt.as_mut()
                {
                    let text: String = text.chars().filter(|c| !c.is_control()).collect();
                    p.edit(|t, at| crate::widgets::line_edit::insert_at(t, at, &text));
                }
            }
            KeyCode::Char(c) => {
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT)
                    && !key.modifiers.contains(KeyModifiers::SUPER)
                    && let Some(p) = self.prompt.as_mut()
                {
                    p.edit(|t, at| crate::widgets::line_edit::insert_at(t, at, &c.to_string()));
                }
            }
            _ => {}
        }
    }

    pub(super) fn commit_prompt(&mut self) {
        let Some(prompt) = self.prompt.as_ref() else {
            return;
        };
        let kind = prompt.kind.clone();
        match kind {
            PromptKind::Create(create_kind) => {
                let name = prompt.buffer.trim().to_string();
                if let Err(msg) = crate::widgets::file_tree::validate_new_name(&name) {
                    if let Some(p) = self.prompt.as_mut() {
                        p.error = Some(msg.to_string());
                    }
                    return;
                }
                let target_dir = prompt.target_dir.clone();
                let result = match create_kind {
                    CreateKind::File => {
                        crate::widgets::file_tree::create_file_in(&target_dir, &name)
                    }
                    CreateKind::Folder => {
                        crate::widgets::file_tree::create_folder_in(&target_dir, &name)
                    }
                };
                match result {
                    Ok(path) => {
                        self.prompt = None;
                        self.status = match create_kind {
                            CreateKind::File => format!("Created file {}", self.status_path(&path)),
                            CreateKind::Folder => {
                                format!("Created folder {}", self.status_path(&path))
                            }
                        };
                        if let Some(idx) = self.tree.index_of_dir(&target_dir) {
                            self.tree.refresh_children(idx);
                            if let Some(new_idx) =
                                self.tree.nodes.iter().position(|n| n.path == path)
                            {
                                self.tree.select(new_idx);
                            }
                        }
                        if create_kind == CreateKind::File {
                            // A tab of its own: `open` reloads the ACTIVE tab
                            // in place, discarding its unsaved edits.
                            if let Err(e) = self.editor.open_pinned(&path) {
                                self.status = format!("Created but could not open: {e}");
                            } else {
                                self.sync_open_file_poll_mtime();
                                self.focus_pane(Pane::Editor);
                            }
                        }
                    }
                    Err(e) => {
                        if let Some(p) = self.prompt.as_mut() {
                            p.error = Some(e.to_string());
                        }
                    }
                }
            }
            PromptKind::Rename(old_path) => {
                let new_name = prompt.buffer.trim().to_string();
                let parent = prompt.target_dir.clone();
                // Validate now, while the prompt can still show the error;
                // the rename itself may wait on the servers' edit (#610).
                match crate::widgets::file_tree::rename_target(&parent, &old_path, &new_name) {
                    Ok(target) => {
                        self.prompt = None;
                        if target != old_path {
                            let op = FileMove::Rename {
                                parent,
                                old: old_path.clone(),
                                new_name,
                            };
                            let is_dir = old_path.is_dir();
                            self.start_file_move(
                                op,
                                vec![crate::lsp::manager::FileRenameOp {
                                    old: old_path,
                                    new: target,
                                    is_dir,
                                }],
                            );
                        }
                    }
                    Err(e) => {
                        if let Some(p) = self.prompt.as_mut() {
                            p.error = Some(e.to_string());
                        }
                    }
                }
            }
            PromptKind::SaveAs => {
                let answer = prompt.buffer.trim().to_string();
                let target = prompt.target_dir.join(&answer);
                let error = if answer.is_empty() {
                    Some(String::from("Enter a path to save to"))
                } else if self.editor.path.as_deref() != Some(target.as_path()) && target.exists() {
                    Some(format!("{answer} already exists"))
                } else {
                    self.save_as(&target).err()
                };
                match error {
                    Some(e) => {
                        if let Some(p) = self.prompt.as_mut() {
                            p.error = Some(e);
                        }
                    }
                    None => self.prompt = None,
                }
            }
            PromptKind::RenameSymbol { path, row, col } => {
                let new_name = prompt.buffer.trim().to_string();
                if new_name.is_empty() {
                    if let Some(p) = self.prompt.as_mut() {
                        p.error = Some("Name cannot be empty".to_string());
                    }
                    return;
                }
                // The prompt captured buffer coordinates; the server wants
                // UTF-16, so convert here rather than casting the char column.
                let (line, character) = self.editor.pos_to_utf16(row, col);
                let Some(lsp) = self.lsp.as_mut() else {
                    if let Some(p) = self.prompt.as_mut() {
                        p.error = Some("No language server for this file".to_string());
                    }
                    return;
                };
                let id = lsp.request_rename(path, line, character, new_name);
                self.rename_request_id = Some(id);
                self.rename_baseline = self.edit_seqs_in_every_group();
                self.prompt = None;
                self.status = String::from("Renaming symbol");
            }
            PromptKind::BreakpointCondition { path, line } => {
                let expr = prompt.buffer.clone();
                self.prompt = None;
                self.commit_breakpoint_condition(path, line, &expr);
            }
            PromptKind::Logpoint { path, line } => {
                let message = prompt.buffer.clone();
                self.prompt = None;
                self.commit_logpoint(path, line, &message);
            }
            PromptKind::HitCondition { path, line } => {
                let hits = prompt.buffer.clone();
                self.prompt = None;
                self.commit_hit_condition(path, line, &hits);
            }
            PromptKind::FunctionBreakpoint => {
                let name = prompt.buffer.trim().to_string();
                self.prompt = None;
                self.commit_function_breakpoint(name);
            }
            PromptKind::DataBreakpoint => {
                let name = prompt.buffer.trim().to_string();
                self.prompt = None;
                self.commit_data_breakpoint(&name);
            }
            PromptKind::ReviewComment => {
                let (body, rel) = (prompt.buffer.trim().to_string(), prompt.target_dir.clone());
                self.prompt = None;
                self.add_pending_review_comment(&body, rel);
            }
            PromptKind::StickyNote => {
                let body = prompt.buffer.trim().to_string();
                self.prompt = None;
                self.add_sticky_note(&body);
            }
            PromptKind::ReviewSummary => {
                let summary = prompt.buffer.trim().to_string();
                self.prompt = None;
                self.submit_review(summary);
            }
            PromptKind::NewProfile => {
                let name = prompt.buffer.trim().to_string();
                match crate::profiles::create_from_current(&name) {
                    Ok(()) => {
                        self.prompt = None;
                        self.switch_profile(Some(name));
                    }
                    Err(e) => {
                        if let Some(p) = self.prompt.as_mut() {
                            p.error = Some(e.to_string());
                        }
                    }
                }
            }
            PromptKind::RenameTerminal(idx) => {
                let name = prompt.buffer.trim().to_string();
                self.prompt = None;
                if let Some(t) = self.terminals.get_mut(idx) {
                    let cleared = name.is_empty();
                    t.set_manual_name(if cleared { None } else { Some(name.clone()) });
                    self.status = if cleared {
                        String::from("Cleared terminal name")
                    } else {
                        format!("Renamed terminal to {name}")
                    };
                    self.save_terminal_session();
                }
            }
            PromptKind::AnnotateTerminal {
                pane,
                line,
                clock,
                start,
                len,
                existing,
            } => {
                let text = prompt.buffer.trim().to_string();
                self.prompt = None;
                if let Some(t) = self.terminals.get_mut(pane) {
                    self.status = String::from(match (existing, text.is_empty()) {
                        (Some(i), true) => {
                            t.remove_annotation(i);
                            "Deleted annotation"
                        }
                        (Some(i), false) => {
                            t.set_annotation_text(i, text);
                            "Updated annotation"
                        }
                        (None, true) => "Annotation cancelled (empty note)",
                        (None, false) => {
                            t.add_annotation(line, clock, start, len, text);
                            "Annotated selection (click the span to read it)"
                        }
                    });
                }
            }
        }
    }
}

/// A path typed into a prompt: `~/` is `home`, and a relative path is under
/// `root`, the workspace, not the directory croft was started from. `None`
/// for `~/…` with no home: it is not a path under the workspace.
pub(super) fn resolve_typed_path(value: &str, home: Option<&Path>, root: &Path) -> Option<PathBuf> {
    let v = value.trim();
    let p = match v.strip_prefix("~/") {
        Some(rest) => home?.join(rest),
        None => PathBuf::from(v),
    };
    Some(if p.is_absolute() { p } else { root.join(p) })
}
