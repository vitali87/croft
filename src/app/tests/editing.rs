//! Editing a buffer: typing, the caret, selections, the clipboard, undo and
//! line operations.

use super::*;

/// #1161: on an `.xml` file both comment commands from the palette comment
/// the caret's line instead of saying the language has none.
#[test]
fn the_comment_commands_comment_an_xml_file() {
    use crate::widgets::command_palette::Command as Cmd;
    let tmp = tempfile::tempdir().unwrap();
    for name in ["doc.xml", "schema.xsd", "Info.plist"] {
        let f = tmp.path().join(name);
        std::fs::write(&f, "<root>\n  <item id=\"1\">hello</item>\n</root>\n").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.editor.open_pinned(&f).unwrap();
        app.editor.cursor_row = 1;
        for cmd in [Cmd::ToggleBlockComment, Cmd::ToggleLineComment] {
            app.run_command(cmd);
            assert_eq!(
                app.editor.lines[1], "  <!-- <item id=\"1\">hello</item> -->",
                "{name} {cmd:?}"
            );
            assert!(!app.status.contains("no "), "{name}: {}", app.status);
            app.editor.cursor_row = 1;
            app.run_command(cmd);
            assert_eq!(
                app.editor.lines[1], "  <item id=\"1\">hello</item>",
                "{name}"
            );
            app.editor.cursor_row = 1;
        }
    }
    // Negative: a file with no comment at all says so.
    let f = tmp.path().join("data.unknownext");
    std::fs::write(&f, "{}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&f).unwrap();
    app.run_command(Cmd::ToggleLineComment);
    assert_eq!(app.editor.lines[0], "{}");
    assert!(app.status.contains("no comment"), "{}", app.status);
}

#[test]
fn ctrl_c_is_editor_copy_key() {
    assert!(is_editor_copy_key(key(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL
    )));
}

#[test]
fn cmd_c_is_editor_copy_key() {
    assert!(is_editor_copy_key(key(
        KeyCode::Char('c'),
        KeyModifiers::SUPER
    )));
}

#[test]
fn plain_c_is_not_editor_copy_key() {
    assert!(!is_editor_copy_key(key(
        KeyCode::Char('c'),
        KeyModifiers::NONE
    )));
}

#[test]
fn ctrl_x_is_editor_cut_key() {
    assert!(is_editor_cut_key(key(
        KeyCode::Char('x'),
        KeyModifiers::CONTROL
    )));
    assert!(is_editor_cut_key(key(
        KeyCode::Char('x'),
        KeyModifiers::SUPER
    )));
    assert!(!is_editor_cut_key(key(
        KeyCode::Char('x'),
        KeyModifiers::NONE
    )));
}

#[test]
fn ctrl_a_is_editor_select_all_key() {
    assert!(is_editor_select_all_key(key(
        KeyCode::Char('a'),
        KeyModifiers::CONTROL
    )));
    assert!(is_editor_select_all_key(key(
        KeyCode::Char('a'),
        KeyModifiers::SUPER
    )));
    assert!(!is_editor_select_all_key(key(
        KeyCode::Char('a'),
        KeyModifiers::NONE
    )));
}

#[test]
fn ctrl_z_is_editor_undo_key() {
    assert!(is_editor_undo_key(key(
        KeyCode::Char('z'),
        KeyModifiers::CONTROL
    )));
    assert!(is_editor_undo_key(key(
        KeyCode::Char('z'),
        KeyModifiers::SUPER
    )));
}

#[test]
fn plain_z_is_not_editor_undo_key() {
    assert!(!is_editor_undo_key(key(
        KeyCode::Char('z'),
        KeyModifiers::NONE
    )));
}

#[test]
fn shift_cmd_z_reserved_for_redo_not_undo() {
    let mods = KeyModifiers::SUPER | KeyModifiers::SHIFT;
    assert!(!is_editor_undo_key(key(KeyCode::Char('z'), mods)));
    // The shifted chord is redo (both Cmd and Ctrl surfaces), and the plain
    // command-modifier Z stays undo, not redo.
    assert!(is_editor_redo_key(key(KeyCode::Char('z'), mods)));
    assert!(is_editor_redo_key(key(
        KeyCode::Char('z'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT
    )));
    assert!(!is_editor_redo_key(key(
        KeyCode::Char('z'),
        KeyModifiers::SUPER
    )));
    assert!(!is_editor_redo_key(key(
        KeyCode::Char('z'),
        KeyModifiers::CONTROL
    )));
}

/// Cmd+C from the croft terminal pane must land the selection on the
/// real macOS clipboard. This is the user-visible regression test
/// for the long-standing "I copied from croft and pbpaste shows the
/// previous value" bug, where the old OSC 52 path was silently
/// dropped by the host terminal. The contract: writing through
/// `copy_to_clipboard` puts the text on the system pasteboard such
/// that `clipboard::read_string` (which is what Cmd+V uses) returns
/// the same bytes. If this ever fails again, copy/paste between
/// croft and other apps is broken and the test must catch it before
/// the binary ships.
#[cfg(target_os = "macos")]
#[test]
fn copy_to_clipboard_writes_text_to_the_macos_system_pasteboard() {
    let _g = crate::clipboard::lock_clipboard_for_test();
    let sentinel = format!(
        "croft-copy-helper-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let used_native = copy_to_clipboard(&sentinel);
    assert!(
        used_native,
        "on macOS, copy_to_clipboard must succeed via NSPasteboard and return true so the call site can skip the OSC 52 fallback (and the host terminal's quirks can't silently drop the write)"
    );
    let got = crate::clipboard::read_string().expect("read_string must see what we just wrote");
    assert_eq!(
        got, sentinel,
        "round-trip through the system pasteboard must be byte-exact - if this ever drifts, every Cmd+C from croft is silently lossy"
    );
}

// Exercises the macOS-native NSPasteboard backend (`clipboard::macos`), which
// only exists on macOS — gate it like the sibling native tests in
// `clipboard.rs` so the Linux/Android test binaries still compile and run
// everything else (an ungated macOS-only test fails to *compile* off-macOS,
// poisoning the whole test target rather than just skipping).
#[cfg(target_os = "macos")]
#[test]
fn lock_clipboard_for_test_restores_prior_clipboard_text_on_drop() {
    // Tests that hit the real NSPasteboard must never leave their
    // sentinels behind. This test simulates a user already having
    // text on the clipboard, runs a guarded section that writes a
    // sentinel, drops the guard, and asserts the clipboard now
    // reads back the simulated "user" text rather than the test
    // sentinel.
    //
    // To avoid leaking the simulated text into the developer's real
    // clipboard, capture the genuine pre-test contents first and
    // restore them at the very end via a Drop guard so even a panic
    // in the middle leaves the host clipboard untouched.
    struct RestoreClip(Option<String>);
    impl Drop for RestoreClip {
        fn drop(&mut self) {
            match self.0.take() {
                Some(s) => {
                    crate::clipboard::macos::write_string_native(&s);
                }
                None => crate::clipboard::macos::clear_native(),
            }
        }
    }
    let _restore = RestoreClip(crate::clipboard::macos::read_string_native());

    let pre = format!("user-preexisting-clip-{}", std::process::id());
    crate::clipboard::macos::write_string_native(&pre);
    {
        let _g = crate::clipboard::lock_clipboard_for_test();
        let sentinel = "test-sentinel-should-not-leak";
        assert!(
            crate::clipboard::write_string(sentinel),
            "guard-held write must hit the real NSPasteboard"
        );
        assert_eq!(
            crate::clipboard::read_string().as_deref(),
            Some(sentinel),
            "while the guard is held the test reads what it just wrote"
        );
    }
    let after = crate::clipboard::macos::read_string_native();
    assert_eq!(
        after.as_deref(),
        Some(pre.as_str()),
        "dropping the guard MUST restore the pre-test clipboard so a cargo test run never leaves a test sentinel on the developer's system clipboard for the next paste to surface"
    );
}

#[test]
fn cmd_backspace_is_recognized_as_delete_node() {
    // macOS Finder convention: ⌘⌫ moves the selection to the Trash.
    assert!(is_delete_node_key(key(
        KeyCode::Backspace,
        KeyModifiers::SUPER
    )));
}

#[test]
fn plain_backspace_is_delete_node_on_mac_layouts() {
    // On Mac keyboards the key labeled "delete" reports as Backspace; in
    // the tree pane (the only context this helper is called from) it must
    // trigger deletion to match the user's expectation.
    assert!(is_delete_node_key(key(
        KeyCode::Backspace,
        KeyModifiers::NONE
    )));
}

/// #1563: Shift+Down from line 1 stopped at the start of the last line, so
/// the selection, and Run Selected Text on it, dropped the script's `done`.
/// Down on the last line now goes to its end, as in VS Code, so Shift+Down
/// selects through it.
#[test]
fn shift_down_selects_through_the_end_of_the_last_line() {
    let (_tmp, mut app) = app_with_run_script();
    let last = app.editor.lines.len() - 1;
    assert_eq!(app.editor.lines[last], "done");
    for _ in 0..6 {
        app.handle_key(key(KeyCode::Down, KeyModifiers::SHIFT))
            .unwrap();
    }
    assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (last, 4));
    assert_eq!(
        app.editor.selection_text(),
        "echo one\necho two\nfor i in 1 2; do\n  echo loop $i\ndone"
    );
}

/// #1563: Down on the last line goes to its end; Up on the first line goes
/// to its start, and Shift+Up selects back to the start of the file.
#[test]
fn down_on_the_last_line_and_up_on_the_first_reach_the_line_ends() {
    let (_tmp, mut app) = app_with_run_script();
    let last = app.editor.lines.len() - 1;
    app.editor.cursor_row = last;
    app.editor.cursor_col = 1;
    app.handle_key(key(KeyCode::Down, KeyModifiers::NONE))
        .unwrap();
    assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (last, 4));

    app.editor.cursor_row = 0;
    app.editor.cursor_col = 8;
    app.handle_key(key(KeyCode::Up, KeyModifiers::NONE))
        .unwrap();
    assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (0, 0));

    app.editor.cursor_row = 1;
    app.editor.cursor_col = 4;
    for _ in 0..3 {
        app.handle_key(key(KeyCode::Up, KeyModifiers::SHIFT))
            .unwrap();
    }
    assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (0, 0));
    assert_eq!(app.editor.selection_text(), "echo one\necho");
}

