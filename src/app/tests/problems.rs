//! The Problems panel, project checks and markdown lint.

use super::*;

/// The right cluster is painted over the left paragraph, but the diagnostics
/// hit rect was clipped only to the frame edge — never to where that cluster
/// starts. So on a narrow terminal the cells that visibly read "Ln 1, Col 1"
/// still answered to the diagnostics click and opened PROBLEMS, while the
/// counts they claimed to be were not on screen at all.
#[test]
fn clicking_the_position_readout_does_not_open_problems() {
    use ratatui::{Terminal, backend::TestBackend};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // 56 cols: the right cluster (51 cells with its │ dividers) fits, but its
    // left edge still lands inside the diagnostics cluster, so the clip below
    // is exercised.
    let mut term = Terminal::new(TestBackend::new(56, 20)).unwrap();
    term.draw(|f| app.render(f)).unwrap();

    // Find the status row and the column where the position readout paints.
    let buf = term.backend().buffer();
    let row = buf.area.height - 1;
    let text: String = (0..buf.area.width)
        .map(|x| buf[(x, row)].symbol().chars().next().unwrap_or(' '))
        .collect();
    let ln_col =
        text.find("Ln ")
            .unwrap_or_else(|| panic!("no position readout in status row {text:?}")) as u16;

    // The right cluster begins with " Ln", so its left edge is one cell before.
    let rx = ln_col.saturating_sub(1);
    assert!(
        app.status_diag_rect.right() <= rx,
        "the diagnostics hit rect must stop where the right cluster starts \
         (rect {:?} reaches {}, cluster starts at {}); row reads {text:?}",
        app.status_diag_rect,
        app.status_diag_rect.right(),
        rx
    );
    // And concretely: the cells that visibly read "Col 1" must not be a
    // diagnostics target.
    let col_at = text.find("Col ").unwrap() as u16;
    assert!(
        !rect_contains(app.status_diag_rect, col_at, row),
        "clicking the column readout opened PROBLEMS"
    );
}

#[test]
fn sync_markdown_lint_flags_a_second_top_level_heading() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("a.md");
    std::fs::write(&file, "# Title\n\nBody.\n\n# Another\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&file).unwrap();
    let changed = app.sync_markdown_lint();
    assert!(changed, "a fresh markdown tab must lint on first sync");
    assert_eq!(app.editor.diagnostics_path(), Some(file.as_path()));
    assert!(
        app.editor
            .diagnostic_spans_for_test()
            .iter()
            .any(|line| !line.is_empty()),
        "MD025 should have produced at least one squiggle"
    );
    let merged = app.merged_diagnostics(&file);
    assert!(merged.iter().any(|d| d.message.contains("MD025")));
}

#[test]
fn sync_markdown_lint_is_quiet_for_a_clean_document_and_reruns_only_on_edit() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("clean.md");
    std::fs::write(&file, "# Title\n\nA clean paragraph.\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&file).unwrap();
    // A clean document has nothing to apply, so the first sync reports no
    // change even though it did run the linter once.
    assert!(!app.sync_markdown_lint());
    assert!(
        app.merged_diagnostics(&file).is_empty(),
        "a clean document must not be flagged"
    );
    // Re-syncing an unchanged buffer must be a no-op (gated by edit_seq).
    assert!(!app.sync_markdown_lint());

    // The other half of this test's name: an EDIT must bump edit_seq, get the
    // buffer re-linted, and surface the new violation. Without this the test
    // passed on a linter that never ran a second time.
    app.editor.goto_bottom();
    app.editor
        .insert_str_as("\n# Second Title\n", crate::provenance::Seat::Navigator);
    assert!(
        app.sync_markdown_lint(),
        "an edit that introduces a second H1 must re-lint and report a change"
    );
    assert!(
        app.merged_diagnostics(&file)
            .iter()
            .any(|d| d.message.contains("MD025")),
        "the second H1 must be flagged after the edit"
    );
}

/// Split panes hold separate buffers of one file, so after an edit in one
/// they carry different `edit_seq`s. The sync must settle on one pane per
/// path rather than alternate between the two, and must not report a change
/// when the diagnostics it stores are the ones already there: either would
/// re-lint and redraw on every tick forever.
#[test]
fn sync_markdown_lint_settles_after_a_split_pane_is_edited() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("doc.md");
    std::fs::write(&file, "# Title\n\n# Second Title\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&file).unwrap();
    app.split_editor();
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 7;
    app.editor.insert_char('!');
    assert!(
        app.sync_markdown_lint(),
        "the first lint stores diagnostics"
    );
    assert!(
        app.merged_diagnostics(&file)
            .iter()
            .any(|d| d.message.contains("MD025")),
        "the edited pane still has two H1s"
    );
    assert!(
        !app.sync_markdown_lint(),
        "nothing changed since the last tick"
    );
    assert!(!app.sync_markdown_lint(), "and it stays settled");
}

/// #851: for a file rumdl reports on, the built-in lint withdraws what it
/// published (rumdl reports the same rules, so each would show twice) and
/// stays quiet; a file rumdl does not report on (one only in a split's other
/// group, never sent to the servers) keeps it; and once rumdl stops
/// reporting on a file the lint runs on it again.
#[test]
fn sync_markdown_lint_stands_down_only_for_files_rumdl_reports_on() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("a.md");
    let other = tmp.path().join("b.md");
    std::fs::write(&file, "# Title\n\nBody.\n\n# Another\n").unwrap();
    std::fs::write(&other, "# Title\n\nBody.\n\n# Another\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&other).unwrap();
    app.editor.open_pinned(&file).unwrap();
    let has_md025 = |app: &App, path: &std::path::Path| {
        app.merged_diagnostics(path)
            .iter()
            .any(|d| d.message.contains("MD025"))
    };
    assert!(app.sync_markdown_lint());
    assert!(has_md025(&app, &file), "the built-in lint reports MD025");
    assert!(has_md025(&app, &other));

    let lsp = || app.lsp.as_ref().expect("a test App has an LSP manager");
    lsp().set_document_servers_for_test(&file, &["marksman", "rumdl"]);
    lsp().set_document_servers_for_test(&other, &["marksman"]);
    assert!(app.sync_markdown_lint(), "standing down is a change");
    assert!(!has_md025(&app, &file), "withdrawn where rumdl reports");
    assert!(
        app.editor
            .diagnostic_spans_for_test()
            .iter()
            .all(|line| line.is_empty()),
        "and its squiggle is gone from the open tab"
    );
    assert!(has_md025(&app, &other), "kept where rumdl does not report");
    assert!(!app.sync_markdown_lint(), "and it stays quiet");

    // Retired from pulls, or crashed for good: rumdl leaves the file.
    app.lsp
        .as_ref()
        .unwrap()
        .set_document_servers_for_test(&file, &["marksman"]);
    assert!(
        app.sync_markdown_lint(),
        "with rumdl gone the lint runs again"
    );
    assert!(has_md025(&app, &file));
}

