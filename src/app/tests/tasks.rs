//! Tasks, running the active file, live run and build output.

use super::*;

#[test]
fn run_spec_for_runs_an_executable_binary_directly() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    // An extensionless file with the +x bit is runnable: run it directly.
    let bin = tmp.path().join("myapp");
    std::fs::write(&bin, b"#!/bin/sh\necho hi\n").unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    let spec = App::run_spec_for(&bin, tmp.path()).expect("executable must be runnable");
    assert_eq!(spec.program, bin.to_string_lossy());
    assert!(spec.args.is_empty(), "the binary is run with no extra args");

    // A non-executable, unknown file (like .git/config) is not runnable.
    let cfg = tmp.path().join("config");
    std::fs::write(&cfg, b"[core]\n").unwrap();
    assert!(App::run_spec_for(&cfg, tmp.path()).is_none());
}

#[test]
fn cmd_shift_b_runs_the_projects_build_task_in_a_named_pane() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("Makefile"), "build:\n\ttrue\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let before = app.terminals.len();
    app.handle_key(key(
        KeyCode::Char('B'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert_eq!(
        app.terminals.len(),
        before + 1,
        "the build task gets its own pane"
    );
    assert_eq!(
        app.terminals[app.active_terminal].label(),
        "Task: make build",
        "the pane is named after the task"
    );
    assert!(app.show_terminal, "the panel reveals");
    assert!(
        matches!(app.focus, Pane::Terminal),
        "focus follows the task"
    );
}

#[test]
fn rerunning_a_task_reuses_its_pane_instead_of_stacking_new_ones() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("Makefile"), "build:\n\ttrue\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let chord = key(
        KeyCode::Char('B'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    );
    app.handle_key(chord).unwrap();
    // Wait for the pane's shell to come back to its prompt so the rerun
    // path sees an idle pane (a busy pane legitimately gets a new one).
    // Load-scaled and LOUD (#397): the fixed 5000ms loop broke out silently
    // on timeout, so under suite load the next step met a pane still busy
    // and the assertion after it misreported the timeout as its own failure.
    crate::test_budget::await_spawned(
        crate::test_budget::tests::TASK_PANE_PROMPT_BASE,
        "the task pane's shell to return to its prompt",
        || app.terminals[app.active_terminal].foreground_is_shell(),
    );
    let after_first = app.terminals.len();
    app.handle_key(chord).unwrap();
    assert_eq!(
        app.terminals.len(),
        after_first,
        "rerunning the same task must reuse its idle pane"
    );
    assert_eq!(
        app.terminals
            .iter()
            .filter(|t| t.label() == "Task: make build")
            .count(),
        1,
        "exactly one pane carries the task's name"
    );
}

#[test]
fn a_reused_task_pane_clears_a_half_typed_prompt_line_first() {
    // A shell sitting at its prompt can hold unsubmitted text in the line
    // editor; writing the task command raw would concatenate with it (the
    // `format_cd_command` test pins the same rule for cd).
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("Makefile"), "build:\n\ttrue\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let chord = key(
        KeyCode::Char('B'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    );
    app.handle_key(chord).unwrap();
    // Load-scaled and LOUD (#397): the fixed 5000ms loop broke out silently
    // on timeout, so under suite load the next step met a pane still busy
    // and the assertion after it misreported the timeout as its own failure.
    crate::test_budget::await_spawned(
        crate::test_budget::tests::TASK_PANE_PROMPT_BASE,
        "the task pane's shell to return to its prompt",
        || app.terminals[app.active_terminal].foreground_is_shell(),
    );
    app.handle_key(chord).unwrap();
    let bytes = app.terminals[app.active_terminal].written_bytes_for_test();
    let needle = b"\x05x\x15make build\r";
    assert!(
        bytes.windows(needle.len()).any(|w| w == needle),
        "the reused-pane write must clear the line editor first (Ctrl-E, a letter, Ctrl-U)"
    );
}

#[test]
fn a_task_pane_from_the_old_workspace_is_not_reused_after_a_re_root() {
    // The pane's shell still sits in workspace A; running B's build task
    // must not execute it there.
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    std::fs::write(a.path().join("Makefile"), "build:\n\ttrue\n").unwrap();
    std::fs::write(b.path().join("Makefile"), "build:\n\ttrue\n").unwrap();
    let mut app = App::new(a.path().to_path_buf()).unwrap();
    let chord = key(
        KeyCode::Char('B'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    );
    app.handle_key(chord).unwrap();
    // Load-scaled and LOUD (#397). The fixed 5000ms loop broke out silently
    // on timeout, and here a still-busy pane passes the assertion below for
    // the wrong reason: a busy pane always gets a sibling, so `after + 1`
    // held without the cwd guard ever being exercised. Failing loudly makes
    // the guard the thing under test.
    crate::test_budget::await_spawned(
        crate::test_budget::tests::TASK_PANE_PROMPT_BASE,
        "the task pane's shell to return to its prompt",
        || app.terminals[app.active_terminal].foreground_is_shell(),
    );
    // Focus another pane so the re-root's active-pane `cd` cannot move the
    // task pane along (the reviewer's scenario: it is NOT cd'd).
    app.active_terminal = 0;
    app.change_workspace_root(b.path().to_path_buf());
    assert!(
        app.last_task.is_none(),
        "Rerun Last Task must not carry a task across a re-root"
    );
    let after = app.terminals.len();
    app.handle_key(chord).unwrap();
    assert_eq!(
        app.terminals.len(),
        after + 1,
        "a task pane whose shell still sits in the old workspace must not be reused"
    );
}

#[test]
fn run_task_palette_command_opens_the_task_picker() {
    use crate::widgets::list_picker::ListPurpose;
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("Makefile"),
        "build:\n\ttrue\ntest:\n\ttrue\n",
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.run_command(crate::widgets::command_palette::Command::RunTask);
    let picker = app
        .list_picker
        .as_ref()
        .expect("Tasks: Run Task must open the picker");
    assert_eq!(picker.purpose, ListPurpose::RunTask);
    assert!(
        picker.rows.iter().any(|r| r.label.contains("make build")),
        "discovered tasks appear as rows: {:?}",
        picker.rows.iter().map(|r| &r.label).collect::<Vec<_>>()
    );
}

#[test]
fn run_build_task_key_predicate_accepts_the_chord_and_rejects_neighbours() {
    assert!(is_run_build_task_key(key(
        KeyCode::Char('b'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT
    )));
    assert!(is_run_build_task_key(key(
        KeyCode::Char('B'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT
    )));
    // Bare Cmd+B is Toggle Side Bar, not Run Build Task.
    assert!(!is_run_build_task_key(key(
        KeyCode::Char('b'),
        KeyModifiers::SUPER
    )));
    assert!(!is_run_build_task_key(key(
        KeyCode::Char('b'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT | KeyModifiers::ALT
    )));
}

/// #843: Cmd+Enter runs the Markdown fence under the caret (#353), and off
/// macOS `Ctrl`+`Enter` is that chord. `is_run_fence_key` took Super only,
/// so without the kitty keyboard protocol the one way to run a fence was
/// clicking the preview's ▷.
#[cfg(not(target_os = "macos"))]
#[test]
fn ctrl_enter_runs_the_markdown_fence_under_the_caret_off_macos() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = markdown_fence_app(tmp.path());
    let before = app.editor.lines.clone();
    app.handle_key(key(KeyCode::Enter, KeyModifiers::CONTROL))
        .unwrap();
    let pending = app
        .pending_run_block
        .as_ref()
        .expect("Ctrl+Enter confirms the fence, as Cmd+Enter does");
    assert_eq!(pending.pane_name, "R.md:1");
    assert_eq!(app.editor.lines, before, "no line break typed");
}

/// Negatives (#1571): a bare `Enter` still splits the line, `Cmd`+`Shift`+
/// `Enter` still opens a line above, `Cmd`+`O` stays an alias for below, and
/// in a Markdown fence `Cmd`+`Enter` still runs the block.
#[test]
fn other_enters_and_the_markdown_fence_keep_their_meaning() {
    let tmp = tempfile::tempdir().unwrap();
    let body = "total = price * qty\nprint(total)\n";
    for (mods, code, want) in [
        (
            KeyModifiers::NONE,
            KeyCode::Enter,
            ["total = ", "price * qty", "print(total)"],
        ),
        (
            KeyModifiers::SUPER | KeyModifiers::SHIFT,
            KeyCode::Enter,
            ["", "total = price * qty", "print(total)"],
        ),
        (
            KeyModifiers::SUPER,
            KeyCode::Char('o'),
            ["total = price * qty", "", "print(total)"],
        ),
    ] {
        let mut app = app_with_open_file(tmp.path(), "i.py", body);
        app.editor.cursor_row = 0;
        app.editor.cursor_col = 8;
        app.handle_key(key(code, mods)).unwrap();
        assert_eq!(app.editor.lines, want, "{mods:?}+{code:?}");
    }

    let mut app = markdown_fence_app(tmp.path());
    let before = app.editor.lines.clone();
    app.handle_key(key(KeyCode::Enter, KeyModifiers::SUPER))
        .unwrap();
    assert!(app.pending_run_block.is_some(), "the fence still runs");
    assert_eq!(app.editor.lines, before, "no line inserted");
}

/// Guard (#843): "Markdown: Run Code Block at Cursor" refuses, and says why,
/// where there is nothing to run: a buffer that is not Markdown, the
/// rendered preview (its ▷ runs blocks), a caret outside every fence.
#[test]
fn run_code_block_at_cursor_refuses_outside_a_markdown_fence() {
    use crate::widgets::command_palette::Command;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "notes.txt", "```sh\necho hi\n```\n");
    app.editor.cursor_row = 1;
    app.run_command(Command::RunCodeBlockAtCursor);
    assert!(app.pending_run_block.is_none());
    assert!(app.status.contains("Markdown"), "{}", app.status);

    let mut app = markdown_fence_app(tmp.path());
    app.toggle_markdown_preview();
    app.status.clear();
    app.run_command(Command::RunCodeBlockAtCursor);
    assert!(app.pending_run_block.is_none(), "not from the preview");
    assert!(app.status.contains("Markdown"), "{}", app.status);

    let mut app = markdown_fence_app(tmp.path());
    app.editor.cursor_row = 0;
    app.run_command(Command::RunCodeBlockAtCursor);
    assert!(app.pending_run_block.is_none());
    assert!(
        app.status.contains("inside a runnable shell fence"),
        "{}",
        app.status
    );
}

/// Guard (#843): on macOS Ctrl+Enter is not the fence chord: Cmd is, and
/// Ctrl stays the terminal's.
#[cfg(target_os = "macos")]
#[test]
fn ctrl_enter_is_not_the_fence_chord_on_macos() {
    assert!(!is_run_fence_key(key(
        KeyCode::Enter,
        KeyModifiers::CONTROL
    )));
    assert!(is_run_fence_key(key(KeyCode::Enter, KeyModifiers::SUPER)));
}

#[test]
fn run_spec_for_python_with_no_venv_falls_back_to_system_python3() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("script.py");
    std::fs::write(&f, b"").unwrap();
    let spec = App::run_spec_for(&f, tmp.path()).unwrap();
    assert_eq!(spec.program, "python3");
    assert_eq!(spec.args, vec![f.to_string_lossy().into_owned()]);
    assert_eq!(spec.cwd, tmp.path());
}

#[test]
fn run_spec_for_node_picks_node_for_dot_js() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("server.js");
    std::fs::write(&f, b"").unwrap();
    let spec = App::run_spec_for(&f, tmp.path()).unwrap();
    assert_eq!(spec.program, "node");
    assert_eq!(spec.args, vec![f.to_string_lossy().into_owned()]);
}

#[test]
fn run_spec_for_bash_picks_bash_for_dot_sh() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("build.sh");
    std::fs::write(&f, b"").unwrap();
    let spec = App::run_spec_for(&f, tmp.path()).unwrap();
    assert_eq!(spec.program, "bash");
    assert_eq!(spec.args, vec![f.to_string_lossy().into_owned()]);
}