/// Negative: Down between lines keeps the caret's column rather than
/// jumping to the line end; only the last line has the new edge.
#[test]
fn down_between_lines_keeps_the_column() {
    let (_tmp, mut app) = app_with_run_script();
    app.editor.cursor_col = 5;
    app.handle_key(key(KeyCode::Down, KeyModifiers::NONE))
        .unwrap();
    assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (1, 5));
    app.handle_key(key(KeyCode::Up, KeyModifiers::NONE))
        .unwrap();
    assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (0, 5));
}

/// Guard (#843): `Ctrl`+`A` in the editor is still line start, never select
/// all: the reason LINUX.md lists Cmd+A as having no `Ctrl` form there.
#[test]
fn ctrl_a_in_the_editor_is_line_start_not_select_all() {
    let mut app = editor_app_with_lines(&["hello world", "next"]);
    app.editor.cursor_col = 5;
    app.handle_key(key(KeyCode::Char('a'), KeyModifiers::CONTROL))
        .unwrap();
    assert_eq!(app.editor.cursor_col, 0);
    assert!(app.editor.selection.is_none());
}

/// Guard (#843): `Ctrl`+`Enter` takes only what Cmd+Enter does. In a buffer
/// that is not Markdown it still breaks the line; in a fence `Ctrl`+`Shift`+
/// `Enter` still opens a line above, and neither `Ctrl`+`Alt`+`Enter` nor a
/// bare `Enter` (what a legacy terminal sends for `Ctrl`+`Enter`) runs it.
#[test]
fn ctrl_enter_leaves_other_buffers_and_other_enters_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "notes.txt", "abcdef");
    app.editor.cursor_col = 3;
    app.handle_key(key(KeyCode::Enter, KeyModifiers::CONTROL))
        .unwrap();
    assert!(app.pending_run_block.is_none());
    // Outside Markdown it is Insert Line Below (#1571); on macOS `Ctrl`
    // stays the terminal's, so there it is a plain line break.
    let want: &[&str] = if cfg!(target_os = "macos") {
        &["abc", "def"]
    } else {
        &["abcdef", ""]
    };
    assert_eq!(app.editor.lines, want, "outside Markdown: no run");

    for mods in [
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        KeyModifiers::CONTROL | KeyModifiers::ALT,
        KeyModifiers::NONE,
    ] {
        let mut app = markdown_fence_app(tmp.path());
        let rows = app.editor.lines.len();
        app.handle_key(key(KeyCode::Enter, mods)).unwrap();
        assert!(
            app.pending_run_block.is_none(),
            "{mods:?}+Enter ran the fence"
        );
        if mods != KeyModifiers::CONTROL | KeyModifiers::ALT {
            assert_eq!(
                app.editor.lines.len(),
                rows + 1,
                "{mods:?}+Enter adds a line"
            );
        }
    }
}

/// #1571: `Ctrl`+`Enter` (`Cmd`+`Enter` on macOS) is VS Code's Insert Line
/// Below: the caret mid-line opens an indented line under it and leaves the
/// current line whole. It used to split the line like `Enter`.
#[test]
fn cmd_enter_inserts_a_line_below_without_splitting_the_line() {
    let chords = if cfg!(target_os = "macos") {
        vec![KeyModifiers::SUPER]
    } else {
        vec![KeyModifiers::CONTROL, KeyModifiers::SUPER]
    };
    for mods in chords {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_open_file(tmp.path(), "i.py", "total = price * qty\nprint(total)\n");
        app.editor.cursor_row = 0;
        app.editor.cursor_col = 8;
        app.handle_key(key(KeyCode::Enter, mods)).unwrap();
        assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (1, 0));
        app.handle_key(key(KeyCode::Char('X'), KeyModifiers::SHIFT))
            .unwrap();
        assert_eq!(
            app.editor.lines,
            ["total = price * qty", "X", "print(total)"],
            "{mods:?}+Enter"
        );
        assert_eq!(app.status, "Opened line below");
    }
}

/// #1571: the new line takes the current line's indent, as VS Code's does.
#[test]
fn cmd_enter_keeps_the_indent_of_the_current_line() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "i.py", "def f():\n    x = 1\n");
    app.editor.cursor_row = 1;
    app.editor.cursor_col = 5;
    app.handle_key(key(KeyCode::Enter, KeyModifiers::SUPER))
        .unwrap();
    assert_eq!(app.editor.lines, ["def f():", "    x = 1", "    "]);
    assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (2, 4));
}

/// #1571: in Markdown prose, outside every runnable fence, `Cmd`+`Enter`
/// is Insert Line Below; only a runnable fence under the caret claims it.
#[test]
fn cmd_enter_in_markdown_prose_inserts_a_line_below() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = markdown_fence_app(tmp.path());
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 2;
    app.handle_key(key(KeyCode::Enter, KeyModifiers::SUPER))
        .unwrap();
    assert!(app.pending_run_block.is_none());
    assert_eq!(app.status, "Opened line below");
    assert_eq!(app.editor.lines[..3], ["# Run", "", ""]);
    assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (1, 0));
}

/// Guard (#843): only the bare `Ctrl`+`K` leads. `Ctrl`+`Shift`+`K` is still
/// Delete Line, and `Ctrl`+`Alt`+`K` arms nothing.
#[test]
fn ctrl_shift_k_still_deletes_the_line_and_ctrl_alt_k_never_leads() {
    let mut app = editor_app_with_lines(&["one", "two"]);
    app.handle_key(key(
        KeyCode::Char('K'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert!(app.cmd_k_leader.is_none());
    assert_eq!(app.editor.lines, ["two"]);
    app.handle_key(key(
        KeyCode::Char('k'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    ))
    .unwrap();
    assert!(app.cmd_k_leader.is_none());
}

/// Guard (#843): the palette's "Kill to End of Line" at the end of a line
/// kills nothing and never joins the next line up, as `Ctrl`+`K` did not.
#[test]
fn kill_to_end_of_line_at_the_end_of_a_line_changes_nothing() {
    use crate::widgets::command_palette::Command;
    let mut app = editor_app_with_lines(&["hello", "next"]);
    app.editor.cursor_col = 5;
    app.status.clear();
    app.run_command(Command::KillToEndOfLine);
    assert_eq!(app.editor.lines, ["hello", "next"]);
    assert!(app.status.is_empty(), "{}", app.status);
}

#[test]
fn editor_shift_alt_down_duplicates_the_current_line_via_handle_key() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.lines = vec![String::from("alpha"), String::from("beta")];
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 3;
    app.handle_key(key(KeyCode::Down, KeyModifiers::ALT | KeyModifiers::SHIFT))
        .unwrap();
    assert_eq!(app.editor.lines, vec!["alpha", "alpha", "beta"]);
    assert_eq!(
        (app.editor.cursor_row, app.editor.cursor_col),
        (1, 3),
        "cursor must follow onto the duplicated line"
    );
}

#[test]
fn editor_shift_alt_up_duplicates_the_current_line_via_handle_key() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.lines = vec![String::from("alpha"), String::from("beta")];
    app.editor.cursor_row = 1;
    app.editor.cursor_col = 2;
    app.handle_key(key(KeyCode::Up, KeyModifiers::ALT | KeyModifiers::SHIFT))
        .unwrap();
    assert_eq!(app.editor.lines, vec!["alpha", "beta", "beta"]);
    assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (1, 2));
}

#[test]
fn editor_alt_right_jumps_one_word_forward() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.lines = vec![String::from("hello world rest")];
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 0;
    app.handle_key(key(KeyCode::Right, KeyModifiers::ALT))
        .unwrap();
    assert_eq!(app.editor.cursor_col, 5);
    app.handle_key(key(KeyCode::Right, KeyModifiers::ALT))
        .unwrap();
    assert_eq!(app.editor.cursor_col, 11);
}

#[test]
fn editor_cmd_gg_jumps_to_top_of_file() {
    let mut app = editor_app_with_lines(&["a", "b", "c", "d"]);
    app.editor.cursor_row = 3;
    app.handle_key(key(KeyCode::Char('g'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('g'), KeyModifiers::SUPER))
        .unwrap();
    assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (0, 0));
}

