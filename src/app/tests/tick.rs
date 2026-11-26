//! The main loop's background pass (#2062), called the way the loop calls it.

use super::*;

/// Every poll and drain the background pass calls on the app, in order, its
/// steps included. The order is part of what the pass does: a crash notice
/// comes before the polls that settle its task (#2609), the language servers
/// are synced before the outline reads their edit count, a run's failure is
/// announced after the Testing panel takes its results, and so on. A call added to the pass is
/// added here where it runs; moving calls between steps keeps this list as
/// it is.
const BACKGROUND_PASS_CALLS: &[&str] = &[
    "app.sync_symbol_views",
    "app.poll_background_panic",
    "app.drain_fs_events",
    "crate::startup_trace::mark",
    "app.peek_terminals_dirty",
    "app.cursor_visible_phase",
    "app.update_spinner_phase",
    "app.mcp_spinner_phase",
    "app.problems_fix_spinner_phase",
    "app.drain_ext_index_refresh",
    "app.drain_codeql_run",
    "app.drain_codeql_ast",
    "app.drain_codeql_model",
    "app.drain_codeql_doc",
    "app.drain_codeql_upgrade",
    "app.drain_codeql_version",
    "app.drain_codeql_log_summary",
    "app.drain_codeql_log_viewer",
    "app.drain_codeql_perf_compare",
    "app.drain_codeql_query_help",
    "app.drain_codeql_pack_job",
    "app.drain_codeql_db_job",
    "app.drain_codeql_cli_download",
    "app.drain_codeql_cli_check",
    "app.drain_codeql_code_search",
    "app.drain_codeql_variant_submit",
    "app.poll_codeql_variant_runs",
    "app.drain_codeql_variant_results",
    "app.drain_codeql_variant_export",
    "app.drain_codeql_queries",
    "app.drain_search_results",
    "app.poll_log_index",
    "app.refresh_remote_if_config_changed",
    "app.drain_remote_pulls",
    "app.drain_relay_copies",
    "app.sync_file_finder_open_lines",
    "app.file_finder.as_mut",
    "app.drain_view_requests",
    "app.drain_hook_requests",
    "app.poll_pr_gh",
    "app.poll_pr_checkout",
    "app.poll_code_scanning",
    "crate::startup_trace::mark",
    "app.sync_approval_check",
    "app.persist_agent_ledger",
    "app.poll_notebook_kernels",
    "app.drain_ports_and_poll",
    "app.poll_session_presence",
    "app.poll_session_typing",
    "app.poll_collab",
    "crate::startup_trace::mark",
    "app.sync_symbol_views",
    "app.drain_terminal_bells",
    "app.settle_block_captures",
    "app.publish_block_outputs",
    "app.refresh_terminal_labels",
    "app.drain_agent_events",
    "app.drain_fleet_results",
    "app.flush_terminal_session",
    "app.tick_auto_save",
    "app.tick_hot_exit",
    "crate::startup_trace::mark",
    "app.poll_highlights",
    "app.poll_chrome_bake",
    "app.poll_welcome_bake",
    "app.poll_saved_session",
    "app.settle_bookmarks",
    "crate::startup_trace::mark",
    "app.tick_live_run",
    "app.sync_markdown_side_previews",
    "app.sync_markdown_scroll",
    "app.tick_minimap",
    "app.tick_code_lens",
    "app.tick_config_watch",
    "app.drain_lsp_code_lens",
    "app.drain_search_editor",
    "app.tick_inline_complete",
    "app.drain_review_ops",
    "app.poll_connect_dialog",
    "app.drain_scrub_views",
    "app.poll_install_session",
    "app.poll_update_watch",
    "app.drain_http_responses",
    "app.tick_redaction_reveal",
    "crate::startup_trace::mark",
    "app.sync_lsp",
    "app.send_due_completion_ask",
    "app.send_due_completion_reask",
    "crate::startup_trace::mark",
    "app.sync_markdown_lint",
    "app.sync_sarif_diagnostics",
    "app.sync_sarif_selection_to_cursor",
    "app.sync_sarif_step_marks",
    "app.refresh_sarif_preview",
    "app.poll_sarif_download",
    "app.sync_git_gutters",
    "app.sync_blame",
    "app.sync_provenance",
    "app.drain_blame",
    "app.drain_git_net",
    "app.drain_git_view",
    "app.drain_git_file_lookup",
    "crate::startup_trace::mark",
    "app.sync_outline",
    "app.drain_lsp_document_symbols",
    "app.workspace_symbols_dispatch_due",
    "app.drain_lsp_workspace_symbols",
    "app.sync_explorer_panels",
    "crate::startup_trace::mark",
    "app.refresh_semantic_tokens_if_requested",
    "app.refresh_inlay_hints_if_requested",
    "app.refresh_workspace_diagnostics_if_requested",
    "app.drain_lsp_completion",
    "app.refresh_signature_help_if_moved",
    "app.drain_lsp_signature_help",
    "app.poll_hover",
    "app.drain_lsp_hover",
    "app.drain_lsp_rust_analyzer",
    "app.drain_blame_hover",
    "app.poll_tab_hover",
    "app.poll_ui_tooltip",
    "app.drain_lsp_definition",
    "app.drain_lsp_declaration",
    "app.drain_lsp_type_definition",
    "app.drain_lsp_implementation",
    "app.drain_lsp_references",
    "app.drain_lsp_call_hierarchy",
    "app.tick_sidebar_auto_hide",
    "app.tick_occurrences",
    "app.tick_linked_editing",
    "app.drain_lsp_linked_editing",
    "app.tick_on_type_formatting",
    "app.drain_lsp_on_type_format",
    "app.drain_lsp_prepare_rename",
    "app.drain_lsp_document_highlights",
    "app.drain_lsp_rename",
    "app.tick_file_moves",
    "app.drain_lsp_format",
    "app.drain_prettier_format",
    "app.drain_lsp_code_actions",
    "app.drain_lsp_semantic_tokens",
    "app.drain_lsp_inlay_hints",
    "app.drain_lsp_document_links",
    "app.drain_lsp_folding_ranges",
    "app.drain_lsp_selection_ranges",
    "app.drain_lsp_document_colors",
    "app.drain_lsp_color_presentations",
    "app.drain_lsp_diagnostics",
    "app.drain_lsp_progress",
    "crate::startup_trace::mark",
    "app.poll_dap",
    "app.poll_debug_build",
    "app.test_worker.drain",
    "app.drain_pending_test_debug",
    "app.drain_pending_cargo_launch",
    "app.say_why_test_runs_ended",
    "app.sync_coverage",
    "app.tick_test_watch",
    "app.testing.take_failed_run",
    "app.testing.counts",
    "app.notifier.emit",
    "app.testing.run_reported_failed",
    "app.roots.primary",
    "crate::startup_trace::mark",
    "app.poll_mcp",
    "app.maybe_seat_navigator",
    "app.poll_pair",
    "app.tick_proactive_navigator",
    "crate::startup_trace::mark",
    "app.drain_voice",
    "app.expire_zen_hint",
    "app.expire_cmd_k",
    "app.screencast.expire",
    "app.poll_zoxide_install",
    "crate::lsp::install::take_status",
    "crate::lsp::install::take_just_installed",
    "app.lsp_last_seen.retain",
];

