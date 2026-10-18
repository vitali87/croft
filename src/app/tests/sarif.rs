//! The SARIF results viewer and GitHub code scanning.

use super::*;

#[test]
fn sarif_log_opens_as_the_results_viewer() {
    let (tmp, log) = sarif_fixture();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&log).unwrap();
    let view = app
        .editor
        .sarif
        .as_ref()
        .expect(".sarif opens in the viewer");
    assert_eq!(view.entries.len(), 1);
    assert_eq!(view.entries[0].file, "src/a.rs");
    assert!(
        app.editor.has_non_text_view(),
        "a save must not write the stub"
    );

    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 30)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let screen = screen_text(&term);
    assert!(
        screen.contains("src/a.rs"),
        "group header painted:\n{screen}"
    );
    assert!(
        screen.contains("Query built from user input."),
        "result row painted:\n{screen}"
    );
    assert!(screen.contains("Locations"), "tab strip painted:\n{screen}");
    // The first result starts selected, so its details are already showing.
    assert!(
        screen.contains("R1 · Taint"),
        "details pane shows the first result:\n{screen}"
    );
}

#[test]
fn sarif_enter_opens_the_location_and_keeps_the_viewer_tab() {
    let (tmp, log) = sarif_fixture();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_preview(&log).unwrap();
    let tabs_before = app.editor.tab_count();
    app.handle_sarif_key(key(KeyCode::Down, KeyModifiers::NONE));
    app.handle_sarif_key(key(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(
        app.editor.path.as_deref(),
        Some(tmp.path().join("src/a.rs").as_path()),
        "the location opened: {}",
        app.status
    );
    assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (1, 8));
    assert_eq!(
        app.editor.tab_count(),
        tabs_before + 1,
        "the viewer was pinned, not replaced by the preview open"
    );
}

#[test]
fn sarif_space_previews_the_code_around_the_result_without_leaving_the_list() {
    // #577: Space shows the lines around the selected result in the
    // details pane; the viewer keeps the keys and Space again hides it.
    let (tmp, log) = sarif_fixture();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_preview(&log).unwrap();
    app.handle_sarif_key(key(KeyCode::Down, KeyModifiers::NONE));
    app.handle_sarif_key(key(KeyCode::Char(' '), KeyModifiers::NONE));
    app.refresh_sarif_preview();
    let view = app.editor.sarif.as_ref().expect("still on the viewer");
    let p = view.current_preview().expect("a preview").clone();
    assert!(p.title.ends_with("a.rs"), "{}", p.title);
    assert_eq!((p.first, p.target), (1, 2));
    assert_eq!(p.lines[1], "    let q = format!(\"{}\", id);");

    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 30)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let screen = screen_text(&term);
    assert!(screen.contains("Preview · "), "{screen}");
    assert!(screen.contains("2     let q = format!"), "{screen}");

    // On a group row there is no result to preview.
    app.handle_sarif_key(key(KeyCode::Up, KeyModifiers::NONE));
    app.refresh_sarif_preview();
    assert!(
        app.editor
            .sarif
            .as_ref()
            .unwrap()
            .current_preview()
            .is_none()
    );
    // Back on the result it comes back; Space turns it off.
    app.handle_sarif_key(key(KeyCode::Down, KeyModifiers::NONE));
    app.refresh_sarif_preview();
    assert!(
        app.editor
            .sarif
            .as_ref()
            .unwrap()
            .current_preview()
            .is_some()
    );
    app.handle_sarif_key(key(KeyCode::Char(' '), KeyModifiers::NONE));
    app.refresh_sarif_preview();
    let view = app.editor.sarif.as_ref().unwrap();
    assert!(!view.previewing && view.current_preview().is_none());
}

#[test]
fn a_sarif_result_whose_file_is_only_in_the_log_opens_the_logs_copy() {
    // #577: `artifacts[].contents` is the file when it is not on this
    // machine: text opens read-only at the result, binary in the hex viewer.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("embedded.sarif");
        std::fs::write(
            &log,
            r#"{"version":"2.1.0","runs":[{"tool":{"driver":{"name":"T"}},
              "artifacts":[
                {"location":{"uri":"file:///build/gen/query.c"},"contents":{"text":"line one\nline two\nint bad;\n"}},
                {"location":{"uri":"file:///build/blob.bin"},"contents":{"binary":"AAEC/w=="}}],
              "results":[
                {"ruleId":"R1","message":{"text":"text"},"locations":[{"physicalLocation":{"artifactLocation":{"uri":"file:///build/gen/query.c","index":0},"region":{"startLine":3,"startColumn":5}}}]},
                {"ruleId":"R2","message":{"text":"bin"},"locations":[{"physicalLocation":{"artifactLocation":{"uri":"file:///build/blob.bin","index":1}}}]}]}]}"#,
        )
        .unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.editor.open_preview(&log).unwrap();
        let select = |app: &mut App, rule: &str| {
            let view = app.editor.sarif.as_mut().unwrap();
            let rows = view.rows();
            view.selected = rows
                .iter()
                .position(|r| matches!(r, crate::sarif::view::Row::Item { entry } if view.entries[*entry].rule_id == rule))
                .unwrap();
        };
        select(&mut app, "R1");
        app.open_selected_sarif_result();
        let opened = app.editor.path.clone().expect("a copy opened");
        assert!(
            opened.starts_with(croft_cache_dir()),
            "{opened:?}: {}",
            app.status
        );
        assert_eq!(opened.file_name().unwrap(), "query.c");
        assert_eq!(app.editor.lines[2], "int bad;");
        assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (2, 4));
        assert!(app.status.contains("read-only"), "{}", app.status);
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&opened).unwrap().permissions().mode() & 0o222,
            0
        );

        app.editor.open_preview(&log).unwrap();
        select(&mut app, "R2");
        app.open_selected_sarif_result();
        let opened = app.editor.path.clone().expect("a copy opened");
        assert_eq!(std::fs::read(&opened).unwrap(), [0, 1, 2, 255]);
        assert!(
            app.editor.has_non_text_view(),
            "binary opens in the hex viewer"
        );
    });
}