#[test]
fn editor_cmd_g_then_plain_g_also_jumps_to_top() {
    let mut app = editor_app_with_lines(&["a", "b", "c", "d"]);
    app.editor.cursor_row = 3;
    app.handle_key(key(KeyCode::Char('g'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('g'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (0, 0));
}

#[test]
fn editor_cmd_g_count_g_jumps_to_specific_line() {
    let mut app = editor_app_with_lines(&["a", "b", "c", "d", "e"]);
    app.handle_key(key(KeyCode::Char('g'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('3'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Char('g'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (2, 0));
}

#[test]
fn editor_cmd_g_multidigit_count_g_jumps_to_specific_line() {
    let mut app = editor_app_with_lines(&(0..30).map(|_| "x").collect::<Vec<_>>());
    app.handle_key(key(KeyCode::Char('g'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('1'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Char('2'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Char('g'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.editor.cursor_row, 11);
}

#[test]
fn editor_cmd_shift_g_jumps_to_bottom_of_file() {
    let mut app = editor_app_with_lines(&["a", "b", "c", "d"]);
    app.handle_key(key(
        KeyCode::Char('G'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert_eq!(app.editor.cursor_row, 3);
    assert_ne!(
        app.sidebar_view,
        SidebarView::SourceControl,
        "cmd+shift+g in editor pane must NOT hijack to source control"
    );
}

#[test]
fn editor_cmd_d_now_selects_the_word_instead_of_deleting() {
    // Cmd+D moved from the old `Cmd+d d` delete-line convenience to VS Code's
    // "Add Selection to Next Find Match"; delete-line now lives on Cmd+Shift+K.
    let mut app = editor_app_with_lines(&["alpha", "beta", "gamma"]);
    app.editor.cursor_row = 1;
    app.handle_key(key(KeyCode::Char('d'), KeyModifiers::SUPER))
        .unwrap();
    assert_eq!(
        app.editor.lines,
        vec!["alpha", "beta", "gamma"],
        "Cmd+D no longer deletes the line"
    );
    assert!(
        app.editor.selection.is_some(),
        "Cmd+D selects the word under the cursor"
    );
}

#[test]
fn editor_cmd_yy_does_not_modify_buffer() {
    let mut app = editor_app_with_lines(&["alpha", "beta", "gamma"]);
    app.editor.cursor_row = 1;
    app.handle_key(key(KeyCode::Char('y'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('y'), KeyModifiers::SUPER))
        .unwrap();
    assert_eq!(app.editor.lines, vec!["alpha", "beta", "gamma"]);
    assert!(app.status.starts_with("Yanked"));
}

#[test]
fn editor_ctrl_a_jumps_to_start_of_line() {
    let mut app = editor_app_with_lines(&["hello world"]);
    app.editor.cursor_col = 7;
    app.handle_key(key(KeyCode::Char('a'), KeyModifiers::CONTROL))
        .unwrap();
    assert_eq!(app.editor.cursor_col, 0);
}

#[test]
fn editor_ctrl_e_jumps_to_end_of_line() {
    let mut app = editor_app_with_lines(&["hello world"]);
    app.editor.cursor_col = 3;
    app.handle_key(key(KeyCode::Char('e'), KeyModifiers::CONTROL))
        .unwrap();
    assert_eq!(app.editor.cursor_col, 11);
}

/// `Ctrl+K` kills where it is not the `Cmd+K` leader: on macOS, and in vim
/// mode off it (#843).
#[test]
fn editor_ctrl_k_kills_to_end_of_line() {
    let mut app = editor_app_with_lines(&["hello world", "next"]);
    app.vim.enabled = cfg!(not(target_os = "macos"));
    app.editor.cursor_col = 5;
    app.handle_key(key(KeyCode::Char('k'), KeyModifiers::CONTROL))
        .unwrap();
    assert_eq!(app.editor.lines, vec!["hello", "next"]);
}

#[test]
fn editor_cmd_count_gg_jumps_to_line_with_leading_count() {
    let mut app = editor_app_with_lines(&["a", "b", "c", "d", "e"]);
    app.handle_key(key(KeyCode::Char('3'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('g'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('g'), KeyModifiers::SUPER))
        .unwrap();
    assert_eq!(app.editor.cursor_row, 2);
}

#[test]
fn editor_cmd_count_yy_yanks_n_lines_with_leading_count() {
    let mut app = editor_app_with_lines(&["a", "b", "c", "d"]);
    app.editor.cursor_row = 0;
    app.handle_key(key(KeyCode::Char('2'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('y'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(KeyCode::Char('y'), KeyModifiers::SUPER))
        .unwrap();
    assert_eq!(app.editor.lines, vec!["a", "b", "c", "d"]);
    assert!(app.status.contains("Yanked 2"));
}

#[test]
fn editor_cmd_count_then_shift_g_jumps_to_specific_line() {
    let mut app = editor_app_with_lines(&["a", "b", "c", "d", "e"]);
    app.handle_key(key(KeyCode::Char('4'), KeyModifiers::SUPER))
        .unwrap();
    app.handle_key(key(
        KeyCode::Char('G'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert_eq!(app.editor.cursor_row, 3);
}

#[test]
fn editor_ctrl_u_kills_to_start_of_line() {
    let mut app = editor_app_with_lines(&["hello world"]);
    app.editor.cursor_col = 6;
    app.handle_key(key(KeyCode::Char('u'), KeyModifiers::CONTROL))
        .unwrap();
    assert_eq!(app.editor.lines, vec!["world"]);
    assert_eq!(app.editor.cursor_col, 0);
}

#[test]
fn editor_cmd_o_opens_line_below_with_indent() {
    let mut app = editor_app_with_lines(&["    foo", "bar"]);
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 4;
    app.handle_key(key(KeyCode::Char('o'), KeyModifiers::SUPER))
        .unwrap();
    assert_eq!(app.editor.lines, vec!["    foo", "    ", "bar"]);
    assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (1, 4));
}

#[test]
fn editor_cmd_shift_enter_opens_line_above_with_indent() {
    // Open-line-above moved off Cmd+Shift+O (now Go to Symbol) onto its real
    // VS Code chord, Cmd+Shift+Enter.
    let mut app = editor_app_with_lines(&["foo", "    bar"]);
    app.editor.cursor_row = 1;
    app.editor.cursor_col = 6;
    app.handle_key(key(
        KeyCode::Enter,
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert_eq!(app.editor.lines, vec!["foo", "    ", "    bar"]);
    assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (1, 4));
}

#[test]
fn editor_cmd_a_still_selects_all() {
    let mut app = editor_app_with_lines(&["hello", "world"]);
    app.handle_key(key(KeyCode::Char('a'), KeyModifiers::SUPER))
        .unwrap();
    assert_eq!(app.editor.selection_text(), "hello\nworld");
}

#[test]
fn editor_alt_left_jumps_one_word_backward() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.lines = vec![String::from("hello world rest")];
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 16;
    app.handle_key(key(KeyCode::Left, KeyModifiers::ALT))
        .unwrap();
    assert_eq!(app.editor.cursor_col, 12);
    app.handle_key(key(KeyCode::Left, KeyModifiers::ALT))
        .unwrap();
    assert_eq!(app.editor.cursor_col, 6);
}

#[test]
fn editor_shift_alt_right_runs_expand_selection_not_word_extension() {
    // #254 reassigned Shift+Alt+Right from word-wise extension to VS
    // Code's Expand Selection (the same collision VS Code resolves the
    // same way). With no path/language, the textual fallback grows to
    // the line span immediately — no LSP round trip.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.lines = vec![String::from("hello world")];
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 0;
    app.handle_key(key(KeyCode::Right, KeyModifiers::ALT | KeyModifiers::SHIFT))
        .unwrap();
    assert_eq!(
        app.editor.selection_text(),
        "hello world",
        "Shift+Option+Right expands to the enclosing range (the line here)"
    );
    // Shift+Alt+Left retraces to the gesture's start.
    app.handle_key(key(KeyCode::Left, KeyModifiers::ALT | KeyModifiers::SHIFT))
        .unwrap();
    assert!(
        app.editor.selection.is_none(),
        "shrink retraced to the caret"
    );
}

#[test]
fn cmd_v_is_recognised_as_editor_paste_key() {
    assert!(is_editor_paste_key(key(
        KeyCode::Char('v'),
        KeyModifiers::SUPER
    )));
    assert!(is_editor_paste_key(key(
        KeyCode::Char('v'),
        KeyModifiers::CONTROL
    )));
    assert!(is_editor_paste_key(key(
        KeyCode::Char('\u{16}'),
        KeyModifiers::NONE
    )));
    assert!(!is_editor_paste_key(key(
        KeyCode::Char('v'),
        KeyModifiers::NONE
    )));
    assert!(!is_editor_paste_key(key(
        KeyCode::Char('a'),
        KeyModifiers::SUPER
    )));
}

#[test]
fn typing_after_select_all_replaces_query() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Search);
    app.search.query = String::from("alpha");
    app.handle_search_key(key(KeyCode::Char('a'), KeyModifiers::SUPER));
    app.handle_search_key(key(KeyCode::Char('z'), KeyModifiers::NONE));
    assert_eq!(app.search.query, "z");
}

#[test]
fn scratch_buffer_saves_under_the_workspace_root_with_a_safe_name() {
    // A scratch buffer (MCP output / Git Output) must anchor under the open
    // project, not write a bare filename to croft's cwd, and must sanitise path
    // separators in the title so the name is a valid single file.
    let tmp = tempfile::tempdir().unwrap();
    let app = App::new(tmp.path().to_path_buf()).unwrap();
    let p = app.scratch_buffer_path("Web: a/b.md");
    assert_eq!(p, tmp.path().join("Web: a-b.md"));
}

#[test]
fn delete_line_deletes_every_carets_line() {
    // Cmd+D (Add Selection to Next Match) and Cmd+Shift+K (Delete Line) landed
    // in the same commit, but Delete Line only ever removed the primary
    // cursor's line: `delete_lines` calls `clear_selection`, which clears the
    // selection and leaves `carets` untouched. VS Code's deleteLines removes
    // every cursor's line, so the two bindings must compose.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let file = tmp.path().join("a.txt");
    std::fs::write(&file, "foo\nbar\nfoo\nbaz\n").unwrap();
    app.editor.open(&file).unwrap();
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 0;

    // Cmd+D twice: the first selects the word under the cursor, the second
    // adds the next match, demoting row 0 to a secondary caret.
    app.start_select_next_occurrence();
    app.start_select_next_occurrence();
    assert_eq!(app.editor.carets.len(), 1, "row 0 is now a secondary caret");
    assert_eq!(
        app.editor.cursor_row, 2,
        "the primary moved to the next match"
    );

    app.delete_current_line();

    assert_eq!(
        app.editor.lines,
        vec![String::from("bar"), String::from("baz")],
        "both matched lines go, not just the primary's"
    );
    assert!(
        app.editor.carets.is_empty(),
        "no stale caret is left painted on a line that no longer exists"
    );
}

#[test]
fn relative_clipboard_text_is_relative_inside_root_and_absolute_outside() {
    use std::path::Path;
    // Inside the workspace root: drop the root prefix.
    assert_eq!(
        relative_clipboard_text(Path::new("/ws/src/a.rs"), Path::new("/ws")),
        "src/a.rs"
    );
    // Outside the root: fall back to the absolute path, never a `../` walk.
    assert_eq!(
        relative_clipboard_text(Path::new("/other/a.rs"), Path::new("/ws")),
        "/other/a.rs"
    );
    // The root itself relativizes to empty.
    assert_eq!(
        relative_clipboard_text(Path::new("/ws"), Path::new("/ws")),
        ""
    );
}

/// Negative: with the editor focused, Ctrl+Z is still undo and never the
/// jump.
#[test]
fn ctrl_z_in_the_editor_still_undoes_and_opens_no_jump() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("a.txt");
    std::fs::write(&file, "one\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.open_at(&file, 0, 0).unwrap();
    app.focus = Pane::Editor;
    app.editor.cursor_col = 3;
    app.editor.insert_char('!');
    assert_eq!(app.editor.lines[0], "one!");
    app.handle_key(key(KeyCode::Char('z'), KeyModifiers::CONTROL))
        .unwrap();
    assert!(app.zoxide_jump.is_none());
    assert_eq!(app.editor.lines[0], "one", "Ctrl+Z undid the edit");
}

#[test]
fn shift_tab_in_editor_dedents_the_current_python_line() {
    let tmp = tempfile::tempdir().unwrap();
    // Two 4-space levels so indent detection (#211) reads the file's step
    // as 4; the old single 8-space jump now detects (correctly) as one
    // 8-wide level.
    let mut app = app_with_open_file(
        tmp.path(),
        "snippet.py",
        "def f():\n    if x:\n        pass\n",
    );
    app.editor.cursor_row = 2;
    app.editor.cursor_col = 8;
    // Shift+Tab reaches the editor as BackTab.
    app.handle_editor_key(key(KeyCode::BackTab, KeyModifiers::SHIFT));
    assert_eq!(
        app.editor.lines[2], "    pass",
        "Shift+Tab must outdent the line one level"
    );
    assert_eq!(app.editor.cursor_col, 4);
}

#[test]
fn tab_in_editor_indents_a_multiline_python_selection() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "block.py", "a = 1\nb = 2\n");
    app.editor.cursor_row = 1;
    app.editor.cursor_col = 5;
    app.editor.selection = Some(crate::widgets::editor::EditorSelection {
        anchor: (0, 0),
        head: (1, 5),
    });
    app.handle_editor_key(key(KeyCode::Tab, KeyModifiers::NONE));
    assert_eq!(app.editor.lines[0], "    a = 1");
    assert_eq!(app.editor.lines[1], "    b = 2");
}

#[test]
fn open_line_above_relocated_from_cmd_shift_o_to_cmd_shift_enter() {
    // The VS Code-correct chord: Cmd/Ctrl+Shift+Enter inserts a line above.
    assert!(is_editor_open_line_above_key(key(
        KeyCode::Enter,
        KeyModifiers::SUPER | KeyModifiers::SHIFT
    )));
    assert!(is_editor_open_line_above_key(key(
        KeyCode::Enter,
        KeyModifiers::CONTROL | KeyModifiers::SHIFT
    )));
    // Cmd+Shift+O no longer triggers it (it now opens Go to Symbol).
    assert!(!is_editor_open_line_above_key(key(
        KeyCode::Char('o'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT
    )));
}

/// #1181: Ctrl+/ in a file with no comment syntax says so, as the palette
/// command does, instead of doing nothing silently; in a file croft has no
/// grammar for but knows the comment of, it comments.
#[test]
fn ctrl_slash_reports_a_file_with_no_comment_and_comments_one_by_extension() {
    let tmp = tempfile::tempdir().unwrap();
    let plain = tmp.path().join("notes.unknownext");
    std::fs::write(&plain, "hello\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&plain).unwrap();
    app.focus_pane(Pane::Editor);
    app.handle_key(key(KeyCode::Char('/'), KeyModifiers::CONTROL))
        .unwrap();
    assert_eq!(app.editor.lines[0], "hello");
    assert_eq!(
        app.status,
        "Toggle Line Comment: no comment for this language"
    );
    let kt = tmp.path().join("App.kt");
    std::fs::write(&kt, "class App\n").unwrap();
    app.editor.open_pinned(&kt).unwrap();
    app.status.clear();
    app.handle_key(key(KeyCode::Char('/'), KeyModifiers::CONTROL))
        .unwrap();
    assert_eq!(app.editor.lines[0], "// class App");
    assert!(app.status.is_empty(), "{}", app.status);
}

/// #1181 negative: Ctrl+/ on a blank line of a language that has a comment
/// does nothing, without claiming the language has no comment.
#[test]
fn ctrl_slash_on_a_blank_line_does_not_report_no_comment() {
    let tmp = tempfile::tempdir().unwrap();
    for (name, text) in [("main.rs", "   \n"), ("index.html", "\n")] {
        let path = tmp.path().join(name);
        std::fs::write(&path, text).unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.editor.open_pinned(&path).unwrap();
        app.focus_pane(Pane::Editor);
        app.status.clear();
        app.handle_key(key(KeyCode::Char('/'), KeyModifiers::CONTROL))
            .unwrap();
        assert_eq!(app.editor.lines[0], text.trim_end_matches('\n'), "{name}");
        assert!(app.status.is_empty(), "{name}: {}", app.status);
    }
}

#[test]
fn running_move_line_down_command_reorders_buffer() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.lines = vec!["a".to_string(), "b".to_string(), "c".to_string()];
    app.editor.cursor_row = 0;
    app.run_command(crate::widgets::command_palette::Command::MoveLineDown);
    assert_eq!(app.editor.lines, vec!["b", "a", "c"]);
}

#[test]
fn cmd_d_selects_word_then_adds_the_next_match_as_a_caret() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("a.rs");
    std::fs::write(&f, "foo foo foo\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&f).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.cursor_col = 1; // inside the first "foo"
    app.handle_key(key(KeyCode::Char('d'), KeyModifiers::SUPER))
        .unwrap();
    assert!(
        app.editor.selection.is_some() && !app.editor.has_multi_cursor(),
        "first Cmd+D selects the word only"
    );
    app.handle_key(key(KeyCode::Char('d'), KeyModifiers::SUPER))
        .unwrap();
    assert!(
        app.editor.has_multi_cursor(),
        "second Cmd+D adds the next match as a caret"
    );
}

/// #1558: `Cmd+Shift+L` with the caret on `foo` selects every whole-word
/// `foo`, and typing replaces them all (not the `foo` inside `food`). It
/// used to do nothing.
#[test]
fn cmd_shift_l_selects_every_occurrence_of_the_word() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = occurrences_app(tmp.path());
    app.editor.cursor_col = 1;
    cmd_shift_l(&mut app);
    assert_eq!(app.editor.carets.len() + 1, 3);
    type_char(&mut app, 'X');
    assert_eq!(app.editor.lines, ["X = 1", "bar = X", "X += 2", "food = 3"]);
}

/// #1558: `Cmd+D` from a hand-made selection also matches plain text; from
/// the caret it still takes whole words.
#[test]
fn cmd_d_from_a_hand_made_selection_matches_as_text() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = occurrences_app(tmp.path());
    select(&mut app, (0, 1), (0, 3));
    for _ in 0..2 {
        app.handle_key(key(KeyCode::Char('d'), KeyModifiers::SUPER))
            .unwrap();
    }
    type_char(&mut app, 'Q');
    assert_eq!(
        app.editor.lines,
        ["fQ = 1", "bar = fQ", "fQ += 2", "food = 3"]
    );

    let mut app = occurrences_app(tmp.path());
    app.editor.cursor_col = 1;
    for _ in 0..4 {
        app.handle_key(key(KeyCode::Char('d'), KeyModifiers::SUPER))
            .unwrap();
    }
    assert_eq!(app.editor.carets.len() + 1, 3, "`food` is no whole `foo`");
}

/// #1558: the word `Cmd+D` selected is forgotten once that selection is
/// dropped, so selecting the same `foo` again by hand matches plain text and
/// reaches the `foo` inside `food`.
#[test]
fn cmd_d_after_esc_and_a_hand_made_reselection_matches_as_text() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = occurrences_app(tmp.path());
    app.editor.cursor_col = 1;
    app.handle_key(key(KeyCode::Char('d'), KeyModifiers::SUPER))
        .unwrap();
    assert_eq!(app.editor.selection.unwrap().normalised(), ((0, 0), (0, 3)));
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert!(app.editor.selection.is_none());
    app.editor.cursor_col = 0;
    for _ in 0..3 {
        app.handle_key(key(KeyCode::Right, KeyModifiers::SHIFT))
            .unwrap();
    }
    assert_eq!(app.editor.selection.unwrap().normalised(), ((0, 0), (0, 3)));
    for _ in 0..4 {
        app.handle_key(key(KeyCode::Char('d'), KeyModifiers::SUPER))
            .unwrap();
    }
    assert_eq!(app.editor.carets.len() + 1, 4, "`food` holds a text `foo`");
}

/// #1558: `Shift+Alt+I` puts a caret at the end of each selected line, so
/// typing appends to all three; a selection ending at column 0 of the next
/// line leaves that line out.
#[test]
fn shift_alt_i_adds_a_cursor_at_each_line_end() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = occurrences_app(tmp.path());
    select(&mut app, (0, 0), (2, 8));
    app.handle_key(key(
        KeyCode::Char('I'),
        KeyModifiers::SHIFT | KeyModifiers::ALT,
    ))
    .unwrap();
    assert!(app.editor.selection.is_none_or(|s| s.anchor == s.head));
    type_char(&mut app, 'Z');
    assert_eq!(
        app.editor.lines,
        ["foo = 1Z", "bar = fooZ", "foo += 2Z", "food = 3"]
    );

    let mut app = occurrences_app(tmp.path());
    select(&mut app, (0, 0), (2, 0));
    assert!(run_from_palette(&mut app, "Add Cursors to Line Ends"));
    type_char(&mut app, ';');
    assert_eq!(
        app.editor.lines,
        ["foo = 1;", "bar = foo;", "foo += 2", "food = 3"]
    );
}

#[test]
fn cmd_shift_k_deletes_the_current_line() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("a.rs");
    std::fs::write(&f, "one\ntwo\nthree\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&f).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.cursor_row = 1; // the "two" line
    app.handle_key(key(
        KeyCode::Char('k'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert_eq!(
        app.editor.lines,
        vec!["one".to_string(), "three".to_string()],
        "Cmd+Shift+K removes the cursor's line"
    );
}

// --- Increment / Decrement Number ------------------------------------------

/// The Cmd+Opt+= / Cmd+Opt+- chords as crossterm decodes them (ALT|SUPER on
/// macOS; Ctrl+Alt on Linux arrives as ALT|CONTROL, which the predicate
/// accepts too).
#[test]
fn the_increment_chord_bumps_the_number_under_the_cursor() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "port = 8080");
    app.editor.cursor_col = 8;
    app.handle_key(key(
        KeyCode::Char('='),
        KeyModifiers::ALT | KeyModifiers::SUPER,
    ))
    .unwrap();
    assert_eq!(app.editor.lines, vec!["port = 8081"]);
}

#[test]
fn the_decrement_chord_bumps_the_number_down() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "port = 8080");
    app.editor.cursor_col = 8;
    app.handle_key(key(
        KeyCode::Char('-'),
        KeyModifiers::ALT | KeyModifiers::CONTROL,
    ))
    .unwrap();
    assert_eq!(app.editor.lines, vec!["port = 8079"]);
}

/// A chord that finds nothing to bump must say so rather than looking like
/// a dead key.
#[test]
fn the_increment_chord_explains_itself_when_there_is_no_number() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "let name = value;");
    app.handle_key(key(
        KeyCode::Char('='),
        KeyModifiers::ALT | KeyModifiers::SUPER,
    ))
    .unwrap();
    assert_eq!(app.editor.lines, vec!["let name = value;"]);
    assert!(
        app.status.contains("No number"),
        "status should explain the no-op, got {:?}",
        app.status
    );
}

#[test]
fn the_emmet_chord_expands_the_abbreviation_in_the_editor() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("page.html");
    std::fs::write(&f, "ul>li*2\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&f).unwrap();
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 7;
    app.handle_editor_key(key(
        KeyCode::Char('e'),
        KeyModifiers::SUPER | KeyModifiers::ALT | KeyModifiers::SHIFT,
    ));
    assert_eq!(
        app.editor.lines,
        vec!["<ul>", "    <li></li>", "    <li></li>", "</ul>"]
    );
    assert_eq!(app.status, "Emmet: expanded abbreviation");
}

/// #1640: the climb-up `^` used to stop the scan, so only the part after it
/// expanded and `header>nav>a*2^` stayed on the line as text.
#[test]
fn the_emmet_chord_expands_an_abbreviation_that_climbs_up() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("climb.html");
    std::fs::write(&f, "header>nav>a*2^h1\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&f).unwrap();
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 17;
    app.handle_editor_key(key(
        KeyCode::Char('e'),
        KeyModifiers::SUPER | KeyModifiers::ALT | KeyModifiers::SHIFT,
    ));
    assert_eq!(
        app.editor.lines,
        vec![
            "<header>",
            "    <nav>",
            "        <a href=\"\"></a>",
            "        <a href=\"\"></a>",
            "    </nav>",
            "    <h1></h1>",
            "</header>",
        ]
    );
    assert_eq!(app.status, "Emmet: expanded abbreviation");
}

/// #1640 negative: a caret in prose is left alone, with the usual status.
#[test]
fn the_emmet_chord_leaves_a_power_in_prose_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("page.html");
    std::fs::write(&f, "<p>2^10\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&f).unwrap();
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 7;
    app.handle_editor_key(key(
        KeyCode::Char('e'),
        KeyModifiers::SUPER | KeyModifiers::ALT | KeyModifiers::SHIFT,
    ));
    assert_eq!(app.editor.lines, vec!["<p>2^10"]);
    assert_eq!(app.status, "Emmet: no abbreviation at the cursor");
}

/// Silence would leave the user unable to tell "wrong language" from
/// "typo in the abbreviation".
#[test]
fn the_emmet_chord_says_so_when_there_is_nothing_to_expand() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("page.html");
    std::fs::write(&f, "div>(span\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&f).unwrap();
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 9;
    app.handle_editor_key(key(
        KeyCode::Char('e'),
        KeyModifiers::SUPER | KeyModifiers::ALT | KeyModifiers::SHIFT,
    ));
    assert_eq!(app.editor.lines, vec!["div>(span"]);
    assert_eq!(app.status, "Emmet: no abbreviation at the cursor");
}

/// A buffer Emmet does not run in gets a status naming the language, not
/// the "no abbreviation" message that sends the user hunting for a typo.
#[test]
fn the_emmet_chord_names_an_unsupported_language() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("main.rs");
    std::fs::write(&f, "ul>li*2\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&f).unwrap();
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 7;
    app.handle_editor_key(key(
        KeyCode::Char('e'),
        KeyModifiers::SUPER | KeyModifiers::ALT | KeyModifiers::SHIFT,
    ));
    assert_eq!(app.editor.lines, vec!["ul>li*2"]);
    assert_eq!(app.status, "Emmet: not available in Rust files");
}

const EMMET_PAGE: &str = "<ul>\n  <li>Home</li>\n  <li>About</li>\n</ul>\n<p>Some text</p>\n";

/// Run palette command `cmd`, then, if it opened a prompt, answer it.
fn run_emmet(app: &mut App, cmd: crate::widgets::command_palette::Command, answer: Option<&str>) {
    app.run_command(cmd);
    if let Some(answer) = answer {
        assert!(app.input_prompt.is_some(), "a prompt asks first");
        type_str(app, answer);
        app.submit_input_prompt();
    }
}

/// #1557: the issue's example: Wrap with Abbreviation around the selected
/// `<ul>` with `nav.menu`.
#[test]
fn wrap_with_abbreviation_wraps_the_selected_lines() {
    use crate::widgets::command_palette::Command;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "page.html", EMMET_PAGE);
    app.editor.selection = Some(crate::widgets::editor::EditorSelection {
        anchor: (0, 0),
        head: (3, 5),
    });
    run_emmet(
        &mut app,
        Command::EmmetWrapWithAbbreviation,
        Some("nav.menu"),
    );
    assert_eq!(
        app.editor.lines.join("\n"),
        "<nav class=\"menu\">\n  <ul>\n    <li>Home</li>\n    <li>About</li>\n  </ul>\n</nav>\n<p>Some text</p>"
    );
    assert!(app.status.contains("wrapped"), "{}", app.status);
    app.editor.undo();
    assert_eq!(
        app.editor.lines.join("\n"),
        EMMET_PAGE.trim_end(),
        "one undo step"
    );
}

/// #1557: with nothing selected, Wrap with Abbreviation takes the element
/// around the caret, a caret just past `</ul>` included; outside any
/// element it takes the caret's line.
#[test]
fn with_nothing_selected_wrap_takes_the_element_at_the_caret() {
    use crate::widgets::command_palette::Command;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "page.html", EMMET_PAGE);
    app.editor.cursor_row = 3;
    app.editor.cursor_col = 5;
    run_emmet(&mut app, Command::EmmetWrapWithAbbreviation, Some("nav"));
    assert_eq!(
        app.editor.lines.join("\n"),
        "<nav>\n  <ul>\n    <li>Home</li>\n    <li>About</li>\n  </ul>\n</nav>\n<p>Some text</p>"
    );
    let mut app = app_with_open_file(
        tmp.path(),
        "page.html",
        "<ul>\n  <li>Home</li>\n</ul>\nplain words\n",
    );
    app.editor.cursor_row = 1;
    app.editor.cursor_col = 7;
    run_emmet(&mut app, Command::EmmetWrapWithAbbreviation, Some("b"));
    assert_eq!(
        app.editor.lines.join("\n"),
        "<ul>\n  <b>\n    <li>Home</li>\n  </b>\n</ul>\nplain words"
    );
    app.editor.cursor_row = 5;
    app.editor.cursor_col = 3;
    run_emmet(&mut app, Command::EmmetWrapWithAbbreviation, Some("p"));
    assert_eq!(
        app.editor.lines.join("\n"),
        "<ul>\n  <b>\n    <li>Home</li>\n  </b>\n</ul>\n<p>\n  plain words\n</p>"
    );
}

/// #1557: Remove Tag, Update Tag and Go to Matching Pair from the palette,
/// with the caret in the `<ul>`.
#[test]
fn remove_and_update_tag_and_go_to_matching_pair() {
    use crate::widgets::command_palette::Command;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "page.html", EMMET_PAGE);
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 1;
    run_emmet(&mut app, Command::EmmetMatchTag, None);
    assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (3, 0));
    run_emmet(&mut app, Command::EmmetUpdateTag, Some("ol"));
    assert_eq!(app.editor.lines[0], "<ol>");
    assert_eq!(app.editor.lines[3], "</ol>");
    run_emmet(&mut app, Command::EmmetRemoveTag, None);
    assert_eq!(
        app.editor.lines.join("\n"),
        "<li>Home</li>\n<li>About</li>\n<p>Some text</p>"
    );
}

/// #1557: Balance outward grows the selection from the caret to the
/// element's content and then the element; inward walks it back.
#[test]
fn balance_outward_and_inward_step_the_selection() {
    use crate::widgets::command_palette::Command;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "page.html", EMMET_PAGE);
    app.editor.cursor_row = 1;
    app.editor.cursor_col = 7;
    run_emmet(&mut app, Command::EmmetBalanceOutward, None);
    assert_eq!(app.editor.selection_text(), "Home");
    run_emmet(&mut app, Command::EmmetBalanceOutward, None);
    assert_eq!(app.editor.selection_text(), "<li>Home</li>");
    run_emmet(&mut app, Command::EmmetBalanceInward, None);
    assert_eq!(app.editor.selection_text(), "Home");
}

/// #1557 negative: outside markup the commands say Emmet is not offered,
/// ask nothing and change nothing; in markup with no tag around the caret,
/// Remove Tag says so.
#[test]
fn emmet_editing_commands_leave_other_files_alone() {
    use crate::widgets::command_palette::Command;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "notes.txt", "<ul>\n</ul>\n");
    for cmd in [
        Command::EmmetWrapWithAbbreviation,
        Command::EmmetUpdateTag,
        Command::EmmetRemoveTag,
        Command::EmmetMatchTag,
        Command::EmmetBalanceOutward,
    ] {
        app.run_command(cmd);
        assert!(app.input_prompt.is_none(), "{cmd:?} asks nothing");
        assert!(
            app.status.contains("not available"),
            "{cmd:?}: {}",
            app.status
        );
    }
    assert_eq!(app.editor.lines.join("\n"), "<ul>\n</ul>");
    let mut html = app_with_open_file(tmp.path(), "plain.html", "just text\n");
    run_emmet(&mut html, Command::EmmetRemoveTag, None);
    assert!(html.status.contains("no tag"), "{}", html.status);
    assert_eq!(html.editor.lines.join("\n"), "just text");
}

/// #1557: the commands are in the palette and VS Code's Emmet keybindings
/// import onto them.
#[test]
fn emmet_editing_commands_are_in_the_palette_and_import() {
    use crate::widgets::command_palette::{ALL_COMMANDS, Command};
    for (cmd, title, vscode) in [
        (
            Command::EmmetWrapWithAbbreviation,
            "Emmet: Wrap with Abbreviation",
            "editor.emmet.action.wrapWithAbbreviation",
        ),
        (
            Command::EmmetRemoveTag,
            "Emmet: Remove Tag",
            "editor.emmet.action.removeTag",
        ),
        (
            Command::EmmetUpdateTag,
            "Emmet: Update Tag",
            "editor.emmet.action.updateTag",
        ),
        (
            Command::EmmetBalanceOutward,
            "Emmet: Balance (outward)",
            "editor.emmet.action.balanceOut",
        ),
        (
            Command::EmmetBalanceInward,
            "Emmet: Balance (inward)",
            "editor.emmet.action.balanceIn",
        ),
        (
            Command::EmmetMatchTag,
            "Emmet: Go to Matching Pair",
            "editor.emmet.action.matchTag",
        ),
    ] {
        assert!(ALL_COMMANDS.contains(&cmd), "{cmd:?}");
        assert_eq!(cmd.title(), title);
        let mut report = crate::import_vscode::Report::default();
        crate::import_vscode::convert_keybindings(
            &serde_json::json!([{ "key": "ctrl+alt+w", "command": vscode }]),
            &mut report,
        );
        assert_eq!(
            report.keybindings,
            vec![(String::from("ctrl+alt+w"), String::from(cmd.id()))],
            "{vscode}"
        );
    }
}

#[test]
fn tab_switches_focus_between_the_find_and_replace_fields() {
    use crate::widgets::editor_find::FindField;
    let tmp = tempfile::tempdir().unwrap();
    // The caret opens off any word, so nothing is seeded and focus starts
    // in the query row (a seeded query would start in the replace row).
    let mut app = app_with_open_file(tmp.path(), "a.txt", "- alpha beta");
    app.handle_key(replace_chord()).unwrap();
    for c in "alpha".chars() {
        app.handle_key(key(KeyCode::Char(c), KeyModifiers::NONE))
            .unwrap();
    }
    assert_eq!(
        app.editor_find.as_ref().unwrap().query,
        "alpha",
        "typing lands in the query row first"
    );
    app.handle_key(key(KeyCode::Tab, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(
        app.editor_find.as_ref().unwrap().focus,
        FindField::Replace,
        "Tab moves focus to the replace row"
    );
    for c in "x".chars() {
        app.handle_key(key(KeyCode::Char(c), KeyModifiers::NONE))
            .unwrap();
    }
    assert_eq!(
        app.editor_find.as_ref().unwrap().replace,
        "x",
        "typing lands in the replace row once focused"
    );
    assert_eq!(
        app.editor_find.as_ref().unwrap().query,
        "alpha",
        "the query is untouched by replace-row typing"
    );
}

/// #1577: a replacement longer than the needle moves the range's end with
/// it, so a range ending mid-line still holds every replaced match and
/// nothing past it.
#[test]
fn the_range_follows_a_longer_replacement() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "f.py", FIND_IN_SELECTION_SRC);
    app.editor.selection = Some(crate::widgets::editor::EditorSelection {
        anchor: (3, 0),
        head: (4, 12),
    });
    // Off any word, so nothing is seeded and the query is typed.
    (app.editor.cursor_row, app.editor.cursor_col) = (5, 0);
    app.handle_key(replace_chord()).unwrap();
    app.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE))
        .unwrap();
    alt_l(&mut app);
    assert_eq!(find_count(&app), 2);
    app.handle_key(key(KeyCode::Tab, KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Enter, KeyModifiers::ALT | KeyModifiers::SUPER))
        .unwrap();
    assert_eq!(app.editor.lines[3], "def b(xx):");
    assert_eq!(app.editor.lines[4], "    return xx * 2");
    assert_eq!(app.editor.find_range, Some(((3, 0), (4, 13))));
    assert_eq!(find_count(&app), 4, "both doubled matches, still in range");
    assert_eq!(app.editor.lines[0], "def a(x):");
    assert_eq!(app.editor.lines[7], "    return x - 3");

    // One at a time too: each Enter in the replace row moves the end on.
    let mut app = app_with_open_file(tmp.path(), "g.py", FIND_IN_SELECTION_SRC);
    (app.editor.cursor_row, app.editor.cursor_col) = (5, 0);
    app.handle_key(replace_chord()).unwrap();
    app.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE))
        .unwrap();
    // Selected after the bar opened, so the one-line selection does not
    // seed the query.
    app.editor.selection = Some(crate::widgets::editor::EditorSelection {
        anchor: (4, 0),
        head: (4, 12),
    });
    alt_l(&mut app);
    app.handle_key(key(KeyCode::Tab, KeyModifiers::NONE))
        .unwrap();
    for c in "value".chars() {
        app.handle_key(key(KeyCode::Char(c), KeyModifiers::NONE))
            .unwrap();
    }
    for _ in 0..3 {
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();
    }
    assert_eq!(app.editor.lines[4], "    return value * 2");
    assert_eq!(app.editor.find_range, Some(((4, 0), (4, 16))));
    assert_eq!(app.editor.lines[7], "    return x - 3", "past the range");
}

/// #1577: a regex replacement's `\r` breaks the line, and the range's end
/// moves down with it, so the selection's later matches stay in it.
#[test]
fn the_range_follows_a_carriage_return_replacement() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = find_in_selection_app(tmp.path());
    app.editor_find.as_mut().unwrap().opts.use_regex = true;
    alt_l(&mut app);
    assert_eq!(app.editor.active_search_match, Some((3, 6, 1)));
    app.handle_key(key(KeyCode::Tab, KeyModifiers::NONE))
        .unwrap();
    for c in "a\\rb".chars() {
        app.handle_key(key(KeyCode::Char(c), KeyModifiers::NONE))
            .unwrap();
    }
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.editor.lines[3], "def b(a");
    assert_eq!(app.editor.lines[4], "b):");
    assert_eq!(app.editor.find_range, Some(((3, 0), (6, 0))));
    assert_eq!(find_count(&app), 1, "the `x` the break pushed down");
}

#[test]
fn tab_expands_a_user_snippet_and_places_the_caret() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.snippets = crate::snippets::SnippetSet::from_json(
        r#"{ "Log": { "prefix": "log", "body": "console.log($1)" } }"#,
    );
    app.editor.insert_str("log");
    app.handle_editor_tab();
    assert_eq!(app.editor.lines[app.editor.cursor_row], "console.log()");
    assert_eq!(
        app.editor.cursor_col, 12,
        "caret lands inside the parentheses at $1"
    );
}

