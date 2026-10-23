//! Vim-style modal editing.

use super::*;

/// #862: vim's `:q` and `:qa` ask like Cmd+W and Ctrl+Q; the `!` forms are
/// the answer given up front and discard without asking.
#[test]
fn vim_quits_ask_unless_forced() {
    let (mut app, tmp) = vim_app("one\n");
    let f = tmp.path().join("buf.txt");
    app.editor.lines[0] = String::from("edited");
    app.editor.dirty = true;
    app.vim_run_ex("qa");
    assert!(!app.quit);
    assert_eq!(app.pending_unsaved, Some(UnsavedExit::Quit));
    app.cancel_unsaved();
    app.vim_run_ex("q");
    assert!(matches!(
        app.pending_unsaved,
        Some(UnsavedExit::CloseTab { .. })
    ));
    app.cancel_unsaved();
    app.vim_run_ex("qa!");
    assert!(app.quit, ":qa! quits without asking");
    app.quit = false;
    app.vim_run_ex("q!");
    assert_eq!(app.pending_unsaved, None);
    assert_eq!(app.editor.path, None, ":q! closed the tab");
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "one\n");
}

/// Negative: Vim's `j` and `k` stop at the last and first line without
/// moving along it, as in Vim; the edge belongs to the arrow keys.
#[test]
fn vim_j_and_k_do_not_move_along_the_edge_lines() {
    let (_tmp, mut app) = app_with_run_script();
    app.vim.enabled = true;
    let last = app.editor.lines.len() - 1;
    app.editor.cursor_row = last;
    app.editor.cursor_col = 1;
    app.handle_key(key(KeyCode::Char('j'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (last, 1));
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 3;
    app.handle_key(key(KeyCode::Char('k'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (0, 3));
}

/// Guard (#843): Cmd+E (vim mode) gets no `Ctrl` form, because `Ctrl`+`E` is
/// the editor's end of line, as the shell's. It stays that, and never
/// toggles vim mode.
#[test]
fn ctrl_e_stays_end_of_line_and_never_toggles_vim_mode() {
    let mut app = editor_app_with_lines(&["hello world"]);
    app.editor.cursor_col = 0;
    app.handle_key(key(KeyCode::Char('e'), KeyModifiers::CONTROL))
        .unwrap();
    assert_eq!(app.editor.cursor_col, 11);
    assert!(!app.vim.enabled);
}

#[test]
fn a_count_before_a_closing_q_is_discarded_too() {
    // The stop gesture takes the `'q'` arm rather than the register branch,
    // so it needs its own take_count(): `3q` while recording must stop and
    // discard the 3, not leave it to retarget the next motion.
    let (mut app, tmp) = vim_app("aaaa\nbbbb\ncccc\ndddd\neeee\n");
    app.macros_path = tmp.path().join("macros.json");
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 0;
    vim_feed_str(&mut app, "qa");
    vim_feed_str(&mut app, "3q");
    assert!(
        app.macro_recording.is_none(),
        "3q still stops the recording"
    );
    vim_feed(&mut app, 'j');
    assert_eq!(
        app.editor.cursor_row, 1,
        "the discarded 3 must not make the next j move three rows"
    );
}

#[test]
fn a_count_before_q_does_not_leak_into_the_recording() {
    // Vim discards a count on `q`. Keeping it would silently retarget the
    // recording's FIRST key — `3qa` then `j` moves three rows instead of one,
    // and the wrong motion is what gets captured. A pending count is
    // invisible, so the corruption only surfaces at replay.
    let (mut app, tmp) = vim_app("aaaa\nbbbb\ncccc\ndddd\neeee\n");
    app.macros_path = tmp.path().join("macros.json");
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 0;
    vim_feed_str(&mut app, "3qa");
    vim_feed(&mut app, 'j');
    assert_eq!(
        app.editor.cursor_row, 1,
        "the stale 3 must not make the recording's first j move three rows"
    );
}

#[test]
fn vim_q_records_into_a_register_and_at_replays_it() {
    // #255: `qa … q` records, `@a` replays, and the register survives as the
    // "last macro" so `@@` repeats it.
    let (mut app, tmp) = vim_app("one\ntwo\nthree\n");
    app.macros_path = tmp.path().join("macros.json");
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 0;

    vim_feed_str(&mut app, "qa");
    assert!(
        app.macro_recording.is_some(),
        "qa starts recording into a register"
    );
    assert_eq!(
        app.macro_recording.as_ref().unwrap().register,
        Some('a'),
        "and remembers which one, for the status badge"
    );

    // Record "insert X at the line start, then go down a line".
    vim_feed_str(&mut app, "IX");
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    vim_feed(&mut app, 'j');
    vim_feed(&mut app, 'q');
    assert!(app.macro_recording.is_none(), "the closing q stops");
    assert_eq!(app.editor.lines[0], "Xone", "the recorded keys still ran");

    // Replay once on line two.
    vim_feed_str(&mut app, "@a");
    assert_eq!(app.editor.lines[1], "Xtwo", "@a replays the register");

    // `@@` repeats the same macro.
    vim_feed_str(&mut app, "@@");
    assert_eq!(app.editor.lines[2], "Xthree", "@@ repeats the last macro");
}

#[test]
fn vim_a_count_replays_the_macro_that_many_times() {
    // `3@a` runs it three times, and a count before `@` is not swallowed by
    // the register letter.
    let (mut app, tmp) = vim_app("a\nb\nc\nd\n");
    app.macros_path = tmp.path().join("macros.json");
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 0;

    vim_feed_str(&mut app, "qz");
    vim_feed_str(&mut app, "IX");
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    vim_feed(&mut app, 'j');
    vim_feed(&mut app, 'q');

    vim_feed_str(&mut app, "3@z");
    assert_eq!(
        (
            app.editor.lines[1].as_str(),
            app.editor.lines[2].as_str(),
            app.editor.lines[3].as_str()
        ),
        ("Xb", "Xc", "Xd"),
        "3@z ran the macro three times"
    );
}

#[test]
fn vim_dd_deletes_the_current_line() {
    let (mut app, _t) = vim_app("alpha\nbeta\ngamma");
    vim_feed_str(&mut app, "dd");
    assert_eq!(app.editor.lines, vec!["beta", "gamma"]);
}

#[test]
fn vim_count_dd_deletes_multiple_lines() {
    let (mut app, _t) = vim_app("a\nb\nc\nd");
    vim_feed_str(&mut app, "2dd");
    assert_eq!(app.editor.lines, vec!["c", "d"]);
}

#[test]
fn vim_x_deletes_char_under_cursor() {
    let (mut app, _t) = vim_app("hello");
    vim_feed(&mut app, 'x');
    assert_eq!(app.editor.lines, vec!["ello"]);
}

#[test]
fn vim_dw_deletes_word_and_trailing_space() {
    let (mut app, _t) = vim_app("foo bar");
    vim_feed_str(&mut app, "dw");
    assert_eq!(app.editor.lines, vec!["bar"]);
}

#[test]
fn vim_de_deletes_to_word_end_inclusive() {
    let (mut app, _t) = vim_app("foo bar");
    vim_feed_str(&mut app, "de");
    assert_eq!(app.editor.lines, vec![" bar"]);
}

/// #934: `cw` on a word changes it up to its end and keeps the blank after
/// it, as Vim's `ce`; on a word's last letter it changes only that letter,
/// a count spans that many words, and on a blank it is still `dw`'s span.
#[test]
fn vim_cw_keeps_the_blank_after_the_word() {
    for (start, keys, want) in [
        (0, "cwONE", "ONE two three"),
        (2, "cwE", "onE two three"),
        (0, "2cwX", "X three"),
        (3, "cwX", "oneXtwo three"),
    ] {
        let (mut app, _t) = vim_app("one two three");
        app.editor.cursor_col = start;
        vim_feed_str(&mut app, keys);
        app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.editor.lines, vec![want], "{keys} from column {start}");
    }
}

/// #934: a change and the text typed in its Insert session are one undo
/// step: `ciw`, `cw`, `cc`, `o`, and `i` with a line break typed.
#[test]
fn vim_u_undoes_a_change_and_its_typed_text_at_once() {
    for keys in ["wciwTWO", "cwONE", "ccNEW", "oNEW", "iA\rB"] {
        let (mut app, _t) = vim_app("one two three\nsecond line");
        for c in keys.chars() {
            let code = if c == '\r' {
                KeyCode::Enter
            } else {
                KeyCode::Char(c)
            };
            app.handle_key(key(code, KeyModifiers::NONE)).unwrap();
        }
        app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();
        vim_feed(&mut app, 'u');
        assert_eq!(
            app.editor.lines,
            vec!["one two three", "second line"],
            "one u after {keys}"
        );
    }
}

/// #934: a change entered on one tab and left with `Esc` on another ends
/// on both, so back on its tab an edit outside vim (a paste) is its own
/// step, and `u` takes back only it.
#[test]
fn a_change_left_on_another_tab_does_not_swallow_the_next_edit() {
    let (mut app, tmp) = vim_app("one two");
    vim_feed_str(&mut app, "cwONE");
    let other = tmp.path().join("other.txt");
    std::fs::write(&other, "x").unwrap();
    app.editor.open_pinned(&other).unwrap();
    assert_eq!(app.editor.active_index(), 1);
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    app.editor.select(0);
    app.editor.insert_str(" PASTE");
    assert_eq!(app.editor.lines, vec!["ONE PASTE two"]);
    vim_feed(&mut app, 'u');
    assert_eq!(app.editor.lines, vec!["ONE two"], "only the paste");
}

/// Negative (#934): the edit after `Esc` is its own step, so `u` takes
/// back only it and leaves the change.
#[test]
fn vim_an_edit_after_a_change_undoes_on_its_own() {
    let (mut app, _t) = vim_app("one two");
    vim_feed_str(&mut app, "cwONE");
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    vim_feed_str(&mut app, "xu");
    assert_eq!(app.editor.lines, vec!["ONE two"]);
    vim_feed(&mut app, 'u');
    assert_eq!(app.editor.lines, vec!["one two"]);
    // The undone step held `cw`'s selection: none is left to paint, and
    // `x` deletes one char, not the word.
    assert_eq!(app.editor.selection, None);
    vim_feed(&mut app, 'x');
    assert_eq!(app.editor.lines[0].chars().count(), 6);
}

/// #934: `.` is not implemented and says so instead of doing nothing.
#[test]
fn vim_dot_says_it_is_not_supported() {
    let (mut app, _t) = vim_app("a\nb\nc");
    vim_feed_str(&mut app, "dd.");
    assert_eq!(app.editor.lines, vec!["b", "c"]);
    assert_eq!(app.status, "Repeat (.) is not supported in vim mode");
}

#[test]
fn vim_diw_deletes_inner_word() {
    let (mut app, _t) = vim_app("foo bar baz");
    // cursor on "foo"
    vim_feed_str(&mut app, "diw");
    assert_eq!(app.editor.lines, vec![" bar baz"]);
}

#[test]
fn vim_dollar_then_d_deletes_to_end_of_line() {
    let (mut app, _t) = vim_app("hello world");
    vim_feed_str(&mut app, "wd$");
    assert_eq!(app.editor.lines, vec!["hello "]);
}

#[test]
fn vim_insert_types_text_then_esc_returns_to_normal() {
    let (mut app, _t) = vim_app("bc");
    vim_feed(&mut app, 'i');
    vim_feed(&mut app, 'a');
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.editor.lines, vec!["abc"]);
    assert_eq!(app.vim.mode(), crate::vim::VimMode::Normal);
}

#[test]
fn vim_append_inserts_after_cursor() {
    let (mut app, _t) = vim_app("ac");
    vim_feed(&mut app, 'a'); // insert after the 'a'
    vim_feed(&mut app, 'b');
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.editor.lines, vec!["abc"]);
}

#[test]
fn vim_open_line_below_enters_insert_on_fresh_line() {
    let (mut app, _t) = vim_app("first");
    vim_feed(&mut app, 'o');
    vim_feed_str(&mut app, "second");
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.editor.lines, vec!["first", "second"]);
}

