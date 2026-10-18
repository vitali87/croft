//! The Testing view: test discovery, runs, coverage and debugging a test.

use super::*;

#[test]
fn alt_clicking_the_gutter_play_glyph_debugs_that_test() {
    use crossterm::event::{MouseButton, MouseEventKind};
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"t\"\nversion = \"0.0.0\"\n",
    )
    .unwrap();
    let f = tmp.path().join("t.rs");
    std::fs::write(&f, "#[test]\nfn my_case() { assert!(true); }\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&f).unwrap();
    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|frame| app.render(frame)).unwrap();
    // Occupy the build slot so the debug route reports it instead of detaching
    // a real `cargo test --no-run` into a TempDir this test is about to delete.
    let (_tx, rx) = std::sync::mpsc::channel();
    app.pending_test_debug = Some((rx, String::from("other"), tmp.path().to_path_buf()));

    let col = app.editor.last_inner.x;
    let row = app.editor.last_inner.y + 1;
    let mut ev = mouse(MouseEventKind::Down(MouseButton::Left), col, row);
    ev.modifiers = KeyModifiers::ALT;
    app.handle_mouse(ev);
    // The ALT guard at the top of the Down(Left) arm returns ~1000 lines before
    // the gutter's own hit-test, so without an explicit glyph check there the
    // documented gesture reaches Go to Definition instead of the debugger.
    // Either debug status proves the click routed to the debugger; which one
    // depends on whether the host has an lldb-dap installed, and a machine
    // without one must not fail a test about a mouse gesture.
    assert!(
        app.status == "A test binary is already building"
            || app.status.starts_with("lldb-dap not found"),
        "Alt+click on the gutter play glyph must debug that test, got {:?}",
        app.status
    );
}

#[test]
fn clicking_the_gutter_play_glyph_runs_that_test() {
    use crossterm::event::{MouseButton, MouseEventKind};
    let tmp = tempfile::tempdir().unwrap();
    // A real Cargo marker: the run gestures refuse a workspace no enabled
    // runner claims, and this test pins the click-to-run wiring, not that.
    std::fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"t\"\nversion = \"0.0.0\"\n",
    )
    .unwrap();
    let f = tmp.path().join("t.rs");
    std::fs::write(&f, "#[test]\nfn my_case() { assert!(true); }\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&f).unwrap();
    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|frame| app.render(frame)).unwrap();
    // The play glyph sits in the sign margin on `fn my_case`'s row (buffer
    // line 1 -> second content row). Drive the REAL mouse dispatch so a
    // splitter grab zone or click-ordering regression fails this test.
    let col = app.editor.last_inner.x;
    let row = app.editor.last_inner.y + 1;
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), col, row));
    assert_eq!(
        app.status, "Running test my_case",
        "the glyph click must start that test by name"
    );
}

#[test]
fn a_workspace_change_cancels_the_pending_test_debug_build() {
    // The drain already refuses to launch a binary whose build root no
    // longer matches, but the occupied slot also kept refusing NEW builds
    // ("A test binary is already building") until the stale compile
    // finished. Re-rooting must free the slot immediately.
    let tmp = tempfile::tempdir().unwrap();
    let inner = tmp.path().join("inner");
    std::fs::create_dir(&inner).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let (_tx, rx) = std::sync::mpsc::channel();
    app.pending_test_debug = Some((rx, String::from("my_case"), tmp.path().to_path_buf()));
    app.change_workspace_root(inner);
    assert!(
        app.pending_test_debug.is_none(),
        "re-rooting drops the in-flight build so a new one can start"
    );
}

#[test]
fn changing_workspace_root_rebinds_the_test_worker_to_the_new_root() {
    let tmp = tempfile::tempdir().unwrap();
    let inner = tmp.path().join("inner");
    std::fs::create_dir_all(&inner).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    assert_eq!(app.test_worker.root(), tmp.path());

    app.change_workspace_root(inner.clone());

    assert_eq!(
        app.test_worker.root(),
        inner,
        "the test runner must follow the Explorer root; left at the launch dir (e.g. ~/Documents, no Cargo.toml) `cargo test -- --list` errors and the Testing view stays empty"
    );
}

#[test]
fn changing_workspace_root_resets_the_testing_panel() {
    let tmp = tempfile::tempdir().unwrap();
    let inner = tmp.path().join("inner");
    std::fs::create_dir_all(&inner).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // The old project's tree (e.g. pytest cases from a Python repo).
    app.testing.apply_case(crate::testing::model::TestCase {
        name: String::from("tests/test_x.py::test_a"),
        status: crate::testing::model::TestStatus::Failed,
    });
    assert!(!app.testing.is_empty());

    app.change_workspace_root(inner.clone());

    assert!(
        app.testing.is_empty(),
        "a workspace re-root must drop the old project's tests; a stale tree also blocks open_testing_view's discover-on-empty, so the new project is never discovered"
    );
    assert_eq!(
        app.testing.failed_count(),
        0,
        "the beaker badge must not keep counting the old project's failures"
    );
}

/// #843 on the path the issue took: on desktop Linux, `Ctrl`+`K` `B` with the
/// caret in the editor opens Testing and leaves the line alone. Before the
/// fix `Ctrl`+`K` killed the rest of the line and `b` was typed in its place.
#[cfg(not(target_os = "macos"))]
#[test]
fn ctrl_k_b_opens_testing_and_leaves_the_line_off_macos() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "notes.txt", "DATA = Path(\"data\")");
    app.editor.cursor_col = 0;
    app.handle_key(key(KeyCode::Char('k'), KeyModifiers::CONTROL))
        .unwrap();
    app.handle_key(key(KeyCode::Char('b'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(
        app.editor.lines[0], "DATA = Path(\"data\")",
        "nothing killed, nothing typed"
    );
    assert_eq!(app.sidebar_view, SidebarView::Testing);
}

/// #1639: a Testing run writes unsaved tabs first: the runner compiles or
/// imports the files on disk.
#[test]
fn a_test_run_saves_the_unsaved_buffer_first() {
    let (tmp, mut app, file) = app_with_unsaved_pinned_file("lib.rs");
    std::fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"t\"\nversion = \"0.0.0\"\n",
    )
    .unwrap();
    app.run_all_tests();
    assert_ne!(app.status, crate::testing::NO_RUNNER_STATUS);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "new\nold\n");
    app.editor.insert_str("newer\n");
    app.testing.apply_case(crate::testing::model::TestCase {
        name: "parse::run".into(),
        status: crate::testing::model::TestStatus::NotRun,
    });
    app.run_named_test(String::from("run"));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "new\nnewer\nold\n");
    assert_eq!(app.status, "Running test parse::run");
}

#[test]
fn cmd_k_enter_reaches_run_test_at_cursor_not_keep_open() {
    // Cmd+K Enter is DOCUMENTED as run-test-at-cursor, but an undocumented
    // earlier Keep Open arm shadowed it from the day it shipped. The chord
    // must reach the Testing handler; Keep Open lives on the tab menu only.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let f = tmp.path().join("a.rs");
    std::fs::write(&f, "x").unwrap();
    app.editor.open_preview(&f).unwrap();
    let idx = app.editor.active_index();
    assert!(app.editor.is_preview(idx), "opened as a preview tab");
    app.handle_key(key(KeyCode::Char('k'), KeyModifiers::SUPER))
        .unwrap();
    assert!(app.cmd_k_leader.is_some(), "Cmd+K arms the chord leader");
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(
        app.status, "Run Test at Cursor: no function at the caret",
        "the chord must land in the Testing handler"
    );
    assert!(
        app.editor.is_preview(app.editor.active_index()),
        "Keep Open no longer owns the chord, so the preview stays a preview"
    );
}