#[test]
fn run_spec_for_unknown_extension_returns_none() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("notes.txt");
    std::fs::write(&f, b"").unwrap();
    assert!(App::run_spec_for(&f, tmp.path()).is_none());
}

#[test]
fn run_spec_passes_apostrophe_paths_through_argv_unescaped() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("O'Reilly.py");
    std::fs::write(&f, b"").unwrap();
    let spec = App::run_spec_for(&f, tmp.path()).unwrap();
    assert_eq!(
        spec.args,
        vec![f.to_string_lossy().into_owned()],
        "argv carries the path verbatim — no shell quoting because no shell is invoked"
    );
}

#[test]
fn run_spec_picks_dot_venv_python_in_the_file_directory_over_system_python3() {
    // Reproduces the user-reported case: croft is opened on a parent
    // directory, the file lives in a subfolder that owns a .venv, and
    // running the file with system python3 misses the project's deps.
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let venv_python = make_venv(&project, ".venv");
    let f = project.join("main.py");
    std::fs::write(&f, b"").unwrap();
    let spec = App::run_spec_for(&f, tmp.path()).unwrap();
    assert_eq!(
        spec.program,
        venv_python.to_string_lossy().into_owned(),
        "venv interpreter must be the spawned program (looked for {})",
        venv_python.display()
    );
    assert_eq!(spec.args, vec![f.to_string_lossy().into_owned()]);
    assert_eq!(
        spec.cwd, project,
        "cwd must be the directory that owns the venv so relative imports / .env files resolve"
    );
}

#[test]
fn run_spec_walks_up_from_file_dir_to_find_venv_in_an_ancestor() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("project");
    let nested = project.join("src").join("pkg");
    std::fs::create_dir_all(&nested).unwrap();
    make_venv(&project, ".venv");
    let f = nested.join("main.py");
    std::fs::write(&f, b"").unwrap();
    let spec = App::run_spec_for(&f, tmp.path()).unwrap();
    assert!(
        spec.program.ends_with(".venv/bin/python"),
        "expected venv python program, got: {}",
        spec.program
    );
    assert_eq!(spec.cwd, project);
}

#[test]
fn run_spec_also_picks_a_plain_venv_directory() {
    let tmp = tempfile::tempdir().unwrap();
    make_venv(tmp.path(), "venv");
    let f = tmp.path().join("main.py");
    std::fs::write(&f, b"").unwrap();
    let spec = App::run_spec_for(&f, tmp.path()).unwrap();
    assert!(
        spec.program.ends_with("venv/bin/python"),
        "expected plain-venv python program, got: {}",
        spec.program
    );
}