/// The `app.` and `crate::` calls the background pass makes, in the order it
/// makes them: the body of `tick_background`, with each of its steps (a
/// function of `tick.rs` handed the app) read in where it is called.
/// Comments are left out.
fn background_pass_calls() -> Vec<String> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/app/tick.rs");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    let mut calls = Vec::new();
    read_pass_calls(&text, "tick_background", &mut calls);
    calls
}

/// Append the calls in the body of `tick.rs`'s function `name` to `calls`,
/// reading a step's calls in where the body calls the step.
fn read_pass_calls(text: &str, name: &str, calls: &mut Vec<String>) {
    let signature = format!("fn {name}(");
    let start = text
        .find(&signature)
        .unwrap_or_else(|| panic!("tick.rs defines {name}"));
    let open = start
        + text[start..]
            .find("{\n")
            .unwrap_or_else(|| panic!("{name} has a body"));
    let end = open
        + text[open..]
            .find("\n}\n")
            .unwrap_or_else(|| panic!("{name} ends"));
    let body: String = text[open..end]
        .lines()
        .map(|line| line.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n");
    let call = regex::Regex::new(
        r"\b(?:app|crate)(?:\s*(?:\.|::)\s*[a-z_][a-z0-9_]*)+\s*\(|\b([a-z_][a-z0-9_]*)\s*\(\s*app\b",
    )
    .expect("call pattern");
    for found in call.captures_iter(&body) {
        match found.get(1) {
            Some(step) => read_pass_calls(text, step.as_str(), calls),
            None => calls.push(
                found[0]
                    .chars()
                    .filter(|c| !c.is_whitespace() && *c != '(')
                    .collect(),
            ),
        }
    }
}

/// The background pass runs every poll and drain in its pinned order.
#[test]
fn the_background_pass_keeps_its_order() {
    let expected: Vec<String> = BACKGROUND_PASS_CALLS
        .iter()
        .map(|c| c.to_string())
        .collect();
    pretty_assertions::assert_eq!(background_pass_calls(), expected);
}

/// An idle croft draws nothing: once startup's background work has landed, a
/// pass that finds nothing new asks for no redraw. A poll that reported a
/// change on every pass would redraw the whole screen on every tick, 125
/// times a second, and over SSH send it all each time.
#[test]
fn an_idle_background_pass_asks_for_no_redraw() {
    // A pass takes any crash notice waiting in the process-wide slot: the
    // tests that put one there on purpose must not lose it to this one, nor
    // this one see theirs.
    let _slot = crash_slot_lock();
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // Startup's own background work lands first, each in a pass that asks for
    // a redraw: the file watcher's install, Go to File's index, and the
    // terminal pane's first label lookup, which starts one label interval
    // (1.5 s) after startup. A single quiet pass is not enough to say it has
    // all landed: the label lookup comes well after the others.
    let started = app.last_label_refresh;
    crate::test_budget::await_spawned(
        std::time::Duration::from_secs(5),
        "startup's background work to land",
        || {
            let quiet = !tick_background(&mut app).changed;
            quiet
                && !app.fs_watch.installing()
                && app.file_finder_index.is_some()
                && app.last_label_refresh > started
                && app.label_pass.is_none()
        },
    );
    for pass in 1..=20 {
        std::thread::sleep(std::time::Duration::from_millis(8));
        assert!(
            !tick_background(&mut app).changed,
            "idle pass {pass} asked for a redraw"
        );
    }
}