#[test]
fn a_test_binary_that_finishes_after_a_workspace_change_is_discarded() {
    // Re-rooting the Explorer while `cargo test --no-run` runs used to make
    // the drain launch the OLD workspace's binary with the NEW workspace's
    // cwd and breakpoint context. A finished build whose originating root no
    // longer matches the current workspace must be dropped, not launched.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    app.pending_test_debug = Some((
        rx,
        String::from("my_case"),
        std::path::PathBuf::from("/somewhere/else"),
    ));
    tx.send(Ok(std::path::PathBuf::from(
        "/somewhere/else/target/debug/deps/x-abc123",
    )))
    .unwrap();
    assert!(
        app.drain_pending_test_debug(),
        "the drain consumed the result"
    );
    assert!(
        app.debug_sessions.is_empty(),
        "no debugger may launch for a foreign workspace's binary"
    );
    assert!(
        app.status.contains("workspace changed"),
        "status explains the discarded build, got: {}",
        app.status
    );
    assert!(
        app.pending_test_debug.is_none(),
        "the pending slot frees for the next request"
    );
}

/// With no enabled runner claiming the workspace (`runner_for` is `None` —
/// no recognised project, or its runner extension disabled), every Testing
/// run gesture must refuse with a status instead of shelling `cargo test`
/// in a directory cargo knows nothing about. Only discovery was guarded;
/// the four run entry points started a spurious run.
#[test]
fn a_workspace_with_no_test_runner_refuses_every_run_gesture() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("notes.txt"), "no project here\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();

    fn assert_refused(app: &mut App, label: &str) {
        assert!(!app.testing.is_busy(), "{label} must not start");
        assert!(
            app.status.contains("No test runner"),
            "{label} must say why it refused: {:?}",
            app.status
        );
        app.status.clear();
    }
    app.run_all_tests();
    assert_refused(&mut app, "run-all");
    app.run_test(String::from("some::test"));
    assert_refused(&mut app, "single run");
    app.run_suite(String::from("suite"));
    assert_refused(&mut app, "suite run");
    app.run_named_test(String::from("named"));
    assert_refused(&mut app, "named run");
}

/// #1841: switching to the Testing view never waits on finding a runner.
/// The worker finds it, so a workspace with none opens the view silently;
/// and reopening the view before the worker answers queues nothing more.
#[test]
fn opening_testing_leaves_finding_the_runner_to_the_worker() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("notes.txt"), "no project here\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let sent = app.test_worker.requests_sent();
    app.open_testing_view();
    assert_eq!(app.sidebar_view, SidebarView::Testing);
    assert!(
        app.test_worker.discover_queued(),
        "the discovery goes to the worker without a runner check first"
    );
    app.open_testing_view();
    app.set_sidebar_view(SidebarView::Explorer);
    app.open_testing_view();
    assert_eq!(
        app.test_worker.requests_sent(),
        sent + 1,
        "reopening before the worker answers must not queue another discovery"
    );
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(500),
        "the worker to find no runner",
        || {
            app.test_worker.drain(&mut app.testing);
            !app.test_worker.discover_queued()
        },
    );
    assert!(!app.testing.take_refusal(), "opening the view says nothing");
    assert!(!app.testing.is_busy());
    assert!(!app.status.contains("No test runner"), "{:?}", app.status);
}

/// #1520: opened at a monorepo root whose packages each declare their own
/// vitest, the Testing view runs in the focused file's package instead of
/// reporting no runner, and follows the focus to another package.
#[test]
fn the_testing_view_follows_the_focused_package_in_a_monorepo() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    std::fs::write(
        root.join("package.json"),
        r#"{"name":"vmono","private":true,"workspaces":["packages/*"]}"#,
    )
    .unwrap();
    for (name, marker) in [("math", "vitest.config.ts"), ("web", "jest.config.js")] {
        let pkg = root.join("packages").join(name);
        std::fs::create_dir_all(pkg.join("src")).unwrap();
        std::fs::write(pkg.join(marker), "export default {};\n").unwrap();
        std::fs::write(pkg.join("src/a.test.ts"), "test('a', () => {});\n").unwrap();
    }
    let mut app = App::new(root.clone()).unwrap();
    app.drain_git_responses();
    assert_eq!(app.active_test_root, root.join("packages/math"));
    assert!(app.testing_runner_available(), "{}", app.status);

    app.editor
        .open_pinned(&root.join("packages/web/src/a.test.ts"))
        .unwrap();
    app.drain_git_responses();
    assert_eq!(app.active_test_root, root.join("packages/web"));
    assert_eq!(app.test_worker.root(), root.join("packages/web"));
}

/// #1520 review: the followed package is cached per focused file, so a
/// runner marker added while the same file stays open must still be picked
/// up by the next Testing refresh or run gesture, not only by a focus change.
#[test]
fn a_runner_added_under_the_open_file_is_followed_on_refresh() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    std::fs::write(
        root.join("package.json"),
        r#"{"name":"vmono","private":true,"workspaces":["packages/*"]}"#,
    )
    .unwrap();
    let pkg = root.join("packages/math");
    std::fs::create_dir_all(pkg.join("src")).unwrap();
    std::fs::write(pkg.join("src/a.test.ts"), "test('a', () => {});\n").unwrap();
    let mut app = App::new(root.clone()).unwrap();
    app.editor.open_pinned(&pkg.join("src/a.test.ts")).unwrap();
    app.drain_git_responses();
    assert_eq!(app.active_test_root, root);
    assert!(!app.testing_runner_available());

    std::fs::write(pkg.join("vitest.config.ts"), "export default {};\n").unwrap();
    app.drain_git_responses();
    assert!(app.testing_runner_available(), "{}", app.status);
    assert_eq!(app.active_test_root, pkg);
    assert_eq!(app.test_worker.root(), pkg);
}

/// #1520, negative: a folder with no runner anywhere keeps its own root and
/// still says no runner was detected.
#[test]
fn a_folder_with_no_runner_keeps_its_root_and_says_so() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("packages/docs")).unwrap();
    std::fs::write(
        root.join("packages/docs/package.json"),
        r#"{"name":"docs"}"#,
    )
    .unwrap();
    let mut app = App::new(root.clone()).unwrap();
    app.drain_git_responses();
    assert_eq!(app.active_test_root, root);
    assert!(!app.testing_runner_available());
    assert_eq!(app.status, crate::testing::NO_RUNNER_STATUS);
}