#[test]
fn run_spec_does_not_walk_above_workspace_root() {
    let tmp = tempfile::tempdir().unwrap();
    // venv lives ABOVE the workspace root — must be ignored.
    make_venv(tmp.path(), ".venv");
    let workspace = tmp.path().join("inner");
    std::fs::create_dir_all(&workspace).unwrap();
    let f = workspace.join("main.py");
    std::fs::write(&f, b"").unwrap();
    let spec = App::run_spec_for(&f, &workspace).unwrap();
    assert_eq!(
        spec.program, "python3",
        "must NOT use a venv that sits above the workspace root, got: {}",
        spec.program
    );
}

#[test]
fn run_active_file_with_no_open_file_records_feedback_and_does_not_spawn_terminal() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let term_count_before = app.terminals.len();
    app.run_active_file();
    assert_eq!(
        app.terminals.len(),
        term_count_before,
        "no file open: must NOT spawn an extra terminal"
    );
    assert!(app.run_debug.feedback_is_error);
    assert!(app.run_debug.feedback.is_some());
}

#[test]
fn run_active_file_with_unknown_extension_records_feedback_and_does_not_spawn_terminal() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("notes.txt");
    std::fs::write(&f, b"hi").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&f).unwrap();
    let term_count_before = app.terminals.len();
    app.run_active_file();
    assert_eq!(app.terminals.len(), term_count_before);
    assert!(app.run_debug.feedback_is_error);
}

#[test]
fn run_active_file_with_python_file_spawns_a_new_terminal_and_focuses_it() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("hello.py");
    std::fs::write(&f, b"print('hi')\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&f).unwrap();
    let term_count_before = app.terminals.len();
    app.run_active_file();
    assert_eq!(
        app.terminals.len(),
        term_count_before + 1,
        "running a known extension must spawn a fresh terminal so the previous one's history isn't clobbered"
    );
    assert!(app.show_terminal, "terminal pane must become visible");
    assert!(matches!(app.focus, Pane::Terminal));
    assert!(!app.run_debug.feedback_is_error);
}

/// #1400: Run writes the unsaved tab before the run command starts.
#[test]
fn run_saves_the_unsaved_buffer_before_running() {
    let (_tmp, mut app, file) = app_with_unsaved_pinned_file("hello.py");
    app.run_active_file();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "new\nold\n");
    assert!(!app.editor.dirty);
}

/// #1639 fixture: [`app_with_unsaved_pinned_file`] with a tasks.json task
/// `run hello` that cats the file.
fn app_with_unsaved_file_and_task(name: &str) -> (tempfile::TempDir, App, PathBuf) {
    let (tmp, app, file) = app_with_unsaved_pinned_file(name);
    let vscode = tmp.path().join(".vscode");
    std::fs::create_dir_all(&vscode).unwrap();
    std::fs::write(
        vscode.join("tasks.json"),
        format!(
            r#"{{ "version": "2.0.0", "tasks": [ {{ "label": "run hello", "type": "shell", "command": "cat {name}", "group": {{ "kind": "build", "isDefault": true }} }} ] }}"#
        ),
    )
    .unwrap();
    (tmp, app, file)
}

/// #1639: Tasks: Run Task writes the unsaved tab before the command
/// starts, as F5 and Run do since #1400, so the task runs the text shown.
#[test]
fn run_task_saves_the_unsaved_buffer_before_the_task_starts() {
    use crate::widgets::list_picker::ListPurpose;
    let (_tmp, mut app, file) = app_with_unsaved_file_and_task("hello.py");
    app.run_command(crate::widgets::command_palette::Command::RunTask);
    assert_eq!(
        app.list_picker.as_ref().map(|p| p.purpose),
        Some(ListPurpose::RunTask)
    );
    app.confirm_list_picker();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "new\nold\n");
    assert!(!app.editor.dirty);
    assert_eq!(app.status, "Running run hello");
}

/// #1639: the build task and Rerun Last Task save first too.
#[test]
fn the_build_task_and_rerun_last_task_save_first() {
    let (_tmp, mut app, file) = app_with_unsaved_file_and_task("hello.py");
    app.run_build_task();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "new\nold\n");
    app.editor.insert_str("newer\n");
    app.rerun_last_task();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "new\nnewer\nold\n");
    assert_eq!(app.unsaved_count(), 0);
}

/// #1639: with saving before a run turned off, the task leaves the disk
/// alone and the status names the file it did not see.
#[test]
fn with_save_before_run_off_a_task_names_the_unsaved_file() {
    let (_tmp, mut app, file) = app_with_unsaved_file_and_task("hello.py");
    app.save_before_debug = false;
    app.run_build_task();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "old\n");
    assert_eq!(
        app.status,
        "hello.py has unsaved changes: the task uses the file on disk - Running run hello"
    );
}

/// #1639 negative: with nothing unsaved, a task writes nothing and says
/// nothing about saving; a workspace with no test runner saves nothing.
#[test]
fn a_task_with_nothing_unsaved_writes_nothing() {
    let (tmp, mut app, file) = app_with_unsaved_file_and_task("hello.py");
    app.save_all();
    let stamp = std::fs::metadata(&file).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    app.run_build_task();
    assert_eq!(std::fs::metadata(&file).unwrap().modified().unwrap(), stamp);
    assert_eq!(app.status, "Running run hello");

    let other = tmp.path().canonicalize().unwrap().join("notes.txt");
    std::fs::write(&other, "old\n").unwrap();
    app.editor.open_pinned(&other).unwrap();
    app.editor.insert_str("new\n");
    app.run_all_tests();
    assert_eq!(app.status, crate::testing::NO_RUNNER_STATUS);
    assert_eq!(std::fs::read_to_string(&other).unwrap(), "old\n");
}