#[test]
fn vim_yank_line_then_paste_duplicates_it() {
    let (mut app, _t) = vim_app("dup\ntail");
    vim_feed_str(&mut app, "yy");
    vim_feed(&mut app, 'p');
    assert_eq!(app.editor.lines, vec!["dup", "dup", "tail"]);
}

/// #933: where the host clipboard can't be read (Linux with no wl-paste,
/// xclip or xsel, as over SSH), `p` puts what croft itself last yanked or
/// deleted, so `yyp` duplicates a line and `ddp` moves one.
#[test]
fn vim_put_works_where_the_host_clipboard_cant_be_read() {
    let (mut app, _t) = vim_app("one\ntwo");
    app.clipboard_reader = || None;
    vim_feed_str(&mut app, "yyp");
    assert_eq!(app.editor.lines, vec!["one", "one", "two"]);
    vim_feed_str(&mut app, "Gddggp");
    assert_eq!(app.editor.lines, vec!["one", "two", "one"]);
}

/// #933: a `p` with nothing to put says so rather than doing nothing.
#[test]
fn vim_put_with_nothing_to_put_says_so() {
    let (mut app, _t) = vim_app("one");
    app.clipboard_reader = || None;
    vim_feed(&mut app, 'p');
    assert_eq!(app.editor.lines, vec!["one"]);
    assert_eq!(app.status, "Nothing to paste: the clipboard can't be read");
}