/// cargo's positional filter is a substring match, so running suite `parse`
/// with a bare pattern also sweeps `parse_utils::b` and every other
/// prefix-sharing sibling. The suite gesture must anchor on `parse::`.
#[test]
fn running_a_suite_never_sweeps_prefix_sharing_siblings() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"t\"\nversion = \"0.0.0\"\n",
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    for n in ["parse::a", "parse_utils::b"] {
        app.testing.apply_case(crate::testing::model::TestCase {
            name: n.into(),
            status: crate::testing::model::TestStatus::NotRun,
        });
    }

    app.run_suite(String::from("parse"));

    use crate::testing::model::TestStatus;
    assert_eq!(
        app.testing.status_of("parse::a"),
        Some(TestStatus::Running),
        "the suite's own case runs"
    );
    assert_eq!(
        app.testing.status_of("parse_utils::b"),
        Some(TestStatus::NotRun),
        "a prefix-sharing sibling suite must stay untouched"
    );
}

/// A cargo workspace whose Testing tree holds `cases`, each with its status.
fn testing_app(cases: &[(&str, crate::testing::model::TestStatus)]) -> (tempfile::TempDir, App) {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"t\"\nversion = \"0.0.0\"\n",
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    for (name, status) in cases {
        app.testing.apply_case(crate::testing::model::TestCase {
            name: (*name).into(),
            status: *status,
        });
    }
    (tmp, app)
}

/// #1535: Testing: Rerun Failed Tests runs exactly the failed cases, by
/// their full names, and leaves the passing ones alone.
#[test]
fn rerun_failed_tests_runs_only_the_failed_cases() {
    use crate::testing::model::TestStatus;
    let (_tmp, mut app) = testing_app(&[
        ("parse::a", TestStatus::Failed),
        ("parse::b", TestStatus::Passed),
        ("lex::c", TestStatus::Failed),
    ]);

    app.run_command(crate::widgets::command_palette::Command::TestingRerunFailed);

    assert_eq!(app.testing.status_of("parse::a"), Some(TestStatus::Running));
    assert_eq!(app.testing.status_of("lex::c"), Some(TestStatus::Running));
    assert_eq!(
        app.testing.status_of("parse::b"),
        Some(TestStatus::Passed),
        "a passing test is not rerun"
    );
    assert_eq!(app.status, "Running 2 tests");
    assert_eq!(
        app.last_test_run,
        Some(LastTestRun::Many(vec!["lex::c".into(), "parse::a".into()])),
        "both failed tests, in the tree's order"
    );
}

/// #1535 negative: with nothing failed, Rerun Failed Tests says so and
/// starts no run at all (not a run of everything).
#[test]
fn rerun_failed_tests_with_no_failure_says_so_and_runs_nothing() {
    use crate::testing::model::TestStatus;
    let (_tmp, mut app) = testing_app(&[("parse::a", TestStatus::Passed)]);

    app.run_command(crate::widgets::command_palette::Command::TestingRerunFailed);

    assert_eq!(app.status, "Rerun Failed Tests: no test has failed");
    assert!(!app.testing.is_busy());
    assert_eq!(app.testing.status_of("parse::a"), Some(TestStatus::Passed));
    assert_eq!(app.last_test_run, None);
}

/// #1535 negative: before any run, Rerun Last Run says so.
#[test]
fn rerun_last_run_before_any_run_says_so() {
    use crate::testing::model::TestStatus;
    let (_tmp, mut app) = testing_app(&[("parse::a", TestStatus::NotRun)]);

    app.run_command(crate::widgets::command_palette::Command::TestingRerunLast);

    assert_eq!(app.status, "Rerun Last Run: no test has run yet");
    assert!(!app.testing.is_busy());
}

/// #1535: Rerun Last Run repeats the last run as it was asked: a suite
/// reruns that suite, not every test.
#[test]
fn rerun_last_run_repeats_the_last_suite() {
    use crate::testing::model::TestStatus;
    let (_tmp, mut app) = testing_app(&[
        ("parse::a", TestStatus::NotRun),
        ("lex::b", TestStatus::NotRun),
    ]);
    app.run_suite(String::from("parse"));
    app.testing.apply_case(crate::testing::model::TestCase {
        name: "parse::a".into(),
        status: TestStatus::Passed,
    });
    app.testing.on_finished(Some(true));
    assert_eq!(app.testing.status_of("parse::a"), Some(TestStatus::Passed));

    app.run_command(crate::widgets::command_palette::Command::TestingRerunLast);

    assert_eq!(app.testing.status_of("parse::a"), Some(TestStatus::Running));
    assert_eq!(
        app.testing.status_of("lex::b"),
        Some(TestStatus::NotRun),
        "a test outside the last run stays untouched"
    );
}

/// #1535 negative: while a run is going, each new command says so instead
/// of starting a second run over it.
#[test]
fn testing_commands_while_a_run_is_going_say_so() {
    use crate::testing::model::TestStatus;
    use crate::widgets::command_palette::Command;
    let (_tmp, mut app) = testing_app(&[
        ("parse::a", TestStatus::Failed),
        ("lex::b", TestStatus::NotRun),
    ]);
    app.run_suite(String::from("parse"));
    assert!(app.testing.is_busy());

    for cmd in [
        Command::TestingRunAll,
        Command::TestingRunFile,
        Command::TestingRerunFailed,
        Command::TestingRerunLast,
    ] {
        app.status.clear();
        app.run_command(cmd);
        assert_eq!(app.status, "Tests are already running", "{cmd:?}");
    }
    assert_eq!(
        app.testing.status_of("lex::b"),
        Some(TestStatus::NotRun),
        "no second run started"
    );
    assert_eq!(app.last_test_run, Some(LastTestRun::Suite("parse".into())));
}

/// #1535: Run Tests in Current File runs the file's own module.
#[test]
fn run_tests_in_current_file_runs_that_module() {
    use crate::testing::model::TestStatus;
    let (tmp, mut app) = testing_app(&[
        ("parse::a", TestStatus::NotRun),
        ("parse_utils::b", TestStatus::NotRun),
    ]);
    let file = tmp.path().join("src/parse.rs");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "#[test]\nfn a() {}\n").unwrap();
    app.open_file_at_launch(&file);

    app.run_command(crate::widgets::command_palette::Command::TestingRunFile);

    assert_eq!(app.status, "Running tests in parse");
    assert_eq!(app.testing.status_of("parse::a"), Some(TestStatus::Running));
    assert_eq!(
        app.testing.status_of("parse_utils::b"),
        Some(TestStatus::NotRun),
        "another file's tests do not run"
    );
}

/// #1535 negative: a crate root has no module to filter on, so the command
/// says so rather than running a filter that sweeps other files' tests.
#[test]
fn run_tests_in_current_file_refuses_a_crate_root() {
    use crate::testing::model::TestStatus;
    let (tmp, mut app) = testing_app(&[("parse::tests::a", TestStatus::NotRun)]);
    let file = tmp.path().join("src/main.rs");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "fn main() {}\n").unwrap();
    app.open_file_at_launch(&file);

    app.run_command(crate::widgets::command_palette::Command::TestingRunFile);

    assert_eq!(
        app.status,
        "Run Tests in Current File: the runner cannot pick out this file's tests; use Run All Tests"
    );
    assert!(!app.testing.is_busy());
}