// ── #1484: a repo's own snippets expand ──

const TEAM_SNIPPETS_1484: &str = r#"{
  // committed by the team
  "Team logger": {
    "scope": "python",
    "prefix": "tlog",
    "body": ["log = get_logger(__name__)", "log.info(\"$1\")$0"]
  }
}"#;

/// Open `name` in an app on `root` and expand `typed` with Tab; the first
/// line after.
fn expand_in_project_1484(root: &std::path::Path, name: &str, typed: &str) -> String {
    let f = root.join(name);
    std::fs::write(&f, "").unwrap();
    let mut app = App::new(root.to_path_buf()).unwrap();
    app.editor.open(&f).unwrap();
    app.editor.insert_str(typed);
    app.handle_editor_tab();
    app.editor.lines[0].clone()
}

#[test]
fn a_vscode_code_snippets_file_expands_in_its_repo() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".vscode")).unwrap();
    std::fs::write(
        tmp.path().join(".vscode/team.code-snippets"),
        TEAM_SNIPPETS_1484,
    )
    .unwrap();
    assert_eq!(
        expand_in_project_1484(tmp.path(), "app.py", "tlog"),
        "log = get_logger(__name__)"
    );
}

#[test]
fn a_croft_snippets_json_in_the_repo_expands() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".croft")).unwrap();
    std::fs::write(tmp.path().join(".croft/snippets.json"), TEAM_SNIPPETS_1484).unwrap();
    assert_eq!(
        expand_in_project_1484(tmp.path(), "app.py", "tlog"),
        "log = get_logger(__name__)"
    );
}