#[test]
fn vim_visual_select_then_delete_removes_selection() {
    let (mut app, _t) = vim_app("abcdef");
    vim_feed(&mut app, 'v'); // visual at col 0 (covers 'a')
    vim_feed(&mut app, 'l'); // extend to 'b'
    vim_feed(&mut app, 'l'); // extend to 'c'
    vim_feed(&mut app, 'd'); // delete a,b,c inclusive
    assert_eq!(app.editor.lines, vec!["def"]);
}

#[test]
fn vim_normal_mode_swallows_plain_letters() {
    let (mut app, _t) = vim_app("text");
    vim_feed(&mut app, 'z'); // unmapped in Normal: must not be inserted
    assert_eq!(app.editor.lines, vec!["text"]);
}

#[test]
fn vim_search_moves_cursor_to_match() {
    let (mut app, _t) = vim_app("alpha needle omega");
    vim_feed(&mut app, '/');
    vim_feed_str(&mut app, "needle");
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.editor.cursor_row, 0);
    assert_eq!(
        app.editor.cursor_col, 6,
        "cursor lands on the 'n' of needle"
    );
}

#[test]
fn vim_search_records_active_match_and_advances_with_n() {
    // Two matches on one line: "x needle y needle z" -> cols 2 and 11.
    let (mut app, _t) = vim_app("x needle y needle z");
    vim_feed(&mut app, '/');
    vim_feed_str(&mut app, "needle");
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(
        app.editor.active_search_match,
        Some((0, 2, 6)),
        "the match the cursor jumped to must be recorded as (row, col_chars, len_chars) so the renderer paints it orange while the other 'needle' stays yellow"
    );
    vim_feed(&mut app, 'n');
    assert_eq!(
        app.editor.active_search_match,
        Some((0, 11, 6)),
        "`n` advances the active match to the next occurrence"
    );
}