/// #1535: each runner's name for a file's own tests, and `None` where the
/// runner cannot select a file alone.
#[test]
fn file_test_suite_names_a_file_s_tests_per_runner() {
    use crate::testing::worker::Runner;
    let root = Path::new("/w");
    let at = |r, p: &str| super::file_test_suite(r, root, &root.join(p));
    assert_eq!(at(Runner::Cargo, "src/parse.rs").as_deref(), Some("parse"));
    assert_eq!(at(Runner::Cargo, "src/a/b.rs").as_deref(), Some("a::b"));
    assert_eq!(at(Runner::Cargo, "src/a/b/mod.rs").as_deref(), Some("a::b"));
    assert_eq!(at(Runner::Cargo, "src/main.rs"), None);
    assert_eq!(at(Runner::Cargo, "src/lib.rs"), None);
    assert_eq!(at(Runner::Cargo, "src/bin/tool.rs"), None);
    assert_eq!(at(Runner::Cargo, "tests/it.rs"), None);
    assert_eq!(at(Runner::Cargo, "README.md"), None);
    assert_eq!(
        at(Runner::Pytest, "tests/test_x.py").as_deref(),
        Some("tests/test_x.py")
    );
    assert_eq!(
        at(Runner::Go, "pkg/a/a_test.go").as_deref(),
        Some("./pkg/a")
    );
    assert_eq!(at(Runner::Go, "a_test.go").as_deref(), Some("."));
    assert_eq!(at(Runner::Jest, "src/a.test.ts"), None);
    assert_eq!(at(Runner::Vitest, "src/a.test.ts"), None);
    assert_eq!(
        super::file_test_suite(Runner::Pytest, root, Path::new("/elsewhere/t.py")),
        None,
        "a file outside the workspace"
    );
}

/// #1535: in a Cargo workspace a member's module is named from its own
/// package, not the workspace root.
#[test]
fn file_test_suite_names_a_workspace_member_s_module() {
    use crate::testing::worker::Runner;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let member = root.join("crates/foo");
    std::fs::create_dir_all(member.join("src/a")).unwrap();
    std::fs::write(member.join("Cargo.toml"), "[package]\nname = \"foo\"\n").unwrap();
    let at = |p: &str| super::file_test_suite(Runner::Cargo, root, &root.join(p));
    assert_eq!(at("crates/foo/src/parser.rs").as_deref(), Some("parser"));
    assert_eq!(at("crates/foo/src/a/b.rs").as_deref(), Some("a::b"));
    assert_eq!(at("crates/foo/src/lib.rs"), None, "a member's crate root");
    assert_eq!(
        at("crates/bar/src/x.rs"),
        None,
        "no package holds it, so it is not under the root's src/"
    );
}

/// #1535 negative: switching the workspace folder forgets the last run,
/// whose names belong to the old project.
#[test]
fn rebinding_the_test_root_forgets_the_last_run() {
    use crate::testing::model::TestStatus;
    let (_tmp, mut app) = testing_app(&[("parse::a", TestStatus::NotRun)]);
    app.run_suite(String::from("parse"));
    assert_eq!(app.last_test_run, Some(LastTestRun::Suite("parse".into())));
    let other = tempfile::tempdir().unwrap();

    app.rebind_test_root(other.path().to_path_buf());

    assert_eq!(app.last_test_run, None);
    app.run_command(crate::widgets::command_palette::Command::TestingRerunLast);
    assert_eq!(app.status, "Rerun Last Run: no test has run yet");
}

/// #1535: the four everyday Testing commands are in the palette.
#[test]
fn the_everyday_testing_commands_are_in_the_palette() {
    let titles: Vec<&str> = crate::widgets::command_palette::ALL_COMMANDS
        .iter()
        .map(|c| c.title())
        .collect();
    for t in [
        "Testing: Run All Tests",
        "Testing: Run Tests in Current File",
        "Testing: Rerun Failed Tests",
        "Testing: Rerun Last Run",
    ] {
        assert!(titles.contains(&t), "{t} missing");
    }
}

/// Run-at-cursor knows only the fn's leaf name. When the discovered tree
/// holds exactly one case with that leaf, the run must target it exactly
/// instead of handing cargo a bare substring that also matches `run_fast`.
#[test]
fn run_at_cursor_with_a_unique_leaf_runs_exactly_that_test() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"t\"\nversion = \"0.0.0\"\n",
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    for n in ["parse::run", "other::run_fast"] {
        app.testing.apply_case(crate::testing::model::TestCase {
            name: n.into(),
            status: crate::testing::model::TestStatus::NotRun,
        });
    }

    app.run_named_test(String::from("run"));

    use crate::testing::model::TestStatus;
    assert_eq!(
        app.status, "Running test parse::run",
        "a unique leaf resolves to its full name for an exact run"
    );
    assert_eq!(
        app.testing.status_of("other::run_fast"),
        Some(TestStatus::NotRun),
        "a test merely containing the leaf must stay untouched"
    );
}

/// Clicking a test's name greps the workspace for its `fn`; that walk runs
/// on a background thread and the editor jump lands via the drain, so a big
/// workspace can't freeze the render loop. Covers hit and miss.
#[test]
fn a_test_name_click_jumps_to_source_via_the_background_locator() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"t\"\nversion = \"0.0.0\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(tmp.path().join("src")).unwrap();
    let lib = tmp.path().join("src/lib.rs");
    std::fs::write(
        &lib,
        "#[cfg(test)]\nmod tests {\n    #[test]\n    fn target_case() { assert!(true); }\n}\n",
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();

    app.jump_to_test_source(String::from("tests::target_case"));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        app.sync_explorer_panels();
        if app.editor.path.as_deref() == Some(lib.as_path()) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the locator result never arrived; editor at {:?}",
            app.editor.path
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(app.editor.cursor_row, 3, "caret lands on `fn target_case`");

    app.jump_to_test_source(String::from("nope::missing"));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        app.sync_explorer_panels();
        if app
            .status
            .contains("No source found for test nope::missing")
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the miss status never arrived, status {:?}",
            app.status
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// #373: the site must be found in AUTHENTIC runner output, summary and
/// all. libtest reprints every failing test's name in a trailing
/// `failures:` block, so a scan anchored on "the last line mentioning the
/// name" lands after every stdout block and finds nothing — the feature
/// silently does nothing for the ecosystem it was written for. This is
/// the regression test for that.
#[test]
fn the_failure_site_survives_a_whole_cargo_test_run() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("src")).unwrap();
    std::fs::write(tmp.path().join("src/lib.rs"), "a\nb\nc\n").unwrap();
    std::fs::write(tmp.path().join("src/other.rs"), "x\ny\nz\n").unwrap();
    let app = App::new(tmp.path().to_path_buf()).unwrap();

    crate::output::clear(crate::output::CHANNEL_TESTS);
    for line in [
        "   Compiling demo v0.1.0",
        "    Finished test profile [unoptimized + debuginfo]",
        "     Running unittests src/lib.rs",
        "",
        "running 2 tests",
        "test tests::adds_two ... FAILED",
        "test tests::adds ... FAILED",
        "",
        "failures:",
        "",
        "---- tests::adds_two stdout ----",
        "thread 'tests::adds_two' panicked at src/other.rs:2:5:",
        "assertion `left == right` failed",
        "",
        "---- tests::adds stdout ----",
        "thread 'tests::adds' panicked at src/lib.rs:3:5:",
        "assertion failed: false",
        "note: run with `RUST_BACKTRACE=1` to display a backtrace",
        "",
        "failures:",
        "    tests::adds",
        "    tests::adds_two",
        "",
        "test result: FAILED. 0 passed; 2 failed; 0 ignored; 0 measured",
    ] {
        crate::output::push(
            crate::output::CHANNEL_TESTS,
            crate::output::OutputLevel::Info,
            line,
        );
    }

    // The trailing summary must not swallow the block, and each test must
    // get ITS OWN assertion — not the other's.
    let adds = app
        .failure_site_of("tests::adds")
        .expect("the site survives the summary that follows every block");
    assert_eq!(
        (adds.file.to_string_lossy().into_owned(), adds.line),
        (String::from("src/lib.rs"), 3)
    );
    let two = app
        .failure_site_of("tests::adds_two")
        .expect("and so does the other test's");
    assert_eq!(
        (two.file.to_string_lossy().into_owned(), two.line),
        (String::from("src/other.rs"), 2),
        "a name that is a PREFIX of another must not borrow its block"
    );

    // And a test that did not fail has no block at all.
    assert!(app.failure_site_of("tests::passes").is_none());
}