/// #1484 negative: a project snippet keeps its scope, and only files in
/// the two project places count.
#[test]
fn project_snippets_keep_their_scope_and_their_folder() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".vscode")).unwrap();
    std::fs::write(
        tmp.path().join(".vscode/team.code-snippets"),
        TEAM_SNIPPETS_1484,
    )
    .unwrap();
    // Tab with nothing to expand indents instead.
    assert_eq!(
        expand_in_project_1484(tmp.path(), "main.rs", "tlog").trim_end(),
        "tlog"
    );

    let other = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(other.path().join(".vscode")).unwrap();
    std::fs::write(other.path().join("team.code-snippets"), TEAM_SNIPPETS_1484).unwrap();
    std::fs::write(other.path().join(".vscode/team.json"), TEAM_SNIPPETS_1484).unwrap();
    assert_eq!(
        expand_in_project_1484(other.path(), "app.py", "tlog").trim_end(),
        "tlog"
    );
}

/// #1484: a snippets file pulled into `.vscode` while croft runs applies
/// within the config watch's interval, and the next check is quiet.
#[test]
fn a_project_snippets_file_added_later_is_picked_up() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("app.py");
    std::fs::write(&f, "").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.config_watch = crate::config_sync::ConfigWatch::new(Vec::new());
    app.watch_project_snippets();
    app.editor.open(&f).unwrap();
    std::fs::create_dir_all(tmp.path().join(".vscode")).unwrap();
    std::fs::write(
        tmp.path().join(".vscode/team.code-snippets"),
        TEAM_SNIPPETS_1484,
    )
    .unwrap();
    let t0 = std::time::Instant::now();
    assert!(app.tick_config_watch_at(t0 + crate::config_sync::ConfigWatch::INTERVAL * 2));
    assert_eq!(app.status, "Project snippets reloaded");
    app.editor.insert_str("tlog");
    app.handle_editor_tab();
    assert_eq!(app.editor.lines[0], "log = get_logger(__name__)");
    assert!(!app.tick_config_watch_at(t0 + crate::config_sync::ConfigWatch::INTERVAL * 4));
}