#[test]
fn a_sarif_log_rewritten_on_disk_keeps_the_readers_place() {
    // #577: a rebuild that rewrites the log (a scan re-run) used to reopen
    // the viewer from scratch, losing the selection, filter, folds and any
    // log added with `o`.
    let tmp = tempfile::tempdir().unwrap();
    let result = |rule: &str, line: u32| {
        format!(
            r#"{{"ruleId":"{rule}","level":"warning","message":{{"text":"{rule} here"}},"locations":[{{"physicalLocation":{{"artifactLocation":{{"uri":"src/{rule}.rs"}},"region":{{"startLine":{line}}}}}}}]}}"#
        )
    };
    let log_of = |results: &[String]| {
        format!(
            r#"{{"version":"2.1.0","runs":[{{"tool":{{"driver":{{"name":"lint"}}}},"results":[{}]}}]}}"#,
            results.join(",")
        )
    };
    let main = tmp.path().join("main.sarif");
    let extra = tmp.path().join("extra.sarif");
    std::fs::write(&main, log_of(&[result("Aa", 1), result("Bb", 2)])).unwrap();
    std::fs::write(&extra, log_of(&[result("Cc", 3)])).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&main).unwrap();
    {
        let view = app.editor.sarif.as_mut().unwrap();
        let log = crate::sarif::load::parse_log(&std::fs::read_to_string(&extra).unwrap()).unwrap();
        assert!(view.add_log(&extra, log));
        view.query_text = String::from("-Cc");
        view.filters
            .hidden_levels
            .insert(crate::sarif::semantics::Level::Note);
        view.collapsed.insert(String::from("some-group"));
        let rows = view.rows();
        view.selected = rows
            .iter()
            .position(|r| matches!(r, crate::sarif::view::Row::Item { entry } if view.entries[*entry].rule_id == "Bb"))
            .unwrap();
    }
    // The scan runs again: a new result lands first, shifting the others.
    std::fs::write(
        &main,
        log_of(&[result("Zz", 9), result("Aa", 1), result("Bb", 2)]),
    )
    .unwrap();
    app.editor.revert_to_disk().unwrap();
    let view = app.editor.sarif.as_ref().unwrap();
    assert_eq!(view.logs.len(), 2, "the added log is still there");
    assert_eq!(view.query_text, "-Cc");
    assert!(
        view.filters
            .hidden_levels
            .contains(&crate::sarif::semantics::Level::Note)
    );
    assert!(view.collapsed.contains("some-group"));
    assert_eq!(view.entries.len(), 4, "the rewritten log was read");
    assert_eq!(
        view.selected_entry().map(|e| e.rule_id.as_str()),
        Some("Bb"),
        "the same result stays selected though it moved"
    );
}

#[test]
fn sarif_columns_are_chosen_with_c_shown_on_rows_and_remembered() {
    // #577: optional columns between a row's position and its message.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let (tmp, log) = sarif_fixture();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.editor.open(&log).unwrap();
        // The row is read at a fixed width; the minimap strip, drawn in
        // every terminal since #1231, would take six of its columns.
        app.minimap_visible = false;
        assert!(
            app.editor.sarif.as_ref().unwrap().columns.is_empty(),
            "none by default"
        );
        let draw = |app: &mut App| {
            let mut term =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 30)).unwrap();
            term.draw(|f| app.render(f)).unwrap();
            screen_text(&term)
        };
        let row = |screen: &str| {
            screen
                .lines()
                .find(|l| l.contains("Query built from user input."))
                .and_then(|l| {
                    // The list part only: the detail pane shares the line.
                    let end = l.find("input.")? + "input.".len();
                    Some(l[..end].to_string())
                })
                .unwrap_or_default()
        };
        assert!(
            !row(&draw(&mut app)).contains("R1 "),
            "{}",
            row(&draw(&mut app))
        );

        // An unknown name is refused and names the choices.
        app.handle_sarif_key(key(KeyCode::Char('C'), KeyModifiers::NONE));
        app.input_prompt.as_mut().unwrap().value = String::from("rule, colour");
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();
        assert!(
            app.status.contains("No column \"colour\""),
            "{}",
            app.status
        );
        assert!(app.status.contains("rule, level, kind"), "{}", app.status);

        app.handle_sarif_key(key(KeyCode::Char('C'), KeyModifiers::NONE));
        app.input_prompt.as_mut().unwrap().value = String::from("rule, baseline");
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.status, "SARIF columns: rule, baseline");
        let shown = row(&draw(&mut app));
        let (rule, base, msg) = (
            shown.find("R1").expect(&shown),
            shown.find("no baseline").expect(&shown),
            shown.find("Query built").unwrap(),
        );
        assert!(rule < base && base < msg, "in the chosen order: {shown}");

        // Remembered: the next viewer opens with them, and C offers them.
        let mut again = App::new(tmp.path().to_path_buf()).unwrap();
        again.editor.open(&log).unwrap();
        assert_eq!(
            again.editor.sarif.as_ref().unwrap().columns,
            [
                crate::sarif::view::ExtraColumn::Rule,
                crate::sarif::view::ExtraColumn::Baseline
            ]
        );
        again.handle_sarif_key(key(KeyCode::Char('C'), KeyModifiers::NONE));
        assert_eq!(again.input_prompt.as_ref().unwrap().value, "rule,baseline");
    });
}

#[test]
fn sarif_filter_words_are_highlighted_in_the_results() {
    // Its own cache: the saved column choice (another test's) would push
    // the message out of the row.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        // #577: the filter's words are marked where a result's message shows them.
        let (tmp, log) = sarif_fixture();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.editor.open(&log).unwrap();
        app.editor.sarif.as_mut().unwrap().set_query("user");
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 30)).unwrap();
        term.draw(|f| app.render(f)).unwrap();
        let buf = term.backend().buffer().clone();
        let (y, line) = (0..buf.area.height)
            .map(|y| {
                let s: String = (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect();
                (y, s)
            })
            .find(|(_, s)| s.contains("Query built from user input."))
            .expect("the row is drawn");
        let at = line[..line.find("user input").unwrap()].chars().count() as u16;
        let marked = |x: u16| {
            buf[(x, y)]
                .modifier
                .contains(ratatui::style::Modifier::UNDERLINED)
        };
        assert!((at..at + 4).all(marked), "\"user\" is marked");
        assert!(!marked(at + 4) && !marked(at - 2), "and nothing around it");
    });
}