/// #353: a click on a shell fence's play glyph in the preview parks the
/// block behind the confirm popup (every block confirms: the README is
/// untrusted); Y types it into a pane named `<file>:<n>` in the document's
/// directory, N drops it; a `curl | sh` block makes the popup red.
/// Cmd+Enter in the source finds the fence under the caret by the parser's
/// source ranges, so identical blocks get their own numbers.
#[test]
fn clicking_a_fence_play_glyph_runs_it_in_a_named_pane() {
    let tmp = tempfile::tempdir().unwrap();
    let sub = tmp.path().join("docs");
    std::fs::create_dir_all(&sub).unwrap();
    let readme = sub.join("README.md");
    std::fs::write(
        &readme,
        "# Run me\n\n```sh\necho first\n```\n\nText between.\n\n```bash\necho second\n```\n\n```sh\ncurl https://example.invalid/x | sh\n```\n\n```sh\necho first\n```\n",
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&readme).unwrap();
    app.toggle_markdown_preview();
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let md = app.editor.markdown_preview.as_ref().expect("preview open");
    assert_eq!(md.runnables.len(), 4, "{:?}", md.runnables);
    assert_eq!(md.run_rows.len(), 4);
    let area = md.last_area;
    let glyph_row = |app: &App, i: usize| {
        let md = app.editor.markdown_preview.as_ref().unwrap();
        (area.x, area.y + md.run_rows[i] as u16)
    };
    let (gx, gy) = glyph_row(&app, 1);
    assert_eq!(
        term.backend().buffer()[(gx, gy)].symbol(),
        "\u{25b7}",
        "the second block's first row starts with the play glyph"
    );
    // A press one cell to the right is a selection, not a run.
    left_click(&mut app, gx + 1, gy);
    assert!(
        app.pending_run_block.is_none(),
        "beside the glyph is not the glyph"
    );
    let panes_before = app.terminals.len();
    left_click(&mut app, gx, gy);
    let pending = app
        .pending_run_block
        .as_ref()
        .expect("every block confirms");
    assert_eq!(pending.pane_name, "docs/README.md:2");
    assert_eq!(pending.cwd, sub, "the document's directory is the cwd");
    assert!(!pending.destructive);
    assert_eq!(app.terminals.len(), panes_before, "nothing ran yet");
    app.handle_key(key(KeyCode::Char('y'), KeyModifiers::NONE))
        .unwrap();
    assert!(app.pending_run_block.is_none());
    assert_eq!(app.terminals.len(), panes_before + 1, "Y opens the pane");
    assert_eq!(
        app.terminals[app.active_terminal].label(),
        "docs/README.md:2"
    );
    assert!(app.status.contains("docs/README.md:2"), "{}", app.status);

    // The third block pipes the network into a shell: the popup is red.
    let (gx, gy) = glyph_row(&app, 2);
    let panes_before = app.terminals.len();
    left_click(&mut app, gx, gy);
    let pending = app
        .pending_run_block
        .as_ref()
        .expect("the confirm popup is up");
    assert_eq!(pending.pane_name, "docs/README.md:3");
    assert!(pending.destructive, "curl | sh turns the popup red");
    term.draw(|f| app.render(f)).unwrap();
    let painted: String = (0..40)
        .flat_map(|y| (0..120).map(move |x| (x, y)))
        .map(|(x, y)| term.backend().buffer()[(x, y)].symbol().to_string())
        .collect();
    assert!(
        painted.contains("LOOKS DESTRUCTIVE"),
        "the popup names the risk"
    );
    assert!(
        painted.contains("curl https://example.invalid/x | sh"),
        "and shows the block whole"
    );
    app.handle_key(key(KeyCode::Char('n'), KeyModifiers::NONE))
        .unwrap();
    assert!(app.pending_run_block.is_none(), "N drops it");
    assert_eq!(app.terminals.len(), panes_before);

    // Cmd+Enter in the source: the fence under the caret, by source range.
    // The last block repeats the first's text and still gets its own number.
    app.toggle_markdown_preview();
    assert!(app.editor.markdown_preview.is_none());
    app.focus_pane(super::Pane::Editor);
    app.editor.cursor_row = 3; // "echo first"
    app.handle_key(key(KeyCode::Enter, KeyModifiers::SUPER))
        .unwrap();
    let pending = app
        .pending_run_block
        .as_ref()
        .expect("Cmd+Enter confirms too");
    assert_eq!(pending.pane_name, "docs/README.md:1");
    app.handle_key(key(KeyCode::Char('n'), KeyModifiers::NONE))
        .unwrap();
    app.focus_pane(super::Pane::Editor);
    app.editor.cursor_row = 17; // the repeated "echo first"
    app.handle_key(key(KeyCode::Enter, KeyModifiers::SUPER))
        .unwrap();
    let pending = app
        .pending_run_block
        .as_ref()
        .expect("the caret is in the fourth fence");
    assert_eq!(
        pending.pane_name, "docs/README.md:4",
        "identical blocks keep their own numbers"
    );
    app.handle_key(key(KeyCode::Char('n'), KeyModifiers::NONE))
        .unwrap();
    // Outside any fence it is Insert Line Below (#1571), never a run.
    app.focus_pane(super::Pane::Editor);
    app.editor.cursor_row = 6; // "Text between."
    let lines = app.editor.lines.len();
    app.handle_key(key(KeyCode::Enter, KeyModifiers::SUPER))
        .unwrap();
    assert!(app.pending_run_block.is_none());
    assert_eq!(app.status, "Opened line below");
    assert_eq!(app.editor.lines.len(), lines + 1);
    assert_eq!(app.editor.cursor_row, 7);
    // Cmd+Shift+Enter is the editor's "insert line above", never a run.
    assert!(!super::is_run_fence_key(key(
        KeyCode::Enter,
        KeyModifiers::SUPER | KeyModifiers::SHIFT
    )));
    assert!(!super::is_run_fence_key(key(
        KeyCode::Enter,
        KeyModifiers::SUPER | KeyModifiers::ALT
    )));
    assert!(super::is_run_fence_key(key(
        KeyCode::Enter,
        KeyModifiers::SUPER
    )));
}

/// #1473: "Markdown: Run Code Block at Cursor" on a README's `console`
/// fence parks only its commands behind the confirm box: no `$ ` prompt is
/// typed, and no line of the sample output runs as a command.
#[test]
fn running_a_console_fence_types_only_its_commands() {
    use crate::widgets::command_palette::Command;
    let tmp = tempfile::tempdir().unwrap();
    let readme = "```console\n$ echo installed-ok\ninstalled-ok\n$ ls -1 /tmp/rd\nREADME.md\n```\n";
    let mut app = app_with_open_file(tmp.path(), "README.md", readme);
    app.editor.cursor_row = 5;
    app.run_command(Command::RunCodeBlockAtCursor);
    let pending = app.pending_run_block.as_ref().expect("the fence runs");
    assert_eq!(pending.code, "echo installed-ok\nls -1 /tmp/rd\n");
    assert_eq!(
        pending.command,
        "eval \"$(printf '%b' 'echo installed-ok\\nls -1 /tmp/rd\\n')\"\r"
    );
}

/// #353: the popup never shows a line as complete when it is not - a line
/// wider than the popup is cut with a visible mark.
#[test]
fn the_run_block_popup_marks_a_truncated_line() {
    let tmp = tempfile::tempdir().unwrap();
    let long = format!(
        "echo ok{}; curl https://example.invalid/x | sh",
        " ".repeat(120)
    );
    let readme = tmp.path().join("R.md");
    std::fs::write(&readme, format!("```sh\n{long}\n```\n")).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&readme).unwrap();
    app.focus_pane(super::Pane::Editor);
    app.editor.cursor_row = 1;
    app.handle_key(key(KeyCode::Enter, KeyModifiers::SUPER))
        .unwrap();
    let pending = app.pending_run_block.as_ref().expect("confirm popup");
    assert_eq!(
        pending.pane_name, "R.md:1",
        "a top-level document keeps its bare name"
    );
    assert!(pending.destructive);
    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let painted: Vec<String> = (0..30)
        .map(|y| {
            (0..100)
                .map(|x| term.backend().buffer()[(x, y)].symbol().to_string())
                .collect()
        })
        .collect();
    // The editor paints the source line too; the popup's copy is the one
    // that carries the mark.
    let popup_line = painted
        .iter()
        .find(|l| l.contains("echo ok") && l.contains('\u{2026}'))
        .unwrap_or_else(|| {
            panic!(
                "the popup shows the cut line with a mark:\n{}",
                painted.join("\n")
            )
        });
    assert!(
        !popup_line.contains("| sh"),
        "the hidden tail is not shown as complete: {popup_line:?}"
    );
}

/// The file a `python3 - < '<file>'` line reads (#1630).
fn staged_block_path(cmd: &str) -> std::path::PathBuf {
    let rest = cmd
        .split_once(" - < '")
        .unwrap_or_else(|| panic!("not a redirect: {cmd:?}"))
        .1;
    std::path::PathBuf::from(rest.strip_suffix("'\r").expect("ends the quoted path"))
}