#[test]
fn vim_gg_and_capital_g_jump_between_ends() {
    let (mut app, _t) = vim_app("one\ntwo\nthree");
    app.handle_key(key(KeyCode::Char('G'), KeyModifiers::SHIFT))
        .unwrap();
    assert_eq!(app.editor.cursor_row, 2);
    vim_feed_str(&mut app, "gg");
    assert_eq!(app.editor.cursor_row, 0);
}

/// #1286: a saved `vim_mode` turns modal editing on, in Normal mode, the
/// way a launch applies it, and taking the pref back off turns it off.
#[test]
fn a_saved_vim_mode_turns_modal_editing_on_in_normal_mode() {
    let (mut app, _t) = vim_app("alpha one");
    app.vim.enabled = false;
    let on = crate::prefs::Prefs {
        vim_mode: true,
        ..Default::default()
    };
    app.apply_merged_settings(&on);
    assert!(app.vim.enabled, "the saved default applies");
    assert_eq!(app.vim.mode(), crate::vim::VimMode::Normal);
    vim_feed_str(&mut app, "dd");
    assert_eq!(app.editor.lines, vec![""], "dd deletes the line, not typed");
    app.apply_merged_settings(&crate::prefs::Prefs::default());
    assert!(!app.vim.enabled, "the pref taken off turns it off");
}

/// #1286: Toggle Vim Mode saves the choice, so the next launch starts in it.
#[test]
fn toggling_vim_mode_saves_it_as_the_default() {
    let (mut app, t) = vim_app("abc");
    app.vim.enabled = false;
    let croft = t.path().join("croft-config");
    std::fs::create_dir_all(&croft).unwrap();
    app.config_dir = croft.clone();
    let saved = || {
        crate::prefs::Prefs::load(&croft.join("config.json"))
            .expect("config written")
            .vim_mode
    };
    app.handle_key(key(KeyCode::Char('e'), KeyModifiers::SUPER))
        .unwrap();
    assert!(app.vim.enabled);
    assert!(saved(), "turning it on is saved");
    app.handle_key(key(KeyCode::Char('e'), KeyModifiers::SUPER))
        .unwrap();
    assert!(!saved(), "turning it off is saved too");
}