#[test]
fn a_sarif_results_steps_are_drawn_in_their_own_files_only() {
    // #577: the selected result's analysis steps appear after their lines,
    // in the file each step is in and no other.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("src")).unwrap();
    std::fs::write(
        tmp.path().join("src/a.rs"),
        "fn a() {\n    let x = input();\n    sink(x);\n}\n",
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("src/b.rs"),
        "fn input() -> String {\n    read()\n}\n",
    )
    .unwrap();
    let step = |uri: &str, line: u32, msg: &str| {
        format!(
            r#"{{"location":{{"message":{{"text":"{msg}"}},"physicalLocation":{{"artifactLocation":{{"uri":"{uri}"}},"region":{{"startLine":{line}}}}}}}}}"#
        )
    };
    let log = tmp.path().join("flow.sarif");
    std::fs::write(
        &log,
        format!(
            r#"{{"version":"2.1.0","runs":[{{"tool":{{"driver":{{"name":"t"}}}},"results":[{{"ruleId":"R1","message":{{"text":"tainted"}},
              "locations":[{{"physicalLocation":{{"artifactLocation":{{"uri":"src/a.rs"}},"region":{{"startLine":3}}}}}}],
              "codeFlows":[{{"threadFlows":[{{"locations":[{},{},{}]}}]}}]}}]}}]}}"#,
            step("src/b.rs", 2, "source"),
            step("src/a.rs", 2, "assigned"),
            step("src/a.rs", 3, "sink")
        ),
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_preview(&log).unwrap();
    app.open_selected_sarif_result();
    assert_eq!(
        app.editor.path.as_deref(),
        Some(tmp.path().join("src/a.rs").as_path())
    );
    app.sync_sarif_step_marks();
    let labels = |app: &App, line: usize| -> Vec<String> {
        app.editor
            .inlay_spans_for_test(line)
            .iter()
            .map(|(_, l, _)| l.trim().to_string())
            .collect()
    };
    assert_eq!(labels(&app, 1), ["\u{25c2} step 2: assigned"]);
    assert_eq!(labels(&app, 2), ["\u{25c2} step 3: sink"]);
    assert!(
        labels(&app, 0).is_empty(),
        "b.rs's step is not drawn in a.rs"
    );
    // The mark sits after the line's text.
    let (col, _, _) = &app.editor.inlay_spans_for_test(1)[0];
    assert_eq!(*col, "    let x = input();".chars().count());
}

#[test]
fn sarif_filter_typing_narrows_the_list() {
    let (tmp, log) = sarif_fixture();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&log).unwrap();
    app.handle_sarif_key(key(KeyCode::Char('/'), KeyModifiers::NONE));
    for c in "nomatch".chars() {
        app.handle_sarif_key(key(KeyCode::Char(c), KeyModifiers::NONE));
    }
    let view = app.editor.sarif.as_ref().unwrap();
    assert_eq!(view.query_text, "nomatch");
    assert!(view.rows().is_empty());
    // Esc leaves the filter box and clears it.
    app.handle_sarif_key(key(KeyCode::Esc, KeyModifiers::NONE));
    let view = app.editor.sarif.as_ref().unwrap();
    assert_eq!(view.query_text, "");
    assert_eq!(view.rows().len(), 2);
}

#[test]
fn sarif_that_does_not_load_opens_as_text_and_says_why() {
    let tmp = tempfile::tempdir().unwrap();
    let bad = tmp.path().join("bad.sarif");
    std::fs::write(&bad, "{\n  \"version\": \"2.1.0\",\n  oops\n}\n").unwrap();
    let old = tmp.path().join("old.sarif");
    std::fs::write(&old, r#"{"version":"1.0.0","runs":[]}"#).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();

    app.editor.open(&bad).unwrap();
    assert!(app.editor.sarif.is_none());
    assert_eq!(app.editor.lines[2], "  oops");
    assert!(
        app.editor.status.contains("invalid JSON at 3:"),
        "status names the position: {}",
        app.editor.status
    );

    app.editor.open(&old).unwrap();
    assert!(app.editor.sarif.is_none());
    assert!(
        app.editor.status.contains("1.0.0"),
        "status names the version: {}",
        app.editor.status
    );
}

#[cfg(unix)]
#[test]
fn a_codeql_query_suite_runs_through_database_analyze_into_sarif() {
    // #578: VS Code's "CodeQL: Run Query Suite" runs the open .qls on the
    // selected database; a suite only runs through `database analyze`, so
    // its results are SARIF whatever its queries' kinds.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    use crate::widgets::command_palette::Command;
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let mut app = codeql_query_fixture(tmp.path(), "select 1");
        app.codeql_program = fake_codeql(bin.path(), r#"{"version":"2.1.0","runs":[]}"#, 0, "");
        // `resolve queries` answers with the suite's queries: one alert
        // query, listed in a file the test can switch to a table query.
        let alert = tmp.path().join("Alert.ql");
        std::fs::write(&alert, "/** @kind problem */ select 1").unwrap();
        let listed = bin.path().join("resolved.json");
        std::fs::write(&listed, format!("[\"{}\"]", alert.display())).unwrap();
        let script = std::fs::read_to_string(&app.codeql_program).unwrap();
        std::fs::write(
            &app.codeql_program,
            script.replacen(
                "\n",
                &format!(
                    "\n[ \"$1\" = resolve ] && {{ cat '{}'; exit 0; }}\n",
                    listed.display()
                ),
                1,
            ),
        )
        .unwrap();
        app.run_command(Command::CodeqlRunQuerySuite);
        assert_eq!(app.status, "Open a .qls query suite to run it");
        assert!(app.codeql_run.is_none());

        let suite = tmp.path().join("s.qls");
        std::fs::write(&suite, "- queries: .\n").unwrap();
        app.editor.open(&suite).unwrap();
        // The CLI reads the saved suite, so unsaved edits are refused.
        app.editor.dirty = true;
        app.run_command(Command::CodeqlRunQuerySuite);
        assert_eq!(
            app.status,
            "Save s.qls before running it: CodeQL reads it from disk"
        );
        assert!(app.codeql_run.is_none());
        app.editor.dirty = false;
        app.run_command(Command::CodeqlRunQuerySuite);
        assert!(
            app.status.contains("Running s.qls on app"),
            "{}",
            app.status
        );
        wait_for_codeql(&mut app);
        let opened = app.editor.path.clone().expect("the results open");
        assert_eq!(opened.extension().and_then(|e| e.to_str()), Some("sarif"));
        let run = &app.codeql.history[0];
        assert_eq!(
            (&run.status, run.name.as_str(), run.database.as_str()),
            (&crate::codeql_query::RunStatus::Succeeded, "s.qls", "app"),
            "{:?}",
            app.codeql.history
        );
        let calls = std::fs::read_to_string(bin.path().join("codeql-calls.log")).unwrap();
        assert!(
            calls.contains(&format!(
                "database analyze {} {} --format=sarif-latest ",
                tmp.path().join("dbs/app").display(),
                suite.display()
            )),
            "{calls}"
        );

        // A suite selecting a table query is refused before the analysis:
        // `database analyze` cannot produce its results.
        let table = tmp.path().join("Table.ql");
        std::fs::write(&table, "select 1").unwrap();
        std::fs::write(
            &listed,
            format!("[\"{}\",\"{}\"]", alert.display(), table.display()),
        )
        .unwrap();
        app.editor.open(&suite).unwrap();
        app.run_command(Command::CodeqlRunQuerySuite);
        wait_for_codeql(&mut app);
        assert!(
            matches!(
                &app.codeql.history[0].status,
                crate::codeql_query::RunStatus::Failed(why) if why.contains("Table.ql")
            ),
            "the refusal names the table query: {:?}",
            app.codeql.history
        );

        // "Run Query on Selected Database" takes a suite too.
        app.editor.open(&suite).unwrap();
        app.run_command(Command::CodeqlRunQuery);
        assert!(
            app.status.contains("Running s.qls on app"),
            "{}",
            app.status
        );
        wait_for_codeql(&mut app);
        assert_eq!(
            Command::from_id("codeql_run_query_suite"),
            Some(Command::CodeqlRunQuerySuite)
        );
    });
}
/// A log whose one result carries a two-step code flow, a stack and rule help.
fn sarif_flow_fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("src")).unwrap();
    std::fs::write(
        tmp.path().join("src/api.rs"),
        "fn a() {}\nlet input = read();\n",
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("src/db.rs"),
        "one\ntwo\nthree\nrun(input);\n",
    )
    .unwrap();
    let log = tmp.path().join("flow.sarif");
    std::fs::write(
        &log,
        r#"{"version":"2.1.0","runs":[{"tool":{"driver":{"name":"lint","rules":[{"id":"R1","name":"Taint",
            "help":{"text":"Use parameters instead."},"properties":{"tags":["security"]}}]}},
          "results":[{"ruleId":"R1","level":"error",
            "message":{"text":"Tainted by [the input](1)."},
            "locations":[{"physicalLocation":{"artifactLocation":{"uri":"src/db.rs"},"region":{"startLine":4,"startColumn":1}}}],
            "relatedLocations":[{"id":1,"physicalLocation":{"artifactLocation":{"uri":"src/api.rs"},"region":{"startLine":2,"startColumn":5}}}],
            "codeFlows":[{"threadFlows":[{"locations":[
              {"location":{"message":{"text":"source"},"physicalLocation":{"artifactLocation":{"uri":"src/api.rs"},"region":{"startLine":2,"startColumn":5}}}},
              {"location":{"message":{"text":"sink"},"physicalLocation":{"artifactLocation":{"uri":"src/db.rs"},"region":{"startLine":4,"startColumn":5}}}}]}]}],
            "stacks":[{"frames":[{"location":{"message":{"text":"frame zero"},"physicalLocation":{"artifactLocation":{"uri":"src/db.rs"},"region":{"startLine":3,"startColumn":2}}}}]}]
          }]}]}"#,
    )
    .unwrap();
    (tmp, log)
}