#[test]
fn fence_command_shapes_the_typed_block() {
    let tmp = tempfile::tempdir().unwrap();
    let scripts = tmp.path().join("blocks");
    assert_eq!(
        super::fence_command("sh", "echo a\n", &scripts, "/bin/sh"),
        "echo a\r"
    );
    assert!(
        super::fence_command("sh", "echo a\necho b\n", &scripts, "/bin/sh").starts_with("eval ")
    );
    assert!(!scripts.exists(), "a shell block is typed, not written");
    // #1630: any other language is read from a private file on stdin, never
    // typed as a heredoc, which fish has no syntax for.
    let cmd = super::fence_command("python3", "print(1)\n", &scripts, "/bin/sh");
    assert!(cmd.starts_with("python3 - < '"), "{cmd:?}");
    assert!(!cmd.contains("<<") && !cmd.contains('\n'), "{cmd:?}");
    let file = staged_block_path(&cmd);
    assert!(file.starts_with(&scripts));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "print(1)\n");
    use std::os::unix::fs::PermissionsExt;
    let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&file), 0o600, "only the user reads the block");
    assert_eq!(mode(&scripts), 0o700);
    // Two runs of one block never share a file.
    let again = super::fence_command("python3", "print(1)\n", &scripts, "/bin/sh");
    assert_ne!(staged_block_path(&again), file);
    let node = super::fence_command("node", "console.log(1)\n", &scripts, "/bin/sh");
    assert!(
        node.starts_with("node - < '") && node.contains(".js'"),
        "{node:?}"
    );
}

/// #1630: the issue's block prints its output when the typed line runs, and
/// the line holds nothing but a command, a redirect and a quoted path, which
/// bash, zsh and fish all read the same way.
#[test]
fn a_python_block_runs_from_its_file_and_prints() {
    if !(crate::markdown::runnable_interpreter("python") == Some("python3")) {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let code = "print(\"hello from python\")\nprint(2 + 2)\n";
    let cmd = super::fence_command("python3", code, &tmp.path().join("blocks"), "/bin/sh");
    assert_eq!(run_typed_line(&cmd, tmp.path()), "hello from python\n4\n");
}

/// A tab or `!` never reaches the shell's line editor as typed input: a
/// python block's tabs and bangs go into its file byte for byte, and a shell
/// block goes as one quoted printf argument that a real shell decodes back
/// to the block.
#[test]
fn fence_blocks_with_tabs_or_bangs_run_exactly_as_shown() {
    let tmp = tempfile::tempdir().unwrap();
    let code = "if True:\n\tprint(\"hi!\")\nx = 'a\\n'\n";
    let cmd = super::fence_command("python3", code, &tmp.path().join("blocks"), "/bin/sh");
    assert!(!cmd.contains('\t') && !cmd.contains('!'), "{cmd:?}");
    assert_eq!(
        std::fs::read_to_string(staged_block_path(&cmd)).unwrap(),
        code
    );
    if crate::markdown::runnable_interpreter("python") == Some("python3") {
        assert_eq!(run_typed_line(&cmd, tmp.path()), "hi!\n");
    }
    // `sh` blocks run in the pane's own shell through eval: a `cd` in the
    // block persists after it.
    let sh = super::fence_command(
        "sh",
        "cd /\necho 'hi!'\n",
        &tmp.path().join("blocks"),
        "/bin/sh",
    );
    assert!(sh.starts_with("eval \"$(printf '%b' "), "{sh}");
    let probe = format!("{}; pwd", sh.trim_end_matches('\r'));
    assert_eq!(run_typed_line(&probe, tmp.path()), "hi!\n/\n");
}

/// #920: a shell block of several lines reaches the shell as ONE command
/// line, so the shell reports one outcome for it: every line's output and
/// the status the block ends with, as `sh` running it as a script would.
#[test]
fn a_multi_line_shell_block_is_typed_as_one_command() {
    let tmp = tempfile::tempdir().unwrap();
    let scripts = tmp.path().join("blocks");
    let code = "echo step-1\nls does-not-exist-920\necho step-3\n(exit 3)\n";
    let cmd = super::fence_command("sh", code, &scripts, "/bin/sh");
    assert_eq!(cmd.matches(['\r', '\n']).count(), 1, "{cmd:?}");
    assert!(cmd.ends_with('\r'), "{cmd:?}");
    let probe = format!("{}; echo exit=$?", cmd.trim_end_matches('\r'));
    let out = run_typed_line(&probe, tmp.path());
    assert!(out.starts_with("step-1\nstep-3\nexit=3\n"), "{out:?}");
    assert!(out.contains("does-not-exist-920"), "{out:?}");
    // A `\` continuation survives. The backslash travels as an octal
    // escape, never as `\\`, which fish's single quotes would fold to one.
    let cont = super::fence_command("sh", "echo one \\\n  two\n", &scripts, "/bin/sh");
    assert!(!cont.contains("\\\\"), "{cont:?}");
    assert_eq!(run_typed_line(&cont, tmp.path()), "one two\n");
}

/// #920: fish before 3.4 rejects `$(...)` as syntax, so a fish pane
/// `source`s the block from a `psub` file, which runs it in the pane's own
/// shell (a `cd` persists) on every fish, and leaves the block's stdin the
/// pane's: a `read` in it still gets what the user types.
#[test]
fn a_multi_line_shell_block_in_a_fish_pane_is_sourced() {
    let tmp = tempfile::tempdir().unwrap();
    let code = "cd /\nread x; echo got $x\necho 'hi!'\t'back\\slash'\n";
    let cmd = super::fence_command("sh", code, &tmp.path().join("blocks"), "/usr/bin/fish");
    assert!(cmd.starts_with("source (printf '%b' '"), "{cmd:?}");
    assert!(cmd.ends_with("' | psub)\r"), "{cmd:?}");
    assert!(!cmd.contains("$("), "{cmd:?}");
    // Not `--no-config`, which fish before 3.3 lacks: an empty config home.
    let Ok(mut fish) = std::process::Command::new("fish")
        .arg("-c")
        .arg(format!("{}; pwd", cmd.trim_end_matches('\r')))
        .env("XDG_CONFIG_HOME", tmp.path())
        .current_dir(tmp.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
    else {
        return;
    };
    use std::io::Write;
    fish.stdin.take().unwrap().write_all(b"typed\n").unwrap();
    let out = fish.wait_with_output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "got typed\nhi! back\\slash\n/\n"
    );
}

/// #920: a block of several lines settles its capture on the WHOLE block.
/// The pane reads typed lines as an interactive shell does, one command per
/// line between OSC 133 marks. Typed line by line, the block finished three
/// times, and its box and `{persist}` fence held only the first command's
/// output and exit.
#[test]
fn a_multi_line_blocks_capture_holds_every_line_and_the_blocks_exit() {
    let tmp = tempfile::tempdir().unwrap();
    let doc = tmp.path().join("BUILD.md");
    let code = "echo step-1\nprintf 'a\\nb\\nc\\n' | sort -r\n(exit 3)\n";
    std::fs::write(&doc, format!("```sh {{persist}}\n{code}```\n")).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let shell = "while :; do printf '\\033]133;A\\007$ \\033]133;B\\007'; \
                 IFS= read -r line || exit; printf '\\033]133;C\\007'; \
                 eval \"$line\"; printf '\\033]133;D;%d\\007' $?; done";
    let mut pane = crate::widgets::terminal::PtyTerminal::new_running(
        "/bin/sh",
        &[String::from("-c"), shell.into()],
        tmp.path(),
    )
    .unwrap();
    pane.set_manual_name(Some(String::from("BUILD.md:1")));
    app.insert_terminal(pane);
    let idx = app.terminals.len() - 1;
    crate::test_budget::await_spawned(
        crate::test_budget::tests::SHELL_PAINT_BASE,
        "the pane's first prompt",
        || !app.terminals[idx].prompt_lines().is_empty(),
    );
    let panes = app.terminals.len();
    app.run_block_in_pane(PendingRunBlock {
        pane_name: String::from("BUILD.md:1"),
        cwd: tmp.path().to_path_buf(),
        command: super::fence_command("sh", code, &tmp.path().join("blocks"), "/bin/sh"),
        code: code.to_string(),
        destructive: false,
        doc: Some(doc.clone()),
        block_line: 0,
        persist: true,
        capture_timeout: None,
    });
    assert_eq!(app.terminals.len(), panes, "the idle pane ran the block");
    let key = (String::from("BUILD.md:1"), 0);
    crate::test_budget::await_spawned(
        crate::test_budget::tests::SHELL_PAINT_BASE,
        "the block's capture to settle",
        || {
            app.settle_block_captures();
            app.block_outputs.contains_key(&key)
        },
    );
    let out = &app.block_outputs[&key];
    assert_eq!(out.text, "step-1\nc\nb\na", "every line's output");
    assert_eq!(out.exit, Some(3), "the exit the block ended with");
    let written = std::fs::read_to_string(&doc).unwrap();
    assert!(written.contains("step-1\nc\nb\na\n"), "{written}");
}

/// Negative (#1630): when the file cannot be written the block is still
/// typed whole, as the heredoc it was, rather than not run at all.
#[test]
fn a_block_that_cannot_be_written_falls_back_to_the_heredoc() {
    let tmp = tempfile::tempdir().unwrap();
    let not_a_dir = tmp.path().join("blocks");
    std::fs::write(&not_a_dir, "").unwrap();
    let cmd = super::fence_command("python3", "print(1)\n", &not_a_dir, "/bin/sh");
    assert_eq!(cmd, "python3 - <<'CROFT_BLOCK'\nprint(1)\nCROFT_BLOCK\r");
    // A block holding the terminator gets a different one.
    let tricky = "print(1)\nCROFT_BLOCK\n";
    let cmd = super::fence_command("python3", tricky, &not_a_dir, "/bin/sh");
    assert!(cmd.starts_with("python3 - <<'CROFT_BLOCK_1'\n"), "{cmd}");
}

/// Negative (#1630): writing a block clears day-old blocks and nothing else.
#[test]
fn writing_a_block_clears_only_old_blocks() {
    let tmp = tempfile::tempdir().unwrap();
    let scripts = tmp.path().join("blocks");
    let first = super::stage_block(&scripts, "python3", "print(1)").unwrap();
    let stale = scripts.join("block-1-0-0.py");
    std::fs::write(&stale, "print(0)\n").unwrap();
    let two_days = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 24 * 60 * 60);
    std::fs::File::options()
        .write(true)
        .open(&stale)
        .unwrap()
        .set_modified(two_days)
        .unwrap();
    let other = scripts.join("notes.txt");
    std::fs::write(&other, "keep").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&other)
        .unwrap()
        .set_modified(two_days)
        .unwrap();
    let second = super::stage_block(&scripts, "python3", "print(2)").unwrap();
    assert!(!stale.exists(), "a day-old block is cleared");
    assert!(first.exists() && second.exists() && other.exists());
}