/// Two panes can reach the same `edit_seq` with different text (one edit in
/// each). Switching which pane is linted must still lint it: the cursor has
/// to recognise a different buffer, not just a different sequence number.
#[test]
fn sync_markdown_lint_relints_a_pane_with_the_same_seq_but_other_text() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("doc.md");
    std::fs::write(&file, "# Title\n\nplain\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&file).unwrap();
    app.split_editor();
    // One edit per pane: the focused pane gains an MD018 heading
    // (`#Bad`), the other a harmless character, so both seqs match.
    app.editor.cursor_row = 2;
    app.editor.cursor_col = 0;
    for c in "#Bad".chars() {
        app.editor.insert_char(c);
    }
    let seq_bad = app.editor.edit_seq;
    // The split put the new, focused pane on the right; move to the left one.
    app.focus_editor_group(true);
    app.editor.cursor_row = 2;
    app.editor.cursor_col = 5;
    // A split does not copy `edit_seq`, so type harmless text until this
    // pane's count meets the other's.
    while app.editor.edit_seq < seq_bad {
        app.editor.insert_char('!');
    }
    assert_eq!(app.editor.edit_seq, seq_bad, "same seq, different text");
    assert!(!app.editor.lines.iter().any(|l| l.starts_with("#B")));
    let md018 = |app: &App| {
        app.merged_diagnostics(&file)
            .iter()
            .any(|d| d.message.contains("MD018"))
    };
    // Lint with the `#Bad` pane focused (it is walked first) ...
    app.focus_editor_group(false);
    app.sync_markdown_lint();
    assert!(md018(&app), "the focused pane has `#Bad`");
    // ... then with the clean pane focused: same seq, other text.
    app.focus_editor_group(true);
    app.sync_markdown_lint();
    assert!(
        !md018(&app),
        "the other pane has no MD018, so it must be re-linted"
    );
}

/// A `.md` held by an INACTIVE split group must still be linted. The gather
/// loop used to walk only `self.editor`, so such a tab was never linted and
/// the cleanup below then dropped its stored diagnostics while it was still
/// open - the squiggles vanished from a pane that was plainly visible.
#[test]
fn sync_markdown_lint_covers_inactive_split_groups() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("doc.md");
    std::fs::write(&file, "# Title\n\n# Second Title\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&file).unwrap();
    app.split_editor(); // both groups hold doc.md; the new one is focused
    // Move the ACTIVE group off the markdown file, so doc.md lives only in
    // the inactive group - the case the gather loop missed. `open_pinned`
    // ADDS a tab, so the old one has to be closed or the active group still
    // holds doc.md and the bug never shows.
    let other = tmp.path().join("other.txt");
    std::fs::write(&other, "plain\n").unwrap();
    app.editor.open_pinned(&other).unwrap();
    let md_idx = app
        .editor
        .iter_tabs()
        .position(|e| e.path.as_deref() == Some(file.as_path()))
        .expect("the active group still holds doc.md before the close");
    app.editor.close_tab(md_idx);
    assert!(
        !app.editor
            .iter_tabs()
            .any(|e| e.path.as_deref() == Some(file.as_path())),
        "doc.md must now live ONLY in the inactive group"
    );

    let held_inactive = app
        .editor_layout
        .inactive_groups()
        .into_iter()
        .flat_map(|g| g.editors.iter())
        .any(|e| e.path.as_deref() == Some(file.as_path()));
    assert!(held_inactive, "the inactive group holds doc.md");

    app.sync_markdown_lint();
    assert!(
        app.merged_diagnostics(&file)
            .iter()
            .any(|d| d.message.contains("MD025")),
        "a markdown tab in an inactive pane must still be linted"
    );

    // And a second sync must not retire those diagnostics as "closed".
    app.sync_markdown_lint();
    assert!(
        app.merged_diagnostics(&file)
            .iter()
            .any(|d| d.message.contains("MD025")),
        "the tab is still open, so its diagnostics must survive the sweep"
    );
}

#[test]
fn sync_markdown_lint_skips_non_markdown_files() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("a.rs");
    std::fs::write(&file, "#Not a heading, this is Rust\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&file).unwrap();
    assert!(!app.sync_markdown_lint());
    assert!(app.merged_diagnostics(&file).is_empty());
}

#[test]
fn problems_panel_aggregates_the_store_across_files() {
    use crate::lsp::manager::DiagnosticSeverity;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let a = tmp.path().join("a.rs");
    let mut s1 = std::collections::HashMap::new();
    s1.insert(
        String::from("rustc"),
        vec![
            diag(0, 1, DiagnosticSeverity::Error),
            diag(2, 3, DiagnosticSeverity::Warning),
        ],
    );
    app.lsp_diagnostics.insert(a, s1);
    let b = tmp.path().join("b.rs");
    let mut s2 = std::collections::HashMap::new();
    s2.insert(
        String::from("rustc"),
        vec![diag(0, 1, DiagnosticSeverity::Error)],
    );
    app.lsp_diagnostics.insert(b, s2);

    app.rebuild_problems();
    assert_eq!(
        app.problems.total_count(),
        3,
        "two files, three diagnostics"
    );
    assert_eq!(app.problems.error_count(), 2);
    assert_eq!(app.problems.warning_count(), 1);
}