#[test]
fn sarif_info_tab_shows_rule_help_tags_and_related_locations() {
    let (tmp, log) = sarif_flow_fixture();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&log).unwrap();
    let screen = draw_screen(&mut app);
    assert!(
        screen.contains("Use parameters instead."),
        "rule help:\n{screen}"
    );
    assert!(screen.contains("security"), "tags:\n{screen}");
    assert!(
        screen.contains("src/api.rs:2:5"),
        "related location:\n{screen}"
    );
}

#[test]
fn sarif_steps_tab_walks_the_code_flow_in_the_editor() {
    let (tmp, log) = sarif_flow_fixture();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_preview(&log).unwrap();
    app.handle_sarif_key(key(KeyCode::Char('d'), KeyModifiers::NONE));
    let screen = draw_screen(&mut app);
    assert!(screen.contains("Steps"), "{screen}");
    assert!(
        screen.contains("source") && screen.contains("sink"),
        "{screen}"
    );
    app.handle_sarif_key(key(KeyCode::Char('n'), KeyModifiers::NONE));
    assert_eq!(
        app.editor.path.as_deref(),
        Some(tmp.path().join("src/api.rs").as_path()),
        "step 1 opened: {}",
        app.status
    );
    assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (1, 4));
    // Back to the viewer tab and on to step 2.
    let viewer = (0..app.editor.tab_count())
        .find(|&i| app.editor.tab_path(i).as_deref() == Some(log.as_path()))
        .expect("the viewer tab stayed open");
    app.editor.select(viewer);
    app.handle_sarif_key(key(KeyCode::Char('n'), KeyModifiers::NONE));
    assert_eq!(
        app.editor.path.as_deref(),
        Some(tmp.path().join("src/db.rs").as_path())
    );
    assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (3, 4));
}

#[test]
fn sarif_stacks_and_raw_tabs() {
    let (tmp, log) = sarif_flow_fixture();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&log).unwrap();
    app.handle_sarif_key(key(KeyCode::Char('d'), KeyModifiers::NONE));
    app.handle_sarif_key(key(KeyCode::Char('d'), KeyModifiers::NONE));
    let screen = draw_screen(&mut app);
    assert!(screen.contains("frame zero"), "stacks tab:\n{screen}");
    assert!(
        screen.contains("src/db.rs:3:2"),
        "frame location with its column:\n{screen}"
    );
    app.handle_sarif_key(key(KeyCode::Char('d'), KeyModifiers::NONE));
    let screen = draw_screen(&mut app);
    assert!(screen.contains("\"ruleId\": \"R1\""), "raw tab:\n{screen}");
    app.handle_sarif_key(key(KeyCode::Char('D'), KeyModifiers::NONE));
    let screen = draw_screen(&mut app);
    assert!(
        screen.contains("frame zero"),
        "D goes back a tab:\n{screen}"
    );
}

#[test]
fn sarif_message_link_opens_its_related_location() {
    let (tmp, log) = sarif_flow_fixture();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_preview(&log).unwrap();
    app.handle_sarif_key(key(KeyCode::Char('L'), KeyModifiers::NONE));
    assert_eq!(
        app.editor.path.as_deref(),
        Some(tmp.path().join("src/api.rs").as_path()),
        "[the input](1) followed: {}",
        app.status
    );
    assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (1, 4));
}