/// #1522: a task or terminal problem matcher can emit `hint` severity too;
/// those build rows stay out of the PROBLEMS list and its count as well.
#[test]
fn task_matcher_hints_stay_out_of_the_problems_list_and_its_count() {
    use crate::lsp::manager::DiagnosticSeverity;
    use crate::widgets::problems::ProblemItem;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let a = tmp.path().join("a.rs");
    let row = |line: u32, severity: DiagnosticSeverity| ProblemItem {
        line,
        col: 0,
        col_utf16: false,
        severity,
        message: String::from("m"),
        source: String::from("task"),
        code: None,
    };
    app.build_diagnostics.insert(
        a.clone(),
        vec![
            (1, row(0, DiagnosticSeverity::Hint)),
            (1, row(1, DiagnosticSeverity::Warning)),
        ],
    );
    app.build_diagnostics.insert(
        tmp.path().join("only_hints.rs"),
        vec![(1, row(0, DiagnosticSeverity::Hint))],
    );
    app.rebuild_problems();
    assert_eq!(app.problems.total_count(), 1, "only the warning counts");
    let listed: Vec<(&std::path::PathBuf, DiagnosticSeverity)> = app
        .problems
        .groups()
        .iter()
        .flat_map(|g| g.items.iter().map(move |i| (&g.path, i.severity)))
        .collect();
    assert_eq!(listed, vec![(&a, DiagnosticSeverity::Warning)]);
}

/// Resident seating never replays a persisted task: a stale record carrying
/// a task must not re-execute an instruction against the buffer on every
/// launch (and an @file task would freeze the UI thread on seat).
#[test]
fn resident_seating_never_replays_a_persisted_task() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = tmp.path().join("collab.sock");
    let seen_task = std::rc::Rc::new(std::cell::RefCell::new(Some(String::from("sentinel"))));
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.pair_record_path = tmp.path().join("x.pair.json");
    app.pair_socket = socket.clone();
    app.pair_host_lock_path = tmp.path().join("x.pair-host.lock");
    app.collab_config = Some((socket, crate::collab::CollabRole::Owner));
    let seen2 = seen_task.clone();
    app.pair_spawn_override = Some(Box::new(move |cfg| {
        *seen2.borrow_mut() = cfg.task.clone();
        anyhow::bail!("stop before a real child");
    }));
    crate::session::write_pair_record(
        &app.pair_record_path,
        &crate::session::PairRecord {
            model: None,
            name: "nav".into(),
            enabled: true,
            task: Some("delete the deprecated helpers".into()),
            provider: None,
            base_url: None,
        },
    )
    .unwrap();
    app.last_pair_check = None;
    app.maybe_seat_navigator();
    assert_eq!(
        *seen_task.borrow(),
        None,
        "the seat must never carry a persisted task"
    );
}

/// A task declaring `["$gcc", "$eslint-stylish"]` puts both tools' errors
/// in PROBLEMS, not just the first matcher's (#1240).
#[test]
fn a_task_with_a_matcher_list_reports_every_matchers_problems() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let matcher =
        crate::problem_matchers::from_tasks_json(&serde_json::json!(["$gcc", "$eslint-stylish"]))
            .map(std::sync::Arc::new);
    app.assign_task_matcher(9, matcher);
    let out = "bad.c:3:10: error: 'y' undeclared\n\nweb/app.js\n  1:5  error  a is unused  no-unused-vars\n";
    assert!(app.apply_build_scan(9, Some(tmp.path()), "gcc -c bad.c; ./lint.sh", out));
    let mut files: Vec<String> = app
        .problems
        .groups()
        .iter()
        .map(|g| g.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    files.sort();
    assert_eq!(files, ["app.js", "bad.c"]);
}