/// #923: with a `config.json` that does not parse, the toggle still works
/// for the session, but the status line says it was not saved and why,
/// rather than a plain "vim mode on" that read as saved.
#[test]
fn a_setting_refused_by_a_malformed_config_says_it_was_not_saved() {
    let (mut app, t) = vim_app("abc");
    app.vim.enabled = false;
    let croft = t.path().join("croft-config");
    std::fs::create_dir_all(&croft).unwrap();
    let broken = "{\n  \"auto_save\": true,\n  \"theme\": black\n}\n";
    std::fs::write(croft.join("config.json"), broken).unwrap();
    app.config_dir = croft.clone();
    app.handle_key(key(KeyCode::Char('e'), KeyModifiers::SUPER))
        .unwrap();
    assert!(app.vim.enabled, "the toggle still applies to the session");
    assert!(
        app.status.starts_with("Setting not saved: "),
        "{}",
        app.status
    );
    assert!(app.status.contains("line 3"), "{}", app.status);
    assert_eq!(
        std::fs::read_to_string(croft.join("config.json")).unwrap(),
        broken,
        "the user's file is left for them to fix"
    );
}

/// #1286 negative: with the vim extension disabled a saved `vim_mode`
/// leaves modal editing off, as Toggle Vim Mode itself does.
#[test]
fn a_disabled_vim_extension_keeps_a_saved_vim_mode_off() {
    let (mut app, _t) = vim_app("abc");
    app.vim.enabled = false;
    app.disabled_extensions.insert(String::from("vim"));
    app.apply_merged_settings(&crate::prefs::Prefs {
        vim_mode: true,
        ..Default::default()
    });
    assert!(!app.vim.enabled);
}

#[test]
fn vim_toggle_works_from_any_pane_even_with_no_editor_focus() {
    let (mut app, _t) = vim_app("abc");
    app.vim.enabled = false;
    // Focus the file tree, not the editor.
    app.focus = Pane::Tree;
    app.handle_key(key(KeyCode::Char('e'), KeyModifiers::SUPER))
        .unwrap();
    assert!(
        app.vim.enabled,
        "Cmd+E must toggle vim mode globally, from any pane, so it can be pre-armed before opening or focusing a file"
    );
    app.handle_key(key(KeyCode::Char('e'), KeyModifiers::SUPER))
        .unwrap();
    assert!(
        !app.vim.enabled,
        "a second Cmd+E from the tree toggles it back off"
    );
}

#[test]
fn vim_toggle_off_clears_search_highlights() {
    let (mut app, _t) = vim_app("x needle y needle z");
    vim_feed(&mut app, '/');
    vim_feed_str(&mut app, "needle");
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert!(
        app.editor.search_highlight.is_some(),
        "precondition: a vim search leaves the match-highlight term set"
    );
    assert!(app.editor.active_search_match.is_some());
    // Toggling vim mode off with Cmd+E is the only way to dismiss vim search
    // highlights, so it must clear both the highlight term and the active match.
    app.handle_key(key(KeyCode::Char('e'), KeyModifiers::SUPER))
        .unwrap();
    assert!(!app.vim.enabled);
    assert!(
        app.editor.search_highlight.is_none(),
        "toggling vim off must clear the search-match highlight term so no matches stay painted"
    );
    assert!(
        app.editor.active_search_match.is_none(),
        "toggling vim off must clear the active-match marker too"
    );
}

#[test]
fn vim_toggle_off_restores_plain_typing() {
    let (mut app, _t) = vim_app("");
    // Cmd+E turns modal editing off.
    app.handle_key(key(KeyCode::Char('e'), KeyModifiers::SUPER))
        .unwrap();
    assert!(!app.vim.enabled);
    vim_feed_str(&mut app, "hi");
    assert_eq!(app.editor.lines, vec!["hi"]);
}

#[test]
fn vim_toggle_on_with_cmd_e_enables_modal_editing() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("buf.txt");
    std::fs::write(&f, "abc").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&f).unwrap();
    app.focus = Pane::Editor;
    // Cmd+E from a plain editor turns modal editing on.
    app.handle_key(key(KeyCode::Char('e'), KeyModifiers::SUPER))
        .unwrap();
    assert!(app.vim.enabled);
    assert_eq!(app.vim.mode(), crate::vim::VimMode::Normal);
    // A plain letter is now a Normal-mode command, not inserted text.
    vim_feed(&mut app, 'x');
    assert_eq!(app.editor.lines, vec!["bc"]);
}