#[test]
fn an_open_sarif_log_publishes_its_results_as_diagnostics() {
    // #577: results reach the editor through the ordinary diagnostics store
    // under "SARIF: <tool>", so squiggles, hover and Problems all see them.
    let (tmp, log) = sarif_fixture();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_preview(&log).unwrap();
    assert!(app.sync_sarif_diagnostics(), "publishing is a change");
    let target = tmp.path().join("src/a.rs");
    let by_source = app
        .lsp_diagnostics
        .get(&target)
        .expect("the result's file has diagnostics");
    let diags = by_source.get("SARIF: lint").expect("keyed by the tool");
    assert_eq!(diags.len(), 1);
    let d = &diags[0];
    assert_eq!((d.start_line, d.start_char), (1, 8));
    assert_eq!(d.end_line, 1);
    assert_eq!(
        d.end_char as usize,
        "    let q = format!(\"{}\", id);".encode_utf16().count(),
        "no endColumn: the range runs to the end of the start line"
    );
    assert_eq!(d.severity, crate::lsp::manager::DiagnosticSeverity::Error);
    assert_eq!(d.message, "[R1] Query built from user input.");
    assert!(
        !app.sync_sarif_diagnostics(),
        "an unchanged log is not a change"
    );

    // The file's own tab gets them as squiggles. The viewer is pinned first,
    // as opening a result does, so the preview open does not replace it.
    app.editor.pin_active();
    app.editor.open_preview(&target).unwrap();
    app.sync_sarif_diagnostics();
    let merged = app.merged_diagnostics(&target);
    assert!(merged.iter().any(|d| d.message.contains("[R1]")));
}

#[test]
fn closing_the_sarif_tab_withdraws_its_diagnostics() {
    let (tmp, log) = sarif_fixture();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&log).unwrap();
    app.sync_sarif_diagnostics();
    let target = tmp.path().join("src/a.rs");
    assert!(app.lsp_diagnostics.contains_key(&target));
    // Replace the log's tab with an ordinary file: no SARIF view is open.
    app.editor.open(&target).unwrap();
    assert!(app.sync_sarif_diagnostics(), "withdrawing is a change");
    assert!(
        app.lsp_diagnostics
            .get(&target)
            .is_none_or(|m| !m.contains_key("SARIF: lint")),
        "nothing left behind"
    );
}

#[test]
fn sarif_levels_map_to_editor_severities() {
    use crate::lsp::manager::DiagnosticSeverity as S;
    use crate::sarif::semantics::Level;
    assert_eq!(crate::sarif::diagnostics::severity(Level::Error), S::Error);
    assert_eq!(
        crate::sarif::diagnostics::severity(Level::Warning),
        S::Warning
    );
    assert_eq!(
        crate::sarif::diagnostics::severity(Level::Note),
        S::Information
    );
    assert_eq!(
        crate::sarif::diagnostics::severity(Level::None),
        S::Information
    );
}

/// A log whose one result carries a fix for the line it reports.
fn sarif_fix_fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("q.rs"), "let q = format!(\"{}\", id);\n").unwrap();
    let log = tmp.path().join("fix.sarif");
    std::fs::write(
        &log,
        r#"{"version":"2.1.0","runs":[{"tool":{"driver":{"name":"lint"}},"columnKind":"unicodeCodePoints",
          "results":[{"ruleId":"R9","level":"warning","message":{"text":"Unsanitised id."},
            "locations":[{"physicalLocation":{"artifactLocation":{"uri":"q.rs"},"region":{"startLine":1,"startColumn":23}}}],
            "fixes":[{"description":{"text":"Sanitise it"},"artifactChanges":[{"artifactLocation":{"uri":"q.rs"},
              "replacements":[{"deletedRegion":{"startLine":1,"startColumn":23,"endColumn":25},"insertedContent":{"text":"clean(id)"}}]}]}]}]}]}"#,
    )
    .unwrap();
    (tmp, log)
}

#[test]
fn sarif_fix_tab_previews_and_f_applies_the_fix_as_an_unsaved_edit() {
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let (tmp, log) = sarif_fix_fixture();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.editor.open_preview(&log).unwrap();
        // The Fix tab is the fifth: Info, Steps, Stacks, Raw, Fix.
        for _ in 0..4 {
            app.handle_sarif_key(key(KeyCode::Char('d'), KeyModifiers::NONE));
        }
        let screen = draw_screen(&mut app);
        assert!(
            screen.contains("Sanitise it"),
            "the fix's description:\n{screen}"
        );
        assert!(
            screen.contains("+ let q = format!(\"{}\", clean(id));"),
            "its preview:\n{screen}"
        );
        app.handle_sarif_key(key(KeyCode::Char('f'), KeyModifiers::NONE));
        assert_eq!(
            app.editor.path.as_deref(),
            Some(tmp.path().join("q.rs").as_path()),
            "the fixed file is open: {}",
            app.status
        );
        assert_eq!(app.editor.lines[0], "let q = format!(\"{}\", clean(id));");
        assert!(
            app.editor.dirty,
            "applied to the buffer, not saved behind the user's back"
        );
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("q.rs")).unwrap(),
            "let q = format!(\"{}\", id);\n",
            "the disk is untouched"
        );
        // The result is marked fixed, and that survives reopening the log.
        let viewer = (0..app.editor.tab_count())
            .find(|&i| app.editor.tab_path(i).as_deref() == Some(log.as_path()))
            .unwrap();
        app.editor.select(viewer);
        assert!(app.editor.sarif.as_ref().unwrap().entries[0].fixed);
        let mut again = App::new(tmp.path().to_path_buf()).unwrap();
        again.editor.open(&log).unwrap();
        assert!(
            again.editor.sarif.as_ref().unwrap().entries[0].fixed,
            "persisted"
        );
        // A fixed result no longer squiggles.
        again.sync_sarif_diagnostics();
        assert!(
            again
                .lsp_diagnostics
                .get(&tmp.path().join("q.rs"))
                .is_none_or(|m| m.values().all(|v| v.is_empty())),
            "fixed results are withdrawn from the editor"
        );
    });
}

#[test]
fn next_and_previous_sarif_result_walk_locations_across_files() {
    let (tmp, log) = sarif_nav_fixture();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&log).unwrap();
    app.editor.pin_active();
    app.open_at(&tmp.path().join("a.rs"), 0, 0).unwrap();
    use crate::widgets::command_palette::Command;
    app.run_command(Command::SarifNextResult);
    assert_eq!(
        (app.editor.cursor_row, app.status.contains("first")),
        (1, true),
        "{}",
        app.status
    );
    app.run_command(Command::SarifNextResult);
    assert_eq!(app.editor.cursor_row, 4);
    app.run_command(Command::SarifNextResult);
    assert_eq!(
        app.editor.path.as_deref(),
        Some(tmp.path().join("b.rs").as_path()),
        "on to the next file"
    );
    app.run_command(Command::SarifNextResult);
    assert_eq!(
        (app.editor.path.as_deref(), app.editor.cursor_row),
        (Some(tmp.path().join("a.rs").as_path()), 1),
        "wraps to the first"
    );
    app.run_command(Command::SarifPreviousResult);
    assert_eq!(
        app.editor.path.as_deref(),
        Some(tmp.path().join("b.rs").as_path())
    );
    assert_eq!(Command::SarifNextResult.title(), "SARIF: Next Result");
}