#[test]
fn missing_pre_launch_task_and_stale_selection_error_loudly() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".croft")).unwrap();
    std::fs::write(
        tmp.path().join(".croft/launch.json"),
        r#"[ { "name": "Built", "type": "lldb", "program": "/nope/bin", "preLaunchTask": "make everything" } ]"#,
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.open_debug_config_picker();
    app.list_picker.as_mut().unwrap().selected = 1;
    app.confirm_list_picker();
    assert!(
        app.status
            .contains("preLaunchTask \"make everything\" not found"),
        "{}",
        app.status
    );
    assert!(
        app.pending_debug_launch.is_none(),
        "nothing parked on error"
    );

    // A selection whose config was deleted falls back to zero-config.
    app.selected_debug_config = Some(String::from("Deleted"));
    app.start_selected_debug();
    assert_eq!(app.selected_debug_config, None, "stale selection cleared");
}

#[test]
fn a_task_problem_matcher_owns_its_pane_and_watch_publishes_win() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let cwd = tmp.path().join("proj");
    std::fs::create_dir(&cwd).unwrap();

    // A tasks.json "$gcc" matcher is exclusive for its pane: gcc-shaped
    // rows report, the tsc-shaped line in the same output does not
    // (VS Code's model — an explicit matcher replaces the defaults).
    let gcc = std::sync::Arc::new(crate::problem_matchers::well_known("$gcc").unwrap());
    app.assign_task_matcher(7, Some(gcc));
    let out = "main.c:7:3: error: expected ';'\nsrc/app.ts(4,10): error TS2322: nope\n";
    assert!(app.apply_build_scan(7, Some(&cwd), "make", out));
    let groups = app.problems.groups().to_vec();
    assert_eq!(groups.len(), 1, "{groups:?}");
    assert!(groups[0].path.ends_with("main.c"));

    // A watch publish replaces the pane's batch wholesale — and the
    // watcher's own exit (a FinishedCommand rescan over its whole
    // history) must not resurrect old cycles: that one scan is skipped.
    assert!(app.install_build_diags(7, Some(&cwd), Vec::new()));
    assert!(app.problems.groups().is_empty(), "empty batch clears");
    app.watch_published_panes.insert(7);
    assert!(
        !app.apply_build_scan(7, Some(&cwd), "tsc --watch", out),
        "the post-watch rescan is skipped"
    );
    // One skip only: the pane's next ordinary command scans normally.
    assert!(app.apply_build_scan(7, Some(&cwd), "make", "main.c:1:1: error: x\n"));
    assert_eq!(app.problems.groups().len(), 1);

    // Clearing the assignment restores the built-in first-match scan.
    app.assign_task_matcher(7, None);
    assert!(app.apply_build_scan(7, Some(&cwd), "make", out));
    assert_eq!(
        app.problems.groups().len(),
        2,
        "without the task matcher both formats report"
    );
}

/// #430: a task must not be typed into a pane whose shell has walked away
/// from the task's directory.
///
/// Both defects lived in `run_project_task`'s copy of a guard that #392
/// fixed only on the runnable-fence side: `starts_with` accepted any
/// DESCENDANT, so a shell that had `cd`'d into a subdirectory was reused
/// and the task ran there. The two callers now share `pane_is_idle_at`,
/// so a correction cannot land on one and miss the other.
#[test]
fn a_task_pane_whose_shell_left_the_directory_is_not_reused() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("Makefile"), "build:\n\ttrue\n").unwrap();
    std::fs::create_dir_all(tmp.path().join("sub")).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let chord = key(
        KeyCode::Char('B'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    );
    app.handle_key(chord).unwrap();

    // Wait for the pane's shell to reach its prompt, as the reuse path
    // requires an idle pane.
    let root = tmp.path().canonicalize().unwrap();
    crate::test_budget::await_spawned(
        std::time::Duration::from_secs(10),
        "the task pane's shell",
        || {
            let t = &app.terminals[app.active_terminal];
            t.foreground_is_shell() && t.kernel_shell_cwd() == Some(root.clone())
        },
    );
    let idx = app.active_terminal;
    let label = app.terminals[idx].label().to_string();
    // The control: standing in the task's own directory, the pane IS this
    // task's pane.
    assert!(
        pane_is_idle_at(&app.terminals[idx], &label, &root),
        "precondition: an idle pane at the task's own directory is reusable"
    );

    // Now walk the shell into a subdirectory, as a user reading a log or
    // poking at a subcrate would.
    app.terminals[idx].write_input(b"cd sub\r");
    let sub = root.join("sub");
    crate::test_budget::await_spawned(
        std::time::Duration::from_secs(10),
        "the shell to enter the subdirectory",
        || app.terminals[idx].kernel_shell_cwd() == Some(sub.clone()),
    );

    assert!(
        !pane_is_idle_at(&app.terminals[idx], &label, &root),
        "a shell that has left the task's directory is not the task's pane: \
         `starts_with` accepted any descendant and ran the task in `sub`"
    );

    // And the whole path agrees: rerunning opens a fresh pane at the right
    // directory rather than typing into the one that walked away.
    let before = app.terminals.len();
    app.handle_key(chord).unwrap();
    assert_eq!(
        app.terminals.len(),
        before + 1,
        "the task gets a pane standing where it will actually run"
    );
}

/// #250: a config's `postDebugTask` runs when the user stops the session.
#[test]
fn stopping_a_session_runs_its_post_debug_task_once() {
    let (_tmp, mut app) = app_with_cleanup_task();
    app.debug_sessions.push("A", stub_member(false));
    app.debug_post_tasks = vec![String::from("cleanup")];
    app.debug_stop_by_user();
    assert_eq!(last_task_label(&app).as_deref(), Some("cleanup"));
    assert!(app.debug_post_tasks.is_empty(), "it runs once, not again");
}

/// With nothing running there is no session to clean up after: a launch
/// that failed before a session started must not trigger the task.
#[test]
fn stopping_with_no_session_runs_no_post_debug_task() {
    let (_tmp, mut app) = app_with_cleanup_task();
    app.debug_post_tasks = vec![String::from("cleanup")];
    app.debug_stop_by_user();
    assert_eq!(last_task_label(&app), None);
}

/// #250: a session that ends by itself runs the task too.
#[test]
fn a_session_ending_on_its_own_runs_its_post_debug_task() {
    let (_tmp, mut app) = app_with_cleanup_task();
    app.debug_sessions.push("A", stub_member(true));
    app.debug_post_tasks = vec![String::from("cleanup")];
    poll_until_shrinks(&mut app, 1);
    assert!(app.debug_sessions.is_empty(), "the stub terminated");
    assert_eq!(last_task_label(&app).as_deref(), Some("cleanup"));
}

/// #867 guard: a postDebugTask reports after the end, so its message is
/// the one left on the status line, not the end message.
#[test]
fn a_post_debug_task_message_outlives_the_end_message() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.debug_sessions.push("out.py", stub_member(true));
    app.debug_post_tasks = vec![String::from("missing")];
    poll_until_shrinks(&mut app, 1);
    assert!(app.debug_sessions.is_empty(), "the stub terminated");
    assert!(
        app.status
            .starts_with("postDebugTask \"missing\" not found"),
        "{}",
        app.status
    );
}

// ---- Live Run (Cmd+K V) ----

#[test]
fn live_run_refuses_a_file_that_is_not_python() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "notes.txt", "hello\n");
    app.run_command(crate::widgets::command_palette::Command::ToggleLiveRun);
    assert!(app.live_run_files.is_empty(), "nothing is armed");
    assert!(app.status.contains("Python"), "{}", app.status);
}