#[test]
fn vim_ex_goto_line_number() {
    let (mut app, _t) = vim_app("l1\nl2\nl3\nl4");
    vim_feed(&mut app, ':');
    vim_feed(&mut app, '3');
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.editor.cursor_row, 2);
}

#[test]
fn vim_cmd_save_shortcut_still_works_in_normal_mode() {
    // A Cmd-modified key must pass through to the editor's own handling
    // rather than being swallowed by Normal mode.
    let (mut app, _t) = vim_app("data");
    vim_feed(&mut app, 'x'); // edit so the buffer is dirty: "ata"
    app.handle_key(key(KeyCode::Char('s'), KeyModifiers::SUPER))
        .unwrap();
    assert_eq!(app.editor.lines, vec!["ata"]);
    assert!(
        !app.editor.dirty,
        "Cmd+S should have saved through Normal mode"
    );
}

/// A file of three functions with a symbol tab on the middle one, `b`
/// (lines 3 to 5), active, and vim on.
fn vim_symbol_tab_on_b(tmp: &tempfile::TempDir) -> App {
    let file = tmp.path().join("three.rs");
    std::fs::write(
        &file,
        "fn a() {\n    1\n}\nfn b() {\n    2\n}\nfn c() {\n    3\n}",
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&file).unwrap();
    app.editor.cursor_row = 4;
    app.run_command(crate::widgets::command_palette::Command::OpenAsSymbolTab);
    app.sync_symbol_views();
    let view = app.editor.symbol_view.as_ref().expect("a symbol tab on b");
    assert_eq!((view.first, view.last), (3, 5));
    app.focus = Pane::Editor;
    app.vim.enabled = true;
    app
}

/// #369: vim operators in a symbol tab stay inside its symbol. Upward
/// (`dgg`, `d5k`, `db` at its first column) they used to reach the hidden
/// lines above it, downward (`dG`, `dw` at its end) the ones below; inside
/// it they still delete what they cover.
#[test]
fn vim_operators_in_a_symbol_tab_stay_inside_its_symbol() {
    let a = ["fn a() {", "    1", "}"];
    let c = ["fn c() {", "    3", "}"];
    let whole = [&a[..], &["fn b() {", "    2", "}"], &c[..]].concat();
    // (keys, caret, the buffer after)
    let cases: [(&str, (usize, usize), Vec<&str>); 5] = [
        ("dgg", (4, 0), [&a[..], &["}"], &c[..]].concat()),
        ("d5k", (4, 0), [&a[..], &["}"], &c[..]].concat()),
        ("db", (3, 0), whole.clone()),
        ("dG", (4, 0), [&a[..], &["fn b() {"], &c[..]].concat()),
        (
            "dw",
            (5, 0),
            [&a[..], &["fn b() {", "    2", ""], &c[..]].concat(),
        ),
    ];
    for (keys, (row, col), after) in cases {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = vim_symbol_tab_on_b(&tmp);
        app.editor.cursor_row = row;
        app.editor.cursor_col = col;
        vim_feed_str(&mut app, keys);
        assert_eq!(app.editor.lines, after, "{keys}");
    }
}

/// #369: a count on a line operator stops at the symbol's last line, and
/// the status reports the lines it covered.
#[test]
fn a_vim_line_count_in_a_symbol_tab_stops_at_its_last_line() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = vim_symbol_tab_on_b(&tmp);
    app.editor.cursor_row = 4;
    app.editor.cursor_col = 0;
    vim_feed_str(&mut app, "5yy");
    assert_eq!(app.status, "Yanked 2 line(s)");
}

/// #1580: vim mode keeps `Ctrl`+`N` only in the editor, where vim reads
/// it; with the side bar focused the chord still opens an untitled tab.
#[test]
fn vim_mode_leaves_ctrl_n_to_the_side_bar() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "b.txt", "keep\n");
    app.vim.enabled = true;
    app.focus_pane(Pane::Tree);
    let ctrl_or_cmd = new_untitled_chords()[0];

    app.handle_key(key(KeyCode::Char('n'), ctrl_or_cmd))
        .unwrap();

    assert_eq!(app.editor.tab_count(), 2, "{ctrl_or_cmd:?}");
    let label = app.editor.tab_display_label(app.editor.active_index());
    assert_eq!(label, "Untitled-1");
}