/// A test-built App never reads the developer's own settings layers: a
/// preference in the real `config.json` (say, the TIMELINE hidden) must not
/// steer app tests. Workspace layers under the test root still load.
#[test]
fn a_test_built_app_reads_no_user_settings_layer() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let user_dir = crate::prefs::config_dir();
    // At startup and again on a live settings reload.
    for when in ["startup", "reload"] {
        if when == "reload" {
            app.remerge_settings();
        }
        let leaked: Vec<_> = app
            .settings_chain
            .iter()
            .filter(|p| p.starts_with(&user_dir))
            .collect();
        assert!(
            leaked.is_empty(),
            "{when}: user layers read under test: {leaked:?}"
        );
        assert!(
            app.settings_chain
                .contains(&crate::config_layers::workspace_config_path(tmp.path())),
            "{when}: the workspace layer must still load: {:?}",
            app.settings_chain
        );
        assert!(
            app.settings_provenance.values().all(|k| !matches!(
                k,
                crate::config_layers::LayerKind::User | crate::config_layers::LayerKind::UserLocal
            )),
            "{when}: a setting came from a user layer: {:?}",
            app.settings_provenance
        );
    }
}

/// Coverage (#263) through the App, from a report arriving the way the
/// worker's drain delivers it: the active file's lines are marked in the
/// gutter lane, the status readout carries its percentage, an edit dims
/// the marks, a missing tool offers its install, and Clear hands the lane
/// back.
#[test]
fn a_coverage_report_marks_the_gutter_and_dims_after_an_edit() {
    use crate::testing::coverage::{Coverage, LineCov};
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let file = root.join("f.rs");
    std::fs::write(&file, "fn a() {}\nfn b() {}\n").unwrap();
    let mut app = App::new(root.clone()).unwrap();
    app.editor.open(&file).unwrap();
    app.testing.on_coverage(Ok(Coverage::from_lcov(
        "SF:f.rs\nDA:1,3\nDA:2,0\nend_of_record\n",
        &root,
    )));
    assert!(app.sync_coverage());
    let lens = app
        .editor
        .coverage
        .clone()
        .expect("the open file is covered");
    assert_eq!(lens.lines.get(&0), Some(&LineCov::Covered));
    assert_eq!(lens.lines.get(&1), Some(&LineCov::Uncovered));
    assert!(!lens.stale);

    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let bar_x = app.editor.last_inner.x + app.editor.last_gutter_width;
    let y = app.editor.last_inner.y;
    let buf = term.backend().buffer();
    assert_eq!(buf[(bar_x, y)].symbol(), "\u{258c}");
    assert_ne!(
        buf[(bar_x, y)].fg,
        buf[(bar_x, y + 1)].fg,
        "covered and uncovered differ"
    );
    let status_row: String = (0..120).map(|x| buf[(x, 29)].symbol()).collect();
    assert!(status_row.contains("50% covered"), "{status_row}");

    app.editor.insert_char('x');
    app.sync_coverage();
    assert!(
        app.editor.coverage.as_ref().unwrap().stale,
        "an edit dims the marks"
    );

    app.testing
        .on_coverage(Err(crate::testing::worker::CoverageError::Missing {
            tool: "cargo-llvm-cov",
            install: String::from("cargo install cargo-llvm-cov"),
        }));
    app.sync_coverage();
    assert!(app.status.contains("cargo-llvm-cov"), "{}", app.status);
    assert_eq!(
        app.coverage_install.as_deref(),
        Some("cargo install cargo-llvm-cov")
    );

    app.run_command(crate::widgets::command_palette::Command::CoverageClear);
    assert!(app.editor.coverage.is_none() && app.testing.coverage.is_none());
}

/// Acceptance for #263 against real pytest-cov: "Run All Tests with
/// Coverage" on a pytest project marks the tested file's lines, uncovered
/// branch-free lines red, and reports a percentage. Set
/// `CROFT_TEST_PYTEST_COV_PYTHON` to a venv python with pytest and
/// pytest-cov; the test links that venv in as the project's `.venv`.
#[test]
#[ignore = "needs pytest and pytest-cov; set CROFT_TEST_PYTEST_COV_PYTHON"]
fn a_real_pytest_cov_run_marks_the_covered_file() {
    use crate::testing::coverage::LineCov;
    let python = std::path::PathBuf::from(std::env::var("CROFT_TEST_PYTEST_COV_PYTHON").unwrap());
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    std::os::unix::fs::symlink(
        python.parent().and_then(Path::parent).unwrap(),
        root.join(".venv"),
    )
    .unwrap();
    std::fs::write(
        root.join("pyproject.toml"),
        "[project]\nname = \"t\"\nversion = \"0\"\n",
    )
    .unwrap();
    std::fs::write(
        root.join("calc.py"),
        "def add(a, b):\n    return a + b\n\n\ndef unused():\n    return 0\n",
    )
    .unwrap();
    std::fs::write(
        root.join("test_calc.py"),
        "from calc import add\n\n\ndef test_add():\n    assert add(1, 2) == 3\n",
    )
    .unwrap();
    let mut app = App::new(root.clone()).unwrap();
    app.editor.open(&root.join("calc.py")).unwrap();
    app.run_all_tests_with_coverage();
    let end = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while app.editor.coverage.is_none() {
        assert!(std::time::Instant::now() < end, "status: {}", app.status);
        let _ = app.test_worker.drain(&mut app.testing);
        app.sync_coverage();
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let lens = app.editor.coverage.clone().unwrap();
    assert_eq!(
        lens.lines.get(&1),
        Some(&LineCov::Covered),
        "`return a + b` ran"
    );
    assert_eq!(
        lens.lines.get(&5),
        Some(&LineCov::Uncovered),
        "`return 0` never ran"
    );
    assert!(
        lens.percent.is_some_and(|p| p > 0.0 && p < 100.0),
        "{:?}",
        lens.percent
    );
}

/// #263: the Testing tree's coverage glyphs run their row with coverage,
/// marking just that scope as running, as a plain run of it would.
#[test]
fn a_rows_coverage_glyph_runs_that_scope_with_coverage() {
    use crate::testing::model::{TestCase, TestStatus};
    use crate::widgets::testing::RowHit;
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"t\"\nversion = \"0.0.0\"\n",
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    for n in ["parse::a", "parse::b", "other::c"] {
        app.testing.apply_case(TestCase {
            name: n.into(),
            status: TestStatus::Passed,
        });
    }
    app.set_sidebar_view(SidebarView::Testing);
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let area = app.testing.last_area;
    let x = area.x + area.width - 5;
    let click = |app: &mut App, want: RowHit| {
        let y = (area.y..area.y + area.height)
            .find(|&y| app.testing.hit_at(x, y) == Some(want.clone()))
            .unwrap_or_else(|| panic!("no {want:?} glyph"));
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        });
    };

    click(&mut app, RowHit::CoverSuite("parse".into()));
    assert_eq!(app.status, "Running suite parse with coverage");
    assert_eq!(app.testing.status_of("parse::a"), Some(TestStatus::Running));
    assert_eq!(app.testing.status_of("parse::b"), Some(TestStatus::Running));
    assert_eq!(app.testing.status_of("other::c"), Some(TestStatus::Passed));
    app.testing.on_finished(Some(true));

    click(&mut app, RowHit::CoverCase("other::c".into()));
    assert_eq!(app.status, "Running test other::c with coverage");
    assert_eq!(app.testing.status_of("other::c"), Some(TestStatus::Running));
    app.testing.on_finished(Some(true));

    // The header's glyph runs everything with coverage.
    let all = app.testing.last_cover_all;
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: all.x,
        row: all.y,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.status, "Running tests with coverage");
}