/// #1484 negative: saving `.vscode/settings.json` touches the folder the
/// watch looks at, but no snippet changed, so the status says nothing
/// about snippets.
#[test]
fn a_settings_change_in_vscode_does_not_announce_snippets() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".vscode")).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.config_watch = crate::config_sync::ConfigWatch::new(Vec::new());
    app.watch_project_snippets();
    app.status = String::from("Ready");
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(tmp.path().join(".vscode/settings.json"), "{}").unwrap();
    let t0 = std::time::Instant::now();
    app.tick_config_watch_at(t0 + crate::config_sync::ConfigWatch::INTERVAL * 2);
    assert_eq!(app.status, "Ready");
}

/// #1484: re-rooting swaps the old repository's snippets for the new one's.
#[test]
fn changing_the_workspace_root_reloads_project_snippets() {
    let old = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(old.path().join(".vscode")).unwrap();
    std::fs::write(
        old.path().join(".vscode/team.code-snippets"),
        TEAM_SNIPPETS_1484,
    )
    .unwrap();
    let new = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(new.path().join(".croft")).unwrap();
    std::fs::write(
        new.path().join(".croft/snippets.json"),
        TEAM_SNIPPETS_1484.replace("tlog", "nlog"),
    )
    .unwrap();
    let mut app = App::new(old.path().to_path_buf()).unwrap();
    app.change_workspace_root(new.path().to_path_buf());
    let f = new.path().join("app.py");
    std::fs::write(&f, "").unwrap();
    app.editor.open(&f).unwrap();
    app.editor.insert_str("tlog");
    app.handle_editor_tab();
    assert_eq!(app.editor.lines[0].trim_end(), "tlog");
    let g = new.path().join("b.py");
    std::fs::write(&g, "").unwrap();
    app.editor.open(&g).unwrap();
    app.editor.insert_str("nlog");
    app.handle_editor_tab();
    assert_eq!(app.editor.lines[0], "log = get_logger(__name__)");
}