#[test]
fn clicking_the_problems_tab_switches_the_panel_view() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let backend = ratatui::backend::TestBackend::new(140, 50);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    assert_eq!(app.bottom_panel_tab, BottomPanelTab::Terminal);
    let r = app.problems_tab_rect;
    assert!(r.width > 0, "the PROBLEMS tab must have a hit rect");
    left_click(&mut app, r.x, r.y);
    assert_eq!(
        app.bottom_panel_tab,
        BottomPanelTab::Problems,
        "a click on the PROBLEMS tab activates that view",
    );
    // And the TERMINAL tab switches back.
    let t = app.terminal_tab_rect;
    left_click(&mut app, t.x, t.y);
    assert_eq!(app.bottom_panel_tab, BottomPanelTab::Terminal);
}

/// Issue #212: typing in the focused Problems panel filters diagnostics
/// by text; Backspace pops a character; Esc clears the filter before it
/// gives the pane back to the editor.
#[test]
fn typing_in_the_problems_panel_filters_and_esc_clears_before_leaving() {
    use crate::lsp::manager::{Diagnostic, DiagnosticSeverity};
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("hello.rs");
    std::fs::write(&file, "fn a() {}\nfn b() {}\nfn run() {}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let mut by_server = std::collections::HashMap::new();
    by_server.insert(
        String::from("rustc"),
        vec![
            Diagnostic {
                start_line: 0,
                start_char: 0,
                end_line: 0,
                end_char: 1,
                severity: DiagnosticSeverity::Error,
                message: String::from("mismatched types"),
                tags: Default::default(),
                code: None,
            },
            Diagnostic {
                start_line: 2,
                start_char: 3,
                end_line: 2,
                end_char: 6,
                severity: DiagnosticSeverity::Warning,
                message: String::from("function `run` is never used"),
                tags: Default::default(),
                code: None,
            },
        ],
    );
    app.lsp_diagnostics.insert(file.clone(), by_server);
    app.rebuild_problems();
    app.bottom_panel_tab = BottomPanelTab::Problems;
    app.focus_pane(Pane::Terminal);

    for c in "mismatch".chars() {
        app.handle_key(key(KeyCode::Char(c), KeyModifiers::NONE))
            .unwrap();
    }
    assert_eq!(
        app.problems.text_filter, "mismatch",
        "plain typing must reach the panel's filter"
    );
    app.handle_key(key(KeyCode::Backspace, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.problems.text_filter, "mismatc", "Backspace pops");
    // Esc clears the filter but keeps the pane focused.
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert!(app.problems.text_filter.is_empty(), "Esc clears the filter");
    assert!(
        app.focus == Pane::Terminal,
        "the first Esc only clears; the pane keeps focus"
    );
    // A second Esc, with nothing to clear, hands the pane back.
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert!(app.focus == Pane::Editor, "the second Esc leaves the panel");
}

#[test]
fn clicking_a_problem_row_opens_that_file_at_the_line() {
    use crate::lsp::manager::DiagnosticSeverity;
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("hello.rs");
    std::fs::write(&file, "fn a() {}\nfn b() {}\nfn run() {}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let mut by_server = std::collections::HashMap::new();
    by_server.insert(
        String::from("rustc"),
        vec![crate::lsp::manager::Diagnostic {
            start_line: 2,
            start_char: 3,
            end_line: 2,
            end_char: 6,
            severity: DiagnosticSeverity::Warning,
            message: String::from("function `run` is never used"),
            tags: Default::default(),
            code: None,
        }],
    );
    app.lsp_diagnostics.insert(file.clone(), by_server);
    app.rebuild_problems();
    app.bottom_panel_tab = BottomPanelTab::Problems;

    let backend = ratatui::backend::TestBackend::new(140, 50);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    // Row 0 is the toolbar, row 1 the file header, row 2 the diagnostic.
    let list = app.problems.last_area;
    left_click(&mut app, list.x + 4, list.y + 2);
    assert_eq!(
        app.editor.path.as_deref(),
        Some(file.as_path()),
        "clicking the row opens the file",
    );
    assert_eq!(app.editor.cursor_row, 2, "and jumps to the diagnostic line");
}

/// #1522: unused names and unreachable code come as hints; like VS Code,
/// the PROBLEMS list and its count leave them out, while the editor (its
/// hover and quick fixes) still has them.
#[test]
fn hints_stay_out_of_the_problems_list_and_its_count() {
    use crate::lsp::manager::{Diagnostic, DiagnosticSeverity, DiagnosticTags};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let a = tmp.path().join("a.ts");
    let unused = DiagnosticTags {
        unnecessary: true,
        deprecated: false,
    };
    let mut by_server = std::collections::HashMap::new();
    by_server.insert(
        String::from("vtsls"),
        vec![
            diag(0, 1, DiagnosticSeverity::Error),
            Diagnostic {
                tags: unused,
                ..diag(2, 5, DiagnosticSeverity::Hint)
            },
            diag(6, 9, DiagnosticSeverity::Hint),
            diag(10, 12, DiagnosticSeverity::Warning),
        ],
    );
    app.lsp_diagnostics.insert(a.clone(), by_server);
    app.rebuild_problems();
    assert_eq!(
        app.problems.total_count(),
        2,
        "only the error and the warning count"
    );
    let listed: Vec<DiagnosticSeverity> = app
        .problems
        .groups()
        .iter()
        .flat_map(|g| g.items.iter().map(|i| i.severity))
        .collect();
    assert_eq!(
        listed,
        vec![DiagnosticSeverity::Error, DiagnosticSeverity::Warning]
    );
    assert_eq!(
        app.merged_diagnostics(&a).len(),
        4,
        "the editor still gets every hint, for its hover and Ctrl+."
    );
}

/// #1522, negative: a file with nothing but hints has no PROBLEMS group,
/// while information diagnostics are still listed.
#[test]
fn a_file_with_only_hints_has_no_problems_group_but_information_is_listed() {
    use crate::lsp::manager::DiagnosticSeverity;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let hints = tmp.path().join("hints.py");
    let info = tmp.path().join("info.py");
    let mut h = std::collections::HashMap::new();
    h.insert(
        String::from("ty"),
        vec![
            diag(0, 3, DiagnosticSeverity::Hint),
            diag(4, 7, DiagnosticSeverity::Hint),
        ],
    );
    app.lsp_diagnostics.insert(hints, h);
    let mut i = std::collections::HashMap::new();
    i.insert(
        String::from("ty"),
        vec![diag(0, 3, DiagnosticSeverity::Information)],
    );
    app.lsp_diagnostics.insert(info.clone(), i);
    app.rebuild_problems();
    let paths: Vec<&std::path::PathBuf> = app.problems.groups().iter().map(|g| &g.path).collect();
    assert_eq!(paths, vec![&info]);
    assert_eq!(app.problems.total_count(), 1);
}

#[test]
fn the_problems_count_paints_an_orange_pill_in_the_tab_strip() {
    use crate::lsp::manager::DiagnosticSeverity;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let a = tmp.path().join("a.rs");
    let mut s1 = std::collections::HashMap::new();
    s1.insert(
        String::from("rustc"),
        vec![diag(0, 1, DiagnosticSeverity::Error)],
    );
    app.lsp_diagnostics.insert(a, s1);
    app.rebuild_problems();
    let backend = ratatui::backend::TestBackend::new(140, 50);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let buf = term.backend().buffer();
    // Somewhere along the PROBLEMS tab the count badge fills with the brand
    // gradient's orange corner, the "circle" the user asked for.
    let strip = app.problems_tab_rect;
    let orange = crate::gradient::rgb_color(crate::gradient::GRAD_TR);
    let painted = (strip.x..strip.x + strip.width).any(|x| buf[(x, strip.y)].bg == orange);
    assert!(painted, "the problem count wears an orange pill");
    // And the rounded right cap of the pill sits in brand orange foreground.
    let cap_r = (strip.x..strip.x + strip.width)
        .any(|x| buf[(x, strip.y)].symbol() == "\u{e0b4}" && buf[(x, strip.y)].fg == orange);
    assert!(cap_r, "the pill has a rounded orange right cap");
}

#[test]
fn build_scan_installs_problems_replaces_per_pane_and_merges_with_lsp() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let cwd = tmp.path().join("proj");
    std::fs::create_dir(&cwd).unwrap();

    let cargo_out = "error[E0308]: mismatched types\n  --> src/main.rs:12:5\n";
    assert!(app.apply_build_scan(0, Some(&cwd), "cargo build", cargo_out));
    let expected = cwd.join("src/main.rs");
    let groups = app.problems.groups().to_vec();
    let g = groups
        .iter()
        .find(|g| g.path == expected)
        .expect("the resolved file must group in PROBLEMS");
    assert_eq!(g.items.len(), 1);
    assert_eq!(g.items[0].source, "rustc");
    assert_eq!((g.items[0].line, g.items[0].col), (11, 4));

    // A second pane contributes a different file; both coexist.
    assert!(app.apply_build_scan(1, Some(&cwd), "make", "main.c:7:3: error: boom\n"));
    assert_eq!(app.problems.groups().len(), 2);

    // Pane 0 rebuilds clean: its contribution clears, pane 1's stays.
    assert!(app.apply_build_scan(0, Some(&cwd), "cargo build", "   Finished dev profile\n"));
    let groups = app.problems.groups().to_vec();
    assert_eq!(groups.len(), 1, "pane 0's fixed errors must vanish");
    assert!(groups[0].path.ends_with("main.c"));

    // Clean output from a pane with no history is a no-op.
    assert!(!app.apply_build_scan(5, Some(&cwd), "true", "all good\n"));

    app.clear_build_diagnostics();
    assert!(app.problems.groups().is_empty());
    assert!(app.status.contains("Cleared 1 build diagnostic"));
}

#[test]
fn a_custom_matcher_from_matchers_json_feeds_problems() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.matchers = std::sync::Arc::new(crate::problem_matchers::MatcherSet::from_json(
        r##"[ { "name": "mylint",
               "pattern": "^(?P<file>\\S+):(?P<line>\\d+):(?P<col>\\d+) (?P<severity>\\w+) (?P<message>.+)$",
               "severity_map": { "E": "error" },
               "applies_to": "mylint*" } ]"##,
    ));
    let cwd = tmp.path().join("proj");
    std::fs::create_dir(&cwd).unwrap();
    let out = "src/a.py:12:5 E undefined name 'x'\n";
    assert!(app.apply_build_scan(0, Some(&cwd), "mylint src/", out));
    let groups = app.problems.groups().to_vec();
    let g = groups
        .iter()
        .find(|g| g.path == cwd.join("src/a.py"))
        .expect("the custom matcher's file must group in PROBLEMS");
    assert_eq!(g.items.len(), 1);
    assert_eq!(g.items[0].source, "mylint");
    assert_eq!(
        g.items[0].severity,
        crate::lsp::manager::DiagnosticSeverity::Error,
        "severity_map turns E into an error"
    );
    assert_eq!((g.items[0].line, g.items[0].col), (11, 4));

    // The applies_to glob keeps the matcher off other commands: the same
    // output under `cargo build` matches nothing, clearing the pane.
    assert!(app.apply_build_scan(0, Some(&cwd), "cargo build", out));
    assert!(app.problems.groups().is_empty(), "non-matching command");
}

