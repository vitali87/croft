//! The main loop's background pass (#2062): what croft's background work has
//! finished since the last tick (file changes, language-server replies, jobs,
//! workers, timers), collected in a fixed order before anything is drawn.
//! The main loop calls it once per tick and decides from its answer whether
//! to draw; a test calls it to run one real pass.

use super::*;

/// What one background pass found, for the main loop's redraw decision.
pub(super) struct BackgroundTick {
    /// Something the pass collected changed what is on screen.
    pub(super) changed: bool,
    /// A terminal pane has shell output not drawn yet. Read where the shell
    /// check has always sat in the pass, so a noisy shell redraws on the same
    /// ticks as before.
    pub(super) pty_pending: bool,
    /// Whether the caret is in the shown half of its blink, read at the same
    /// point of the pass; the loop compares it with the value it last drew.
    pub(super) blink_visible: bool,
    /// The spinners' phase, read at the same point; the loop compares it with
    /// the value it last drew.
    pub(super) spinner_phase: u128,
}

/// Run one background pass: every poll and drain, in the order the main loop
/// has always run them. Each call's own effects (the status line, a panel's
/// state) happen here; the answer says only whether the screen needs drawing.
pub(super) fn tick_background(app: &mut App) -> BackgroundTick {
    // Same-file tabs agree before the watcher is read: a save from one
    // symbol tab leaves its siblings holding the saved text, so the
    // change event reads as their own write, not a conflict (#369).
    let symbol_views_settled = app.sync_symbol_views();
    // A background task's crash notice comes before the polls that
    // settle the task, so a task that says what its crash cost, or how
    // to start it again, has the status line after it (#2609).
    let panic_changed = app.poll_background_panic();
    // Pull in any filesystem-watcher events first so the tree reflects
    // disk reality on the very next frame.
    let fs_changed = app.drain_fs_events();
    crate::startup_trace::mark("file changes read");
    // Peek without clearing so we can coalesce PTY-only redraws without
    // losing bytes that the reader thread has already advanced into the
    // terminal grid.
    let pty_pending = app.peek_terminals_dirty();
    let blink_visible = app.cursor_visible_phase();
    // Either spinner advancing must wake a redraw; both stay 0 when idle.
    let spinner_phase =
        app.update_spinner_phase() + app.mcp_spinner_phase() + app.problems_fix_spinner_phase();
    let ext_index_changed = app.drain_ext_index_refresh();
    let codeql_changed = app.drain_codeql_run()
        | app.drain_codeql_ast()
        | app.drain_codeql_model()
        | app.drain_codeql_doc()
        | app.drain_codeql_upgrade()
        | app.drain_codeql_version()
        | app.drain_codeql_log_summary()
        | app.drain_codeql_log_viewer()
        | app.drain_codeql_perf_compare()
        | app.drain_codeql_query_help()
        | app.drain_codeql_pack_job()
        | app.drain_codeql_db_job()
        | app.drain_codeql_cli_download()
        | app.drain_codeql_cli_check()
        | app.drain_codeql_code_search()
        | app.drain_codeql_variant_submit()
        | app.poll_codeql_variant_runs()
        | app.drain_codeql_variant_results()
        | app.drain_codeql_variant_export()
        | app.drain_codeql_queries();
    let search_changed = app.drain_search_results();
    let log_index_changed = app.poll_log_index();
    let remote_changed = app.refresh_remote_if_config_changed();
    let pulls_changed = app.drain_remote_pulls() | app.drain_relay_copies();
    let finder_changed = app.sync_file_finder_open_lines()
        | app
            .file_finder
            .as_mut()
            .is_some_and(|f| f.poll_line_check());
    let view_changed = app.drain_view_requests();
    let hook_changed = app.drain_hook_requests()
        | app.poll_pr_gh()
        | app.poll_pr_checkout()
        | app.poll_code_scanning();
    crate::startup_trace::mark("background results collected");
    app.sync_approval_check();
    app.persist_agent_ledger(false);
    let kernel_changed = app.poll_notebook_kernels();
    let ports_changed = app.drain_ports_and_poll();
    let session_presence_changed = app.poll_session_presence();
    let session_typing_changed = app.poll_session_typing();
    let collab_changed = app.poll_collab();
    crate::startup_trace::mark("agents, ports and collaborators polled");
    let symbol_views_changed = app.sync_symbol_views() | symbol_views_settled;
    let bells_changed = app.drain_terminal_bells();
    // A block whose command has finished gets its output box (#354).
    let captures_changed = app.settle_block_captures();
    // Every tick, not only when a capture settles: this is what notices
    // the editor has moved to another document (#354 review).
    app.publish_block_outputs();
    let labels_changed =
        app.refresh_terminal_labels() | app.drain_agent_events() | app.drain_fleet_results();
    app.flush_terminal_session();
    let auto_save_changed = app.tick_auto_save();
    app.tick_hot_exit(std::time::Instant::now());
    crate::startup_trace::mark("terminal panes and saves ticked");
    let highlights_changed = app.poll_highlights();
    // Pictures baked off the UI thread (#1973) go up as they land.
    let images_changed =
        app.poll_chrome_bake() | app.poll_welcome_bake() | app.poll_saved_session();
    // A closed tab's bookmarks are kept, and come back with its file.
    let bookmarks_changed = app.settle_bookmarks();
    crate::startup_trace::mark("highlights and pictures polled");
    let live_run_changed = app.tick_live_run()
        | app.sync_markdown_side_previews()
        | app.sync_markdown_scroll()
        | app.tick_minimap();
    app.tick_code_lens();
    let code_lens_changed = app.tick_config_watch()
        | app.drain_lsp_code_lens()
        | app.drain_search_editor()
        | app.tick_inline_complete()
        | app.drain_review_ops();
    let connect_changed = app.poll_connect_dialog() | app.drain_scrub_views();
    let install_changed = app.poll_install_session();
    let update_changed = app.poll_update_watch();
    let http_changed = app.drain_http_responses();
    let reveal_changed = app.tick_redaction_reveal();
    crate::startup_trace::mark("watchers and dialogs polled");
    app.sync_lsp();
    app.send_due_completion_ask();
    app.send_due_completion_reask();
    crate::startup_trace::mark("language servers synced");
    let markdown_lint_changed = app.sync_markdown_lint();
    let sarif_diagnostics_changed = app.sync_sarif_diagnostics();
    app.sync_sarif_selection_to_cursor();
    app.sync_sarif_step_marks();
    app.refresh_sarif_preview();
    app.poll_sarif_download();
    app.sync_git_gutters();
    app.sync_blame();
    app.sync_provenance();
    let blame_changed = app.drain_blame() | app.drain_git_net() | app.drain_git_view();
    crate::startup_trace::mark("lint, gutters and blame synced");
    // Request/refresh the OUTLINE for the active file (after sync_lsp so the
    // edit-seq it reads is current) and advance follow-cursor.
    let outline_sync_changed = app.sync_outline();
    let outline_changed = app.drain_lsp_document_symbols();
    app.workspace_symbols_dispatch_due();
    let ws_symbols_changed = app.drain_lsp_workspace_symbols();
    // Refresh the Explorer's other stacked sub-views (Open Editors list,
    // Timeline git history, Dependencies) and drain their workers.
    let explorer_panels_changed = app.sync_explorer_panels();
    crate::startup_trace::mark("outline and Explorer views synced");
    app.refresh_semantic_tokens_if_requested();
    app.refresh_inlay_hints_if_requested();
    app.refresh_workspace_diagnostics_if_requested();
    let lsp_changed = app.drain_lsp_completion();
    app.refresh_signature_help_if_moved();
    let sig_help_changed = app.drain_lsp_signature_help();
    app.poll_hover();
    let hover_changed =
        app.drain_lsp_hover() | app.drain_lsp_rust_analyzer() | app.drain_blame_hover();
    let tab_hover_changed = app.poll_tab_hover();
    let ui_tooltip_changed = app.poll_ui_tooltip();
    let definition_changed = app.drain_lsp_definition();
    let declaration_changed = app.drain_lsp_declaration();
    let type_definition_changed = app.drain_lsp_type_definition();
    let implementation_changed = app.drain_lsp_implementation();
    let references_changed = app.drain_lsp_references();
    let call_hierarchy_changed = app.drain_lsp_call_hierarchy();
    let sidebar_hide_changed = app.tick_sidebar_auto_hide();
    let occ_tick_changed = app.tick_occurrences();
    let linked_tick_changed = app.tick_linked_editing();
    let linked_changed = app.drain_lsp_linked_editing();
    app.tick_on_type_formatting();
    let on_type_changed = app.drain_lsp_on_type_format();
    let prepare_rename_changed = app.drain_lsp_prepare_rename();
    let occurrences_changed = app.drain_lsp_document_highlights();
    let rename_changed = app.drain_lsp_rename() | app.tick_file_moves();
    let format_changed = app.drain_lsp_format() | app.drain_prettier_format();
    let code_action_changed = app.drain_lsp_code_actions();
    let semantic_changed = app.drain_lsp_semantic_tokens();
    let inlay_changed = app.drain_lsp_inlay_hints();
    app.drain_lsp_document_links();
    let folds_changed = app.drain_lsp_folding_ranges();
    let selection_ranges_changed = app.drain_lsp_selection_ranges();
    let colors_changed = app.drain_lsp_document_colors();
    let color_pres_changed = app.drain_lsp_color_presentations();
    let diagnostics_changed = app.drain_lsp_diagnostics();
    let progress_changed = app.drain_lsp_progress();
    crate::startup_trace::mark("language server replies read");
    let dap_changed = app.poll_dap() | app.poll_debug_build();
    let tests_changed = app.test_worker.drain(&mut app.testing)
        | app.drain_pending_test_debug()
        | app.drain_pending_cargo_launch();
    app.say_why_test_runs_ended();
    // One notification per red run (#358), from the latch the panel
    // sets when a run ends, never per test case.
    let coverage_changed = app.sync_coverage();
    let watch_changed = app.tick_test_watch();
    if app.testing.take_failed_run() {
        let (passed, failed, _) = app.testing.counts();
        app.notifier.emit(
            crate::notifications::Event::TestsFailed {
                failed,
                passed,
                reported: app.testing.run_reported_failed(),
            },
            app.roots.primary(),
            &remote_host_label(),
        );
    }
    crate::startup_trace::mark("debugger and tests polled");
    let mcp_changed = app.poll_mcp();
    let pair_changed =
        app.maybe_seat_navigator() | app.poll_pair() | app.tick_proactive_navigator();
    crate::startup_trace::mark("AI tools polled");
    let voice_changed = app.drain_voice();
    let zen_hint_changed = app.expire_zen_hint(std::time::Instant::now());
    let cmd_k_changed = app.expire_cmd_k(std::time::Instant::now());
    let screencast_changed = app.screencast.expire(std::time::Instant::now());
    let zoxide_changed = app.poll_zoxide_install();
    // Surface managed language-server install progress in the status bar so
    // the background work (which can take a few seconds) is visible.
    let install_status_changed = match crate::lsp::install::take_status() {
        Some(msg) => {
            app.status = msg;
            true
        }
        None => false,
    };
    // When a managed install finishes, forget the LSP sync state for the
    // docs that language serves so the next `sync_lsp` re-opens them. That
    // re-open makes the manager re-probe and spawn the just-installed
    // server, so "installed" becomes "working" without a further action.
    let just_installed = crate::lsp::install::take_just_installed();
    if !just_installed.is_empty() {
        app.lsp_last_seen
            .retain(|path, _| !just_installed_covers_path(&just_installed, path));
    }

    let changed = zen_hint_changed
        || cmd_k_changed
        || screencast_changed
        || zoxide_changed
        || fs_changed
        || mcp_changed
        || pair_changed
        || tests_changed
        || coverage_changed
        || watch_changed
        || ext_index_changed
        || codeql_changed
        || search_changed
        || log_index_changed
        || remote_changed
        || pulls_changed
        || finder_changed
        || view_changed
        || hook_changed
        || kernel_changed
        || ports_changed
        || session_presence_changed
        || session_typing_changed
        || collab_changed
        || symbol_views_changed
        || bells_changed
        || captures_changed
        || labels_changed
        || auto_save_changed
        || highlights_changed
        || images_changed
        || bookmarks_changed
        || live_run_changed
        || code_lens_changed
        || ws_symbols_changed
        || connect_changed
        || install_changed
        || update_changed
        || panic_changed
        || http_changed
        || reveal_changed
        || lsp_changed
        || sig_help_changed
        || hover_changed
        || tab_hover_changed
        || ui_tooltip_changed
        || definition_changed
        || declaration_changed
        || type_definition_changed
        || implementation_changed
        || references_changed
        || call_hierarchy_changed
        || sidebar_hide_changed
        || occ_tick_changed
        || linked_tick_changed
        || linked_changed
        || on_type_changed
        || prepare_rename_changed
        || occurrences_changed
        || rename_changed
        || format_changed
        || code_action_changed
        || semantic_changed
        || inlay_changed
        || folds_changed
        || selection_ranges_changed
        || colors_changed
        || color_pres_changed
        || diagnostics_changed
        || progress_changed
        || voice_changed
        || outline_sync_changed
        || outline_changed
        || explorer_panels_changed
        || install_status_changed
        || blame_changed
        || dap_changed
        || markdown_lint_changed
        || sarif_diagnostics_changed;

    BackgroundTick {
        changed,
        pty_pending,
        blink_visible,
        spinner_phase,
    }
}