/// Watch mode (#263) end to end through the App: the eye on a test's row
/// watches it, a save anywhere under the runner's root reruns exactly that
/// test once the debounce passes, without moving the sidebar, and a rerun
/// that turns red says which test failed.
#[test]
fn watching_a_test_reruns_it_after_a_save_and_reports_it_turning_red() {
    use crate::testing::model::{TestCase, TestStatus};
    use crate::testing::watch::WatchScope;
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"t\"\nversion = \"0.0.0\"\n",
    )
    .unwrap();
    let src = tmp.path().join("lib.rs");
    std::fs::write(&src, "fn a() {}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    for n in ["parse::a", "parse::b"] {
        app.testing.apply_case(TestCase {
            name: n.into(),
            status: TestStatus::Passed,
        });
    }
    app.set_sidebar_view(SidebarView::Testing);
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
    term.draw(|f| app.render(f)).unwrap();

    // Find parse::a's eye where it is drawn, and click it.
    let area = app.testing.last_area;
    let x = area.x + area.width - 3;
    let y = (area.y..area.y + area.height)
        .find(|&y| {
            app.testing.hit_at(x, y)
                == Some(crate::widgets::testing::RowHit::ToggleWatch(
                    WatchScope::Test("parse::a".into()),
                ))
        })
        .expect("parse::a has an eye");
    assert_eq!(term.backend().buffer()[(x, y)].symbol(), "\u{ea70}");
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    });
    assert!(
        app.testing
            .watch
            .is_watching(&WatchScope::Test("parse::a".into()))
    );

    // The user goes back to the files and saves one.
    app.set_sidebar_view(SidebarView::Explorer);
    app.editor.open(&src).unwrap();
    app.editor.insert_char('x');
    app.save();
    assert!(!app.tick_test_watch(), "not before the debounce");
    std::thread::sleep(crate::testing::watch::DEBOUNCE + std::time::Duration::from_millis(50));
    assert!(app.tick_test_watch());
    assert_eq!(app.testing.status_of("parse::a"), Some(TestStatus::Running));
    assert_eq!(app.testing.status_of("parse::b"), Some(TestStatus::Passed));
    assert_eq!(
        app.sidebar_view,
        SidebarView::Explorer,
        "a rerun never moves the sidebar"
    );

    // The rerun ends red: the status says which test, once.
    app.testing.apply_case(TestCase {
        name: "parse::a".into(),
        status: TestStatus::Failed,
    });
    app.testing.on_finished(Some(false));
    app.tick_test_watch();
    assert!(
        app.status.contains("Watched tests failed: parse::a"),
        "{}",
        app.status
    );
}