/// A checker that failed to RUN must not read as a clean project (#256).
///
/// `tsc` prints global errors (`error TS5023: Unknown compiler option`)
/// with no `file(line,col)` prefix, so no matcher claims them and the row
/// count is zero - which is exactly what a clean project looks like.
/// Reporting "no problems" there tells the user their code is fine when the
/// checker never checked it. The exit status is the only thing separating
/// the two, which is why it cannot simply be ignored.
#[cfg(unix)]
#[test]
fn a_project_checker_that_failed_to_run_does_not_report_a_clean_project() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();

    // A failed invocation: non-zero exit, output no matcher claims.
    app.finish_project_check_for_test(
        tmp.path(),
        "error TS5023: Unknown compiler option '--nosuchflag'.\n",
        "",
        false,
    );
    assert!(
        app.status.contains("failed"),
        "an invocation failure must say so, got {:?}",
        app.status
    );
    assert!(
        !app.status.contains("no problems"),
        "and must NOT read as a clean project, got {:?}",
        app.status
    );
    assert!(
        app.status.contains("TS5023"),
        "naming the reason, got {:?}",
        app.status
    );

    // npx reports a missing compiler on STDOUT, not stderr, and opens with
    // a blank line and an ANSI-coloured banner. The earlier version of this
    // test put the text on stderr - the implementation's own wrong
    // assumption, encoded in the fixture, so the bug passed the suite.
    app.finish_project_check_for_test(
        tmp.path(),
        "\n\x1b[41m\x1b[37m This is not the tsc command you are looking for \x1b[0m\n",
        "",
        false,
    );
    assert!(
        app.status.contains("not the tsc command"),
        "the reason is found past a blank line and escapes, got {:?}",
        app.status
    );
    assert!(
        !app.status.contains('\u{1b}'),
        "and carries no escape sequences, got {:?}",
        app.status
    );
    assert_ne!(
        app.status, "The project checker failed: ",
        "an empty reason is the bug this replaced"
    );

    // A genuinely clean project: zero rows AND a zero exit.
    app.finish_project_check_for_test(tmp.path(), "", "", true);
    assert_eq!(
        app.status, "Whole-project check: no problems",
        "a real clean run still reports clean"
    );
}