/// #1369, negative: the same log not written by a CodeQL run (no history
/// entry names it) has no database to ask, so croft still asks where the
/// file is.
#[test]
fn a_sarif_log_from_no_codeql_run_still_asks_to_locate_its_file() {
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let app_dir = tmp.path().join("cq/app");
        std::fs::create_dir_all(&app_dir).unwrap();
        std::fs::write(app_dir.join("web.py"), WEB_PY_1369).unwrap();
        let db = tmp.path().join("db");
        std::fs::create_dir_all(&db).unwrap();
        std::fs::write(
            db.join(crate::codeql_db::DB_MARKER),
            format!("sourceLocationPrefix: {}\n", app_dir.display()),
        )
        .unwrap();
        let log = codeql_run_on(tmp.path(), &db);
        let other = tmp.path().join("ci.sarif");
        std::fs::copy(&log, &other).unwrap();
        let mut app = App::new(tmp.path().join("cq")).unwrap();
        app.editor.open_preview(&other).unwrap();
        app.editor.sarif.as_mut().unwrap().selected = 1;
        app.open_selected_sarif_result();
        assert!(
            matches!(
                app.input_prompt.as_ref().map(|p| &p.purpose),
                Some(crate::widgets::input_prompt::InputPurpose::SarifLocate { .. })
            ),
            "a Locate prompt: {}",
            app.status
        );
    });
}

#[test]
fn the_run_tab_shows_the_tool_its_invocation_and_notifications() {
    // #577, beyond VS Code: run.tool, invocations and notifications.
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("run.sarif");
    std::fs::write(
        &log,
        r#"{"version":"2.1.0","runs":[{
          "tool":{"driver":{"name":"CodeQL","semanticVersion":"2.19.3","informationUri":"https://codeql.github.com"}},
          "invocations":[{"commandLine":"codeql database analyze db --format=sarif-latest","executionSuccessful":true,"exitCode":0,
            "startTimeUtc":"2026-09-24T10:00:00Z","endTimeUtc":"2026-09-24T10:03:12Z",
            "toolExecutionNotifications":[{"level":"warning","message":{"text":"Skipped vendor/huge.js: too large"}}]}],
          "results":[]}]}"#,
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&log).unwrap();
    for _ in 0..3 {
        app.handle_sarif_key(key(KeyCode::Tab, KeyModifiers::NONE));
    }
    assert_eq!(
        app.editor.sarif.as_ref().unwrap().tab,
        crate::sarif::view::Tab::Run
    );
    let screen = draw_screen(&mut app);
    for want in [
        "CodeQL 2.19.3",
        "https://codeql.github.com",
        "codeql database analyze db --format=sarif-latest",
        "exit 0",
        "Skipped vendor/huge.js: too large",
    ] {
        assert!(screen.contains(want), "{want:?} on the Run tab:\n{screen}");
    }
    // Back round to Locations.
    app.handle_sarif_key(key(KeyCode::Tab, KeyModifiers::NONE));
    assert_eq!(
        app.editor.sarif.as_ref().unwrap().tab,
        crate::sarif::view::Tab::Locations
    );
}

#[test]
fn e_exports_the_visible_sarif_results_to_a_csv_file() {
    // #577: the filtered list leaves the editor as a spreadsheet.
    let (tmp, log) = sarif_fixture();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&log).unwrap();
    app.handle_sarif_key(key(KeyCode::Char('E'), KeyModifiers::SHIFT));
    let prompt = app.input_prompt.as_ref().expect("E asks where to write");
    assert!(matches!(
        prompt.purpose,
        crate::widgets::input_prompt::InputPurpose::SarifExport
    ));
    assert_eq!(
        prompt.value, "sarif-results.csv",
        "a default name is offered"
    );
    app.submit_sarif_export("out/results.csv");
    let written = std::fs::read_to_string(tmp.path().join("out/results.csv"))
        .expect("written relative to the workspace, creating the folder");
    assert!(
        written.starts_with("rule,level,file,line,column,message"),
        "{written}"
    );
    assert_eq!(
        written.lines().count(),
        2,
        "one result plus the header: {written}"
    );
    assert!(app.status.contains("1 result"), "{}", app.status);
}

#[test]
fn the_sarif_key_legend_names_the_fix_log_baseline_and_export_keys() {
    // #577: every viewer key is discoverable from its footer at a common width.
    let (tmp, log) = sarif_fixture();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&log).unwrap();
    let screen = draw_screen(&mut app);
    for want in ["f fix", "o add log", "b baseline", "E export"] {
        assert!(screen.contains(want), "{want} missing:\n{screen}");
    }
}

/// A stand-in `gh` answering from canned files: each rule maps a substring
/// of the arguments to the file whose contents it prints.
#[cfg(unix)]
fn fake_gh(dir: &std::path::Path, rules: &[(&str, &str)]) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let mut script = String::from(
        "#!/bin/sh\necho \"$*\" >> \"$(dirname \"$0\")/gh-calls.log\"\ncase \"$*\" in\n",
    );
    for (i, (pattern, body)) in rules.iter().enumerate() {
        let file = dir.join(format!("gh-reply-{i}"));
        std::fs::write(&file, body).unwrap();
        script.push_str(&format!("  *'{pattern}'*) cat '{}' ;;\n", file.display()));
    }
    script.push_str("  *) echo '{\"message\":\"unexpected call\"}' >&2; exit 1 ;;\nesac\n");
    let gh = dir.join("gh");
    std::fs::write(&gh, script).unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    gh
}