/// #845: a discovery that failed without listing anything showed an empty
/// tree and no reason; the reason was in OUTPUT, unannounced. The Testing
/// view now points there, and both a click on the pointer and `o` open the
/// bottom panel on OUTPUT with the Test Runner channel selected, whichever
/// channel it was showing.
#[test]
fn a_failed_discovery_opens_the_test_runner_output_on_click_or_o() {
    use crate::output::{CHANNEL_TESTS, OutputLevel};
    crate::output::push(
        CHANNEL_TESTS,
        OutputLevel::Info,
        "E   ModuleNotFoundError: No module named 'groceries'",
    );
    let other = "app-test-runner-output-other";
    crate::output::push(other, OutputLevel::Info, "unrelated");
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Testing);
    app.testing
        .on_busy_started(crate::testing::model::Activity::Discovering);
    app.testing.on_finished(Some(false));
    draw(&mut app, 100, 30);
    let hint = app.testing.last_output_hint;
    assert!(hint.width > 0, "the failed discovery points at its output");
    assert_eq!(app.bottom_panel_tab, BottomPanelTab::Terminal);
    left_click(&mut app, hint.x, hint.y);
    assert_eq!(app.bottom_panel_tab, BottomPanelTab::Output);
    assert_eq!(app.output.selected_name().as_deref(), Some(CHANNEL_TESTS));

    app.output.sync();
    assert!(app.output.select_by_name(other));
    app.bottom_panel_tab = BottomPanelTab::Problems;
    app.focus_pane(Pane::Tree);
    app.handle_key(key(KeyCode::Char('o'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.bottom_panel_tab, BottomPanelTab::Output);
    assert_eq!(app.output.selected_name().as_deref(), Some(CHANNEL_TESTS));
}

/// #856: "Running test …" stayed in the status bar long after the run it
/// announced had finished, because only the Testing tree heard the end.
/// End to end against a stand-in test runner (a `codeql` script), the
/// outcome now replaces it: a pass, a failure, and a run that errored
/// before reporting any test. A message something else put there while
/// the run was going is left alone.
#[cfg(unix)]
#[test]
fn a_finished_test_run_replaces_its_running_status_with_the_outcome() {
    use crate::testing::model::{TestCase, TestStatus};
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("test/Find")).unwrap();
    std::fs::write(root.join("test/qlpack.yml"), "name: acme/tests\ntests: .\n").unwrap();
    std::fs::write(root.join("test/Find/Find.qlref"), "Find.ql\n").unwrap();
    std::fs::write(root.join("test/Find/Find.expected"), "").unwrap();
    // The stand-in prints `out` and exits with `code`. Each run rewrites
    // those two, never the program, which may still be starting up.
    let bin = tempfile::tempdir().unwrap();
    let (out, code) = (bin.path().join("out"), bin.path().join("code"));
    let program = bin.path().join("codeql");
    std::fs::write(
        &program,
        format!(
            "#!/bin/sh\ncat '{}'\nexit \"$(cat '{}')\"\n",
            out.display(),
            code.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut app = App::new(root.clone()).unwrap();
    app.set_codeql_program(program);
    app.testing.apply_case(TestCase {
        name: "test/Find::Find.qlref".into(),
        status: TestStatus::NotRun,
    });
    let root_s = root.display().to_string();
    let start = |app: &mut App, stdout: &str, exit: i32| {
        std::fs::write(&out, stdout.replace("@ROOT@", &root_s)).unwrap();
        std::fs::write(&code, exit.to_string()).unwrap();
        app.run_named_test(String::from("Find.qlref"));
        assert_eq!(app.status, "Running test test/Find::Find.qlref");
    };
    let finish = |app: &mut App| {
        crate::test_budget::await_spawned(
            std::time::Duration::from_millis(500),
            "the stand-in test run to finish",
            || {
                let _ = app.test_worker.drain(&mut app.testing);
                app.tick_test_watch();
                !app.testing.is_busy()
            },
        );
    };
    let pass = "[1/1 comp 1s eval 9ms] PASSED @ROOT@/test/Find/Find.qlref\n";

    start(&mut app, pass, 0);
    finish(&mut app);
    assert!(
        app.status.starts_with("Find.qlref passed ("),
        "{}",
        app.status
    );

    start(
        &mut app,
        "[1/1 comp 1s eval 9ms] FAILED(RESULT) @ROOT@/test/Find/Find.qlref\n\
         --- expected\n+++ actual\n+| 1 |\n",
        1,
    );
    finish(&mut app);
    assert!(
        app.status.starts_with("Find.qlref failed ("),
        "{}",
        app.status
    );

    start(&mut app, "A fatal error occurred: no pack here\n", 2);
    finish(&mut app);
    assert_eq!(app.status, "Run failed — see OUTPUT › Test Runner");

    start(&mut app, pass, 0);
    app.status = String::from("Saved Find.ql");
    finish(&mut app);
    assert_eq!(
        app.status, "Saved Find.ql",
        "a newer message is not clobbered"
    );
}

/// #1639: a test run started with saving turned off shows the unsaved
/// file's warning in front of its "Running …" status, and its outcome
/// still replaces that status when it ends.
#[cfg(unix)]
#[test]
fn a_test_run_started_with_the_unsaved_warning_still_reports_its_outcome() {
    let mut s = CodeqlStandIn::new();
    s.will(STAND_IN_PASS, 0);
    let file = PathBuf::from(&s.root).join("test/Find/Find.ql");
    std::fs::write(&file, "old\n").unwrap();
    s.app.editor.open_pinned(&file).unwrap();
    s.app.editor.insert_str("new\n");
    s.app.save_before_debug = false;
    s.app.run_named_test(String::from("Find.qlref"));
    assert_eq!(
        s.app.status,
        "Find.ql has unsaved changes: the test run uses the file on disk - Running test test/Find::Find.qlref"
    );
    s.finish();
    assert!(
        s.app.status.starts_with("Find.qlref passed ("),
        "{}",
        s.app.status
    );
}

/// #1639: with a watched scope, the save a test run makes before it
/// starts schedules no second, watched run after it.
#[cfg(unix)]
#[test]
fn the_save_before_a_test_run_schedules_no_watched_rerun() {
    let mut s = CodeqlStandIn::new();
    s.will(STAND_IN_PASS, 0);
    s.app
        .testing
        .watch
        .toggle(crate::testing::watch::WatchScope::All);
    let file = PathBuf::from(&s.root).join("test/Find/Find.ql");
    assert!(file.starts_with(&s.app.active_test_root));
    std::fs::write(&file, "old\n").unwrap();
    s.app.editor.open_pinned(&file).unwrap();
    s.app.editor.insert_str("new\n");
    s.app.run_named_test(String::from("Find.qlref"));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "new\nold\n");
    assert_eq!(s.app.testing.watch.due_at(), None);
    s.finish();
    assert!(!s.app.testing.is_busy());
    assert_eq!(s.app.testing.watch.due_at(), None);
}

/// #856 guard: a coverage run that never ran (CodeQL tests have no source
/// lines to cover) ends with no verdict. Its reason stays in the status
/// bar rather than a made-up outcome, and the test's row goes back to
/// how it was.
#[cfg(unix)]
#[test]
fn a_coverage_run_that_never_ran_keeps_its_reason_in_the_status() {
    let mut s = CodeqlStandIn::new();
    s.app
        .run_scope_with_coverage(String::from("test/Find::Find.qlref"), false);
    assert_eq!(
        s.app.status,
        "Running test test/Find::Find.qlref with coverage"
    );
    s.finish();
    assert_eq!(s.app.status, "Coverage is not available for CodeQL tests");
    assert_eq!(
        s.app.testing.status_of("test/Find::Find.qlref"),
        Some(crate::testing::model::TestStatus::NotRun)
    );
}

/// #845 guard: `o` opens the runner's output only from the Testing view
/// itself. With that view showing a failed discovery but the editor
/// focused, `o` is a character typed into the buffer, and the bottom
/// panel stays where it was.
#[test]
fn o_typed_in_the_editor_beside_a_failed_discovery_stays_a_character() {
    let mut app = editor_app_with_lines(&["ab"]);
    app.set_sidebar_view(SidebarView::Testing);
    app.testing
        .on_busy_started(crate::testing::model::Activity::Discovering);
    app.testing.on_finished(Some(false));
    draw(&mut app, 100, 30);
    assert!(app.testing.last_output_hint.width > 0);
    app.focus_pane(Pane::Editor);
    app.editor.cursor_col = 2;
    app.handle_key(key(KeyCode::Char('o'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.editor.lines, vec!["abo"]);
    assert_eq!(app.bottom_panel_tab, BottomPanelTab::Terminal);
}

/// #263: the coverage report opens as a tab, least covered file first,
/// and without a run it says how to get one.
#[test]
fn show_coverage_report_opens_the_last_run_as_a_tab() {
    use crate::widgets::command_palette::Command;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.run_command(Command::TestingShowCoverageReport);
    assert!(app.status.contains("No coverage yet"), "{}", app.status);
    let root = app.active_test_root.clone();
    app.testing.coverage = Some(crate::testing::coverage::Coverage::from_lcov(
        "SF:a.rs\nDA:1,1\nDA:2,0\nend_of_record\n",
        &root,
    ));
    app.run_command(Command::TestingShowCoverageReport);
    assert_eq!(
        app.editor.path.as_deref(),
        Some(Path::new("Coverage Report"))
    );
    assert_eq!(app.editor.lines[0], "Coverage: 50.0% of 2 lines");
    assert!(
        app.editor.lines[2].ends_with(" a.rs"),
        "{:?}",
        app.editor.lines
    );
}

/// #263 against real pytest-cov: "Run Test at Cursor with Coverage" runs
/// only the caret's test, so code that only another test reaches stays
/// uncovered. Set `CROFT_TEST_PYTEST_COV_PYTHON` as for the whole-run test.
#[test]
#[ignore = "needs pytest and pytest-cov; set CROFT_TEST_PYTEST_COV_PYTHON"]
fn a_real_scoped_coverage_run_covers_only_the_test_at_the_cursor() {
    use crate::testing::coverage::LineCov;
    let python = std::path::PathBuf::from(std::env::var("CROFT_TEST_PYTEST_COV_PYTHON").unwrap());
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    std::os::unix::fs::symlink(
        python.parent().and_then(Path::parent).unwrap(),
        root.join(".venv"),
    )
    .unwrap();
    std::fs::write(
        root.join("pyproject.toml"),
        "[project]\nname = \"t\"\nversion = \"0\"\n",
    )
    .unwrap();
    std::fs::write(
        root.join("calc.py"),
        "def add(a, b):\n    return a + b\n\n\ndef sub(a, b):\n    return a - b\n",
    )
    .unwrap();
    std::fs::write(
        root.join("test_calc.py"),
        "from calc import add, sub\n\n\ndef test_add():\n    assert add(1, 2) == 3\n\n\ndef test_sub():\n    assert sub(3, 2) == 1\n",
    )
    .unwrap();
    let mut app = App::new(root.clone()).unwrap();
    app.editor.open(&root.join("test_calc.py")).unwrap();
    app.editor.cursor_row = 4; // inside test_add
    app.run_command(crate::widgets::command_palette::Command::RunTestAtCursorWithCoverage);
    assert!(app.status.contains("with coverage"), "{}", app.status);
    let end = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while app.testing.coverage.is_none() {
        assert!(std::time::Instant::now() < end, "status: {}", app.status);
        let _ = app.test_worker.drain(&mut app.testing);
        app.sync_coverage();
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let cov = app.testing.coverage.clone().unwrap();
    let calc = cov
        .files
        .get(&root.join("calc.py"))
        .expect("calc.py measured");
    assert_eq!(calc.line(2), Some(LineCov::Covered), "`return a + b` ran");
    assert_eq!(
        calc.line(6),
        Some(LineCov::Uncovered),
        "only test_sub reaches `return a - b`"
    );
}

/// #263: the open file rewritten on disk under a clean buffer dims its
/// coverage marks, as an edit in the buffer does: they describe old text.
#[test]
fn an_external_rewrite_of_the_open_file_dims_its_coverage() {
    use crate::testing::coverage::Coverage;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let file = root.join("f.rs");
    std::fs::write(&file, "fn a() {}\nfn b() {}\n").unwrap();
    let mut app = App::new(root.clone()).unwrap();
    app.editor.open_pinned(&file).unwrap();
    app.testing.on_coverage(Ok(Coverage::from_lcov(
        "SF:f.rs\nDA:1,3\nDA:2,0\nend_of_record\n",
        &root,
    )));
    assert!(app.sync_coverage());
    assert!(!app.editor.coverage.as_ref().unwrap().stale);
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(&file, "fn b() {}\n\nfn a() {}\n").unwrap();
    assert!(app.reload_open_file_after_external_change());
    app.sync_coverage();
    assert!(
        app.editor.coverage.as_ref().unwrap().stale,
        "the reloaded file's marks are stale"
    );
}

/// #263: a watched scope reruns on a source save, but not on a save of a
/// file git ignores (build output, a `.env`).
#[test]
fn a_save_of_an_ignored_file_does_not_rerun_watched_tests() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(args)
            .output()
            .unwrap()
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(root.join(".gitignore"), "*.log\nout/\n").unwrap();
    std::fs::create_dir_all(root.join("out")).unwrap();
    let mut app = App::new(root.clone()).unwrap();
    app.active_test_root = root.clone();
    app.testing
        .watch
        .toggle(crate::testing::watch::WatchScope::All);
    let later = std::time::Instant::now() + std::time::Duration::from_secs(5);
    for ignored in ["run.log", "out/gen.rs"] {
        app.record_history_snapshot_of(&root.join(ignored), Default::default(), None);
        assert_eq!(
            app.testing.watch.take_due(later, false),
            None,
            "{ignored} is ignored"
        );
    }
    app.record_history_snapshot_of(&root.join("lib.rs"), Default::default(), None);
    assert_eq!(
        app.testing.watch.take_due(later, false),
        Some(crate::testing::watch::WatchScope::All),
        "a source save reruns"
    );
}

/// #263 against a real cargo-llvm-cov: "Run All Tests with Coverage" on a
/// crate marks the lines its test ran green and a function no test calls
/// red, with a percentage between. Needs `cargo llvm-cov` (and the
/// `llvm-tools-preview` component it drives).
#[test]
#[ignore = "needs cargo-llvm-cov"]
fn a_real_cargo_llvm_cov_run_marks_the_covered_file() {
    use crate::testing::coverage::LineCov;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/lib.rs"),
        // Two literals, not one continued across a line: a continuation
        // line opening with `#[` reads as an attribute to the doc gate's
        // line classifier, which then never sees the closing quote.
        concat!(
            "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n\npub fn unused() -> i32 {\n    0\n}\n\n",
            "#[cfg(test)]\nmod tests {\n    #[test]\n    fn adds() {\n        assert_eq!(super::add(1, 2), 3);\n    }\n}\n",
        ),
    )
    .unwrap();
    let mut app = App::new(root.clone()).unwrap();
    app.editor.open(&root.join("src/lib.rs")).unwrap();
    app.run_all_tests_with_coverage();
    let end = std::time::Instant::now() + std::time::Duration::from_secs(300);
    while app.editor.coverage.is_none() {
        assert!(std::time::Instant::now() < end, "status: {}", app.status);
        let _ = app.test_worker.drain(&mut app.testing);
        app.sync_coverage();
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let lens = app.editor.coverage.clone().unwrap();
    assert_eq!(lens.lines.get(&1), Some(&LineCov::Covered), "`a + b` ran");
    assert_eq!(
        lens.lines.get(&5),
        Some(&LineCov::Uncovered),
        "`unused`'s body never ran: {:?}",
        lens.lines
    );
    assert!(
        lens.percent.is_some_and(|p| p > 0.0 && p < 100.0),
        "{:?}",
        lens.percent
    );
}

/// #1534: a run asked for while another is in flight says why nothing
/// happened and how to stop the one running, instead of doing nothing.
#[test]
fn a_test_run_while_one_is_in_flight_says_how_to_stop_it() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("Cargo.toml"), "[package]\nname = \"t\"\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.testing
        .on_busy_started(crate::testing::model::Activity::Running);
    app.run_all_tests();
    assert_eq!(app.status, TEST_RUN_IN_PROGRESS);
    app.status.clear();
    app.run_test(String::from("m::a"));
    assert_eq!(app.status, TEST_RUN_IN_PROGRESS);
}

/// #1534: `s` in the Testing view cancels a run in flight; with nothing
/// running, Testing: Cancel Test Run says so.
#[test]
fn cancel_test_run_stops_a_run_and_says_so_when_idle() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.cancel_test_run();
    assert_eq!(app.status, "No test run in progress");
    app.testing
        .on_busy_started(crate::testing::model::Activity::Running);
    app.handle_testing_key(key(KeyCode::Char('s'), KeyModifiers::NONE));
    assert_eq!(app.status, "Cancelling test run");
}