/// #1484: a `.croft/snippets.json` created while croft runs applies, though
/// it did not exist when the watch began.
#[test]
fn a_croft_snippets_json_created_later_is_picked_up() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.config_watch = crate::config_sync::ConfigWatch::new(Vec::new());
    app.watch_project_snippets();
    std::fs::create_dir_all(tmp.path().join(".croft")).unwrap();
    std::fs::write(tmp.path().join(".croft/snippets.json"), TEAM_SNIPPETS_1484).unwrap();
    let t0 = std::time::Instant::now();
    assert!(app.tick_config_watch_at(t0 + crate::config_sync::ConfigWatch::INTERVAL * 2));
    assert_eq!(app.status, "Project snippets reloaded");
}

#[test]
fn a_multi_line_validated_range_falls_back_to_the_word_prompt() {
    // The server may validate a span crossing lines; there is no single-line
    // text to pre-fill from, so the word-under-cursor fallback takes over
    // rather than opening a prompt with an empty buffer.
    use crate::lsp::manager::PrepareRenameResult;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("lib.rs");
    std::fs::write(&path, "fn main() {\n    let value = 1;\n}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&path).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.cursor_row = 1;
    app.editor.cursor_col = 9; // inside `value`
    app.prepare_rename_request = Some((4, path.clone(), 1, 9, app.editor.edit_seq));
    assert!(app.apply_prepare_rename_verdict(PrepareRenameResult {
        request_id: 4,
        unsupported: false,
        error: None,
        target: Some(((1, 8, 2, 4), None)),
    }));
    assert_eq!(
        app.prompt.as_ref().expect("fallback prompt opened").buffer,
        "value",
        "a cross-line range yields no pre-fill text, so the fallback runs"
    );
}

/// Moving the caret or editing is a new position, asked once more; a
/// non-empty answer there still installs its ranges.
#[test]
fn a_moved_caret_or_an_edit_asks_once_more_and_ranges_still_install() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = linked_ts_app(tmp.path());
    app.tick_linked_editing();
    assert!(linked_wake(&mut app).is_some());

    app.editor.cursor_col = 8;
    app.tick_linked_editing();
    let moved: Vec<u64> = (0..20).filter_map(|_| linked_wake(&mut app)).collect();
    assert_eq!(
        moved.len(),
        1,
        "the moved caret asks exactly once: {moved:?}"
    );

    app.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE))
        .unwrap();
    app.tick_linked_editing();
    let id = linked_tick_only(&mut app).expect("an edit asks again");
    answer_linked(&app, id, vec![(1, 0, 1, 7), (1, 9, 1, 16)]);
    app.drain_lsp_linked_editing();
    assert!(app.editor.has_linked_ranges(), "a real answer installs");
    let more: Vec<u64> = (0..20).filter_map(|_| linked_wake(&mut app)).collect();
    assert!(
        more.is_empty(),
        "inside the installed set nothing is asked: {more:?}"
    );
}

#[test]
fn editing_drops_stale_document_links() {
    use crate::lsp::manager::DocumentLinkItem;
    // Link ranges are measured against the text the server saw; an edit
    // shifts them, so a Ctrl+click would otherwise follow the wrong span
    // until the next batch lands.
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("a.rs");
    std::fs::write(
        &path,
        "// link here
",
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&path).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.apply_document_links(
        path.clone(),
        vec![DocumentLinkItem {
            line: 0,
            character: 3,
            end_line: 0,
            end_character: 12,
            target: String::from("https://example.com"),
        }],
    );
    assert_eq!(
        app.editor.document_link_at(0, 5),
        Some("https://example.com"),
        "the link is live before the edit"
    );
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 0;
    app.editor.insert_char('x');
    assert_eq!(
        app.editor.document_link_at(0, 5),
        None,
        "the edit dropped the stale ranges"
    );
}

/// #257 round-3 review finding: a step that runs out of budget must leave
/// the user where they are. Marking the count partial and then clearing the
/// active match left the bar reading "1+ matches" with nothing highlighted
/// and no way back to the match they were on.
#[test]
fn a_step_that_runs_out_of_reach_keeps_the_match_it_already_had() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("far.log");
    // One match at the very top, then far more filler than a sweep may read,
    // so stepping forward from it exhausts the budget without finding another.
    let mut body = String::from("\u{1b}[31mERROR\u{1b}[0m first and only\n");
    let filler = "quiet line with nothing of interest ".repeat(4);
    for _ in 0..(crate::log_view::FIND_SCAN_BYTES / filler.len() + 3000) {
        body.push_str(&filler);
        body.push('\n');
    }
    std::fs::write(&p, &body).unwrap();

    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.open(&p).unwrap();
    assert!(app.editor.log.is_some());

    app.handle_key(key(KeyCode::Char('f'), KeyModifiers::SUPER))
        .unwrap();
    for ch in "ERROR".chars() {
        app.handle_key(key(KeyCode::Char(ch), KeyModifiers::NONE))
            .unwrap();
    }
    assert_eq!(
        app.editor.active_search_match.map(|(r, _, _)| r),
        Some(0),
        "typing lands on the only match"
    );

    // Enter steps forward, runs out of budget, and finds nothing more.
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(
        app.editor.active_search_match.map(|(r, _, _)| r),
        Some(0),
        "an out-of-reach step must leave the highlight where it was, not \
         clear it and leave the user with a count and nothing to look at"
    );
    assert!(
        app.editor_find
            .as_ref()
            .is_some_and(|s| s.count_truncated()),
        "and the bar must say the search did not cover the whole file"
    );
}

/// #404 review: a wide character whose first half lands on the body's last
/// cell does not fit whole, and `set_stringn` drops it entirely. The
/// selection band must not colour that cell, or the annotator paints where
/// the painter deliberately put nothing, the same disagreement the cell map
/// exists to close, at the boundary.
#[test]
fn a_wide_character_clipped_at_the_right_edge_gets_no_selection_band() {
    use ratatui::layout::Rect;
    use ratatui::widgets::Widget;

    let tmp = tempfile::tempdir().unwrap();
    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 8,
    };
    // Learn the body width from a probe render, so the fixture puts the
    // wide character exactly on the last cell whatever the chrome costs.
    let probe = tmp.path().join("probe.log");
    std::fs::write(&probe, "\u{1b}[31mx\u{1b}[0m\n").unwrap();
    let mut e = crate::widgets::editor::Editor::new();
    e.open(&probe).unwrap();
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut e).render(area, &mut buf);
    let width = e.log.as_ref().unwrap().last_body.width as usize;
    assert!(width > 2);

    let p = tmp.path().join("edge.log");
    let text = format!("{}\u{4e2d}", "x".repeat(width - 1));
    std::fs::write(&p, format!("\u{1b}[31m{text}\u{1b}[0m\n")).unwrap();
    let mut e = crate::widgets::editor::Editor::new();
    e.open(&p).unwrap();
    let chars = text.chars().count();
    e.log.as_mut().unwrap().selection = Some(((0, 0), (0, chars)));
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut e).render(area, &mut buf);
    let body = e.log.as_ref().unwrap().last_body;
    let last = body.x + body.width - 1;
    let selected_bg = crate::theme::Theme::BLACK.selection();
    assert_eq!(
        buf[(last, body.y)].symbol(),
        " ",
        "the painter drops a wide character that does not fit whole"
    );
    assert_ne!(
        buf[(last, body.y)].style().bg,
        Some(selected_bg),
        "so the band must not colour the cell it left blank"
    );
    assert_eq!(
        buf[(last - 1, body.y)].style().bg,
        Some(selected_bg),
        "while the character before it is selected as usual"
    );
}

/// #369: a mirrored tab undoes as far as the tab that was typed in.
///
/// A typing burst is one undo step where it was typed, so it is one step in
/// the file's tab too, and a selection away from the edit survives it.
#[test]
fn a_mirrored_typing_burst_is_one_undo_step_and_keeps_a_distant_selection() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, _) = app_with_symbol_tab_on_b(&tmp);
    app.sync_symbol_views();
    let before = app.editor.lines.clone();
    let mut sel = crate::widgets::editor::EditorSelection::new(0, 0);
    sel.head = (0, 4);
    app.editor.editors[0].selection = Some(sel);

    app.editor.cursor_row = 4;
    app.editor.cursor_col = 5;
    for c in ['a', 'b', 'c'] {
        app.handle_key(key(KeyCode::Char(c), KeyModifiers::NONE))
            .unwrap();
    }
    assert_eq!(app.editor.editors[0].lines[4], "    2abc");
    assert_eq!(
        app.editor.editors[0].selection,
        Some(sel),
        "a selection clear of the edit is kept"
    );
    assert!(app.editor.editors[0].undo());
    assert!(app.editor.editors[1].undo());
    assert_eq!(app.editor.editors[1].lines, before);
    assert_eq!(
        app.editor.editors[0].lines, before,
        "one undo takes back the whole burst in the file's tab too"
    );
}

#[test]
fn a_rewrite_that_only_changes_bytes_still_refreshes_the_byte_facts() {
    // A switch back from CRLF changes no line text, so the rows come back
    // identical -- but `right_crlf` and the "bytes differ, lines equal"
    // banner feed the hunk patch and the header. The
    // same-content skip must see those facts as content, or stage/revert
    // would build a patch from bytes no longer on disk.
    let (tmp, f) = repo_with_seed("a\nb\n");
    let root = tmp.path().to_path_buf();
    let mut app = App::new(root.clone()).unwrap();
    std::fs::write(&f, "a\r\nb\r\n").unwrap();
    app.editor
        .open_head_diff_with_text(
            std::path::PathBuf::from("seed.txt (HEAD)"),
            "a\nb\n",
            &f,
            true,
        )
        .unwrap();
    app.tag_open_diff(crate::widgets::diff::DiffSource::HeadVsWorking {
        root: root.clone(),
        rel: String::from("seed.txt"),
    });
    let d = app.editor.diff.as_ref().unwrap();
    assert!(
        d.right_crlf.iter().all(|&cr| cr) && d.bytes_differ_but_lines_equal,
        "precondition: {d:?}"
    );

    rewrite_later(&f, "a\nb\n");
    assert!(
        app.refresh_open_diff_views(false, &[], &no_paths()),
        "the byte-only rewrite counts as a change"
    );
    let d = app.editor.diff.as_ref().unwrap();
    assert!(
        !d.right_crlf.iter().any(|&cr| cr),
        "the working side is LF again"
    );
    assert!(
        !d.bytes_differ_but_lines_equal,
        "and the banner's fact is cleared"
    );
    assert!(
        !app.refresh_open_diff_views(false, &[], &no_paths()),
        "and it is not rebuilt again"
    );
}