#[cfg(unix)]
#[test]
fn a_code_scanning_analysis_opens_from_github_in_the_sarif_viewer() {
    // #577: VS Code's SARIF viewer pulls a repository's code scanning
    // analyses; croft lists them and opens the chosen one as a log.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let gh = fake_gh(
            bin.path(),
            &[
                (
                    "analyses?per_page=30",
                    r#"[{"id":201,"ref":"refs/heads/main","commit_sha":"abc","error":"","category":"",
                        "created_at":"2026-09-20T10:00:00Z","results_count":1,"tool":{"name":"CodeQL","version":"2.19.3"}}]"#,
                ),
                (
                    "analyses/201",
                    r#"{"version":"2.1.0","runs":[{"tool":{"driver":{"name":"CodeQL"}},
                        "results":[{"ruleId":"rust/sql-injection","message":{"text":"From GitHub."}}]}]}"#,
                ),
            ],
        );
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.gh_program = gh;
        app.run_command(crate::widgets::command_palette::Command::SarifOpenCodeScanning);
        let picker = app.list_picker.as_ref().expect("the analyses are offered");
        assert_eq!(
            picker.purpose,
            crate::widgets::list_picker::ListPurpose::CodeScanningAnalysis
        );
        assert_eq!(picker.rows.len(), 1);
        assert_eq!(picker.rows[0].id, "201");
        assert!(
            picker.rows[0].label.contains("CodeQL 2.19.3"),
            "{}",
            picker.rows[0].label
        );
        app.confirm_list_picker();
        let view = app
            .editor
            .sarif
            .as_ref()
            .expect("the analysis opens in the viewer");
        assert!(view.entries.iter().any(|e| e.message == "From GitHub."));
        let calls = std::fs::read_to_string(bin.path().join("gh-calls.log")).unwrap();
        assert!(calls.contains("Accept: application/sarif+json"), "{calls}");
    });
}

#[cfg(unix)]
#[test]
fn code_scanning_reports_githubs_own_error() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let gh = fake_gh(bin.path(), &[]);
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.gh_program = gh;
    app.run_command(crate::widgets::command_palette::Command::SarifOpenCodeScanning);
    assert!(app.list_picker.is_none());
    assert!(app.status.contains("unexpected call"), "{}", app.status);
}

#[cfg(unix)]
#[test]
fn x_dismisses_the_selected_results_code_scanning_alert_with_a_reason() {
    // #577: VS Code's SARIF viewer dismisses GitHub alerts from the list.
    // This result carries no alert number, so it is matched to the alerts
    // list by rule, path and line.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("src")).unwrap();
    std::fs::write(tmp.path().join("src/db.rs"), "a\nb\nc\n").unwrap();
    let log = tmp.path().join("scan.sarif");
    std::fs::write(
        &log,
        r#"{"version":"2.1.0","runs":[{"tool":{"driver":{"name":"CodeQL"}},"results":[
            {"ruleId":"rust/sql-injection","message":{"text":"Tainted."},
             "locations":[{"physicalLocation":{"artifactLocation":{"uri":"src/db.rs"},"region":{"startLine":2}}}]}]}]}"#,
    )
    .unwrap();
    let bin = tempfile::tempdir().unwrap();
    let gh = fake_gh(
        bin.path(),
        &[
            (
                "code-scanning/alerts?per_page=100",
                r#"[{"number":8,"state":"open","rule":{"id":"rust/sql-injection"},
                     "most_recent_instance":{"location":{"path":"src/db.rs","start_line":2}}}]"#,
            ),
            ("PATCH", r#"{"number":8,"state":"dismissed"}"#),
        ],
    );
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.gh_program = gh;
    app.editor.open(&log).unwrap();
    app.editor.sarif.as_mut().unwrap().selected = 1;
    app.handle_sarif_key(key(KeyCode::Char('X'), KeyModifiers::SHIFT));
    let picker = app.list_picker.as_ref().expect("X asks why");
    assert_eq!(
        picker.purpose,
        crate::widgets::list_picker::ListPurpose::DismissAlert
    );
    let labels: Vec<&str> = picker.rows.iter().map(|r| r.label.as_str()).collect();
    assert_eq!(labels, ["False positive", "Won't fix", "Used in tests"]);
    // Pick "Won't fix", then give a comment.
    app.list_picker.as_mut().unwrap().selected = 1;
    app.confirm_list_picker();
    assert!(matches!(
        app.input_prompt.as_ref().map(|p| &p.purpose),
        Some(
            crate::widgets::input_prompt::InputPurpose::DismissAlertComment {
                number: 8,
                reason: 1
            }
        )
    ));
    app.dismiss_code_scanning_alert(8, 1, "tracked elsewhere");
    let calls = std::fs::read_to_string(bin.path().join("gh-calls.log")).unwrap();
    assert!(
        calls.contains("PATCH repos/{owner}/{repo}/code-scanning/alerts/8 -f state=dismissed -f dismissed_reason=won't fix -f dismissed_comment=tracked elsewhere"),
        "{calls}"
    );
    assert!(app.status.contains("#8"), "{}", app.status);
    let view = app.editor.sarif.as_ref().unwrap();
    assert_eq!(
        view.entries[0].suppression,
        crate::sarif::semantics::SuppressionState::Suppressed,
        "the dismissed result leaves the default list"
    );
}

#[test]
fn the_info_tab_lists_the_results_taxa() {
    // #577: CWE / OWASP entries a result names, resolved through taxonomies.
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("t.sarif");
    std::fs::write(
        &log,
        r#"{"version":"2.1.0","runs":[{"tool":{"driver":{"name":"T"}},
            "taxonomies":[{"name":"CWE","taxa":[{"id":"CWE-89","name":"SqlInjection"}]}],
            "results":[{"ruleId":"R1","message":{"text":"m"},
              "taxa":[{"id":"CWE-89","toolComponent":{"name":"CWE"}}]}]}]}"#,
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&log).unwrap();
    app.editor.sarif.as_mut().unwrap().selected = 1;
    let screen = draw_screen(&mut app);
    assert!(screen.contains("CWE CWE-89: SqlInjection"), "{screen}");
}