/// Each producer clears only ITS OWN rows from a shared file (#532).
///
/// `build_diagnostics` held one flat `Vec` per file, so a re-run removed the
/// whole entry and took the other producer's rows with it - and
/// `install_build_diags` recorded a path in `touched` only when the entry was
/// empty, so the SECOND producer to reach a file never listed it and could
/// not clear its rows at all. Two halves of one problem: rows did not carry
/// who produced them.
///
/// #530 made this routine rather than rare: the whole-project sweep reports
/// every file in the project, so it overlaps any pane build by default.
#[test]
fn each_producer_clears_only_its_own_rows_from_a_shared_file() {
    let tmp = tempfile::tempdir().unwrap();
    let shared = tmp.path().join("shared.ts");
    std::fs::write(&shared, "export const a = 1;\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let line = |code: &str| format!("shared.ts(1,1): error {code}: boom.\n");

    // Pane 1 reports the file, then the sweep reports it too.
    app.apply_build_scan(1, Some(tmp.path()), "tsc", &line("TS1001"));
    app.ingest_project_check(Some(tmp.path()), &line("TS2002"));
    app.rebuild_problems();
    let rows = |app: &App| -> Vec<String> {
        app.problems
            .groups()
            .iter()
            .filter(|g| g.path == shared)
            .flat_map(|g| g.items.iter().map(|i| i.message.clone()))
            .collect()
    };
    assert_eq!(
        rows(&app).len(),
        2,
        "both producers' rows are on the shared file, got {:?}",
        rows(&app)
    );

    // The SECOND producer re-runs clean. Its row must go; the pane's stays.
    app.ingest_project_check(Some(tmp.path()), "");
    app.rebuild_problems();
    let after = rows(&app);
    assert_eq!(
        after.len(),
        1,
        "the sweep's row goes and the pane's remains, got {after:?}"
    );
    assert!(
        after[0].contains("TS1001"),
        "the surviving row is the PANE's, not the sweep's: {after:?}"
    );

    // And the reverse: the pane re-runs clean, leaving nothing.
    app.apply_build_scan(1, Some(tmp.path()), "tsc", "");
    app.rebuild_problems();
    assert!(
        rows(&app).is_empty(),
        "with both producers clean the file has no rows, got {:?}",
        rows(&app)
    );
}

/// A file BOTH the sweep and a pane report is counted once, by whoever is
/// reporting (#256).
///
/// `build_diagnostics[f]` holds every producer's items for that file, so
/// summing the store double-counts a shared file. Both orderings are pinned
/// because they fail in opposite directions and one alone would look
/// correct.
///
/// The undercount this doc used to also describe - the second producer never
/// listing a shared file, so it could not clear its rows - was the other half
/// of #532 and is fixed; the rows carry an owner now.
#[cfg(unix)]
#[test]
fn a_file_both_producers_report_is_not_double_counted() {
    let tmp = tempfile::tempdir().unwrap();
    let shared = tmp.path().join("shared.ts");
    let only = tmp.path().join("only.ts");
    std::fs::write(&shared, "export const a = 1;\n").unwrap();
    std::fs::write(&only, "export const b = 2;\n").unwrap();
    let line = |f: &str| format!("{f}(1,1): error TS2322: Type error.\n");

    // Sweep first, then a pane reports the SAME file.
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.finish_project_check_for_test(tmp.path(), &line("shared.ts"), "", false);
    assert_eq!(app.status, "Whole-project check: 1 problem");
    app.apply_build_scan(1, Some(tmp.path()), "tsc", &line("shared.ts"));
    app.finish_project_check_for_test(tmp.path(), &line("shared.ts"), "", false);
    assert_eq!(
        app.status, "Whole-project check: 1 problem",
        "the pane's row for the same file is not the sweep's to count"
    );

    // Pane first, then the sweep reports that file AND another.
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.apply_build_scan(1, Some(tmp.path()), "tsc", &line("shared.ts"));
    app.finish_project_check_for_test(
        tmp.path(),
        &format!("{}{}", line("shared.ts"), line("only.ts")),
        "",
        false,
    );
    assert_eq!(
        app.status, "Whole-project check: 2 problems",
        "the sweep found two, even though a pane got to one of them first"
    );
}

/// The sweep's status counts ITS OWN findings, not the whole panel (#256).
///
/// `problems.total_count()` is the panel's projection: it mixes in every
/// language server's diagnostics and every pane's build output, and the
/// Open Files scope filters it. Under that scope a sweep whose findings are
/// all in unopened files would have reported "no problems" - defeating the
/// one thing the feature exists to do.
#[cfg(unix)]
#[test]
fn the_project_check_count_is_its_own_not_the_panels() {
    use crate::lsp::manager::DiagnosticSeverity;
    let tmp = tempfile::tempdir().unwrap();
    let swept = tmp.path().join("swept.ts");
    let other = tmp.path().join("other.rs");
    std::fs::write(&swept, "export const x: string = 1;\n").unwrap();
    std::fs::write(&other, "fn main() {}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();

    // An unrelated language-server diagnostic is already in the panel.
    let mut by_server = std::collections::HashMap::new();
    by_server.insert(
        String::from("rust-analyzer"),
        vec![diag(0, 4, DiagnosticSeverity::Error)],
    );
    app.lsp_diagnostics.insert(other.clone(), by_server);

    app.finish_project_check_for_test(
        tmp.path(),
        "swept.ts(1,14): error TS2322: Type 'number' is not assignable.\n",
        "",
        false,
    );
    assert_eq!(
        app.status, "Whole-project check: 1 problem",
        "the sweep found one; the LSP's row is not its to count"
    );
    assert!(
        app.problems.total_count() > 1,
        "staging check: the panel really does hold more than the sweep's row"
    );

    // Under Open Files the panel hides the unopened file - the count must
    // not follow it down, or the sweep reports nothing it just found.
    app.problems.scope = crate::widgets::problems::ProblemScope::OpenFiles;
    app.rebuild_problems();
    app.finish_project_check_for_test(
        tmp.path(),
        "swept.ts(1,14): error TS2322: Type 'number' is not assignable.\n",
        "",
        false,
    );
    assert_eq!(
        app.status, "Whole-project check: 1 problem",
        "the scope filters the PANEL, not what the sweep found"
    );
}

/// "Problems: Check Whole Project" puts a whole-project checker's findings
/// into PROBLEMS for files nobody opened, and a re-run REPLACES them (#256).
///
/// Replacement is the half worth pinning. A checker that reports two errors
/// and then one after a fix must leave one row, not three: the panel is a
/// current state, and a stale row sends the reader to a line that is now
/// correct. `apply_build_scan` already owns that semantics per producer, so
/// the project check gets its own producer id rather than borrowing a
/// pane's: a pane's next command would otherwise wipe the sweep's rows, and
/// the sweep would wipe the pane's.
#[test]
fn a_project_check_reports_unopened_files_and_replaces_on_rerun() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.ts");
    let b = tmp.path().join("b.ts");
    std::fs::write(&a, "export const x: string = 1;\n").unwrap();
    std::fs::write(&b, "export const y: string = 2;\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();

    // Neither file is opened: the sweep is exactly for files with no buffer.
    assert!(app.editor.path.is_none(), "no file is open");

    // Two errors, in tsc's parenthesised form.
    app.ingest_project_check(
        None,
        "a.ts(1,14): error TS2322: Type 'number' is not assignable to type 'string'.\n\
         b.ts(1,14): error TS2322: Type 'number' is not assignable to type 'string'.\n",
    );
    app.rebuild_problems();
    let paths: Vec<_> = app
        .problems
        .groups()
        .iter()
        .map(|g| g.path.clone())
        .collect();
    assert!(
        paths.contains(&a) && paths.contains(&b),
        "both unopened files must reach PROBLEMS, got {paths:?}"
    );

    // The user fixes b.ts and re-runs: one row, not three.
    app.ingest_project_check(
        None,
        "a.ts(1,14): error TS2322: Type 'number' is not assignable to type 'string'.\n",
    );
    app.rebuild_problems();
    let paths: Vec<_> = app
        .problems
        .groups()
        .iter()
        .map(|g| g.path.clone())
        .collect();
    assert!(
        paths.contains(&a),
        "the still-broken file keeps its row, got {paths:?}"
    );
    assert!(
        !paths.contains(&b),
        "a re-run REPLACES the previous sweep: the fixed file's row must go, \
         got {paths:?}"
    );

    // A pane's own build output must not wipe the sweep, which is why the
    // sweep does not borrow a pane id. Pane 1 reports on its own file.
    let c = tmp.path().join("c.ts");
    std::fs::write(&c, "export const z = 3;\n").unwrap();
    app.apply_build_scan(
        1,
        Some(tmp.path()),
        "tsc",
        "c.ts(1,1): error TS1005: ';' expected.\n",
    );
    app.rebuild_problems();
    let paths: Vec<_> = app
        .problems
        .groups()
        .iter()
        .map(|g| g.path.clone())
        .collect();
    assert!(
        paths.contains(&a) && paths.contains(&c),
        "a pane's build output and the project sweep coexist, got {paths:?}"
    );
}

/// #256 step 1: a server that publishes project-wide diagnostics (rust-analyzer
/// does this from `cargo check`) names files the user never opened. The panel
/// builds from the diagnostics store rather than from open buffers, so those
/// rows DO appear — this pins that half, which the issue lists as unverified.
#[test]
fn problems_shows_diagnostics_for_a_file_that_was_never_opened() {
    use crate::lsp::manager::DiagnosticSeverity;
    let tmp = tempfile::tempdir().unwrap();
    let opened = tmp.path().join("opened.rs");
    let never = tmp.path().join("never_opened.rs");
    std::fs::write(&opened, "fn main() {}\n").unwrap();
    std::fs::write(&never, "fn helper() {}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&opened).unwrap();

    let mut by_server = std::collections::HashMap::new();
    by_server.insert(
        String::from("rust-analyzer"),
        vec![diag(0, 4, DiagnosticSeverity::Error)],
    );
    app.lsp_diagnostics.insert(never.clone(), by_server);
    app.rebuild_problems();

    let paths: Vec<_> = app.problems.groups().iter().map(|g| &g.path).collect();
    assert!(
        paths.contains(&&never),
        "a diagnostic for an unopened file must still reach PROBLEMS"
    );
}

/// #256: the scope toggle changes which files the panel lists, and the badge
/// counts the same list — both read one projection, so they cannot disagree.
#[test]
fn problems_scope_toggle_filters_unopened_files_and_the_badge_follows() {
    use crate::lsp::manager::DiagnosticSeverity;
    use crate::widgets::problems::ProblemScope;
    let tmp = tempfile::tempdir().unwrap();
    let opened = tmp.path().join("opened.rs");
    let never = tmp.path().join("never_opened.rs");
    std::fs::write(&opened, "fn main() {}\n").unwrap();
    std::fs::write(&never, "fn helper() {}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&opened).unwrap();

    for p in [&opened, &never] {
        let mut by_server = std::collections::HashMap::new();
        by_server.insert(
            String::from("rust-analyzer"),
            vec![diag(0, 4, DiagnosticSeverity::Error)],
        );
        app.lsp_diagnostics.insert(p.clone(), by_server);
    }

    app.problems.scope = ProblemScope::WholeProject;
    app.rebuild_problems();
    assert_eq!(
        app.problems.groups().len(),
        2,
        "whole project lists the unopened file too"
    );
    let whole_count = app.problems.total_count();

    app.problems.scope = ProblemScope::OpenFiles;
    app.rebuild_problems();
    let paths: Vec<_> = app.problems.groups().iter().map(|g| &g.path).collect();
    assert_eq!(paths, vec![&opened], "open files drops the unopened one");
    assert!(
        app.problems.total_count() < whole_count,
        "the badge counts the same filtered list"
    );
}

/// #296 review: Open Files scope filters on the open-buffer set, so the panel
/// must refresh when that set changes — not only when diagnostics do.
/// Previously `rebuild_problems` ran on diagnostic events alone, so opening a
/// file that already had stored diagnostics showed NOTHING until some
/// unrelated churn happened to trigger a rebuild.
#[test]
fn opening_a_file_refreshes_a_scoped_problems_panel() {
    use crate::lsp::manager::DiagnosticSeverity;
    use crate::widgets::problems::ProblemScope;
    let tmp = tempfile::tempdir().unwrap();
    let later = tmp.path().join("later.rs");
    std::fs::write(&later, "fn helper() {}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.problems.scope = ProblemScope::OpenFiles;

    // A diagnostic arrives for a file that is not open yet.
    let mut by_server = std::collections::HashMap::new();
    by_server.insert(
        String::from("rust-analyzer"),
        vec![diag(0, 4, DiagnosticSeverity::Error)],
    );
    app.lsp_diagnostics.insert(later.clone(), by_server);
    app.rebuild_problems();
    assert!(
        app.problems.groups().is_empty(),
        "not open yet, so Open Files scope hides it"
    );

    // Opening it must bring the row in WITHOUT any new diagnostic push.
    app.editor.open_pinned(&later).unwrap();
    app.sync_lsp();
    let paths: Vec<_> = app.problems.groups().iter().map(|g| &g.path).collect();
    assert!(
        paths.contains(&&later),
        "opening the file refreshes the scoped panel"
    );
}

/// The config token round-trips: writing a scope back and reading it must
/// yield the same scope. `label()` is display text, so using it here would
/// produce a value `from_config` only accepts by accident.
#[test]
fn problem_scope_config_tokens_round_trip() {
    use crate::widgets::problems::ProblemScope;
    for scope in [ProblemScope::OpenFiles, ProblemScope::WholeProject] {
        assert_eq!(
            ProblemScope::from_config(scope.to_config()),
            scope,
            "{} must survive a save/load cycle",
            scope.to_config()
        );
    }
    // The palette label pasted into a config is understood, not silently
    // treated as an unrecognised value.
    assert_eq!(
        ProblemScope::from_config("Open Files"),
        ProblemScope::OpenFiles
    );
    // Anything genuinely unrecognised shows more rather than hiding rows.
    assert_eq!(
        ProblemScope::from_config("nonsense"),
        ProblemScope::WholeProject
    );
}

/// A PROBLEMS count that goes to zero clears the screen only if a badge was
/// drawn: start-up and graphics re-inits no longer wipe the screen.
#[test]
fn a_zero_problem_count_clears_nothing_when_no_badge_was_drawn() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.overlays
        .activity
        .set_images(super::ActivityBarImages::default());
    app.problems_badge_count = usize::MAX;
    app.refresh_problems_badge();
    assert!(!app.consume_problems_badge_image_clear());
}

/// A solution-style tsconfig (`"files": []` plus `references`, as Vite's
/// TypeScript templates write) checks each referenced project, so their
/// errors are reported instead of "no problems" (#1291).
#[cfg(unix)]
#[test]
fn the_project_check_checks_each_referenced_tsconfig() {
    let tmp = fake_tsc_workspace(
        r#"{
  // Vite's layout
  "files": [],
  "references": [{ "path": "./tsconfig.app.json" }, { "path": "./tsconfig.node.json" }],
}"#,
    );
    let (status, calls) = run_project_check(&tmp);
    assert_eq!(
        calls,
        "-p ./tsconfig.app.json --noEmit\n-p ./tsconfig.node.json --noEmit\n"
    );
    assert_eq!(status, "Whole-project check: 1 problem");
}

/// A root tsconfig with `references` that also owns files gets its own
/// `--noEmit` run: tsc does not check references on a plain project
/// check, so skipping the root would hide its files' errors (#1291).
#[cfg(unix)]
#[test]
fn a_root_with_references_and_its_own_files_is_checked_too() {
    let tmp = fake_tsc_workspace(
        r#"{ "include": ["src"], "references": [{ "path": "./tsconfig.app.json" }] }"#,
    );
    let (status, calls) = run_project_check(&tmp);
    assert_eq!(calls, "--noEmit\n-p ./tsconfig.app.json --noEmit\n");
    assert_eq!(status, "Whole-project check: 1 problem");
}

/// A reference that is itself a solution-style config is followed to its
/// leaves rather than run on its own, which would compile nothing (#1291).
#[cfg(unix)]
#[test]
fn a_nested_solution_reference_is_checked_through_its_leaves() {
    let tmp = fake_tsc_workspace(r#"{ "files": [], "references": [{ "path": "./web" }] }"#);
    std::fs::create_dir_all(tmp.path().join("web")).unwrap();
    std::fs::write(
        tmp.path().join("web/tsconfig.json"),
        r#"{ "files": [], "references": [{ "path": "./tsconfig.app.json" }, { "path": ".." }] }"#,
    )
    .unwrap();
    std::fs::write(tmp.path().join("web/tsconfig.app.json"), "{}").unwrap();
    let (status, calls) = run_project_check(&tmp);
    assert_eq!(calls, "-p ./web/./tsconfig.app.json --noEmit\n");
    assert_eq!(status, "Whole-project check: 1 problem");
}

/// A diagnostic for the Problems panel, as a language server reports it.
fn problem(
    line: u32,
    severity: crate::lsp::manager::DiagnosticSeverity,
    message: &str,
) -> crate::widgets::problems::ProblemItem {
    crate::widgets::problems::ProblemItem {
        line,
        col: 0,
        col_utf16: true,
        severity,
        message: message.into(),
        source: "ruff".into(),
        code: None,
    }
}

#[test]
fn the_problems_tab_is_announced_with_its_counts_and_top_entry() {
    use crate::lsp::manager::DiagnosticSeverity::{Error, Warning};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus = Pane::Terminal;
    app.bottom_panel_tab = BottomPanelTab::Problems;
    let snap = app.a11y_snapshot();
    assert_eq!(snap.focus, "Problems, no problems");
    assert_eq!(snap.item, None);
    app.problems
        .set_groups(vec![crate::widgets::problems::ProblemGroup {
            path: tmp.path().join("app.py"),
            name: "app.py".into(),
            rel_dir: String::new(),
            items: vec![
                problem(3, Error, "Undefined name `totl`"),
                problem(4, Error, "Type mismatch"),
                problem(4, Warning, "Unused variable"),
            ],
        }]);
    let snap = app.a11y_snapshot();
    assert_eq!(snap.focus, "Problems, 2 errors, 1 warning");
    assert_eq!(
        snap.item.as_deref(),
        Some("app.py line 4: Undefined name `totl` (ruff)")
    );
    // #1540: a diagnostic with a rule announces it as the row shows it.
    let mut coded = problem(0, Error, "`os` imported but unused");
    coded.code = Some(crate::lsp::manager::DiagnosticCode {
        value: String::from("F401"),
        source: Some(String::from("Ruff")),
        href: None,
    });
    app.problems
        .set_groups(vec![crate::widgets::problems::ProblemGroup {
            path: tmp.path().join("app.py"),
            name: "app.py".into(),
            rel_dir: String::new(),
            items: vec![coded],
        }]);
    assert_eq!(
        app.a11y_snapshot().item.as_deref(),
        Some("app.py line 1: `os` imported but unused (Ruff(F401))")
    );
}

/// #1488: a watcher that clears the screen (`tsc --watch`, `cargo watch -c`)
/// erases its own output mark, so its finish reports no output. That finish
/// never reached the build scan, the one-shot skip meant for it stayed set,
/// and it swallowed the NEXT command's scan instead: PROBLEMS kept the
/// watcher's stale cycle and dropped the new build's errors.
#[test]
fn a_build_after_a_screen_clearing_watcher_reaches_problems() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let pane = app.run_project_task(long_task("tsc --watch")).unwrap();
    let cwd = tmp.path().to_path_buf();
    let stale = "main.ts(1,7): error TS2322: Type 'string' is not assignable to type 'number'.\n";
    let fresh = "main.ts(2,7): error TS2322: Type 'number' is not assignable to type 'string'.\n";
    // The watcher's last cycle, published mid-run.
    assert!(app.apply_build_scan(pane, Some(&cwd), "tsc --watch", stale));
    app.watch_published_panes.insert(pane);
    // Ctrl+C: the watcher's own finish, its output erased by the clears.
    finish_pane_command(&mut app, pane, Some(130), 5_000);
    // The next build in the same pane.
    assert!(
        app.apply_build_scan(pane, Some(&cwd), "tsc", fresh),
        "the build after the watcher is scanned"
    );
    let groups = app.problems.groups().to_vec();
    assert_eq!(groups.len(), 1, "{groups:?}");
    let lines: Vec<u32> = groups[0].items.iter().map(|d| d.line).collect();
    assert_eq!(
        lines,
        vec![1],
        "the build's line 2 error (0-based 1), not the watcher's line 1"
    );
}

/// #1594: a user matcher named `go` is not one of the loose built-in rows,
/// so its rows are kept even for a file not on disk.
#[test]
fn a_custom_matcher_named_go_keeps_rows_for_missing_files() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.matchers = std::sync::Arc::new(crate::problem_matchers::MatcherSet::from_json(
        r##"[ { "name": "go",
               "pattern": "^(?P<file>\\S+):(?P<line>\\d+):(?P<col>\\d+) (?P<severity>\\w+) (?P<message>.+)$",
               "applies_to": "mygo*" } ]"##,
    ));
    let out = "gen/missing.go:3:1 error generated file absent\n";
    assert!(app.apply_build_scan(0, Some(tmp.path()), "mygo build", out));
    let groups = app.problems.groups().to_vec();
    let g = groups
        .iter()
        .find(|g| g.path == tmp.path().join("gen/missing.go"))
        .expect("a user matcher's row must not need an existing file");
    assert_eq!(g.items[0].source, "go");
}