// --- #1218: a multi-line paste keeps its shape under the caret ----------------

fn shop_app(tmp: &std::path::Path) -> App {
    let mut app = app_with_open_file(
        tmp,
        "shop.py",
        "class Shop:\n    def total(self, items):\n        if items:\n            n = 0\n            \n        return 0\n",
    );
    app.focus_pane(Pane::Editor);
    app.editor.cursor_row = 4;
    app.editor.cursor_col = 12;
    app
}

#[test]
fn a_bracketed_paste_into_a_nested_block_keeps_the_block_shape() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = shop_app(tmp.path());
    app.handle_paste("for x in items:\n    if x:\n        print(x)");
    assert_eq!(
        &app.editor.lines[4..7],
        &[
            "            for x in items:",
            "                if x:",
            "                    print(x)",
        ]
    );
}

#[test]
fn with_paste_reindent_off_a_paste_is_verbatim() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = shop_app(tmp.path());
    app.paste_reindent = false;
    app.handle_paste("for x in items:\n    if x:\n        print(x)");
    assert_eq!(app.editor.lines[5], "    if x:");
}

#[test]
fn a_multiline_paste_can_go_in_as_one_line() {
    let (mut app, _tmp) = plain_paste_app();
    app.handle_paste("echo a\r\n  echo b\r\n\r\n");
    app.handle_key(key(KeyCode::Char('l'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(pasted(&app), "echo a   echo b");
}

#[test]
fn a_cancelled_multiline_paste_writes_nothing() {
    let (mut app, _tmp) = plain_paste_app();
    app.handle_paste(TWO_LINES);
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(pasted(&app), "");
    assert!(app.pending_terminal_paste.is_none());
    // The next key is typing again, not an answer to the popup.
    app.handle_key(key(KeyCode::Char('l'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(pasted(&app), "l");
}

#[test]
fn the_confirmation_says_how_many_lines_will_run() {
    let (mut app, _tmp) = plain_paste_app();
    app.handle_paste(TWO_LINES);
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let screen: String = term
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect();
    assert!(screen.contains("PASTE 2 LINES"), "{screen}");
}

#[test]
fn a_bracketed_paste_or_a_single_line_goes_straight_in() {
    // Negative: a shell that brackets pastes runs nothing until Enter, and
    // one line with no line break runs nothing either.
    let (mut app, _tmp) = plain_paste_app();
    app.handle_paste("echo one-line");
    assert_eq!(pasted(&app), "echo one-line");
    app.terminals[0].feed_bytes_for_test(b"\x1b[?2004h");
    app.handle_paste(TWO_LINES);
    assert!(app.pending_terminal_paste.is_none());
    assert!(
        pasted(&app).ends_with(&format!("\x1b[200~{TWO_LINES}\x1b[201~")),
        "{:?}",
        pasted(&app)
    );
}

#[test]
fn a_second_paste_while_asking_is_refused_and_the_first_still_waits() {
    // The popup holds the first paste; a later one, multi-line or not,
    // neither replaces it nor slips into the shell behind the popup.
    let (mut app, _tmp) = plain_paste_app();
    app.handle_paste(TWO_LINES);
    app.handle_paste("rm -rf build\nls\n");
    app.handle_paste("echo sneaked");
    assert_eq!(pasted(&app), "", "nothing reaches the shell");
    assert_eq!(app.status, "Resolve the pending paste before pasting again");
    assert_eq!(
        app.pending_terminal_paste.as_deref(),
        Some(TWO_LINES.as_bytes()),
        "the first paste is the one still asked about"
    );
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(pasted(&app), TWO_LINES);
}

#[test]
fn once_the_paste_is_answered_clicks_and_pastes_work_again() {
    // Negative: the guards last only while the popup is up.
    let (mut app, _tmp) = two_pane_paste_app();
    app.handle_paste(TWO_LINES);
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    app.handle_paste("echo again");
    assert_eq!(pasted(&app), "echo again");
    let other = app.terminal_rail_rects[1];
    left_click(&mut app, other.x + 1, other.y);
    assert_eq!(app.active_terminal, 1, "the rail row switches panes again");
}

fn caret_on_screen(app: &App) -> bool {
    let bar = usize::from(app.editor.last_scrollbar.width > 0);
    let width = (app.editor.last_inner.width as usize)
        .saturating_sub(app.editor.last_gutter_width as usize + 2 + bar);
    app.editor.cursor_col >= app.editor.scroll_col
        && app.editor.cursor_col < app.editor.scroll_col + width
}

#[test]
fn end_on_a_wide_line_scrolls_the_caret_into_view() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, _term) = wide_line_app(tmp.path());
    app.handle_key(key(KeyCode::End, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.editor.cursor_col, 609);
    assert!(app.editor.scroll_col > 0, "the view follows the caret");
    assert!(
        caret_on_screen(&app),
        "scroll_col {}",
        app.editor.scroll_col
    );
}

#[test]
fn typing_past_the_right_edge_keeps_the_caret_in_view() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, _term) = wide_line_app(tmp.path());
    app.editor.cursor_col = 60;
    for _ in 0..40 {
        app.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE))
            .unwrap();
    }
    assert_eq!(app.editor.cursor_col, 100);
    assert!(
        caret_on_screen(&app),
        "scroll_col {}",
        app.editor.scroll_col
    );
}

#[test]
fn shift_end_extends_the_selection_and_follows_the_caret() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, _term) = wide_line_app(tmp.path());
    app.handle_key(key(KeyCode::End, KeyModifiers::SHIFT))
        .unwrap();
    assert!(app.editor.selection.is_some());
    assert!(
        caret_on_screen(&app),
        "scroll_col {}",
        app.editor.scroll_col
    );
}

#[test]
fn enter_at_the_end_of_a_wide_line_scrolls_back_to_column_one() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, _term) = wide_line_app(tmp.path());
    app.handle_key(key(KeyCode::End, KeyModifiers::NONE))
        .unwrap();
    app.editor.scroll_col = 560;
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (1, 0));
    assert_eq!(app.editor.scroll_col, 0);
}

#[test]
fn end_on_a_short_line_leaves_the_view_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, _term) = wide_line_app(tmp.path());
    app.editor.cursor_row = 1;
    app.handle_key(key(KeyCode::End, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.editor.cursor_col, 5);
    assert_eq!(app.editor.scroll_col, 0);
}

#[test]
fn ctrl_a_selects_the_query_and_ctrl_backspace_deletes_a_word() {
    let (mut app, _tmp) = find_seed_app(2, 0);
    app.handle_key(ctrl('f')).unwrap();
    type_str(&mut app, "write rows");
    app.handle_key(key(KeyCode::Backspace, KeyModifiers::CONTROL))
        .unwrap();
    assert_eq!(find_query(&app), "write ");
    app.handle_key(ctrl('a')).unwrap();
    type_str(&mut app, "read");
    assert_eq!(find_query(&app), "read");
}

// ── #1483: one snippet croft cannot read no longer empties the whole set ──

/// A Python buffer holding `typed`, with a snippet set that mixes a plain
/// prefix and an array one; returns the first line after Tab.
fn expand_with_array_prefix_snippets(typed: &str) -> String {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("loop.py");
    std::fs::write(&f, "").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&f).unwrap();
    app.snippets = crate::snippets::SnippetSet::from_json(
        r#"{
            "Python main guard": { "prefix": "main", "body": "if __name__ == '__main__':", "scope": "python" },
            "For loop": { "prefix": ["for", "fori"], "body": ["for ${1:i} in range(${2:10}):", "    $0"], "scope": "python" }
        }"#,
    );
    app.editor.insert_str(typed);
    app.handle_editor_tab();
    app.editor.lines[0].clone()
}

/// Snippets the way `croft import-vscode` writes them from VS Code's
/// per-language files (#1641).
fn react_snippets() -> crate::snippets::SnippetSet {
    crate::snippets::SnippetSet::from_json(
        r#"{
          "Component": { "prefix": "rfc", "body": "function $1() {}", "scope": "javascriptreact" },
          "TS component": { "prefix": "tfc", "body": "const $1: FC = () => null", "scope": "typescriptreact" },
          "Log": { "prefix": "clg", "body": "console.log($1)", "scope": "javascript" },
          "Main": { "prefix": "ifm", "body": "if __name__ == '__main__':", "scope": "python" },
          "Key": { "prefix": "key", "body": "\"$1\": ", "scope": "jsonc" }
        }"#,
    )
}

/// Open `name` (empty) with the snippets above, type `prefix`, press Tab,
/// and return the line.
fn tab_after(name: &str, prefix: &str) -> String {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join(name);
    std::fs::write(&p, "\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&p).unwrap();
    app.snippets = react_snippets();
    app.editor.insert_str(prefix);
    app.handle_editor_tab();
    app.editor.lines[app.editor.cursor_row].clone()
}

/// #1641: a `javascriptreact` snippet expands in a `.jsx` file, and a
/// `jsonc` one in a `.jsonc` file, as in VS Code.
#[test]
fn a_javascriptreact_snippet_expands_in_a_jsx_file() {
    assert_eq!(tab_after("App.jsx", "rfc"), "function () {}");
    assert_eq!(tab_after("tsconfig.jsonc", "key"), "\"\": ");
}

/// Negative (#1641): `.tsx` keeps `typescriptreact`, a `javascript`
/// snippet keeps working in `.jsx` as it did, a `.js` file does not take
/// `javascriptreact` ones, and other languages' snippets stay out.
#[test]
fn jsx_snippet_scopes_leave_other_files_and_languages_alone() {
    assert_eq!(tab_after("App.tsx", "tfc"), "const : FC = () => null");
    assert_eq!(tab_after("App.jsx", "clg"), "console.log()");
    assert!(!tab_after("app.js", "rfc").contains("function"));
    assert!(!tab_after("App.jsx", "ifm").contains("__main__"));
}

#[test]
fn tab_expands_each_word_of_an_array_prefix() {
    assert_eq!(
        expand_with_array_prefix_snippets("fori"),
        "for i in range(10):"
    );
    assert_eq!(
        expand_with_array_prefix_snippets("for"),
        "for i in range(10):"
    );
    // The snippet beside it still expands too.
    assert_eq!(
        expand_with_array_prefix_snippets("main"),
        "if __name__ == '__main__':"
    );
}