#[cfg(unix)]
#[test]
fn code_scanning_offers_the_current_branchs_analyses_and_falls_back_to_all() {
    // #577: like VS Code's viewer, the analyses of the branch being worked
    // on come first; a branch with none shows the repository's.
    let tmp = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let one = |id: u64, git_ref: &str| {
        format!(
            r#"[{{"id":{id},"ref":"{git_ref}","commit_sha":"c","error":"","category":"","created_at":"2026-09-20T10:00:00Z","results_count":0,"tool":{{"name":"CodeQL"}}}}]"#
        )
    };
    let on_branch = one(301, "refs/heads/feat/x");
    let everything = one(300, "refs/heads/main");
    let gh = fake_gh(
        bin.path(),
        &[
            ("ref=refs%2Fheads%2Ffeat%2Fx", &on_branch),
            ("ref=refs%2Fheads%2Fempty", "[]"),
            ("analyses?per_page=30", &everything),
        ],
    );
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.gh_program = gh;
    app.source_control.status.branch = Some(String::from("feat/x"));
    app.run_command(crate::widgets::command_palette::Command::SarifOpenCodeScanning);
    let picker = app.list_picker.take().expect("offered");
    assert_eq!(
        picker
            .rows
            .iter()
            .map(|r| r.id.as_str())
            .collect::<Vec<_>>(),
        ["301"]
    );
    assert!(picker.title.contains("feat/x"), "{}", picker.title);
    app.source_control.status.branch = Some(String::from("empty"));
    app.run_command(crate::widgets::command_palette::Command::SarifOpenCodeScanning);
    let picker = app.list_picker.take().expect("offered");
    assert_eq!(
        picker
            .rows
            .iter()
            .map(|r| r.id.as_str())
            .collect::<Vec<_>>(),
        ["300"]
    );
}

/// #577: export to a `.sarif` name writes the visible results as SARIF,
/// each run kept with its tool and rules, filtered results left out and a
/// run with nothing left dropped; any other name still gets CSV.
#[test]
fn sarif_export_writes_the_visible_results_as_sarif_or_csv() {
    use crate::sarif::semantics::Level;
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("two.sarif");
    std::fs::write(
        &log,
        r#"{"version":"2.1.0","runs":[
            {"tool":{"driver":{"name":"lint","rules":[{"id":"R1"},{"id":"R2"}]}},
             "results":[{"ruleId":"R1","level":"error","message":{"text":"bad"}},
                        {"ruleId":"R2","level":"note","message":{"text":"meh"}}]},
            {"tool":{"driver":{"name":"other"}},
             "results":[{"ruleId":"X","level":"note","message":{"text":"quiet"}}]}]}"#,
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&log).unwrap();
    app.editor
        .sarif
        .as_mut()
        .unwrap()
        .filters
        .hidden_levels
        .insert(Level::Note);
    app.submit_sarif_export("out.sarif");
    assert!(app.status.contains("Exported 1 result"), "{}", app.status);
    let out: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(tmp.path().join("out.sarif")).unwrap())
            .unwrap();
    assert_eq!(out["version"], "2.1.0");
    let runs = out["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 1, "the run left empty is dropped");
    assert_eq!(runs[0]["tool"]["driver"]["name"], "lint");
    assert_eq!(
        runs[0]["tool"]["driver"]["rules"].as_array().unwrap().len(),
        2
    );
    let results = runs[0]["results"].as_array().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["ruleId"], "R1");
    // And the exported log opens again in the viewer.
    app.editor.open(&tmp.path().join("out.sarif")).unwrap();
    assert_eq!(app.editor.sarif.as_ref().unwrap().entries.len(), 1);

    app.editor.open(&log).unwrap();
    app.submit_sarif_export("out.csv");
    let csv = std::fs::read_to_string(tmp.path().join("out.csv")).unwrap();
    assert!(csv.starts_with("rule,level,file"), "{csv}");
}

#[test]
fn code_scanning_follows_the_branch_when_on_and_only_says_so_when_prompting() {
    // #577: `code_scanning = on` loads the branch's newest analysis per
    // tool at the nearest scanned commit, and again after a checkout of
    // another branch; `prompt` names them and waits; `off` never asks.
    use std::os::unix::fs::PermissionsExt;
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(tmp.path())
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "a@b"]);
        git(&["config", "user.name", "a"]);
        std::fs::write(tmp.path().join("a.txt"), "a").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "scanned"]);
        let scanned = git(&["rev-parse", "HEAD"]);
        std::fs::write(tmp.path().join("a.txt"), "b").unwrap();
        git(&["commit", "-q", "-am", "not scanned yet"]);

        let log = tmp.path().join("gh-args");
        let gh = tmp.path().join("gh");
        std::fs::write(
            &gh,
            format!(
                r#"#!/bin/sh
echo "$@" >> '{log}'
case "$*" in
  *sarif+json*) echo '{{"version": "2.1.0", "runs": []}}' ;;
  *) echo '[{{"id": 7, "ref": "refs/heads/main", "commit_sha": "{scanned}", "tool": {{"name": "CodeQL"}}, "created_at": "2026-09-01T00:00:00Z", "results_count": 0}}, {{"id": 6, "ref": "refs/heads/main", "commit_sha": "0000", "tool": {{"name": "CodeQL"}}, "created_at": "2026-09-02T00:00:00Z", "results_count": 0}}]' ;;
esac
"#,
                log = log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.gh_program = gh.clone();
        app.source_control.status.in_repo = true;
        app.source_control.status.branch = Some(String::from("main"));

        // Off: nothing is asked.
        app.poll_code_scanning();
        assert!(
            app.code_scan_list.is_none() && !log.exists(),
            "off never calls gh"
        );

        app.code_scanning = crate::sarif::github::CodeScanningMode::On;
        let settle = |app: &mut App| {
            crate::test_budget::await_spawned(
                std::time::Duration::from_secs(10),
                "code scanning",
                || {
                    app.poll_code_scanning();
                    app.code_scan_list.is_none() && app.code_scan_fetch.is_none()
                },
            );
        };
        app.poll_code_scanning();
        settle(&mut app);
        assert_eq!(app.status, "Loaded 1 code scanning analysis");
        assert!(
            app.editor.sarif.is_some(),
            "the analysis opened in the viewer"
        );
        let calls = std::fs::read_to_string(&log).unwrap();
        assert!(
            calls.contains("ref=refs%2Fheads%2Fmain") || calls.contains("ref=refs/heads/main"),
            "{calls}"
        );
        assert!(
            calls.contains("analyses/7"),
            "the one at the nearest scanned commit: {calls}"
        );
        assert!(!calls.contains("analyses/6"), "{calls}");

        // The same branch is not looked up again; another one is.
        std::fs::remove_file(&log).unwrap();
        app.poll_code_scanning();
        assert!(app.code_scan_list.is_none() && !log.exists());
        app.code_scanning = crate::sarif::github::CodeScanningMode::Prompt;
        app.source_control.status.branch = None;
        app.source_control.status.detached_hash = Some(scanned.clone());
        app.poll_code_scanning();
        settle(&mut app);
        assert!(
            app.status
                .starts_with("Code scanning has 1 analysis for detached HEAD"),
            "{}",
            app.status
        );
        let calls = std::fs::read_to_string(&log).unwrap();
        assert!(
            !calls.contains("sarif+json"),
            "prompt downloads nothing: {calls}"
        );
    });
}