/// #1594: a `go build` error and a flake8 row land in PROBLEMS for files
/// that exist; the same shapes naming no file are not problems.
#[test]
fn go_and_lint_rows_reach_problems_only_for_real_files() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("main.go"), "package main\n").unwrap();
    std::fs::write(tmp.path().join("app.py"), "import os\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let out = "./main.go:4:6: declared and not used: unused\n\
               app.py:1:1: F401 'os' imported but unused\n\
               notes.go:9:9: just some text\n\
               ghost.py:3:1: E999 not a real file\n\
               app.py:2: error: Incompatible types in assignment\n";
    assert!(app.apply_build_scan(3, Some(tmp.path()), "go build ./... ; flake8 app.py", out));
    let mut rows: Vec<(String, u32, String)> = app
        .problems
        .groups()
        .iter()
        .flat_map(|g| {
            let name = g.path.file_name().unwrap().to_string_lossy().into_owned();
            g.items
                .iter()
                .map(move |i| (name.clone(), i.line, i.source.clone()))
        })
        .collect();
    rows.sort();
    assert_eq!(
        rows,
        [
            (String::from("app.py"), 0, String::from("lint")),
            (String::from("app.py"), 1, String::from("no-column")),
            (String::from("main.go"), 3, String::from("go")),
        ]
    );
}