#[test]
fn live_run_arms_only_the_active_file_and_disarming_clears_its_trailers() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.py", "x = 1\n");
    let path = app.editor.path.clone().unwrap();
    app.run_command(crate::widgets::command_palette::Command::ToggleLiveRun);
    assert_eq!(
        app.live_run_files.iter().collect::<Vec<_>>(),
        vec![&path],
        "exactly the active file is armed"
    );
    app.editor.live_run = Some(crate::live_run::View::default());
    app.run_command(crate::widgets::command_palette::Command::ToggleLiveRun);
    assert!(app.live_run_files.is_empty());
    assert!(
        app.editor.live_run.is_none(),
        "a disarmed file keeps no trailers"
    );
}

/// The whole loop against a real interpreter, when the machine has one:
/// arm, tick until the run lands, and find its values on the tab.
#[test]
fn live_run_lands_a_real_run_on_the_tab() {
    let has_python = std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !has_python {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.py", "n = len('abc')\nprint(n * 2)\n");
    app.run_command(crate::widgets::command_palette::Command::ToggleLiveRun);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while app.editor.live_run.is_none() && std::time::Instant::now() < deadline {
        app.tick_live_run();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let view = app.editor.live_run.as_ref().expect("the run landed");
    assert_eq!(
        view.note(0, "n = len('abc')").map(|n| n[0].0.as_str()),
        Some("n = 3")
    );
    assert_eq!(
        view.note(1, "print(n * 2)").map(|n| n[0].0.as_str()),
        Some("\u{25b8} 6")
    );
    assert!(app.status.starts_with("Live Run: ok"), "{}", app.status);
}

/// #856 (comment): a task set "Running <label>" and kept it after its pane
/// was back at the prompt. The pane's command-finished report now replaces
/// it with the exit code and how long the command took.
#[test]
fn a_finished_task_replaces_its_running_status_with_the_outcome() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let pane = app.run_project_task(long_task("make report")).unwrap();
    assert_eq!(app.status, "Running make report");
    finish_pane_command(&mut app, pane, Some(0), 300);
    assert_eq!(app.status, "make report finished (exit 0, 0.3 s)");

    let pane = app.run_project_task(long_task("make check")).unwrap();
    assert_eq!(app.status, "Running make check");
    finish_pane_command(&mut app, pane, Some(2), 1500);
    assert_eq!(app.status, "make check failed (exit 2, 1.5 s)");
}

/// #856 (comment): a runnable fenced block's "Running <pane>" is settled
/// by its pane's command-finished report, as a task's is.
#[test]
fn a_finished_fenced_block_replaces_its_running_status_with_the_outcome() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.run_block_in_pane(PendingRunBlock {
        pane_name: String::from("README.md:1"),
        cwd: tmp.path().to_path_buf(),
        command: String::from("sleep 600\r"),
        code: String::from("sleep 600\n"),
        destructive: false,
        doc: None,
        block_line: 0,
        persist: false,
        capture_timeout: None,
    });
    assert_eq!(app.status, "Running README.md:1");
    let pane = app
        .terminals
        .iter()
        .find(|t| t.label() == "README.md:1")
        .expect("the block's pane")
        .uid();
    finish_pane_command(&mut app, pane, Some(1), 300);
    assert_eq!(app.status, "README.md:1 failed (exit 1, 0.3 s)");
}

/// #912 (comment): a task typed into a shell that it then ends (`echo
/// boom; exit 3`) kept "Running fail" for good: a shell that exits sends
/// no command-finished mark. Its exit code is now the task's outcome, and
/// the pane stays so the output can be read.
#[test]
fn a_task_that_exits_its_shell_reports_the_exit_code() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let mut task = long_task("fail");
    task.command = String::from("echo out_$((40 + 2)); exit 3");
    let pane = app.run_project_task(task).unwrap();
    assert_eq!(app.status, "Running fail");
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(1500),
        "the task's shell to exit",
        || {
            app.drain_terminal_bells();
            app.status != "Running fail"
        },
    );
    assert!(
        app.status.starts_with("fail failed (exit 3, "),
        "{}",
        app.status
    );
    let t = app
        .terminals
        .iter()
        .find(|t| t.uid() == pane)
        .expect("the task's pane stays");
    assert!(t.has_exited());
    assert!(t.visible_text().contains("out_42"), "the output stays");
}

/// #856 guard: a task's finish settles only a status that still shows its
/// run. Something newer in the bar (a save) stays.
#[test]
fn a_task_finishing_after_a_newer_status_leaves_that_status() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let pane = app.run_project_task(long_task("make report")).unwrap();
    app.status = String::from("Saved Makefile");
    finish_pane_command(&mut app, pane, Some(0), 300);
    assert_eq!(app.status, "Saved Makefile");
}

/// #856 guard: each task's finish settles its own status only. With task
/// `a` started after `b`, the bar shows `a`'s run; `b` finishing first
/// leaves it, and `a` finishing then reports `a`.
#[test]
fn a_second_task_finishing_leaves_the_first_tasks_running_status() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let b = app.run_project_task(long_task("b")).unwrap();
    let a = app.run_project_task(long_task("a")).unwrap();
    assert_ne!(a, b, "two tasks, two panes");
    assert_eq!(app.status, "Running a");
    finish_pane_command(&mut app, b, Some(0), 300);
    assert_eq!(app.status, "Running a", "b's end is not a's");
    finish_pane_command(&mut app, a, Some(0), 300);
    assert_eq!(app.status, "a finished (exit 0, 0.3 s)");
}

/// #856 guard: a task's outcome stands in for the long-command notice of a
/// pane the user left only when the outcome is shown. With a newer status
/// in the bar, the long task's end still gets the notice.
#[test]
fn a_long_task_reports_its_outcome_or_else_the_long_command_notice() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let pane = app.run_project_task(long_task("make report")).unwrap();
    app.focus_pane(Pane::Editor);
    finish_pane_command(&mut app, pane, Some(0), 12_000);
    assert_eq!(app.status, "make report finished (exit 0, 12.0 s)");

    let pane = app.run_project_task(long_task("make check")).unwrap();
    app.focus_pane(Pane::Editor);
    app.status = String::from("Saved Makefile");
    finish_pane_command(&mut app, pane, Some(0), 12_000);
    assert_eq!(
        app.status,
        "Command in Task: make check finished: exit 0 in 12.0s"
    );
}

/// A root tsconfig that names no files and no references compiles
/// nothing, so the check says that rather than "no problems".
#[cfg(unix)]
#[test]
fn a_tsconfig_that_includes_nothing_is_never_reported_clean() {
    let tmp = fake_tsc_workspace(r#"{ "files": [] }"#);
    let (status, calls) = run_project_check(&tmp);
    assert_eq!(
        status,
        "Whole-project check: tsconfig.json includes no files"
    );
    assert_eq!(calls, "", "nothing is run");
}

/// Negative: a plain tsconfig is still checked by one `tsc --noEmit`, and
/// `"files": []` with an `include`, or with an `extends` base that may
/// supply one, is a plain tsconfig.
#[cfg(unix)]
#[test]
fn a_plain_tsconfig_still_gets_one_no_emit_run() {
    for tsconfig in [
        r#"{ "compilerOptions": { "strict": true } }"#,
        r#"{ "files": [], "include": ["src"] }"#,
        r#"{ "extends": "./tsconfig.base.json", "files": [] }"#,
    ] {
        let tmp = fake_tsc_workspace(tsconfig);
        let (status, calls) = run_project_check(&tmp);
        assert_eq!(calls, "--noEmit\n", "{tsconfig}");
        assert_eq!(status, "Whole-project check: no problems", "{tsconfig}");
    }
}
