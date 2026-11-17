//! The terminal pane's own tests, moved out of terminal.rs (#2062).

use super::*;

#[test]
fn a_replaced_binary_still_names_a_runnable_path() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("croft");
    std::fs::write(&bin, b"").unwrap();
    let deleted = std::path::PathBuf::from(format!("{} (deleted)", bin.display()));
    assert_eq!(live_exe(deleted), Some(bin.clone()));
    assert_eq!(live_exe(bin.clone()), Some(bin));
    assert_eq!(live_exe(dir.path().join("gone (deleted)")), None);
}

/// The zsh the shim tests drive, or an ANNOUNCED skip on a machine
/// without one (#52). CI provisions zsh, so the gate holds where it
/// matters; on a zsh-less dev box these tests must skip with a reason
/// — a hard spawn failure (or a silent `return`) makes "suite green"
/// ambiguous. Same convention as the cross-compile and PDF gates.
fn zsh_or_skip() -> Option<&'static str> {
    let zsh = "/bin/zsh";
    if std::path::Path::new(zsh).exists() {
        Some(zsh)
    } else {
        eprintln!("SKIPPED: {zsh} not installed on this machine");
        None
    }
}

fn fresh_term(cols: usize, rows: usize) -> Term<VoidListener> {
    let cfg = Config {
        scrolling_history: 1000,
        ..Config::default()
    };
    let size = TermSize::new(cols, rows);
    Term::new(cfg, &size, VoidListener::default())
}

fn feed(term: &mut Term<VoidListener>, bytes: &[u8]) {
    let mut p = Processor::<StdSyncHandler>::new();
    p.advance(term, bytes);
}

#[test]
fn block_selection_highlights_and_extracts_a_rectangle() {
    let mut term = fresh_term(20, 5);
    feed(&mut term, b"alpha1\r\nbravo2\r\ncharlie3\r\n");
    // A rectangle over rows 0..=2, cols 1..=3, anchored bottom-right so
    // the bounds must come from independent min/max, not the row-major
    // normalisation linear selections use.
    let sel = Selection {
        anchor: (2, 3),
        head: (0, 1),
        block: true,
    };
    let (rl, cl, rh, ch) = sel.block_bounds();
    assert_eq!((rl, cl, rh, ch), (0, 1, 2, 3));
    assert!(cell_in_block_selection(1, 2, rl, cl, rh, ch));
    assert!(!cell_in_block_selection(1, 0, rl, cl, rh, ch));
    assert!(!cell_in_block_selection(1, 4, rl, cl, rh, ch));
    assert!(!cell_in_block_selection(3, 2, rl, cl, rh, ch));
    assert_eq!(
        block_selection_text(&term, rl, cl as usize, rh, ch as usize),
        "lph\nrav\nhar",
        "a block selection copies the same column slice from every row"
    );
}

#[test]
fn annotations_anchor_to_content_and_ride_the_scrollback() {
    let tmp = tempfile::tempdir().unwrap();
    let script = "n=note; echo ${n}-worthy-line; read x; i=0; while [ $i -lt 30 ]; do echo fill-$i; i=$((i+1)); done; sleep 30";
    let mut term =
        PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script.into()], tmp.path())
            .unwrap();
    term.last_inner = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 24,
    };
    let mut waited = 0u32;
    while !term
        .grid_lines()
        .0
        .iter()
        .any(|l| l.starts_with("note-worthy-line"))
    {
        assert!(waited < 8000, "line never arrived");
        std::thread::sleep(std::time::Duration::from_millis(20));
        waited += 20;
    }
    let (lines, top) = term.grid_lines();
    let line = top
        + lines
            .iter()
            .position(|l| l.starts_with("note-worthy-line"))
            .unwrap() as i32;
    let clock = term.scroll_clock();
    term.add_annotation(line, clock, 0, 16, String::from("this is where it broke"));
    // The screen cell over the span resolves to the note.
    let (idx, text) = term
        .annotation_at(3, line as u16)
        .expect("the annotated cell must resolve");
    assert_eq!(idx, 0);
    assert_eq!(text, "this is where it broke");
    assert!(
        term.annotation_at(30, line as u16).is_none(),
        "cells past the span carry no note"
    );

    // 30 more lines push the annotated row toward (or into) history;
    // the anchor drifts with the content.
    term.write_input(b"\n");
    let mut waited = 0u32;
    while !term.grid_lines().0.iter().any(|l| l.starts_with("fill-29")) {
        assert!(waited < 8000, "filler never arrived");
        std::thread::sleep(std::time::Duration::from_millis(20));
        waited += 20;
    }
    // The reader thread can still be scrolling (the newline after
    // `fill-29`) between reading the grid and reading the anchors, so
    // compare the two only once they come from the same screen.
    let mut waited = 0u32;
    let (expect_line, cur) = loop {
        let (lines, top) = term.grid_lines();
        let expect_line = top
            + lines
                .iter()
                .position(|l| l.starts_with("note-worthy-line"))
                .unwrap() as i32;
        let cur = term.annotations_current();
        if (cur.len() == 1 && cur[0].0 == expect_line) || waited >= 2000 {
            break (expect_line, cur);
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
        waited += 20;
    };
    assert_eq!(cur.len(), 1);
    assert_eq!(
        cur[0].0, expect_line,
        "the annotation must still sit on its content line"
    );
    assert_eq!(cur[0].3, "this is where it broke");
}

#[test]
fn prompt_click_arrows_map_a_click_to_cursor_motion() {
    let tmp = tempfile::tempdir().unwrap();
    // A prompt with 133;A/B marks and typed text, cursor parked after
    // the text (no newline). `read` keeps the shell at the prompt.
    let script = "h=hello; printf '\\033]133;A\\007$ \\033]133;B\\007'; printf \"${h}-world\"; read x; sleep 30";
    let mut term =
        PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script.into()], tmp.path())
            .unwrap();
    term.last_inner = Rect {
        x: 1,
        y: 1,
        width: 80,
        height: 24,
    };
    let mut waited = 0u32;
    while !term
        .grid_lines()
        .0
        .iter()
        .any(|l| l.contains("hello-world"))
    {
        assert!(waited < 8000, "prompt text never arrived");
        std::thread::sleep(std::time::Duration::from_millis(20));
        waited += 20;
    }
    let (lines, top) = term.grid_lines();
    let vrow = (lines
        .iter()
        .position(|l| l.contains("hello-world"))
        .unwrap() as i32
        + top) as u16; // display_offset 0: grid line == viewport row
    // Click on the 'h' (grid col 2): the cursor sits after 'd' (col 13),
    // so 11 left-arrows bring it there.
    let bytes = term
        .prompt_click_arrows(1 + 2, 1 + vrow)
        .expect("a prompt-row click must produce motion");
    assert_eq!(bytes, b"\x1b[D".repeat(11));
    // Clamped left of the input start: col 0 is the "$ " prompt glyphs.
    let bytes = term.prompt_click_arrows(1, 1 + vrow).unwrap();
    assert_eq!(bytes, b"\x1b[D".repeat(11), "clamps to the 133;B column");
    // A click on the cursor cell itself is a no-op.
    assert!(term.prompt_click_arrows(1 + 13, 1 + vrow).is_none());
    // A click on another row does nothing.
    assert!(term.prompt_click_arrows(1 + 2, 1 + vrow + 1).is_none());
}

#[test]
fn row_times_record_when_each_line_arrived() {
    // Deterministic (#170): a loaded scheduler can hold the reader
    // past the writer's sleep, coalescing both echoes into ONE read
    // that stamp_chunk marks with a single now_ms — so no real-PTY
    // timing assertion can pin per-chunk stamps. Drive the stamping
    // path directly instead, one chunk per arrival.
    let (_tmp, t) = quiet_pty();
    let mut prev = 0i64;
    feed_pty(&t, b"first-marker\r\n");
    t.stamp_chunk_for_test(&mut prev, 1000);
    feed_pty(&t, b"second-marker\r\n");
    t.stamp_chunk_for_test(&mut prev, 1400);
    let (lines, top) = t.grid_lines();
    let a = lines
        .iter()
        .position(|l| l.starts_with("first-marker"))
        .expect("first line on the grid");
    let b = lines
        .iter()
        .position(|l| l.starts_with("second-marker"))
        .expect("second line on the grid");
    let ta = t
        .row_time(top + a as i32)
        .expect("the first line must be stamped");
    let tb = t
        .row_time(top + b as i32)
        .expect("the second line must be stamped");
    assert_eq!(ta, 1000, "first row keeps its own chunk's arrival time");
    assert_eq!(tb, 1400, "second row gets the later chunk's arrival time");
}

#[test]
fn reader_thread_stamps_arriving_rows() {
    // End-to-end: the real reader loop must feed stamp_chunk. Only
    // presence and ordering are asserted — any gap assertion races
    // the scheduler (#170).
    let tmp = tempfile::tempdir().unwrap();
    let script = "a=first; echo ${a}-marker; sleep 30";
    let term =
        PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script.into()], tmp.path())
            .unwrap();
    let mut waited = 0u32;
    loop {
        let (lines, top) = term.grid_lines();
        if let Some(a) = lines.iter().position(|l| l.starts_with("first-marker")) {
            assert!(
                term.row_time(top + a as i32).is_some(),
                "the reader thread must stamp rows it delivers"
            );
            break;
        }
        assert!(waited < 8000, "marker never arrived");
        std::thread::sleep(std::time::Duration::from_millis(20));
        waited += 20;
    }
}

#[test]
fn resize_reflows_long_lines_instead_of_truncating() {
    // Scrollback soft-reflow is an engine property (alacritty_terminal's
    // Term::resize reflows the normal screen); this pins it so a future
    // engine upgrade can't silently lose it.
    let mut term = fresh_term(30, 5);
    feed(&mut term, b"abcdefghijklmnopqrstuvwxyz1234\r\n");
    term.resize(TermSize::new(10, 5));
    let top = term.grid().topmost_line().0;
    let bottom = term.screen_lines() as i32 - 1;
    let mut all = String::new();
    for l in top..=bottom {
        let (s, _) = row_text_and_cols(&term, l);
        all.push_str(s.trim_end());
    }
    assert!(
        all.contains("abcdefghijklmnopqrstuvwxyz1234"),
        "text must survive a narrowing resize by reflowing: {all:?}"
    );
    // Widening back re-joins the soft-wrapped rows onto one line.
    term.resize(TermSize::new(40, 5));
    let joined: Vec<String> = (term.grid().topmost_line().0..term.screen_lines() as i32)
        .map(|l| row_text_and_cols(&term, l).0.trim_end().to_string())
        .collect();
    assert!(
        joined
            .iter()
            .any(|r| r.contains("abcdefghijklmnopqrstuvwxyz1234")),
        "widening must re-join the reflowed rows: {joined:?}"
    );
}

/// Every row of the model, scrollback first, trailing blanks trimmed.
fn all_rows(term: &Term<VoidListener>) -> Vec<String> {
    (term.grid().topmost_line().0..term.screen_lines() as i32)
        .map(|l| row_text_and_cols(term, l).0.trim_end().to_string())
        .collect()
}

fn resize_to(term: &mut Term<VoidListener>, cols: usize) {
    resize_term(term, TermSize::new(cols, 6));
}

const PANEL: [&str; 3] = [
    "╭──────────── Assistant ─────╮",
    "│ a reply laid out to fit 30 │",
    "╰────────────────────────────╯",
];

/// [`PANEL`] clipped to 12 columns.
const PANEL_AT_12: [&str; 3] = ["╭───────────", "│ a reply la", "╰───────────"];

/// A 30-column pane showing [`PANEL`], then `after`.
fn panel_then(after: &str) -> Term<VoidListener> {
    let mut term = fresh_term(30, 6);
    let bytes = format!("{}\r\n{after}", PANEL.join("\r\n"));
    feed(&mut term, bytes.as_bytes());
    term
}

/// A panel printed to the pane's width keeps one row per row when the
/// pane narrows, and comes back whole when it widens (#1857).
#[test]
fn narrowing_clips_a_panel_and_widening_brings_it_back() {
    let mut term = panel_then("$ ");
    resize_to(&mut term, 12);
    let rows = all_rows(&term);
    assert_eq!(rows[..3], PANEL_AT_12, "{rows:?}");
    assert_eq!(rows[3], "$");
    // Through a narrower step and a wider one that still cuts it.
    for cols in [8, 20, 30] {
        resize_to(&mut term, cols);
    }
    assert_eq!(all_rows(&term)[..3], PANEL, "{:?}", all_rows(&term));
}

/// The cut cells ride the row into scrollback as output scrolls it off.
#[test]
fn a_clipped_row_keeps_its_cells_in_scrollback() {
    let mut term = panel_then("");
    resize_to(&mut term, 12);
    feed(&mut term, b"1\r\n2\r\n3\r\n4\r\n5\r\n6\r\n$ ");
    let rows = all_rows(&term);
    assert_eq!(
        rows[..4],
        [PANEL_AT_12[0], PANEL_AT_12[1], PANEL_AT_12[2], "1"]
    );
    resize_to(&mut term, 30);
    let rows = all_rows(&term);
    assert_eq!(rows[..3], PANEL, "{rows:?}");
}

/// A row a program rewrote while clipped is its new content alone:
/// widening must not bring the old row's cut-off end back after it.
#[test]
fn a_row_rewritten_while_clipped_drops_what_was_cut() {
    let mut term = panel_then("$ ");
    resize_to(&mut term, 12);
    feed(&mut term, b"\x1b[2;1H\x1b[2Knew\x1b[4;3H");
    resize_to(&mut term, 30);
    let rows = all_rows(&term);
    assert_eq!(rows[..3], [PANEL[0], "new", PANEL[2]], "{rows:?}");
}

/// Lines the pane soft-wrapped, and the row the cursor is on, reflow
/// as before: they are text, not a layout.
#[test]
fn soft_wrapped_lines_and_the_cursor_row_still_reflow() {
    let (long, typed) = (
        "abcdefghijklmnopqrstuvwxyz0123456789",
        "$ echo 0123456789abc",
    );
    let mut term = fresh_term(30, 6);
    feed(&mut term, format!("{long}\r\n{typed}").as_bytes());
    resize_to(&mut term, 10);
    assert_eq!(all_rows(&term).concat(), format!("{long}{typed}"));
    resize_to(&mut term, 40);
    let rows = all_rows(&term);
    assert_eq!(rows[..2], [long, typed], "{rows:?}");
}

/// A wide char the new edge would split is cut whole, and comes back.
#[test]
fn a_wide_char_at_the_cut_comes_back_whole() {
    let row = "123456789日本 and the rest";
    let mut term = fresh_term(30, 6);
    feed(&mut term, format!("{row}\r\n").as_bytes());
    resize_to(&mut term, 10);
    assert_eq!(all_rows(&term)[..2], ["123456789", ""]);
    resize_to(&mut term, 30);
    assert_eq!(all_rows(&term)[0], row);
}

/// A pane losing lines as it narrows drops the rows below the cursor's,
/// and what was cut off them goes too, not onto the panel above.
#[test]
fn rows_a_shorter_pane_drops_take_their_cut_cells_with_them() {
    let mut term = fresh_term(30, 6);
    let bytes = format!(
        "1\r\n{}\r\n$ \r\na row below the cursor\x1b[5;3H",
        PANEL.join("\r\n")
    );
    feed(&mut term, bytes.as_bytes());
    resize_term(&mut term, TermSize::new(12, 3));
    let rows = all_rows(&term);
    assert_eq!(
        rows,
        ["1", PANEL_AT_12[0], PANEL_AT_12[1], PANEL_AT_12[2], "$"]
    );
    resize_to(&mut term, 30);
    let rows = all_rows(&term);
    assert_eq!(
        rows[..5],
        ["1", PANEL[0], PANEL[1], PANEL[2], "$"],
        "{rows:?}"
    );
    assert!(rows[5..].iter().all(String::is_empty), "{rows:?}");
}

#[test]
fn scroll_offset_centers_a_scrollback_match_and_clamps_to_the_range() {
    // 24-row pane, 100 lines of scrollback (max_off 100). A match on an
    // old line (-80) centers it: 12 - (-80) = 92, within [0, 100].
    assert_eq!(scroll_offset_for_line(24, 100, -80), 92);
    // A match on the live screen (line 20) wants a negative offset but is
    // clamped to 0 — you cannot scroll below the live bottom.
    assert_eq!(scroll_offset_for_line(24, 100, 20), 0);
    // A match older than the deepest scrollback is clamped to max_off.
    assert_eq!(scroll_offset_for_line(24, 100, -200), 100);
    // No scrollback (alt screen / fresh grid): always pinned to bottom.
    assert_eq!(scroll_offset_for_line(24, 0, -5), 0);
}

#[test]
fn mouse_report_encoding_matches_the_xterm_wire_format() {
    let none = MouseMods::default();
    let shift = MouseMods {
        shift: true,
        ..MouseMods::default()
    };
    // SGR: 1-based coords, 'M' press / 'm' release, wheel = 64/65.
    assert_eq!(
        encode_mouse_report(true, MouseButtonKind::Left, MouseAction::Press, 0, 0, none),
        b"\x1b[<0;1;1M"
    );
    assert_eq!(
        encode_mouse_report(
            true,
            MouseButtonKind::Left,
            MouseAction::Release,
            4,
            2,
            none
        ),
        b"\x1b[<0;5;3m"
    );
    assert_eq!(
        encode_mouse_report(
            true,
            MouseButtonKind::WheelDown,
            MouseAction::Press,
            9,
            9,
            none
        ),
        b"\x1b[<65;10;10M"
    );
    // Motion adds 32; Shift adds 4.
    assert_eq!(
        encode_mouse_report(
            true,
            MouseButtonKind::Left,
            MouseAction::Motion,
            0,
            0,
            shift
        ),
        b"\x1b[<36;1;1M"
    );
    // Legacy X10: 0x20-based bytes, release collapses the button to code 3.
    assert_eq!(
        encode_mouse_report(false, MouseButtonKind::Left, MouseAction::Press, 0, 0, none),
        &[0x1b, b'[', b'M', 32, 33, 33]
    );
    assert_eq!(
        encode_mouse_report(
            false,
            MouseButtonKind::Left,
            MouseAction::Release,
            0,
            0,
            none
        ),
        &[0x1b, b'[', b'M', 35, 33, 33]
    );
}

#[test]
fn dsr_cursor_position_query_round_trips_into_the_pty_response_channel() {
    // Regression for atuin reverse-search (Ctrl+R) panicking with
    // "Error: The cursor position could not be read within a normal
    // duration": atuin sends DSR (`ESC[6n`) and waits for the
    // terminal to write back `ESC[<row>;<col>R` on stdin. Before
    // this fix, croft's `VoidListener` discarded every event
    // alacritty emitted, so the reply never reached the shell and
    // atuin timed out. With the responder wiring in place, the
    // listener must publish the reply on the channel that the
    // background thread forwards to the PTY master.
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let listener = VoidListener {
        pty_response_tx: Some(tx),
        size: Some(Arc::new(std::sync::Mutex::new((80, 24)))),
        bell: None,
        clipboard: None,
    };
    let cfg = Config::default();
    let size = TermSize::new(80, 24);
    let mut term = Term::new(cfg, &size, listener);
    let mut processor = Processor::<StdSyncHandler>::new();
    processor.advance(&mut term, b"\x1b[6n");
    let response = rx
        .recv_timeout(std::time::Duration::from_millis(500))
        .expect("DSR cursor-position query must produce a PtyWrite event");
    assert_eq!(
        response, "\x1b[1;1R",
        "a fresh terminal's cursor is at row 1, column 1 (1-based), so the reply must be ESC[1;1R"
    );
}

/// OSC 7501: Claude Code reports its status only once the terminal
/// answers the probe. It sends a device-attributes query right behind
/// it, which every terminal answers, so the probe's answer must reach
/// the program FIRST: the first ten bytes it reads back are the answer.
#[test]
fn the_osc_7501_probe_is_answered_ahead_of_the_device_attributes_reply() {
    let tmp = tempfile::tempdir().unwrap();
    let script = r#"stty raw -echo; printf '\033]7501;?\033\\\033[c'; r=$(head -c 10 | od -An -tx1 | tr -d ' \n'); stty sane; printf 'got:%s\n' "$r"; sleep 30"#;
    let term = PtyTerminal::new_running(
        "/bin/sh",
        &[String::from("-c"), String::from(script)],
        tmp.path(),
    )
    .unwrap();
    crate::test_budget::await_spawned(
        crate::test_budget::tests::SHELL_PAINT_BASE,
        "the probe's answer to be read back",
        // Not bare `got:`: the pane's header line echoes the script.
        || term.visible_text().contains("got:1b"),
    );
    assert!(
        term.visible_text().contains("got:1b5d373530313b3f1b5c"),
        "{}",
        term.visible_text()
    );
}

#[test]
fn text_area_size_request_replies_with_the_current_grid_geometry() {
    // CSI 18 t ("report text area size in characters") is what
    // helix, neovim, and other modern TUIs use to learn the
    // viewport size beyond what TIOCGWINSZ surfaces. The reply
    // must echo the live cols/rows held in `size_shared` so a
    // resize inside croft propagates to the embedded program
    // without a fresh listener instance.
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let size = Arc::new(std::sync::Mutex::new((120u16, 40u16)));
    let listener = VoidListener {
        pty_response_tx: Some(tx),
        size: Some(size),
        bell: None,
        clipboard: None,
    };
    let cfg = Config::default();
    let term_size = TermSize::new(120, 40);
    let mut term = Term::new(cfg, &term_size, listener);
    let mut processor = Processor::<StdSyncHandler>::new();
    processor.advance(&mut term, b"\x1b[18t");
    let response = rx
        .recv_timeout(std::time::Duration::from_millis(500))
        .expect("CSI 18 t must produce a TextAreaSizeRequest reply on the channel");
    assert_eq!(
        response, "\x1b[8;40;120t",
        "CSI 18 t reply format is ESC[8;<rows>;<cols>t per xterm convention"
    );
}

#[test]
fn interactive_shell_invocation_passes_login_flag_for_zsh() {
    let (program, args) = interactive_shell_invocation("/bin/zsh");
    assert_eq!(program, "/bin/zsh");
    assert_eq!(
        args,
        vec!["-l".to_string()],
        "zsh must be spawned as a login shell so ~/.zprofile is sourced and the embedded terminal inherits the same env / keybindings as the user's native iTerm2 shell"
    );
}

#[test]
fn interactive_shell_invocation_passes_login_flag_for_bash_fish_ksh() {
    for path in [
        "/bin/bash",
        "/usr/local/bin/fish",
        "/bin/ksh",
        "/usr/bin/tcsh",
    ] {
        let (_, args) = interactive_shell_invocation(path);
        assert_eq!(
            args,
            vec!["-l".to_string()],
            "{} must be spawned with -l so its login-shell rc files are sourced",
            path
        );
    }
}

#[test]
fn interactive_shell_invocation_skips_login_flag_for_unknown_shells() {
    let (_, args) = interactive_shell_invocation("/opt/exotic/myshell");
    assert!(
        args.is_empty(),
        "an unknown shell must be spawned without -l in case the flag means something else there"
    );
}

#[test]
fn a_large_write_to_a_child_not_reading_never_blocks_the_caller() {
    let tmp = tempfile::tempdir().unwrap();
    let mut term = PtyTerminal::new_running(
        "/bin/sh",
        &[String::from("-c"), String::from("stty raw -echo; sleep 3")],
        tmp.path(),
    )
    .unwrap();
    let big = vec![b'x'; 1 << 20];
    let started = std::time::Instant::now();
    term.write_input(&big);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(1),
        "the write went to the pane's writer thread, not the caller"
    );
}

#[test]
fn a_space_at_the_wrap_still_separates_words_for_redaction() {
    let tmp = tempfile::tempdir().unwrap();
    let mut term = PtyTerminal::new(tmp.path()).unwrap();
    term.resize(22, 10);
    // `Authorization: Bearer` is 21 chars: the space lands in column 22,
    // the last, and the token starts the next row.
    let token = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2lnbmF0dXJl";
    term.feed_bytes_for_test(format!("\x1b[2J\x1b[HAuthorization: Bearer {token}\r\n").as_bytes());
    let (lines, wraps, _) = term.grid_lines_wrapped();
    assert!(
        lines.concat().contains(&format!("Bearer {token}")),
        "{lines:?}"
    );
    let set = crate::triggers::TriggerSet::default().with_builtin_redactions();
    let masked = crate::triggers::mask_rows(&lines, &wraps, &set).concat();
    assert!(!masked.contains("c2lnbmF0dXJl"), "{masked}");
}

/// Find's logical lines join only rows that soft-wrap, and a position
/// maps back to the row that shows it, which is where the jump scrolls
/// (#1275).
#[test]
fn find_lines_join_wrapped_rows_and_map_a_position_to_its_row() {
    let rows = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let lines = join_wrapped_rows(
        rows(&["abc", "def", "gh", "solo", "x"]),
        &[true, true, false, false, false],
        -3,
    );
    assert_eq!(
        lines
            .iter()
            .map(|l| (l.text.as_str(), l.start))
            .collect::<Vec<_>>(),
        vec![("abcdefgh", -3), ("solo", 0), ("x", 1)]
    );
    assert_eq!(lines[0].row_lens, vec![3, 3, 2]);
    assert_eq!(lines[0].line_of(0), -3);
    assert_eq!(lines[0].line_of(3), -2);
    assert_eq!(lines[0].line_of(7), -1);
    assert_eq!(
        lines[0].line_of(99),
        -1,
        "past the end stays on the last row"
    );
    assert_eq!(lines[1].line_of(2), 0);
}

#[test]
fn a_secret_cut_by_a_soft_wrap_is_masked_on_both_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let mut term = PtyTerminal::new(tmp.path()).unwrap();
    term.resize(30, 10);
    let token = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
    term.feed_bytes_for_test(format!("\x1b[2J\x1b[Hexport GH={token}\r\n").as_bytes());
    let set = crate::triggers::TriggerSet::default().with_builtin_redactions();
    let (lines, wraps, _) = term.grid_lines_wrapped();
    let masked = crate::triggers::mask_rows(&lines, &wraps, &set);
    let all: String = masked.concat();
    assert!(
        !all.contains("ghp_") && !all.contains("0123456789"),
        "{masked:?}"
    );
    let t = term.term.lock();
    let first = (0..t.screen_lines() as i32)
        .find(|&l| row_text_and_cols(&t, l).0.contains("export"))
        .unwrap();
    let (logical, offset) = logical_row_text(&t, first + 1);
    assert!(logical.contains(token), "{logical}");
    assert_eq!(offset, 30);
}

#[test]
fn a_pane_gets_a_utf8_ctype_when_the_inherited_one_is_not() {
    let s = |v: &str| Some(v.to_string());
    // What macOS forwards over ssh, and no locale at all.
    assert_eq!(
        utf8_ctype_fix(None, s("UTF-8"), None),
        Some(("LC_CTYPE", "C.UTF-8"))
    );
    assert_eq!(
        utf8_ctype_fix(None, None, None),
        Some(("LC_CTYPE", "C.UTF-8"))
    );
    assert_eq!(
        utf8_ctype_fix(None, None, s("C")),
        Some(("LC_CTYPE", "C.UTF-8"))
    );
    assert_eq!(
        utf8_ctype_fix(s("UTF-8"), None, None),
        Some(("LC_ALL", "C.UTF-8"))
    );
    // Already UTF-8, or pinned by the user: untouched.
    assert_eq!(utf8_ctype_fix(None, None, s("en_US.UTF-8")), None);
    assert_eq!(utf8_ctype_fix(None, s("de_DE.utf8@euro"), None), None);
    assert_eq!(utf8_ctype_fix(s("POSIX"), None, None), None);
}

#[test]
fn a_paste_cannot_close_its_own_brackets() {
    let tmp = tempfile::tempdir().unwrap();
    let mut term = PtyTerminal::new(tmp.path()).unwrap();
    // The mode as the parser tracks it (the reader keeps one parser, so
    // a sequence split across reads lands too).
    term.feed_bytes_for_test(b"prompt \x1b[?1049;2004h");
    term.paste_input(b"ls\x1b[201~rm -rf ~\r");
    let written = term.written_bytes_for_test();
    let tail = &written[written.len() - b"\x1b[200~ls[201~rm -rf ~\r\x1b[201~".len()..];
    assert_eq!(tail, b"\x1b[200~ls[201~rm -rf ~\r\x1b[201~");
    // And no brackets once the program turns the mode off.
    term.feed_bytes_for_test(b"\x1b[?2004l");
    term.paste_input(b"x");
    assert!(term.written_bytes_for_test().ends_with(b"\x1b[201~x"));
}

#[test]
fn pty_starts_dirty_so_first_frame_renders() {
    let tmp = tempfile::tempdir().unwrap();
    let term = PtyTerminal::new(tmp.path()).unwrap();
    assert!(
        term.take_dirty(),
        "first take_dirty must be true so we draw the initial state"
    );
}

#[test]
fn take_dirty_clears_the_flag() {
    let tmp = tempfile::tempdir().unwrap();
    let term = PtyTerminal::new(tmp.path()).unwrap();
    let _ = term.take_dirty();
    assert!(
        !term.take_dirty(),
        "second take_dirty without new bytes must be false"
    );
}

#[test]
fn write_input_marks_dirty() {
    let tmp = tempfile::tempdir().unwrap();
    let mut term = PtyTerminal::new(tmp.path()).unwrap();
    let _ = term.take_dirty();
    term.write_input(b"echo hi\r");
    assert!(
        term.take_dirty(),
        "write_input must mark the terminal dirty"
    );
}

/// #357: the REAL reader thread records output into the rewind buffer.
///
/// The app-level test drives `feed_bytes_for_test`, a `#[cfg(test)]`
/// sibling hand-edited to mirror the reader. That proves the buffer works
/// but not that the production path uses it — deleting the reader's
/// `rb.push` leaves it green, which was measured, not assumed. This
/// spawns a real shell and reads what the reader thread actually stored.
#[test]
fn the_reader_thread_records_shell_output_for_rewind() {
    let tmp = tempfile::tempdir().unwrap();
    let mut term = PtyTerminal::new(tmp.path()).unwrap();
    // The needle only ever appears as command OUTPUT, so matching it in
    // the buffer proves the recorded bytes came through the pty rather
    // than from the echoed input line.
    term.write_input(b"echo REWIND_$(echo recorded)\n");
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(2000),
        "the shell to print the needle",
        || term.visible_text().contains("REWIND_recorded"),
    );

    let rb = term.rewind().lock().unwrap();
    let (_, frames) = rb.replay_from(u64::MAX);
    let seen: Vec<u8> = frames.iter().flat_map(|f| f.data.clone()).collect();
    assert!(
        String::from_utf8_lossy(&seen).contains("REWIND_recorded"),
        "the reader thread parsed output without recording it: {} bytes held",
        rb.bytes()
    );
}

/// #694: closing a pane frees its rewind buffer at once and leaves the
/// shared budget, rather than waiting on the last handle to drop.
///
/// Asserted on a clone of the handle, standing in for the reader thread's
/// copy: that clone is exactly what kept a closed pane's buffer alive.
#[test]
fn closing_a_pane_frees_its_rewind_buffer() {
    let tmp = tempfile::tempdir().unwrap();
    let mut term = PtyTerminal::new(tmp.path()).unwrap();
    term.write_input(b"echo FREED_$(echo soon)\n");
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(2000),
        "the shell to print the needle",
        || term.visible_text().contains("FREED_soon"),
    );
    let handle = term.rewind().clone();
    assert!(
        !handle.lock().unwrap().is_empty(),
        "precondition: the pane recorded output"
    );
    assert!(handle.lock().unwrap().capacity() > 0);

    drop(term);
    let rb = handle.lock().unwrap();
    assert!(rb.is_empty(), "a closed pane kept {} bytes", rb.bytes());
    assert_eq!(rb.capacity(), 0, "a closed pane kept its share");
}

/// Output arriving BEFORE an OSC 133 mark must be recorded too.
///
/// The reader splits each read at OSC events and advances the parser per
/// segment, leaving `done` at the LAST mark in the chunk. Recording
/// `buf[done..n]` therefore keeps only the tail, and with shell
/// integration on — marks arriving around every prompt and command — that
/// silently discards most of the session, which is the one thing this
/// buffer exists to keep.
///
/// `the_reader_thread_records_shell_output_for_rewind` cannot see this:
/// its script emits no marks, so `done` stays 0 and the tail happens to
/// BE the whole chunk. The fixture agrees with the implementation. This
/// test puts a mark between two needles so the two differ.
#[test]
fn output_before_an_osc_mark_is_recorded_not_just_the_tail() {
    let tmp = tempfile::tempdir().unwrap();
    let mut term = PtyTerminal::new(tmp.path()).unwrap();
    // Both needles must be command OUTPUT ONLY. The shell ECHOES the
    // input line before running it, and that echo arrives in one chunk
    // with no mark before it — so a needle written literally in the
    // command reaches the buffer via the echo whether or not the
    // recording under test works, and the test passes vacuously. `$(..)`
    // is unexpanded in the echo, so these two strings exist only in the
    // command's output. (The sibling test above documents this; I used it
    // as a template and dropped the property it exists to explain.)
    term.write_input(b"printf \"BEFORE_$(echo MARK)\\n\\033]133;C\\007AFTER_$(echo MARK)\\n\"\n");
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(2000),
        "the shell to print both needles",
        || {
            let v = term.visible_text();
            v.contains("BEFORE_MARK") && v.contains("AFTER_MARK")
        },
    );

    let rb = term.rewind().lock().unwrap();
    let (_, frames) = rb.replay_from(u64::MAX);
    let seen: Vec<u8> = frames.iter().flat_map(|f| f.data.clone()).collect();
    let text = String::from_utf8_lossy(&seen);
    // The tail alone is the bug: AFTER_MARK present, BEFORE_MARK dropped.
    // Asserting BOTH is what distinguishes the fix from the defect — the
    // AFTER_MARK half is the paired presence assertion that keeps the
    // BEFORE_MARK claim from passing over an empty buffer.
    assert!(
        text.contains("AFTER_MARK"),
        "nothing was recorded at all: {} bytes held",
        rb.bytes()
    );
    assert!(
        text.contains("BEFORE_MARK"),
        "output before the OSC mark was parsed but not recorded -- only \
             the tail after the last mark survived: {:?}",
        text
    );
}

/// Drop must REAP the shell on every path. portable-pty's `kill()` reaps
/// only when the shell dies inside its SIGHUP grace loop; a shell that
/// ignores HUP gets the SIGKILL escalation, which never waits — so every
/// such closed pane left a zombie for the life of the process.
#[test]
fn dropping_a_terminal_reaps_a_hup_ignoring_shell() {
    let tmp = tempfile::tempdir().unwrap();
    let mut term = PtyTerminal::new(tmp.path()).unwrap();
    let pid = term.pid().expect("spawned shell has a pid") as i32;
    // The needle only ever appears as command OUTPUT ($(..) is unexpanded
    // in the input echo), so matching it proves the trap line executed.
    term.write_input(b"trap '' HUP; echo TRAP_$(echo armed)\n");
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(1200),
        "the shell to confirm the HUP trap",
        || term.visible_text().contains("TRAP_armed"),
    );
    drop(term);
    let mut status = 0i32;
    let r = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
    assert!(
        r < 0,
        "the shell was left as an unreaped zombie (waitpid still found it: r={r})"
    );
}

/// #912: a hosted shell that exits while croft attaches can have its
/// exit arrive in the same read as the greeting. Its code is kept, and
/// the output before it still reaches the pane.
#[test]
fn an_exit_in_the_pane_hosts_greeting_keeps_its_code() {
    use crate::session_host::{Control, encode_bytes_frame, encode_control_frame};
    use std::io::{Read, Write};
    let tmp = tempfile::tempdir().unwrap();
    let socket = tmp.path().join("pane.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let host = std::thread::spawn(move || {
        let (mut conn, _) = listener.accept().unwrap();
        let mut hello = [0u8; 512];
        let _ = conn.read(&mut hello);
        let mut greeting = encode_control_frame(&Control::PaneInfo { pid: 4242 });
        greeting.extend(encode_bytes_frame(b"bye\r\n"));
        greeting.extend(encode_control_frame(&Control::Exit { code: 7 }));
        conn.write_all(&greeting).unwrap();
    });
    let mut link = PaneLink::attach(&socket, 80, 24).unwrap();
    host.join().unwrap();
    let mut out = Vec::new();
    link.reader.read_to_end(&mut out).unwrap();
    assert_eq!(out, b"bye\r\n");
    assert_eq!(link.backend.try_exit(true), Some(Some(7)));
}

/// #912: a shell that exits while its pane lives is reaped then, not
/// left a zombie until the pane is dropped, and its exit code goes
/// under its output.
#[test]
fn a_shell_that_exits_is_reaped_and_its_exit_code_shown() {
    let tmp = tempfile::tempdir().unwrap();
    let mut term = PtyTerminal::new(tmp.path()).unwrap();
    let pid = term.pid().expect("spawned shell has a pid") as i32;
    term.write_input(b"exit 3\n");
    let mut code = None;
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(1500),
        "the pane to notice its shell exited",
        || {
            code = term.poll_exit();
            code.is_some()
        },
    );
    assert_eq!(code, Some(Some(3)));
    assert!(term.has_exited());
    assert_eq!(term.poll_exit(), None, "the exit is reported once");
    let mut status = 0i32;
    let r = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
    assert!(r < 0, "the exited shell is still a zombie (r={r})");
    assert_eq!(term.pid(), None, "a reaped pid may name another process");
    let text = term.visible_text();
    assert!(
        text.contains("[Process exited with code 3]"),
        "no exit footer:\n{text}"
    );
}

/// Drop must return promptly even when a background job keeps the pty
/// slave open: on Linux the reader's blocked `read` never returns then
/// (no tty revoke like macOS), so joining it would freeze the UI until
/// the job exits. The shutdown pipe is what wakes the reader.
#[test]
fn a_background_job_cannot_stall_a_dropped_pane() {
    let tmp = tempfile::tempdir().unwrap();
    let mut term = PtyTerminal::new(tmp.path()).unwrap();
    term.write_input(b"sleep 30 &\n");
    std::thread::sleep(std::time::Duration::from_millis(1500));
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        drop(term);
        let _ = tx.send(());
    });
    assert!(
        rx.recv_timeout(std::time::Duration::from_secs(10)).is_ok(),
        "drop hung: the reader never woke while a background job held the pty slave"
    );
}

#[test]
fn format_cd_command_single_quotes_plain_path() {
    let bytes = format_cd_command(std::path::Path::new("/tmp/foo"));
    assert_eq!(
        bytes,
        b"\x05x\x15cd '/tmp/foo'\n".to_vec(),
        "Ctrl-E, a letter, Ctrl-U must precede the cd so a half-typed prompt line is cleared first, then the path is single-quoted and newline-terminated to fire immediately"
    );
}

#[test]
fn format_cd_command_escapes_embedded_apostrophe() {
    let bytes = format_cd_command(std::path::Path::new("/tmp/it's a dir"));
    assert_eq!(
        bytes,
        b"\x05x\x15cd '/tmp/it'\\''s a dir'\n".to_vec(),
        "embedded ' must be escaped as '\\'' so the cd does not break out of single quotes when the path contains an apostrophe (POSIX-standard quoting)"
    );
}

#[test]
fn format_cd_command_quotes_paths_with_spaces() {
    let bytes = format_cd_command(std::path::Path::new("/tmp/a b/c d"));
    assert_eq!(
        bytes,
        b"\x05x\x15cd '/tmp/a b/c d'\n".to_vec(),
        "single-quoting must preserve spaces verbatim so cd lands at the right directory"
    );
}

#[test]
fn foreground_is_shell_flips_false_while_a_foreground_command_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let mut term = PtyTerminal::new(tmp.path()).unwrap();

    let mut waited = 0u32;
    while waited < 4000 && !term.foreground_is_shell() {
        std::thread::sleep(std::time::Duration::from_millis(20));
        waited += 20;
    }
    assert!(
        term.foreground_is_shell(),
        "a shell at its prompt is the tty's foreground process group, so a cd is safe to inject"
    );

    term.write_input(b"sleep 10\n");
    let mut waited = 0u32;
    while waited < 4000 && term.foreground_is_shell() {
        std::thread::sleep(std::time::Duration::from_millis(20));
        waited += 20;
    }
    assert!(
        !term.foreground_is_shell(),
        "while a foreground command owns the tty, the foreground group is the command not the shell, so a cd must not be injected"
    );
}

/// Spawn a pane whose "shell" is a script holding the tty foreground
/// group with a job-control child — the rc-startup state the #94 flake
/// caught `change_workspace_root` in. Returns the pane after the child
/// has taken the foreground (bounded wait, asserted).
fn startup_owned_pane(tmp: &tempfile::TempDir) -> PtyTerminal {
    let script = tmp.path().join("slow-startup.sh");
    std::fs::write(&script, "#!/bin/bash\nset -m\nsleep 30\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let term = PtyTerminal::new_running(script.to_str().unwrap(), &[], tmp.path()).unwrap();
    let mut waited = 0u32;
    while waited < 4000 && term.foreground_is_shell() {
        std::thread::sleep(std::time::Duration::from_millis(20));
        waited += 20;
    }
    assert!(
        !term.foreground_is_shell(),
        "precondition: the startup child must own the foreground group"
    );
    term
}

#[test]
fn cwd_seed_stays_safe_through_startup_until_a_prompt_mark_arrives() {
    let tmp = tempfile::tempdir().unwrap();
    let term = startup_owned_pane(&tmp);
    assert!(
        term.cwd_seed_is_safe(),
        "an untouched pane whose foreground group is shell startup must stay seedable: the seed queues as type-ahead for the first prompt"
    );
    // Once shell integration reports a prompt, startup is over — a
    // non-shell foreground group is a real app from here on.
    term.push_mark_for_test(crate::shell_integration::OscEvent::PromptStart, 0);
    assert!(
        !term.cwd_seed_is_safe(),
        "after the first prompt mark, a non-shell foreground group must suppress the seed"
    );
}

#[test]
fn cwd_seed_turns_unsafe_once_the_pane_has_received_input() {
    let tmp = tempfile::tempdir().unwrap();
    let mut term = startup_owned_pane(&tmp);
    // Any input could have launched the app that owns the tty, so the
    // startup carve-out must die with the first written byte.
    term.write_input(b"q");
    assert!(
        !term.cwd_seed_is_safe(),
        "after input has been written, a non-shell foreground group must suppress the seed"
    );
}

/// A foreground process that owns the tty (an ssh client, exactly)
/// controls everything the pane prints — including an OSC 7 report
/// impersonating this machine's own hostname. The self-reported host
/// is no proof of locality then: `local_shell_cwd` must refuse while
/// the shell is not the foreground group, whatever the claim says.
#[test]
fn a_foreground_process_cannot_impersonate_a_local_cwd_report() {
    let out = std::process::Command::new("hostname").output().unwrap();
    let local = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if local.is_empty() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let mut term = PtyTerminal::new(tmp.path()).unwrap();
    let mut waited = 0u32;
    while waited < 4000 && !term.foreground_is_shell() {
        std::thread::sleep(std::time::Duration::from_millis(20));
        waited += 20;
    }
    // Stage the decoy: the shim's own prompt-time OSC 7 reports the
    // shell's real cwd (the tempdir), so `shell_cwd()` is `Some` long
    // before the forged claim exists — an `is_some()` stage exits on
    // this report and reads the wrong value (#69).
    let mut waited = 0u32;
    while waited < 4000 && term.shell_cwd().is_none() {
        std::thread::sleep(std::time::Duration::from_millis(20));
        waited += 20;
    }
    // A foreground job (the ssh stand-in) prints an OSC 7 claiming
    // this machine's own hostname. It sleeps BEFORE printing, so the
    // tty is lost to the job well before the claim can be parsed —
    // deterministically the ordering that flaked under suite load.
    term.write_input(
        format!("sleep 0.5; printf '\\033]7;file://{local}/tmp\\007'; sleep 10\n").as_bytes(),
    );
    let mut waited = 0u32;
    while waited < 4000 && term.foreground_is_shell() {
        std::thread::sleep(std::time::Duration::from_millis(20));
        waited += 20;
    }
    assert!(!term.foreground_is_shell(), "staging: a job owns the tty");
    // Wait for the claim ITSELF: any earlier `Some` is the decoy.
    let claim = std::path::PathBuf::from("/tmp");
    let mut waited = 0u32;
    while waited < 6000 && term.shell_cwd().as_deref() != Some(claim.as_path()) {
        std::thread::sleep(std::time::Duration::from_millis(20));
        waited += 20;
    }
    assert_eq!(
        term.shell_cwd(),
        Some(claim),
        "staging: the claim was captured"
    );
    assert_eq!(
        term.local_shell_cwd(),
        None,
        "a tty-owning foreground process must not place local splits by claim"
    );
}

/// The buffered claim outliving its forger: a foreground job prints a
/// local-looking OSC 7 and dies, but the reader thread only PARSES
/// those bytes later (here: the render side held the term lock, the
/// production delay). Any tty-ownership sample taken at parse time
/// sees the shell back at its prompt — PTY bytes carry no author, so
/// timing can never establish provenance. The claim must be rejected
/// on content: it names a directory the shell is not actually in.
#[test]
fn a_claim_parsed_only_after_its_job_died_is_still_untrusted() {
    let Some(zsh) = zsh_or_skip() else { return };
    let tmp = tempfile::tempdir().unwrap();
    // zsh -f: job control (the job gets its own pgroup) but no
    // integration, so nothing overwrites the forged claim.
    let mut term =
        PtyTerminal::new_running(zsh, &[String::from("-f"), String::from("-i")], tmp.path())
            .unwrap();
    let mut waited = 0u32;
    while waited < 4000 && !term.foreground_is_shell() {
        std::thread::sleep(std::time::Duration::from_millis(20));
        waited += 20;
    }
    // The job sleeps before printing, leaving time to stall the
    // reader below; then it prints the claim and lingers briefly so
    // the bytes enter the kernel buffer while it owns the tty.
    let term_arc = term.term.clone();
    term.write_input(
        b"/bin/sh -c 'sleep 1; printf \"\\033]7;file://localhost/tmp\\007\"; sleep 1'\n",
    );
    // Wait for the job to take the tty and the reader to drain the
    // command echo, THEN stall parsing by taking the term lock. The
    // lock cannot be held from the start: with the reader parked,
    // zsh's startup burst fills the kernel pty queue and blocks the
    // shell before it ever runs the line. The claim plus one prompt
    // redraw are small enough that nothing blocks during the stall.
    let mut waited = 0u32;
    while waited < 4000 && term.foreground_is_shell() {
        std::thread::sleep(std::time::Duration::from_millis(20));
        waited += 20;
    }
    assert!(!term.foreground_is_shell(), "staging: the job took the tty");
    std::thread::sleep(std::time::Duration::from_millis(150));
    let guard = term_arc.lock();
    // With parsing stalled, wait out the job's remaining life: claim
    // printed, job dead, shell back at its prompt. tcgetpgrp is
    // kernel state, untouched by our lock.
    let mut waited = 0u32;
    while waited < 8000 && !term.foreground_is_shell() {
        std::thread::sleep(std::time::Duration::from_millis(20));
        waited += 20;
    }
    assert!(
        term.foreground_is_shell(),
        "staging: the job is gone and the shell owns the tty again"
    );
    std::thread::sleep(std::time::Duration::from_millis(300));
    drop(guard);
    let mut waited = 0u32;
    while waited < 4000 && term.shell_cwd() != Some(std::path::PathBuf::from("/tmp")) {
        std::thread::sleep(std::time::Duration::from_millis(20));
        waited += 20;
    }
    assert_eq!(
        term.shell_cwd(),
        Some(std::path::PathBuf::from("/tmp")),
        "staging: the claim was captured after the job died"
    );
    assert_eq!(
        term.local_shell_cwd(),
        None,
        "a claim whose bytes were parsed after its job died must not be trusted"
    );
}

/// The forged claim outliving its forger: a foreground job prints an
/// OSC 7 with a local-looking host and EXITS; the shell retakes the
/// tty without re-reporting (no integration), so a foreground check at
/// consultation time passes on the stale cache. A report that arrived
/// from a job never becomes trusted, however long ago the job died.
#[test]
fn a_stale_claim_from_a_dead_foreground_job_stays_untrusted() {
    let Some(zsh) = zsh_or_skip() else { return };
    let tmp = tempfile::tempdir().unwrap();
    // zsh -f: job control (the claim's job gets its own pgroup, like a
    // real ssh client) but NO integration — the shell never overwrites
    // the stale claim with a fresh trusted report of its own.
    let mut term =
        PtyTerminal::new_running(zsh, &[String::from("-f"), String::from("-i")], tmp.path())
            .unwrap();
    let mut waited = 0u32;
    while waited < 4000 && !term.foreground_is_shell() {
        std::thread::sleep(std::time::Duration::from_millis(20));
        waited += 20;
    }
    // An EXTERNAL job (its own pgroup, like an ssh client — a bare
    // printf would be a zsh builtin printing from the shell's own
    // pgroup) settles as the tty owner, prints the local-looking
    // claim, lingers so the capture happens while it owns the tty,
    // and dies.
    term.write_input(
        b"/bin/sh -c 'sleep 1; printf \"\\033]7;file://localhost/tmp\\007\"; sleep 1'\n",
    );
    let mut waited = 0u32;
    while waited < 8000
        && !(term.foreground_is_shell()
            && term.shell_cwd() == Some(std::path::PathBuf::from("/tmp")))
    {
        std::thread::sleep(std::time::Duration::from_millis(20));
        waited += 20;
    }
    assert_eq!(
        term.shell_cwd(),
        Some(std::path::PathBuf::from("/tmp")),
        "staging: the claim was captured"
    );
    assert!(
        term.foreground_is_shell(),
        "staging: the job is gone and the shell owns the tty again"
    );
    assert_eq!(
        term.local_shell_cwd(),
        None,
        "a claim captured while a job owned the tty must never become trusted"
    );
}

#[test]
fn clear_screen_and_scrollback_wipes_the_grid() {
    fn dump(term: &PtyTerminal) -> String {
        let t = term.term.lock();
        let rows = t.screen_lines();
        let cols = t.columns();
        let off = t.grid().display_offset() as i32;
        extract_selection_text(&t, -off, 0, rows as i32 - 1 - off, cols.saturating_sub(1))
    }
    let tmp = tempfile::tempdir().unwrap();
    // The spawn banner prints the command's argv, so the probe must never
    // appear verbatim in argv: under load the banner lands before the
    // child's output, the wait below matches the BANNER, and the clear
    // fires before the probe exists - which the assert then finds alive.
    // The shell concatenation keeps argv and output distinct.
    let mut term = PtyTerminal::new_running(
        "/bin/sh",
        &[String::from("-c"), String::from("echo CLEAR-PROBE-\"\"XYZ")],
        tmp.path(),
    )
    .unwrap();
    let mut waited = 0u32;
    while waited < 4000 && !dump(&term).contains("CLEAR-PROBE-XYZ") {
        std::thread::sleep(std::time::Duration::from_millis(20));
        waited += 20;
    }
    assert!(
        dump(&term).contains("CLEAR-PROBE-XYZ"),
        "probe present before clear"
    );
    term.clear_screen_and_scrollback();
    assert!(
        !dump(&term).contains("CLEAR-PROBE-XYZ"),
        "clear wipes the screen and scrollback"
    );
}

#[test]
fn change_cwd_writes_cd_command_to_pty() {
    let tmp = tempfile::tempdir().unwrap();
    let mut term = PtyTerminal::new(tmp.path()).unwrap();
    let _ = term.take_dirty();
    let target = tempfile::tempdir().unwrap();
    term.change_cwd(target.path());
    assert!(
        term.take_dirty(),
        "change_cwd must mark the terminal dirty so the next frame paints the new prompt"
    );
}

#[test]
fn new_running_spawns_program_directly_and_produces_output() {
    let tmp = tempfile::tempdir().unwrap();
    let term = PtyTerminal::new_running(
        "/bin/echo",
        &[String::from("croft-direct-spawn")],
        tmp.path(),
    )
    .unwrap();
    // Wait on the BYTE COUNTER, not the dirty flag: `pty_dirty` is
    // constructed `true`, so peeking it succeeds before /bin/echo has
    // written anything and this test would pass just as happily if direct
    // spawns stopped reaching the reader thread entirely. `pending_bytes`
    // only moves when the reader actually advanced output.
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(500),
        "the directly spawned /bin/echo to deliver bytes through the PTY",
        || term.peek_pending_bytes() > 0,
    );
    assert!(
        term.peek_pending_bytes() > 0,
        "direct-spawned /bin/echo must produce output without any write_input"
    );
}

/// #473: the grid must take the new size BEFORE the child hears about it.
///
/// `resize` used to send the pty ioctl (which delivers SIGWINCH) first
/// and resize alacritty's grid second. A program that repaints on
/// WINCH - Claude Code does, with a full frame laid out for the new
/// size - can have that frame parsed into the OLD grid whenever the
/// render thread loses the CPU between the two steps. A frame laid out
/// for one size and parsed into another leaves rows of stale text the
/// program believes it erased: its renderer diffs against its own model
/// and never repaints what it thinks is already blank. Replaying a
/// recorded Claude Code session with the two steps swapped reproduces
/// the residue; the correct order replays clean.
///
/// The order is observed, not timed: the child records its size the
/// moment WINCH arrives, and the test holds the grid lock across the
/// whole `resize`. If the ioctl goes out before the grid is resized,
/// the child reports the new size WHILE the lock is held, which the
/// correct order makes impossible however long the wait.
#[test]
fn the_grid_takes_the_new_size_before_the_child_hears_the_winch() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("winch-size");
    // The path travels as `$1` rather than being spliced into the
    // script: a temp dir with a space, a `;` or a quote in it would break
    // an interpolated redirection, while "$1" is expanded by the shell
    // itself when WINCH fires.
    let script = "trap 'stty size > \"$1\"' WINCH; echo trap-armed; while :; do sleep 0.05; done";
    let mut pane = PtyTerminal::new_running(
        "/bin/sh",
        &[
            String::from("-c"),
            String::from(script),
            String::from("sh"),
            out.display().to_string(),
        ],
        tmp.path(),
    )
    .unwrap();
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(500),
        "the child shell to arm its WINCH trap",
        || pane.visible_text().contains("trap-armed"),
    );
    let grid = pane.grid_lock_for_test();
    let held = grid.lock();
    // A budget, not a race: the wrong order sends the ioctl before it
    // ever asks for the lock, so the child's report lands within
    // milliseconds; the right order cannot produce one while the lock
    // is held, so waiting longer only makes a true failure slower.
    let window = crate::test_budget::spawn_budget(std::time::Duration::from_millis(150));
    std::thread::scope(|s| {
        s.spawn(|| pane.resize(30, 10));
        std::thread::sleep(window);
        let reported = std::fs::read_to_string(&out).ok();
        // Released BEFORE asserting, or a failure deadlocks the scope
        // on a resize that is still waiting for the grid.
        drop(held);
        assert!(
            reported.is_none(),
            "the child saw the new size ({:?}) while the grid lock was still held: \
                 the pty was resized before the grid",
            reported.map(|s| s.trim().to_string())
        );
    });
    // Positive control: the trap works and the resize went through.
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(500),
        "the child to report the new size once the grid lock was released",
        || std::fs::read_to_string(&out).is_ok_and(|s| s.trim() == "10 30"),
    );
    // The parser grid itself, not the cached `cols` field: `resize`
    // writes that field before it touches the grid, so it reads 30 even
    // when the grid was never resized.
    assert_eq!(
        grid.lock().columns(),
        30,
        "the grid took the new column count"
    );
}

#[test]
fn pristine_covers_only_an_untouched_interactive_shell() {
    let tmp = tempfile::tempdir().unwrap();
    let mut shell = PtyTerminal::new(tmp.path()).unwrap();
    assert!(
        shell.is_pristine(),
        "a freshly spawned shell with no input and no name is pristine"
    );
    shell.write_input(b"echo touched\n");
    assert!(
        !shell.is_pristine(),
        "any input byte permanently clears pristineness"
    );

    let mut named = PtyTerminal::new(tmp.path()).unwrap();
    named.set_manual_name(Some(String::from("srv")));
    assert!(
        !named.is_pristine(),
        "a manual name is user state; the pane must not be replaceable"
    );
    named.set_manual_name(None);
    assert!(
        !named.is_pristine(),
        "a rename latches: clearing the name back must not make the pane replaceable"
    );

    let run = PtyTerminal::new_running(
        "/bin/sh",
        &[String::from("-c"), String::from("sleep 1")],
        tmp.path(),
    )
    .unwrap();
    assert!(
        !run.is_pristine(),
        "a launched-program pane is doing user work despite having seen no input"
    );
}

#[test]
fn tpgid_is_read_past_a_command_name_with_spaces_and_parens() {
    let stat = "4242 (my (odd) sh) S 1 4242 4242 34816 5151 4194560 0 0";
    assert_eq!(parse_tpgid(stat), Some(5151));
    assert_eq!(
        parse_tpgid("1 (init) S 0 1 1 0 -1 4194560"),
        None,
        "no terminal"
    );
    assert_eq!(parse_tpgid("garbage"), None);
}

/// A pane host for `argv` on a socket in `dir`, served on a thread.
#[cfg(target_os = "linux")]
fn serve_pane(
    dir: &std::path::Path,
    argv: &[&str],
) -> (std::path::PathBuf, std::thread::JoinHandle<Result<i32>>) {
    let spec = crate::pane_host::PaneSpec {
        socket: dir.join("pane.sock"),
        cwd: Some(dir.to_path_buf()),
        env: vec![(String::from("TERM"), String::from("xterm-256color"))],
        cols: 80,
        rows: 24,
        argv: argv.iter().map(|a| a.to_string()).collect(),
    };
    let socket = spec.socket.clone();
    let host = std::thread::spawn(move || crate::pane_host::serve(&spec));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !crate::session::is_alive(&socket) {
        assert!(
            std::time::Instant::now() < deadline,
            "the host never listened"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    (socket, host)
}

#[cfg(target_os = "linux")]
#[test]
fn a_hosted_pane_outlives_a_croft_that_never_closed_it() {
    let tmp = tempfile::tempdir().unwrap();
    let (socket, host) = serve_pane(tmp.path(), &["sh"]);
    let mut pane = PtyTerminal::attach_hosted(&socket).unwrap();
    assert_eq!(pane.host_socket(), Some(socket.as_path()));
    let pid = pane.pid().expect("the host names its shell");
    assert_eq!(pane.shell_pid(), Some(pid as i32));
    pane.write_input(b"echo first-$((40+2))\r");
    wait_for_grid(&pane, |l| l.iter().any(|l| l.contains("first-42")));
    // The shell owns its terminal at the prompt, read without a master.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
    while pane.foreground_pid() != Some(pid as i32) {
        assert!(
            std::time::Instant::now() < deadline,
            "{:?}",
            pane.foreground_pid()
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    // croft dies without dropping its panes (the OOM killer): the pane
    // is never closed, so its shell must keep running.
    std::mem::forget(pane);
    let mut again = PtyTerminal::attach_hosted(&socket).unwrap();
    assert_eq!(again.pid(), Some(pid), "the same shell");
    wait_for_grid(&again, |l| l.iter().any(|l| l.contains("first-42")));
    again.resize(100, 30);
    again.write_input(b"stty size\r");
    wait_for_grid(&again, |l| l.iter().any(|l| l.trim() == "30 100"));

    // Closing the pane ends the shell and its host.
    drop(again);
    host.join().unwrap().unwrap();
    assert!(!socket.exists());
}

/// Poll `grid_lines` until a predicate matches or 4s elapse.
fn wait_for_grid<F: Fn(&[String]) -> bool>(term: &PtyTerminal, pred: F) -> Vec<String> {
    let mut waited_ms = 0u32;
    loop {
        let (lines, _top) = term.grid_lines();
        if pred(&lines) {
            return lines;
        }
        assert!(
            waited_ms < 4000,
            "expected output never reached the grid; grid was: {lines:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
        waited_ms += 20;
    }
}

/// The terminal's find highlight follows the editor's: Croft Loch tints
/// each matched cell's own background amber and keeps an ink that reads
/// on it, where a boxed theme stamps its black-on-gold block.
#[test]
fn terminal_find_matches_are_an_amber_tint_on_croft_loch_and_gold_when_boxed() {
    let tmp = tempfile::tempdir().unwrap();
    let mut term = PtyTerminal::new_running(
        "/bin/sh",
        &[
            String::from("-c"),
            String::from("printf 'find the needle here\\n'; sleep 30"),
        ],
        tmp.path(),
    )
    .unwrap();
    wait_for_grid(&term, |ls| ls.iter().any(|l| l.starts_with("find the")));
    term.set_search(
        Some(String::from("needle")),
        crate::widgets::search::SearchOpts::default(),
    );
    let area = Rect::new(0, 0, 60, 10);
    term.last_inner = Rect::new(1, 1, 58, 8);
    // The cells of "needle" on the first row showing the phrase: the
    // spawn banner echoes the command, so that may be the banner's copy
    // rather than the output's, but every occurrence is a match.
    let needle_cells = |buf: &Buffer| -> Vec<(u16, u16)> {
        (0..area.height)
            .find_map(|y| {
                let row: String = (0..area.width).map(|x| buf[(x, y)].symbol()).collect();
                let col = row.find("find the needle")?;
                let x0 = row[..col].chars().count() as u16 + 9;
                Some((x0..x0 + 6).map(|x| (x, y)).collect())
            })
            .expect("the output row rendered")
    };
    let gold = Color::Rgb(0xff, 0xd7, 0x4a);

    assert!(
        crate::widgets::surface::tones(term.theme).is_some(),
        "Croft Loch by default"
    );
    let mut buf = Buffer::empty(area);
    Widget::render(&mut term, area, &mut buf);
    for (x, y) in needle_cells(&buf) {
        let cell = &buf[(x, y)];
        assert_ne!(cell.bg, gold, "no solid block on Croft Loch");
        let (Color::Rgb(fr, fg, fb), Color::Rgb(br, bg, bb)) = (cell.fg, cell.bg) else {
            panic!("RGB inks on the match: {cell:?}");
        };
        assert!(br > bb + 24, "the fill is a warm amber tint: {cell:?}");
        assert!(crate::theme::contrast_ratio((fr, fg, fb), (br, bg, bb)) >= 4.5);
        assert!(!cell.modifier.contains(Modifier::BOLD));
    }

    term.theme = crate::theme::Theme::BLACK;
    let mut buf = Buffer::empty(area);
    Widget::render(&mut term, area, &mut buf);
    for (x, y) in needle_cells(&buf) {
        assert_eq!(buf[(x, y)].bg, gold, "a boxed theme keeps the gold block");
        assert_eq!(buf[(x, y)].fg, Color::Black);
    }
}

/// bash's line editor rings the bell for Ctrl-U on an empty line, and
/// croft reported that bell over the "Running …" status of the task it
/// had just typed into an idle pane. What croft types ahead of a command
/// never rings, and still clears a half-typed line.
#[test]
fn clearing_the_line_ahead_of_a_command_never_rings_the_bell() {
    let tmp = tempfile::tempdir().unwrap();
    let mut term = PtyTerminal::new_running(
        "bash",
        &[
            String::from("--norc"),
            String::from("--noprofile"),
            String::from("-i"),
        ],
        tmp.path(),
    )
    .unwrap();
    // The line editor is up once it has drawn this prompt.
    term.write_input(b"PS1='ready> '\r");
    wait_for_grid(&term, |l| l.iter().any(|l| l.starts_with("ready>")));
    let _ = term.take_bell();

    term.write_input(format!("{CLEAR_TYPED_LINE}echo cleared-$((40+2))\r").as_bytes());
    wait_for_grid(&term, |l| l.iter().any(|l| l.trim() == "cleared-42"));
    term.write_input(format!("echo stale{CLEAR_TYPED_LINE}echo fresh-$((40+2))\r").as_bytes());
    let lines = wait_for_grid(&term, |l| l.iter().any(|l| l.trim() == "fresh-42"));

    assert!(
        !lines.iter().any(|l| l.contains("stale")),
        "the half-typed line ran: {lines:?}"
    );
    assert!(!term.take_bell(), "clearing the line rang the bell");
}

#[test]
fn ansi_named_colors_render_through_the_theme_palette() {
    // SGR 31 red must come out as the theme palette's red (VS Code's
    // #cd3131 by default), not the host terminal's Color::Red — croft
    // owns its terminal palette the way VS Code does. A palette swap
    // repaints with the new colors.
    let tmp = tempfile::tempdir().unwrap();
    // The sentinel is assembled at runtime (`${s}RED`) so the pane's
    // `▶ …` run-label header — which contains the raw command text —
    // can never match the scan below.
    let mut term = PtyTerminal::new_running(
        "/bin/sh",
        &[
            String::from("-c"),
            String::from("s=QQ; printf \"\\033[31m${s}RED\\033[0m\\n\"; sleep 30"),
        ],
        tmp.path(),
    )
    .unwrap();
    wait_for_grid(&term, |ls| ls.iter().any(|l| l.contains("QQRED")));
    let area = Rect::new(0, 0, 60, 10);
    let find_q_fg = |term: &mut PtyTerminal| -> Option<Color> {
        let mut buf = Buffer::empty(area);
        Widget::render(term, area, &mut buf);
        for y in 0..area.height {
            for x in 1..area.width - 2 {
                if buf[(x, y)].symbol() == "Q"
                    && buf[(x + 1, y)].symbol() == "Q"
                    && buf[(x + 2, y)].symbol() == "R"
                {
                    return Some(buf[(x, y)].fg);
                }
            }
        }
        None
    };
    assert_eq!(
        find_q_fg(&mut term),
        Some(Color::Rgb(0xcd, 0x31, 0x31)),
        "SGR red must map through the default palette"
    );
    let mut palette = crate::theme::VSCODE_ANSI;
    palette[1] = (0x12, 0x34, 0x56);
    term.set_palette(palette);
    assert_eq!(
        find_q_fg(&mut term),
        Some(Color::Rgb(0x12, 0x34, 0x56)),
        "a palette swap must repaint SGR red with the new color"
    );
}

/// #360: a redact-trigger match paints as a run of mask glyphs of the
/// same width while the grid keeps the real text, so a click on the
/// mask can still recover it; the count feeds the status chip; a
/// reveal window paints the text as typed.
#[test]
fn redacted_spans_paint_as_masks_and_the_real_text_stays_recoverable() {
    let tmp = tempfile::tempdir().unwrap();
    let mut term = PtyTerminal::new_running(
        "/bin/sh",
        &[
            String::from("-c"),
            String::from(
                "printf 'key AKIAIOSFODNN7EXAMPLE end\\ntail AKIAIOSFODNN7EXAMPLE\\n'; sleep 30",
            ),
        ],
        tmp.path(),
    )
    .unwrap();
    // The pane's spawn banner echoes the command line, key included, so
    // wait for the OUTPUT line, which starts at column 0.
    wait_for_grid(&term, |ls| ls.iter().any(|l| l.starts_with("tail AKIA")));
    term.set_triggers(std::sync::Arc::new(
        crate::triggers::TriggerSet::default().with_builtin_redactions(),
    ));
    // Boxed, so a row reads from the border (`│key `).
    term.theme = crate::theme::Theme::BLACK;
    let area = Rect::new(0, 0, 60, 10);
    term.last_inner = Rect::new(1, 1, 58, 8);
    let mut buf = Buffer::empty(area);
    Widget::render(&mut term, area, &mut buf);
    let row_text = |buf: &Buffer, y: u16| -> String {
        (0..area.width)
            .map(|x| buf[(x, y)].symbol().to_string())
            .collect()
    };
    let rows: Vec<String> = (0..area.height).map(|y| row_text(&buf, y)).collect();
    let (y, line) = rows
        .iter()
        .enumerate()
        .map(|(y, l)| (y as u16, l.clone()))
        .find(|(_, l)| l.starts_with("\u{2502}key "))
        .unwrap_or_else(|| {
            panic!(
                "the key line rendered: {rows:#?} grid={:?} label={:?}",
                term.grid_lines().0,
                term.label()
            )
        });
    assert!(
        line.contains(&"\u{2022}".repeat(20)) && !line.contains("AKIA"),
        "the key paints as twenty masks: {line:?}"
    );
    assert!(
        line.contains(" end"),
        "text around the key is untouched: {line:?}"
    );
    assert!(
        term.redacted_on_screen >= 1,
        "the status chip counts at least the output line's span"
    );

    // The grid still holds the real text: a click on the mask finds it.
    // A column, not a byte offset: the border glyph before it is multibyte.
    let x = line.chars().position(|c| c == '\u{2022}').unwrap() as u16;
    assert_eq!(
        term.redacted_at(x, y).as_deref(),
        Some("AKIAIOSFODNN7EXAMPLE"),
        "the mask click recovers the value"
    );
    assert_eq!(
        term.redacted_at(x.saturating_sub(2), y),
        None,
        "off the mask: nothing"
    );
    // A row whose masked token is its LAST text: a click a few cells
    // right of the mask, still inside the pane, resolves to that blank
    // cell's own char (the colmap is dense) and reveals nothing.
    let (ty, tline) = rows
        .iter()
        .enumerate()
        .map(|(y, l)| (y as u16, l.clone()))
        .find(|(_, l)| l.starts_with("\u{2502}tail "))
        .expect("the tail line rendered");
    let tx = tline.chars().position(|c| c == '\u{2022}').unwrap() as u16;
    assert_eq!(
        term.redacted_at(tx, ty).as_deref(),
        Some("AKIAIOSFODNN7EXAMPLE"),
        "the last token's mask still reveals on itself"
    );
    assert!(tx + 24 < area.width - 1, "the probe stays inside the pane");
    assert_eq!(
        term.redacted_at(tx + 24, ty),
        None,
        "blank cells right of the row's text reveal nothing"
    );

    // A reveal window paints the text as typed and counts nothing.
    term.reveal_redactions = true;
    let mut buf = Buffer::empty(area);
    Widget::render(&mut term, area, &mut buf);
    assert!(row_text(&buf, y).contains("AKIAIOSFODNN7EXAMPLE"));
    assert_eq!(term.redacted_on_screen, 0);
}

#[test]
fn faint_cells_render_with_the_dim_modifier() {
    // Claude Code paints its inline ghost suggestion as default-foreground
    // text under SGR 2 (faint). The pane must forward the flag as
    // Modifier::DIM, or the suggestion renders at full brightness and is
    // indistinguishable from text the user typed.
    let tmp = tempfile::tempdir().unwrap();
    let mut term = PtyTerminal::new_running(
        "/bin/sh",
        &[
            String::from("-c"),
            String::from("s=QQ; printf \"\\033[2m${s}GHOST\\033[0m\\n\"; sleep 30"),
        ],
        tmp.path(),
    )
    .unwrap();
    wait_for_grid(&term, |ls| ls.iter().any(|l| l.contains("QQGHOST")));
    let area = Rect::new(0, 0, 60, 10);
    let mut buf = Buffer::empty(area);
    Widget::render(&mut term, area, &mut buf);
    for y in 0..area.height {
        for x in 1..area.width - 2 {
            if buf[(x, y)].symbol() == "Q"
                && buf[(x + 1, y)].symbol() == "Q"
                && buf[(x + 2, y)].symbol() == "G"
            {
                assert!(
                    buf[(x, y)].modifier.contains(Modifier::DIM),
                    "SGR 2 (faint) cells must carry Modifier::DIM"
                );
                return;
            }
        }
    }
    panic!("the faint sentinel never appeared in the rendered buffer");
}

/// A quiet pane fed the issue's bytes (#1288): two struck-through todo
/// items, a concealed password and a plain word.
fn strike_and_conceal_pane() -> (PtyTerminal, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let term = PtyTerminal::new_running(
        "/bin/sh",
        &[String::from("-c"), String::from("sleep 30")],
        tmp.path(),
    )
    .unwrap();
    term.feed_bytes_for_test(b"\x1b[2J\x1b[H\x1b[9mDONE\x1b[0m [\x1b[8mhunter2\x1b[0m] PLAIN\r\n");
    (term, tmp)
}

#[test]
fn struck_through_cells_paint_crossed_out_and_concealed_cells_paint_blank() {
    let (mut term, _tmp) = strike_and_conceal_pane();
    let area = Rect::new(0, 0, 60, 10);
    let mut buf = Buffer::empty(area);
    Widget::render(&mut term, area, &mut buf);
    let rows: Vec<String> = (0..area.height)
        .map(|y| (0..area.width).map(|x| buf[(x, y)].symbol()).collect())
        .collect();
    let y = rows
        .iter()
        .position(|r| r.contains("DONE"))
        .expect("the todo row is painted") as u16;
    let row = &rows[y as usize];
    assert!(!row.contains("hunter2"), "SGR 8 text is hidden: {row:?}");
    assert!(row.contains("DONE [       ] PLAIN"), "{row:?}");
    // Cells, not bytes: the pane border is a multi-byte glyph.
    let at = |word: &str| row[..row.find(word).unwrap()].chars().count() as u16;
    for x in at("DONE")..at("DONE") + 4 {
        assert!(
            buf[(x, y)].modifier.contains(Modifier::CROSSED_OUT),
            "SGR 9 cell at {x} is struck through"
        );
    }
    for x in at("PLAIN")..at("PLAIN") + 5 {
        assert!(
            !buf[(x, y)]
                .modifier
                .intersects(Modifier::CROSSED_OUT | Modifier::HIDDEN),
            "plain cell at {x} carries neither"
        );
    }
}

#[test]
fn concealed_text_still_copies_and_exports_with_its_sgr() {
    // Negative: hiding is paint-only, as in xterm. The grid, and so a
    // copy, keeps the real text, and an export with styles writes the
    // attributes back out as 8 and 9 for the reader to honour.
    let (term, _tmp) = strike_and_conceal_pane();
    let (lines, _) = term.grid_lines();
    assert!(
        lines.iter().any(|l| l.contains("DONE [hunter2] PLAIN")),
        "{lines:?}"
    );
    let exported = term.grid_lines_ansi();
    let (_, raw) = exported
        .iter()
        .find(|(p, _)| p.contains("hunter2"))
        .expect("the row is exported");
    assert!(raw.contains("\x1b[9mDONE"), "{raw:?}");
    assert!(raw.contains("\x1b[8mhunter2"), "{raw:?}");
}

#[test]
fn imgcat_style_inline_images_are_captured_with_a_grid_anchor() {
    use base64::Engine;
    // A 1x1 red PNG; what `imgcat tiny.png` would emit.
    let mut png_buf = Vec::new();
    image::RgbaImage::from_pixel(1, 1, image::Rgba([255, 0, 0, 255]))
        .write_to(
            &mut std::io::Cursor::new(&mut png_buf),
            image::ImageFormat::Png,
        )
        .expect("encode test png");
    let png: &[u8] = &png_buf;
    let b64 = base64::engine::general_purpose::STANDARD.encode(png);
    let tmp = tempfile::tempdir().unwrap();
    let script = format!(
        "printf 'before\\n'; printf '\\033]1337;File=inline=1:{b64}\\007\\n'; printf 'after\\n'; sleep 30"
    );
    let term =
        PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script], tmp.path()).unwrap();
    wait_for_grid(&term, |ls| ls.iter().any(|l| l.contains("after")));
    let mut waited = 0u32;
    let imgs = loop {
        let imgs = term.pane_images();
        if !imgs.is_empty() {
            break imgs;
        }
        assert!(waited < 8000, "inline image never captured");
        std::thread::sleep(std::time::Duration::from_millis(40));
        waited += 40;
    };
    assert_eq!(imgs.len(), 1);
    assert_eq!(imgs[0].data.as_slice(), png, "payload must round-trip");
    // Anchor: the row right after the `before` line (the run-label
    // header above it wraps, so absolute row numbers are not stable).
    let (lines, top) = term.grid_lines();
    let before_row = lines
        .iter()
        .rposition(|l| l.trim_end() == "before")
        .expect("the before line must be on the grid") as i32
        + top;
    assert_eq!(
        imgs[0].line,
        before_row + 1,
        "anchor must sit on the image's grid row; grid: {lines:?}"
    );
}

#[test]
fn wide_chars_read_back_without_phantom_spacer_spaces() {
    // A CJK char occupies two grid columns: the WIDE_CHAR cell plus a
    // WIDE_CHAR_SPACER cell whose `c` is ' '. Text extraction must skip
    // the spacer, or copied/searched text comes out as "日 本 語" and a
    // find for "日本語" can never match.
    let tmp = tempfile::tempdir().unwrap();
    let term =
        PtyTerminal::new_running("/bin/echo", &[String::from("日本語 ok")], tmp.path()).unwrap();
    let lines = wait_for_grid(&term, |ls| ls.iter().any(|l| l.contains('日')));
    assert!(
        lines.iter().any(|l| l.contains("日本語 ok")),
        "wide chars must read back contiguously (no spacer-cell spaces); grid was: {lines:?}"
    );
}

#[test]
fn wide_chars_render_the_glyph_once_with_a_blank_spacer_cell() {
    // The render loop writes the double-width glyph into its WIDE_CHAR
    // cell and must leave the following spacer cell blank — ratatui skips
    // the cell after a width-2 symbol when diffing, so anything else
    // there would corrupt column alignment.
    let tmp = tempfile::tempdir().unwrap();
    let mut term =
        PtyTerminal::new_running("/bin/echo", &[String::from("日X")], tmp.path()).unwrap();
    wait_for_grid(&term, |ls| ls.iter().any(|l| l.contains('日')));
    let area = Rect::new(0, 0, 40, 10);
    let mut buf = Buffer::empty(area);
    Widget::render(&mut term, area, &mut buf);
    let mut found = false;
    for y in 0..area.height {
        for x in 0..area.width.saturating_sub(2) {
            if buf[(x, y)].symbol() == "日" {
                assert_eq!(
                    buf[(x + 1, y)].symbol(),
                    " ",
                    "the spacer cell after a wide glyph must stay blank"
                );
                assert_eq!(
                    buf[(x + 2, y)].symbol(),
                    "X",
                    "the next glyph must land two columns after the wide char"
                );
                found = true;
            }
        }
    }
    assert!(found, "the wide glyph must appear in the rendered buffer");
}

#[test]
fn hyperlink_at_returns_the_osc8_uri_under_the_cell() {
    // OSC 8 hyperlinks live in the cell, invisible in the text. The app's
    // Cmd/Ctrl+click handler asks `hyperlink_at` before falling back to
    // the plain-text URL regex.
    let tmp = tempfile::tempdir().unwrap();
    let script = "printf '\\033]8;;https://example.com/doc\\033\\\\CLICK-ME\\033]8;;\\033\\\\\\n'";
    let term =
        PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script.into()], tmp.path())
            .unwrap();
    // The run header echoes the command text, so "CLICK-ME" appears there
    // too — as plain text without a link. Wait for the child's actual
    // output row (the one without "printf").
    let is_output = |l: &String| l.contains("CLICK-ME") && !l.contains("printf");
    let lines = wait_for_grid(&term, |ls| ls.iter().any(is_output));
    let row = lines.iter().position(is_output).unwrap();
    let col = lines[row].find("CLICK-ME").unwrap();
    assert_eq!(
        term.hyperlink_at(row, col).as_deref(),
        Some("https://example.com/doc"),
        "the cell under the linked text must expose the OSC 8 URI"
    );
    assert_eq!(
        term.hyperlink_at(0, 0),
        None,
        "cells outside the link (the header row) carry no URI"
    );
}

#[test]
fn osc133_marks_record_kind_exit_code_and_grid_line() {
    // Emit a prompt mark before "PROMPT", a command-start mark, and a
    // finished mark carrying exit 3. Marks must land on the grid lines
    // where the cursor sat when they arrived.
    let tmp = tempfile::tempdir().unwrap();
    let script =
        "printf 'before\\n\\033]133;A\\007PROMPT-LINE\\n\\033]133;C\\007out\\n\\033]133;D;3\\007'";
    let term =
        PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script.into()], tmp.path())
            .unwrap();
    let is_output = |l: &String| l.contains("PROMPT-LINE") && !l.contains("printf");
    let lines = wait_for_grid(&term, |ls| {
        ls.iter().any(is_output) && !term.command_marks().is_empty()
    });
    let (_, top) = term.grid_lines();
    let prompt_row = lines.iter().position(is_output).unwrap();
    let marks = term.command_marks();
    let prompt_line = marks
        .iter()
        .find(|(kind, _)| *kind == crate::shell_integration::OscEvent::PromptStart)
        .map(|(_, line)| *line)
        .expect("a PromptStart mark must be recorded");
    assert_eq!(
        prompt_line,
        top + prompt_row as i32,
        "the prompt mark must sit on the line where PROMPT-LINE was printed"
    );
    assert!(
        marks
            .iter()
            .any(|(kind, _)| *kind == crate::shell_integration::OscEvent::CommandEnd(Some(3))),
        "the finished mark must carry exit code 3; marks: {marks:?}"
    );
}

#[test]
fn marks_follow_their_content_into_scrollback() {
    // A mark recorded on the live screen must keep pointing at the same
    // content after later output scrolls that content into history.
    let tmp = tempfile::tempdir().unwrap();
    let script = "printf '\\033]133;A\\007MARKED-PROMPT\\n'; i=0; while [ $i -lt 40 ]; do echo filler-$i; i=$((i+1)); done";
    let term =
        PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script.into()], tmp.path())
            .unwrap();
    wait_for_grid(&term, |ls| ls.iter().any(|l| l.contains("filler-39")));
    // The script's tail (trailing newline, shell exit) can scroll another
    // row or two AFTER filler-39 becomes visible. `lines` and `top` must
    // come from ONE settled snapshot, or the row index and the grid top
    // describe different moments and the compare is off by exactly the
    // late scroll (#110). Settle = two consecutive identical reads.
    let (lines, top) = {
        let mut prev = term.grid_lines();
        let mut waited = 0u32;
        loop {
            std::thread::sleep(std::time::Duration::from_millis(40));
            let cur = term.grid_lines();
            if cur == prev || waited >= 4000 {
                break cur;
            }
            prev = cur;
            waited += 40;
        }
    };
    let content_row = lines
        .iter()
        .position(|l| l.contains("MARKED-PROMPT") && !l.contains("printf"))
        .expect("the marked line must still be in scrollback");
    let marks = term.command_marks();
    let prompt_line = marks
        .iter()
        .find(|(kind, _)| *kind == crate::shell_integration::OscEvent::PromptStart)
        .map(|(_, line)| *line)
        .expect("the mark must survive the scroll");
    assert_eq!(
        prompt_line,
        top + content_row as i32,
        "the mark must move with its content into history (negative grid lines)"
    );
    assert!(
        prompt_line < 0,
        "after 40 filler lines the mark is in scrollback"
    );
}

#[test]
fn osc7_updates_the_shell_cwd() {
    let tmp = tempfile::tempdir().unwrap();
    let script = "printf '\\033]7;file://anyhost/tmp/croft%%20dir\\007ok\\n'";
    let term =
        PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script.into()], tmp.path())
            .unwrap();
    wait_for_grid(&term, |ls| {
        ls.iter().any(|l| l.contains("ok") && !l.contains("printf"))
    });
    assert_eq!(
        term.shell_cwd(),
        Some(std::path::PathBuf::from("/tmp/croft dir")),
        "OSC 7 must update the pane's live cwd"
    );
}

/// A remote shell's OSC 7 (an in-pane ssh with integration) names a
/// path on the REMOTE machine: consumers that act on the local
/// filesystem (split-in-cwd, session restore) must not trust it just
/// because the same path happens to exist locally.
#[test]
fn a_remote_reported_cwd_is_not_trusted_for_local_use() {
    let tmp = tempfile::tempdir().unwrap();
    let script = "printf '\\033]7;file://prod-db-1/tmp\\007ok\\n'; sleep 30";
    let term =
        PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script.into()], tmp.path())
            .unwrap();
    wait_for_grid(&term, |ls| {
        ls.iter().any(|l| l.contains("ok") && !l.contains("printf"))
    });
    assert_eq!(
        term.shell_cwd(),
        Some(std::path::PathBuf::from("/tmp")),
        "staging: the raw report is visible"
    );
    assert_eq!(
        term.local_shell_cwd(),
        None,
        "a path reported by another machine must not be reused locally"
    );
    // The truthful counterpart: the reporter really sits in the
    // directory it claims (spawned in /tmp), so the claim matches the
    // kernel's cwd for the pane's shell and localhost IS this machine.
    let script = "printf '\\033]7;file://localhost/tmp\\007ok\\n'; sleep 30";
    let term2 = PtyTerminal::new_running(
        "/bin/sh",
        &[String::from("-c"), script.into()],
        std::path::Path::new("/tmp"),
    )
    .unwrap();
    wait_for_grid(&term2, |ls| {
        ls.iter().any(|l| l.contains("ok") && !l.contains("printf"))
    });
    assert_eq!(
        term2.local_shell_cwd(),
        Some(std::path::PathBuf::from("/tmp")),
        "a localhost report IS this machine (RFC 8089)"
    );
}

#[test]
fn osc9_notifications_drain_once() {
    let tmp = tempfile::tempdir().unwrap();
    let script = "printf '\\033]9;build done\\007ok\\n'";
    let term =
        PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script.into()], tmp.path())
            .unwrap();
    wait_for_grid(&term, |ls| {
        ls.iter().any(|l| l.contains("ok") && !l.contains("printf"))
    });
    assert_eq!(term.drain_notifications(), vec![String::from("build done")]);
    assert!(
        term.drain_notifications().is_empty(),
        "a second drain returns nothing"
    );
}

#[test]
fn osc777_and_osc99_notifications_drain_like_osc9() {
    let tmp = tempfile::tempdir().unwrap();
    let script = "printf '\\033]777;notify;Build;finished OK\\007\\033]99;;kitty-style note\\033\\\\\\033]9;iTerm2-style note\\007ok\\n'";
    let term =
        PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script.into()], tmp.path())
            .unwrap();
    // The echoed command wraps past the pane's width, so a wrapped half
    // of it can hold "ok" without "printf": wait for the output line.
    wait_for_grid(&term, |ls| ls.iter().any(|l| l.trim() == "ok"));
    assert_eq!(
        term.drain_notifications(),
        vec![
            String::from("Build: finished OK"),
            String::from("kitty-style note"),
            String::from("iTerm2-style note"),
        ]
    );
}

#[test]
fn notify_triggers_fire_on_completed_output_lines() {
    let tmp = tempfile::tempdir().unwrap();
    // The shell waits for a line of input, so the trigger set is in
    // place before the matching output is ever produced.
    let script = "read x; echo deploy failed; sleep 30";
    let mut term =
        PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script.into()], tmp.path())
            .unwrap();
    let set = crate::triggers::TriggerSet::from_json(
        r#"[ { "regex": "deploy (\\w+)", "action": "notify", "message": "deploy went \\1" } ]"#,
    );
    assert!(set.has_events());
    term.set_triggers(std::sync::Arc::new(set));
    term.write_input(b"\n");
    let mut waited = 0u32;
    let hits = loop {
        let hits = term.drain_trigger_hits();
        if !hits.is_empty() {
            break hits;
        }
        assert!(waited < 8000, "trigger never fired");
        std::thread::sleep(std::time::Duration::from_millis(40));
        waited += 40;
    };
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0].action, crate::triggers::TriggerAction::Notify);
    assert_eq!(hits[0].message, "deploy went failed");
}

/// A banner printed just before handing off to a full-screen app lands
/// in the same PTY chunk as the alt-screen entry: gating the trigger
/// scan on the chunk's FINAL mode dropped that whole chunk (and let a
/// chunk that merely ENDED on the primary screen fire on alt content).
/// The scanner tracks the boundary positionally, so the reader must
/// feed it every chunk unconditionally.
#[test]
fn a_trigger_fires_for_primary_text_that_shares_a_chunk_with_alt_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let script = "read x; printf 'deploy failed\\n\\033[?1049h'; sleep 1; printf '\\033[?1049lok\\n'; sleep 30";
    let mut term =
        PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script.into()], tmp.path())
            .unwrap();
    let set = crate::triggers::TriggerSet::from_json(
        r#"[ { "regex": "deploy (\\w+)", "action": "notify", "message": "deploy went \\1" } ]"#,
    );
    assert!(set.has_events());
    term.set_triggers(std::sync::Arc::new(set));
    term.write_input(b"\n");
    let mut waited = 0u32;
    let hits = loop {
        let hits = term.drain_trigger_hits();
        if !hits.is_empty() {
            break hits;
        }
        assert!(
            waited < 8000,
            "the primary-screen line before the alt entry never fired"
        );
        std::thread::sleep(std::time::Duration::from_millis(40));
        waited += 40;
    };
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0].message, "deploy went failed");
}

#[test]
fn poisoned_zdotdir_from_a_nested_croft_still_loads_the_user_rc() {
    // Regression: every pane exports ZDOTDIR=<shim>, so a croft launched
    // FROM a croft pane inherited it and treated the shim as the user's
    // dotfile dir — the shim then sourced itself in a recursion loop and
    // the user's real .zshrc (their theme, aliases) never ran. A
    // poisoned CROFT_USER_ZDOTDIR pointing at the shim must be ignored
    // in favour of $HOME.
    let Some(zsh) = zsh_or_skip() else { return };
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join(".zshrc"),
        "USER_RC_SENTINEL=loaded\nexport USER_RC_SENTINEL\n",
    )
    .unwrap();
    let cfg_dir = tempfile::tempdir().unwrap();
    let shim = crate::shell_integration::ensure_zsh_shim(cfg_dir.path()).unwrap();
    let mut cmd = CommandBuilder::new(zsh);
    cmd.arg("-i");
    cmd.cwd(home.path());
    cmd.env("HOME", home.path());
    // The poison: both vars point at the shim itself, exactly what a
    // pane's environment hands a nested croft.
    cmd.env("ZDOTDIR", &shim);
    cmd.env("CROFT_USER_ZDOTDIR", &shim);
    let mut term = PtyTerminal::spawn_with(cmd, None).unwrap();
    let mut waited = 0u32;
    while term.prompt_lines().is_empty() {
        assert!(
            waited < 8000,
            "no prompt mark; grid: {:?}",
            term.grid_lines().0
        );
        std::thread::sleep(std::time::Duration::from_millis(40));
        waited += 40;
    }
    term.write_input(b"echo SENTINEL_IS=$USER_RC_SENTINEL\r");
    let mut waited = 0u32;
    loop {
        let (lines, _) = term.grid_lines();
        assert!(
            !lines.iter().any(|l| l.contains("recursion limit")),
            "the shim must never source itself; grid: {lines:?}"
        );
        if lines
            .iter()
            .any(|l| l.contains("SENTINEL_IS=loaded") && !l.contains("echo"))
        {
            break;
        }
        assert!(
            waited < 8000,
            "user rc never ran under a poisoned ZDOTDIR; grid: {lines:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(40));
        waited += 40;
    }
}

/// Build a mark stream from the old `(kind, line, col, dur)` shape,
/// numbering ids by position. The fixtures below care about pairing, not
/// identity, so an id per row keeps them readable while still exercising
/// the id path.
fn views(
    marks: &[(
        crate::shell_integration::OscEvent,
        i32,
        usize,
        Option<std::time::Duration>,
    )],
) -> Vec<MarkView> {
    marks
        .iter()
        .enumerate()
        .map(|(i, (kind, line, col, dur))| MarkView {
            id: i as u64 + 1,
            kind: kind.clone(),
            line: *line,
            col: *col,
            dur: *dur,
        })
        .collect()
}

#[test]
fn pair_decorations_matches_prompts_with_exits() {
    use crate::shell_integration::OscEvent as E;
    let ms = std::time::Duration::from_millis;
    // Two full cycles (ok then failing), then a bare prompt with no
    // command: two records, the bare prompt contributes nothing. The
    // PromptEnd cell names where the typed command starts; the output
    // span runs from the CommandStart line up to the CommandEnd line.
    let marks = [
        (E::PromptStart, -10, 0, None),
        (E::PromptEnd, -10, 5, None),
        (E::CommandStart, -9, 0, None),
        (E::CommandEnd(Some(0)), -7, 0, Some(ms(2400))),
        (E::PromptStart, -7, 0, None),
        (E::PromptEnd, -7, 5, None),
        (E::CommandStart, -6, 0, None),
        (E::CommandEnd(Some(3)), -2, 0, Some(ms(150))),
        (E::PromptStart, -2, 0, None),
    ];
    assert_eq!(
        pair_decorations(&views(&marks)),
        vec![
            CommandDecoration {
                // The `CommandStart` mark is the third row of the
                // fixture, and the id comes from that mark, not from the
                // prompt before it.
                id: 3,
                line: -10,
                exit: Some(0),
                duration: Some(ms(2400)),
                input: Some((-10, 5)),
                output_start: -9,
                output_end: -7,
            },
            CommandDecoration {
                id: 7,
                line: -7,
                exit: Some(3),
                duration: Some(ms(150)),
                input: Some((-7, 5)),
                output_start: -6,
                output_end: -2,
            },
        ]
    );
    // A duplicate CommandEnd (a second integration layer echoing the
    // marks) has no pending CommandStart and must be dropped.
    let dup = [
        (E::PromptStart, -5, 0, None),
        (E::CommandStart, -5, 0, None),
        (E::CommandEnd(Some(0)), -4, 0, Some(ms(90))),
        (E::CommandEnd(Some(0)), -4, 0, None),
    ];
    assert_eq!(pair_decorations(&views(&dup)).len(), 1);
    // A command still running (no CommandEnd yet) has no record.
    let running = [(E::PromptStart, 0, 0, None), (E::CommandStart, 0, 0, None)];
    assert!(pair_decorations(&views(&running)).is_empty());
}

#[test]
fn command_output_and_input_extract_and_select() {
    // Copy Output / Re-run Command need the exact spans back out of the
    // grid: the typed command (from the PromptEnd cell) and the output
    // rows between CommandStart and CommandEnd. Selecting the output
    // highlights precisely those rows.
    let tmp = tempfile::tempdir().unwrap();
    let script = "printf '\\033]133;A\\007$ \\033]133;B\\007cmd --flag\\n\\033]133;C\\007out1\\nout2\\n\\033]133;D;0\\007'";
    let mut term =
        PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script.into()], tmp.path())
            .unwrap();
    let mut waited = 0u32;
    while term.command_decorations().is_empty() {
        assert!(
            waited < 8000,
            "no decoration; marks: {:?}",
            term.command_marks()
        );
        std::thread::sleep(std::time::Duration::from_millis(40));
        waited += 40;
    }
    let deco = term.command_decorations()[0];
    assert_eq!(term.command_input_text(&deco), "cmd --flag");
    assert_eq!(term.command_output_text(&deco), "out1\nout2");
    term.select_command_output(&deco);
    let sel = term.selection().expect("output must be selected");
    let (sr, _, er, _) = sel.normalised();
    assert_eq!(
        (sr, er),
        (deco.output_start, deco.output_end - 1),
        "selection covers exactly the output rows"
    );
    assert_eq!(term.selection_text(), "out1\nout2");
}

#[test]
fn human_duration_formats_ms_seconds_minutes() {
    let d = std::time::Duration::from_millis;
    assert_eq!(human_duration(d(480)), "480ms");
    assert_eq!(human_duration(d(3400)), "3.4s");
    assert_eq!(human_duration(d(59_940)), "59.9s");
    assert_eq!(human_duration(d(125_000)), "2m 05s");
}

#[test]
fn osc133_decorations_carry_exit_and_duration_end_to_end() {
    // A real PTY run emitting the mark protocol: the finished command
    // must surface as one decoration with its exit code and a duration
    // covering the sleep between CommandStart and CommandEnd, and the
    // same completion must arrive once through the notification drain.
    let tmp = tempfile::tempdir().unwrap();
    let script = "printf '\\033]133;A\\007$ cmd\\n\\033]133;B\\007\\033]133;C\\007'; sleep 1; printf 'out\\n\\033]133;D;2\\007'";
    let term =
        PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script.into()], tmp.path())
            .unwrap();
    let mut waited = 0u32;
    loop {
        let decos = term.command_decorations();
        if let Some(d) = decos.first() {
            assert_eq!(d.exit, Some(2), "exit code from 133;D;2");
            let dur = d.duration.expect("duration must be measured");
            // Marks are arrival-stamped by the reader thread, so a late
            // pickup of the pre-sleep chunk shrinks the measured gap
            // under suite load (#65): the floor sits at half the sleep.
            assert!(
                dur >= std::time::Duration::from_millis(500),
                "duration must cover the sleep; got {dur:?}"
            );
            break;
        }
        assert!(
            waited < 8000,
            "no decoration appeared; marks: {:?}",
            term.command_marks()
        );
        std::thread::sleep(std::time::Duration::from_millis(40));
        waited += 40;
    }
    let finished = term.drain_finished_commands();
    assert_eq!(finished.len(), 1, "one completion in the drain");
    assert_eq!(finished[0].exit, Some(2));
    assert!(finished[0].dur >= std::time::Duration::from_millis(500));
    assert!(
        term.drain_finished_commands().is_empty(),
        "a second drain returns nothing"
    );
}

#[test]
fn decoration_dot_paints_on_the_left_border_and_hit_tests() {
    // The finished command's prompt row gets a VS Code-style dot on the
    // pane's left border: red here (exit 2). Clicking the dot resolves
    // back to the record via decoration_at_screen.
    let tmp = tempfile::tempdir().unwrap();
    let script =
        "printf '\\033]133;A\\007$ cmd\\n\\033]133;B\\007\\033]133;C\\007out\\n\\033]133;D;2\\007'";
    let mut term =
        PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script.into()], tmp.path())
            .unwrap();
    let mut waited = 0u32;
    while term.command_decorations().is_empty() {
        assert!(
            waited < 8000,
            "no decoration; marks: {:?}",
            term.command_marks()
        );
        std::thread::sleep(std::time::Duration::from_millis(40));
        waited += 40;
    }
    // The dot sits on a boxed pane's border line.
    term.theme = crate::theme::Theme::BLACK;
    let area = Rect::new(0, 0, 40, 12);
    let mut buf = Buffer::empty(area);
    (&mut term).render(area, &mut buf);
    let deco = term.command_decorations()[0];
    // Live view: display_offset 0, viewport row = grid line; +1 for the
    // top border.
    let y = 1 + u16::try_from(deco.line).unwrap();
    let cell = &buf[(0, y)];
    assert_eq!(
        cell.symbol(),
        "●",
        "dot on the left border at the prompt row"
    );
    assert_eq!(
        cell.style().fg,
        Some(Color::Rgb(0xf1, 0x4c, 0x4c)),
        "non-zero exit paints the error red"
    );
    // The border above the dot is untouched.
    assert_eq!(buf[(0, y - 1)].symbol(), "│");
    assert_eq!(
        term.decoration_at_screen(0, y),
        Some(deco),
        "clicking the dot resolves the record"
    );
    assert_eq!(
        term.decoration_at_screen(0, y - 1),
        None,
        "no record on a bare border row"
    );
}

/// Croft Loch keeps a command that went well on its palette: the dot is
/// the accent eased into the canvas, not VS Code's blue. An unfocused
/// pane's top edge is the hairline, so its seam with the tab strip is
/// drawn even without the focus lip.
#[test]
fn croft_loch_paints_a_quiet_accent_dot_and_a_hairline_top_edge() {
    let tmp = tempfile::tempdir().unwrap();
    let script =
        "printf '\\033]133;A\\007$ cmd\\n\\033]133;B\\007\\033]133;C\\007out\\n\\033]133;D;0\\007'";
    let mut term =
        PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script.into()], tmp.path())
            .unwrap();
    let mut waited = 0u32;
    while term.command_decorations().is_empty() {
        assert!(waited < 8000, "no decoration");
        std::thread::sleep(std::time::Duration::from_millis(40));
        waited += 40;
    }
    term.theme = crate::theme::Theme::LOCH;
    term.focused = false;
    let t = crate::widgets::surface::tones(term.theme).unwrap();
    let area = Rect::new(0, 0, 40, 12);
    let mut buf = Buffer::empty(area);
    (&mut term).render(area, &mut buf);
    let deco = term.command_decorations()[0];
    let y = 1 + u16::try_from(deco.line).unwrap();
    assert_eq!(buf[(0, y)].symbol(), "●");
    assert_eq!(
        buf[(0, y)].fg,
        crate::gradient::rgb_color(crate::gradient::lerp_rgb(t.accent, t.canvas, 0.25))
    );
    assert_eq!(buf[(5, 0)].symbol(), "\u{2594}");
    assert_eq!(buf[(5, 0)].fg, t.hairline());
}

#[test]
fn foreign_terminal_shim_zdotdir_chains_to_the_user_rc() {
    // Regression: Ghostty and kitty inject their own shell integration by
    // pointing ZDOTDIR at their shim dir before launching their child.
    // When that child is croft itself (ghostty --initial-command=croft),
    // croft inherited the foreign shim as the "user" dotfile dir — a dir
    // with no .zshrc — so the user's real rc (theme, aliases) never ran.
    // The wrapper contract (kitty invented it, Ghostty copied it) is that
    // the foreign .zshenv restores the REAL ZDOTDIR when sourced; croft's
    // shim must re-read ZDOTDIR after chaining and source the user's rc
    // from wherever it landed.
    let Some(zsh) = zsh_or_skip() else { return };
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join(".zshrc"),
        "USER_RC_SENTINEL=loaded\nexport USER_RC_SENTINEL\n",
    )
    .unwrap();
    // A Ghostty-style wrapper shim: only a .zshenv, which restores the
    // real ZDOTDIR (here: unset, meaning HOME) and chains onward.
    let foreign = tempfile::tempdir().unwrap();
    std::fs::write(
        foreign.path().join(".zshenv"),
        "builtin unset ZDOTDIR\n[[ -r \"$HOME/.zshenv\" ]] && builtin source \"$HOME/.zshenv\"\n",
    )
    .unwrap();
    let cfg_dir = tempfile::tempdir().unwrap();
    let shim = crate::shell_integration::ensure_zsh_shim(cfg_dir.path()).unwrap();
    let mut cmd = CommandBuilder::new(zsh);
    cmd.arg("-i");
    cmd.cwd(home.path());
    cmd.env("HOME", home.path());
    cmd.env("ZDOTDIR", &shim);
    // What resolve_user_zdotdir hands the pane: the inherited foreign dir.
    cmd.env("CROFT_USER_ZDOTDIR", foreign.path());
    let mut term = PtyTerminal::spawn_with(cmd, None).unwrap();
    let mut waited = 0u32;
    while term.prompt_lines().is_empty() {
        assert!(
            waited < 8000,
            "no prompt mark; grid: {:?}",
            term.grid_lines().0
        );
        std::thread::sleep(std::time::Duration::from_millis(40));
        waited += 40;
    }
    term.write_input(b"echo SENTINEL_IS=$USER_RC_SENTINEL\r");
    let mut waited = 0u32;
    loop {
        let (lines, _) = term.grid_lines();
        if lines
            .iter()
            .any(|l| l.contains("SENTINEL_IS=loaded") && !l.contains("echo"))
        {
            break;
        }
        assert!(
            waited < 8000,
            "user rc never ran behind a foreign terminal shim; grid: {lines:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(40));
        waited += 40;
    }
}

#[test]
fn zsh_shim_emits_prompt_marks_end_to_end() {
    // The real proof: an interactive zsh reading croft's ZDOTDIR shim
    // must emit OSC 133 prompt marks and an OSC 7 cwd report at its
    // first prompt, with the user's own (here: empty) dotfiles sourced.
    let Some(zsh) = zsh_or_skip() else { return };
    let user_dir = tempfile::tempdir().unwrap();
    let cfg_dir = tempfile::tempdir().unwrap();
    let shim = crate::shell_integration::ensure_zsh_shim(cfg_dir.path()).unwrap();
    let mut cmd = CommandBuilder::new(zsh);
    cmd.arg("-i");
    cmd.cwd(user_dir.path());
    cmd.env("ZDOTDIR", &shim);
    cmd.env("CROFT_USER_ZDOTDIR", user_dir.path());
    let term = PtyTerminal::spawn_with(cmd, None).unwrap();
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(1000),
        "zsh to emit a prompt mark",
        || !term.prompt_lines().is_empty(),
    );
    assert!(
        term.shell_cwd().is_some(),
        "the shim's precmd must also report the cwd via OSC 7"
    );
}

/// The zsh shim must emit `$PWD` percent-escaped (fish already does):
/// the sniffer unconditionally percent-DECODES OSC 7 paths, so a real
/// directory containing `%41` used to be reported as `A`, corrupting
/// the cwd every consumer sees.
#[test]
fn a_percent_directory_round_trips_through_the_zsh_shim() {
    let Some(zsh) = zsh_or_skip() else { return };
    let user_dir = tempfile::tempdir().unwrap();
    let odd = user_dir.path().join("a%41b");
    std::fs::create_dir(&odd).unwrap();
    let cfg_dir = tempfile::tempdir().unwrap();
    let shim = crate::shell_integration::ensure_zsh_shim(cfg_dir.path()).unwrap();
    let mut cmd = CommandBuilder::new(zsh);
    cmd.arg("-i");
    cmd.cwd(&odd);
    cmd.env("ZDOTDIR", &shim);
    cmd.env("CROFT_USER_ZDOTDIR", user_dir.path());
    let term = PtyTerminal::spawn_with(cmd, None).unwrap();
    // Two state-gated stages instead of one wall-clock guess: under full-
    // suite load, interactive zsh startup alone can eat a fixed budget and
    // the timeout measures the machine, not the shim (#109). First wait
    // for the prompt mark (proof the precmd hook ran), THEN the OSC 7
    // report — emitted by that same precmd — must follow almost at once.
    let mut waited = 0u32;
    while term.prompt_lines().is_empty() {
        assert!(
            waited < 20_000,
            "zsh never emitted a prompt mark; grid: {:?}",
            term.grid_lines().0
        );
        std::thread::sleep(std::time::Duration::from_millis(40));
        waited += 40;
    }
    let mut waited = 0u32;
    let cwd = loop {
        if let Some(c) = term.shell_cwd() {
            break c;
        }
        assert!(
            waited < 4000,
            "the shim's precmd ran (prompt mark present) but never reported a cwd; grid: {:?}",
            term.grid_lines().0
        );
        std::thread::sleep(std::time::Duration::from_millis(40));
        waited += 40;
    };
    assert!(
        cwd.ends_with("a%41b"),
        "the literal directory name must survive the OSC 7 round trip, got {cwd:?}"
    );
}

/// A bash new enough for `$ENV` + `--posix` injection (>= 4.4), for the
/// e2e tests: Homebrew/Linux bash qualifies, macOS's system 3.2 not.
fn modern_bash() -> Option<&'static str> {
    ["/opt/homebrew/bin/bash", "/usr/local/bin/bash", "/bin/bash"]
        .into_iter()
        .find(|b| bash_env_injection_supported(b))
}

/// An empty Enter in a bash whose user set PROMPT_COMMAND as an ARRAY
/// (bash 5.1+, ble.sh and prompt frameworks do) must not fabricate a
/// command: the DEBUG-trap guard joined the array with a SPACE
/// (`${arr[*]}` under default IFS), so its own precmd entries never
/// matched and every blank prompt emitted a spurious 133;C + 133;D —
/// a phantom decoration that hijacked Cmd+K Shift+R/C/S.
#[test]
fn an_empty_enter_with_an_array_prompt_command_fabricates_no_command() {
    let Some(bash) = modern_bash() else {
        return;
    };
    // Array PROMPT_COMMAND execution needs bash >= 5.1.
    let out = std::process::Command::new(bash)
        .args(["-c", "echo ${BASH_VERSINFO[0]}${BASH_VERSINFO[1]}"])
        .output()
        .unwrap();
    let v: u32 = String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .unwrap_or(0);
    if v < 51 {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join(".bashrc"),
        "mytheme() { :; }\nPROMPT_COMMAND=(mytheme)\n",
    )
    .unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let mut cmd = CommandBuilder::new(bash);
    cmd.cwd(home.path());
    cmd.env("HOME", home.path());
    let args = apply_shell_integration_env(&mut cmd, bash, cfg.path(), Vec::new());
    for a in &args {
        cmd.arg(a);
    }
    cmd.arg("-i");
    let mut term = PtyTerminal::spawn_with(cmd, None).unwrap();
    let mut waited = 0u32;
    while term.prompt_lines().is_empty() {
        assert!(waited < 8000, "bash never emitted a prompt mark");
        std::thread::sleep(std::time::Duration::from_millis(40));
        waited += 40;
    }
    let prompts_before = term.prompt_lines().len();
    term.write_input(b"\n");
    let mut waited = 0u32;
    while term.prompt_lines().len() <= prompts_before {
        assert!(waited < 8000, "the empty Enter never produced a new prompt");
        std::thread::sleep(std::time::Duration::from_millis(40));
        waited += 40;
    }
    assert!(
        pair_decorations(&term.marks_snapshot()).is_empty(),
        "an empty Enter must not fabricate a finished command"
    );
}

/// #1101: a string PROMPT_COMMAND ending in `;` (zoxide's init), in
/// `&` or a `# comment`, or starting with `;`, joined to croft's hooks
/// with `;`, made bash reject the whole string at every prompt, so no
/// mark was ever sent. Each shape must still mark a failed command, and
/// empty Enters must still fabricate none: `_croft_precmd` runs first
/// and is what the DEBUG trap compares, never the user's entry.
#[test]
fn a_prompt_command_ending_in_a_separator_keeps_the_marks() {
    let Some(bash) = modern_bash() else {
        return;
    };
    for user in [
        "hook;",
        "hook; ",
        "hook\n",
        ";hook",
        "hook # note",
        "hook &",
    ] {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join(".bashrc"),
            format!(
                "hook() {{ :; }}\nPROMPT_COMMAND=$'{}'\n",
                user.replace('\n', "\\n")
            ),
        )
        .unwrap();
        let cfg = tempfile::tempdir().unwrap();
        let mut cmd = CommandBuilder::new(bash);
        cmd.cwd(home.path());
        cmd.env("HOME", home.path());
        for a in apply_shell_integration_env(&mut cmd, bash, cfg.path(), Vec::new()) {
            cmd.arg(a);
        }
        cmd.arg("-i");
        let mut term = PtyTerminal::spawn_with(cmd, None).unwrap();
        let wait = |term: &PtyTerminal, what: &str, done: &dyn Fn(&PtyTerminal) -> bool| {
            crate::test_budget::await_spawned(std::time::Duration::from_millis(500), what, || {
                done(term)
            });
        };
        wait(&term, "bash's first prompt mark", &|t| {
            !t.prompt_lines().is_empty()
        });
        // Two: a start mark fabricated by the first would be closed
        // by the second as a finished command.
        for _ in 0..2 {
            let prompts = term.prompt_lines().len();
            term.write_input(b"\n");
            wait(&term, "the prompt after an empty Enter", &|t| {
                t.prompt_lines().len() > prompts
            });
        }
        assert!(
            pair_decorations(&term.marks_snapshot()).is_empty(),
            "{user:?}: an empty Enter fabricated a command"
        );
        term.write_input(b"false\r");
        wait(&term, "the failed command's end mark", &|t| {
            t.command_marks()
                .iter()
                .any(|(k, _)| *k == crate::shell_integration::OscEvent::CommandEnd(Some(1)))
        });
        let (lines, _) = term.grid_lines();
        assert!(
            !lines.iter().any(|l| l.contains("syntax error")),
            "{user:?}: {lines:?}"
        );
    }
}

#[test]
fn shell_integration_env_prepends_posix_for_modern_bash_only() {
    // bash is injected by inserting `--posix` before `-l` and pointing
    // $ENV at croft's shim — but only from 4.4 (the kitty/Ghostty
    // floor): macOS's system bash 3.2 ignores $ENV in posix mode, so
    // it reads the shim through `--rcfile` instead (VS Code's way),
    // non-login, with the shim replaying the login files. zsh
    // (ZDOTDIR) and fish (XDG_DATA_DIRS) are env-only and add nothing.
    let login = || vec!["-l".to_string()];
    let cfg = tempfile::tempdir().unwrap();
    if let Some(bash) = modern_bash() {
        let mut cmd = CommandBuilder::new(bash);
        assert_eq!(
            apply_shell_integration_env(&mut cmd, bash, cfg.path(), login()),
            vec!["--posix".to_string(), "-l".to_string()]
        );
    }
    assert!(!parse_bash_version_supported("3.2"));
    assert!(!parse_bash_version_supported("4.3"));
    assert!(!parse_bash_version_supported("garbage"));
    assert!(parse_bash_version_supported("4.4"));
    assert!(parse_bash_version_supported("5.2"));
    assert!(parse_bash_version_supported("10.0"));
    if std::path::Path::new("/bin/bash").exists() && !bash_env_injection_supported("/bin/bash") {
        // On a macOS box with system bash 3.2: the shim as its rcfile,
        // with no `-l`, which would make bash ignore `--rcfile`.
        let shim = crate::shell_integration::ensure_bash_shim(cfg.path()).unwrap();
        let mut cmd = CommandBuilder::new("/bin/bash");
        assert_eq!(
            apply_shell_integration_env(&mut cmd, "/bin/bash", cfg.path(), login()),
            vec!["--rcfile".to_string(), shim.display().to_string()],
            "bash 3.2 must take the shim as its rcfile, without posix injection"
        );
    }
    for shell in ["/bin/zsh", "/usr/bin/fish", "/bin/dash"] {
        let mut cmd = CommandBuilder::new(shell);
        assert_eq!(
            apply_shell_integration_env(&mut cmd, shell, cfg.path(), login()),
            login(),
            "{shell} must not gain args"
        );
    }
}

#[test]
fn zsh_shim_marks_the_prompt_end_for_input_extraction() {
    // Copy Output / Re-run Command read the typed command from the
    // PromptEnd (133;B) mark. The shim must emit it itself — relying on
    // a chained foreign integration (Ghostty's) leaves remote Linux
    // panes with no input span at all.
    let Some(zsh) = zsh_or_skip() else { return };
    let user_dir = tempfile::tempdir().unwrap();
    let cfg_dir = tempfile::tempdir().unwrap();
    let shim = crate::shell_integration::ensure_zsh_shim(cfg_dir.path()).unwrap();
    let mut cmd = CommandBuilder::new(zsh);
    cmd.arg("-i");
    cmd.cwd(user_dir.path());
    cmd.env("ZDOTDIR", &shim);
    cmd.env("CROFT_USER_ZDOTDIR", user_dir.path());
    let term = PtyTerminal::spawn_with(cmd, None).unwrap();
    let mut waited = 0u32;
    loop {
        let marks = term.command_marks();
        if marks
            .iter()
            .any(|(k, _)| *k == crate::shell_integration::OscEvent::PromptEnd)
        {
            break;
        }
        assert!(
            waited < 8000,
            "zsh never emitted a PromptEnd mark; marks: {marks:?}, grid: {:?}",
            term.grid_lines().0
        );
        std::thread::sleep(std::time::Duration::from_millis(40));
        waited += 40;
    }
}

#[test]
fn bash_shim_via_posix_env_emits_marks_and_sources_the_user_profile() {
    // The kitty/Ghostty bash injection end-to-end, exactly as croft
    // spawns it: `bash --posix -l` with ENV pointing at croft's shim.
    // In posix mode bash reads ONLY $ENV, so the shim must back out of
    // posix mode, replay the login startup files (the user's
    // .bash_profile here), and emit the full mark cycle with exit codes.
    // Needs a bash >= 4.4 (macOS system 3.2 ignores $ENV in posix mode
    // and is never injected).
    let Some(bash) = modern_bash() else {
        return;
    };
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join(".bash_profile"),
        "USER_RC_SENTINEL=loaded\nexport USER_RC_SENTINEL\n",
    )
    .unwrap();
    let cfg_dir = tempfile::tempdir().unwrap();
    let shim = crate::shell_integration::ensure_bash_shim(cfg_dir.path()).unwrap();
    let mut cmd = CommandBuilder::new(bash);
    cmd.arg("--posix");
    cmd.arg("-l");
    cmd.cwd(home.path());
    cmd.env("HOME", home.path());
    cmd.env("ENV", &shim);
    cmd.env("CROFT_BASH_INJECT", "1");
    let mut term = PtyTerminal::spawn_with(cmd, None).unwrap();
    let mut waited = 0u32;
    while term.prompt_lines().is_empty() {
        assert!(
            waited < 8000,
            "bash never emitted a prompt mark; grid: {:?}",
            term.grid_lines().0
        );
        std::thread::sleep(std::time::Duration::from_millis(40));
        waited += 40;
    }
    assert!(
        term.shell_cwd().is_some(),
        "the shim's precmd must report cwd via OSC 7"
    );
    term.write_input(b"echo SENTINEL_IS=$USER_RC_SENTINEL\r");
    let mut waited = 0u32;
    loop {
        let (lines, _) = term.grid_lines();
        if lines
            .iter()
            .any(|l| l.contains("SENTINEL_IS=loaded") && !l.contains("echo"))
        {
            break;
        }
        assert!(
            waited < 8000,
            "user .bash_profile never ran through the shim; grid: {lines:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(40));
        waited += 40;
    }
    // A failing command must round-trip its exit code through 133;D and
    // the shim must mark the prompt end for input extraction.
    term.write_input(b"false\r");
    let mut waited = 0u32;
    loop {
        use crate::shell_integration::OscEvent as E;
        let marks = term.command_marks();
        if marks.iter().any(|(k, _)| *k == E::CommandEnd(Some(1))) {
            assert!(
                marks.iter().any(|(k, _)| *k == E::PromptEnd),
                "bash PS1 must carry the 133;B input-start mark; marks: {marks:?}"
            );
            break;
        }
        assert!(
            waited < 8000,
            "no CommandEnd(1) after `false`; marks: {marks:?}, grid: {:?}",
            term.grid_lines().0
        );
        std::thread::sleep(std::time::Duration::from_millis(40));
        waited += 40;
    }
}

/// A fish binary for the e2e: common install paths, then `$PATH`.
/// Absent on plain macOS (skips), present on Linux boxes and via nix.
fn find_fish() -> Option<String> {
    let fixed = [
        "/opt/homebrew/bin/fish",
        "/usr/local/bin/fish",
        "/usr/bin/fish",
    ];
    if let Some(f) = fixed.iter().find(|f| std::path::Path::new(f).exists()) {
        return Some((*f).to_string());
    }
    let path = std::env::var("PATH").ok()?;
    path.split(':')
        .map(|d| std::path::Path::new(d).join("fish"))
        .find(|p| p.is_file())
        .map(|p| p.display().to_string())
}

#[test]
fn fish_integration_restores_xdg_dirs_and_yields_single_native_marks() {
    // fish end-to-end through croft's injection env: fish 4 must emit
    // the marks natively (croft's vendor script installs NO second
    // emitter, unlike Ghostty's which double-marks), the exit code must
    // round-trip, and the script must scrub the injection from the
    // session env so children never see it.
    let Some(fish) = find_fish() else {
        return;
    };
    let home = tempfile::tempdir().unwrap();
    let cfg_dir = tempfile::tempdir().unwrap();
    let dir = crate::shell_integration::ensure_fish_integration(cfg_dir.path()).unwrap();
    let mut cmd = CommandBuilder::new(&fish);
    cmd.arg("-i");
    cmd.arg("-l");
    cmd.cwd(home.path());
    cmd.env("HOME", home.path());
    cmd.env(
        "XDG_DATA_DIRS",
        crate::shell_integration::fish_xdg_data_dirs(&dir, None),
    );
    cmd.env("CROFT_FISH_XDG_DATA_DIR", &dir);
    let mut term = PtyTerminal::spawn_with(cmd, None).unwrap();
    let mut waited = 0u32;
    while term.prompt_lines().is_empty() {
        assert!(
            waited < 8000,
            "fish never emitted a prompt mark; grid: {:?}",
            term.grid_lines().0
        );
        std::thread::sleep(std::time::Duration::from_millis(40));
        waited += 40;
    }
    term.write_input(b"false\r");
    let mut waited = 0u32;
    loop {
        use crate::shell_integration::OscEvent as E;
        let marks = term.command_marks();
        if marks.iter().any(|(k, _)| *k == E::CommandEnd(Some(1))) {
            break;
        }
        assert!(
            waited < 8000,
            "no CommandEnd(1) after `false`; marks: {marks:?}, grid: {:?}",
            term.grid_lines().0
        );
        std::thread::sleep(std::time::Duration::from_millis(40));
        waited += 40;
    }
    // Exactly one finished record for `false`: a second emitter would
    // double-decorate every command.
    let failing: Vec<_> = term
        .command_decorations()
        .into_iter()
        .filter(|d| d.exit == Some(1))
        .collect();
    assert_eq!(failing.len(), 1, "duplicate decorations: {failing:?}");
    // The injection env must be gone inside the session.
    term.write_input(b"echo DIRS=$XDG_DATA_DIRS; set -q CROFT_FISH_XDG_DATA_DIR; and echo SI_LEAKED; or echo SI_CLEAN\r");
    let needle = format!("{}", dir.display());
    let mut waited = 0u32;
    loop {
        let (lines, _) = term.grid_lines();
        if lines
            .iter()
            .any(|l| l.contains("SI_CLEAN") && !l.contains("echo"))
        {
            assert!(
                !lines
                    .iter()
                    .any(|l| l.contains("SI_LEAKED") && !l.contains("echo")),
                "grid: {lines:?}"
            );
            let dirs_line = lines
                .iter()
                .find(|l| l.contains("DIRS=") && !l.contains("echo"))
                .cloned()
                .unwrap_or_default();
            assert!(
                !dirs_line.contains(&needle),
                "XDG_DATA_DIRS still carries the injection dir: {dirs_line}"
            );
            break;
        }
        assert!(
            waited < 8000,
            "env scrub probe never answered; grid: {lines:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(40));
        waited += 40;
    }
}

#[test]
fn the_pane_bin_goes_first_on_the_pane_path() {
    let mut cmd = CommandBuilder::new("/bin/sh");
    cmd.env("PATH", "/opt/homebrew/bin:/pane-bin:/usr/bin");
    apply_pane_bin(&mut cmd, Some(std::path::Path::new("/pane-bin")));
    assert_eq!(
        cmd.get_env("PATH"),
        Some(std::ffi::OsStr::new("/pane-bin:/opt/homebrew/bin:/usr/bin"))
    );
    assert_eq!(
        cmd.get_env(crate::shell_integration::PANE_BIN_ENV),
        Some(std::ffi::OsStr::new("/pane-bin"))
    );
}

#[test]
fn a_pane_with_no_bin_of_its_own_drops_an_inherited_one() {
    // A croft launched from a croft pane inherits that pane's bin, which
    // names the OUTER croft. Passing it on would hand this croft's panes
    // a different croft than the window they run in.
    let mut cmd = CommandBuilder::new("/bin/sh");
    cmd.env("PATH", "/outer-pane-bin:/usr/bin");
    cmd.env(crate::shell_integration::PANE_BIN_ENV, "/outer-pane-bin");
    apply_pane_bin(&mut cmd, None);
    assert_eq!(cmd.get_env(crate::shell_integration::PANE_BIN_ENV), None);
    assert_eq!(cmd.get_env("PATH"), Some(std::ffi::OsStr::new("/usr/bin")));
}

#[test]
fn an_unset_path_is_left_for_the_shell_to_default() {
    // PATH=<pane bin> alone would leave the shell without /usr/bin.
    let mut cmd = CommandBuilder::new("/bin/sh");
    cmd.env_remove("PATH");
    apply_pane_bin(&mut cmd, Some(std::path::Path::new("/pane-bin")));
    assert_eq!(cmd.get_env("PATH"), None);
    assert_eq!(
        cmd.get_env(crate::shell_integration::PANE_BIN_ENV),
        Some(std::ffi::OsStr::new("/pane-bin"))
    );
}

/// A pane bin linking to a stand-in for the running croft, and another
/// dir whose `croft` a user rc puts first on PATH (#1854). Each prints
/// which one it is.
struct TwoCrofts {
    _tmp: tempfile::TempDir,
    pane_bin: std::path::PathBuf,
    other_dir: std::path::PathBuf,
}

fn two_crofts() -> TwoCrofts {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let script = |path: &std::path::Path, name: &str| {
        std::fs::write(path, format!("#!/bin/sh\necho CROFT_IS={name}\n")).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    let running = tmp.path().join("running-croft");
    script(&running, "running");
    let other_dir = tmp.path().join("other");
    std::fs::create_dir(&other_dir).unwrap();
    script(&other_dir.join("croft"), "other");
    let pane_bin =
        crate::shell_integration::ensure_pane_bin(&tmp.path().join("cache"), &running).unwrap();
    TwoCrofts {
        _tmp: tmp,
        pane_bin,
        other_dir,
    }
}

/// Type `input`; the rest of the first line on screen starting `prefix`.
fn pane_answer(term: &mut PtyTerminal, input: &[u8], prefix: &str) -> String {
    term.write_input(input);
    let answer = || {
        term.grid_lines()
            .0
            .iter()
            .find_map(|l| l.trim().strip_prefix(prefix).map(str::to_string))
    };
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(1000),
        &format!("an answer to {:?}", String::from_utf8_lossy(input)),
        || answer().is_some(),
    );
    answer().unwrap()
}

/// Type `croft` at the pane's first prompt; the name of the croft that ran.
fn croft_that_answers(term: &mut PtyTerminal) -> String {
    // Text on screen, not a prompt mark, so a shell croft does not
    // integrate is waited for too.
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(1000),
        "the shell's first prompt",
        || term.grid_lines().0.iter().any(|l| !l.trim().is_empty()),
    );
    pane_answer(term, b"croft\r", "CROFT_IS=")
}

/// A zsh login pane whose user `file` prepends the dir holding the other
/// croft, with the pane bin when `pane_bin`.
fn zsh_login_pane(
    crofts: &TwoCrofts,
    home: &std::path::Path,
    cfg_dir: &std::path::Path,
    file: &str,
    pane_bin: bool,
) -> Option<PtyTerminal> {
    let zsh = zsh_or_skip()?;
    std::fs::write(
        home.join(file),
        format!("export PATH=\"{}:$PATH\"\n", crofts.other_dir.display()),
    )
    .unwrap();
    let shim = crate::shell_integration::ensure_zsh_shim(cfg_dir).unwrap();
    let mut cmd = CommandBuilder::new(zsh);
    cmd.arg("-l");
    cmd.arg("-i");
    cmd.cwd(home);
    cmd.env("HOME", home);
    cmd.env("ZDOTDIR", &shim);
    cmd.env("CROFT_USER_ZDOTDIR", home);
    apply_pane_bin(&mut cmd, pane_bin.then_some(crofts.pane_bin.as_path()));
    Some(PtyTerminal::spawn_with(cmd, None).unwrap())
}

#[test]
fn a_zsh_pane_runs_its_own_croft_when_the_user_profile_prepends_another() {
    // #1854: the window was cargo's croft 0.2.32 while `croft --version`
    // in its pane printed Homebrew's 0.2.28, because `.zprofile` runs
    // `brew shellenv`, which puts /opt/homebrew/bin first after croft
    // set the pane's PATH.
    let crofts = two_crofts();
    let (home, cfg_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let Some(mut term) = zsh_login_pane(&crofts, home.path(), cfg_dir.path(), ".zprofile", true)
    else {
        return;
    };
    assert_eq!(croft_that_answers(&mut term), "running");
}

#[test]
fn a_zsh_pane_runs_its_own_croft_when_the_user_login_file_prepends_another() {
    // A login zsh reads `.zlogin` after `.zshrc`, so a prepend there
    // lands after the shim's `.zshrc` has put the pane bin first.
    let crofts = two_crofts();
    let (home, cfg_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let Some(mut term) = zsh_login_pane(&crofts, home.path(), cfg_dir.path(), ".zlogin", true)
    else {
        return;
    };
    assert_eq!(croft_that_answers(&mut term), "running");
}

#[test]
fn a_zsh_pane_without_a_pane_bin_keeps_the_user_path() {
    // With no pane bin the shims must leave PATH as the user's files
    // made it: their own croft first, and no empty entry, which a shell
    // reads as the current directory.
    let crofts = two_crofts();
    let (home, cfg_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let Some(mut term) = zsh_login_pane(&crofts, home.path(), cfg_dir.path(), ".zlogin", false)
    else {
        return;
    };
    assert_eq!(croft_that_answers(&mut term), "other");
    assert_eq!(
        pane_answer(
            &mut term,
            b"case :$PATH: in *::*) echo E=1;; *) echo E=0;; esac\r",
            "E="
        ),
        "0"
    );
}

#[test]
fn a_bash_login_pane_runs_its_own_croft_when_the_profile_prepends_another() {
    // The production argv, so macOS's bash 3.2, which cannot take the
    // `$ENV` injection, is covered as well as a modern bash.
    let bash = "/bin/bash";
    if !std::path::Path::new(bash).exists() {
        return;
    }
    let crofts = two_crofts();
    let (home, cfg_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    std::fs::write(
        home.path().join(".bash_profile"),
        format!("export PATH=\"{}:$PATH\"\n", crofts.other_dir.display()),
    )
    .unwrap();
    let mut cmd = interactive_shell_command(bash, cfg_dir.path());
    cmd.cwd(home.path());
    cmd.env("HOME", home.path());
    cmd.env("HISTFILE", home.path().join(".bash_history"));
    apply_pane_bin(&mut cmd, Some(&crofts.pane_bin));
    let mut term = PtyTerminal::spawn_with(cmd, None).unwrap();
    assert_eq!(croft_that_answers(&mut term), "running");
}

#[test]
fn a_bash_login_pane_without_env_injection_reports_command_exits() {
    // macOS's bash 3.2 takes the shim through `--rcfile`, so its panes
    // get the prompt and command marks a modern bash does.
    if !std::path::Path::new("/bin/bash").exists() || bash_env_injection_supported("/bin/bash") {
        return;
    }
    let (home, cfg_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut cmd = interactive_shell_command("/bin/bash", cfg_dir.path());
    cmd.cwd(home.path());
    cmd.env("HOME", home.path());
    cmd.env("HISTFILE", home.path().join(".bash_history"));
    let mut term = PtyTerminal::spawn_with(cmd, None).unwrap();
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(1000),
        "bash 3.2's first prompt mark",
        || !term.prompt_lines().is_empty(),
    );
    term.write_input(b"false\r");
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(1000),
        "the CommandEnd(1) mark for `false`",
        || {
            term.command_marks()
                .iter()
                .any(|(k, _)| *k == crate::shell_integration::OscEvent::CommandEnd(Some(1)))
        },
    );
}

#[test]
fn a_non_login_bash_without_env_injection_replays_no_login_files() {
    // Below 4.4 bash reads the shim as its rcfile, which a login bash
    // ignores, so the shim replays the login files itself when asked.
    // Only a login spawn may ask.
    if !std::path::Path::new("/bin/bash").exists() || bash_env_injection_supported("/bin/bash") {
        return;
    }
    let cfg = tempfile::tempdir().unwrap();
    let shim = crate::shell_integration::ensure_bash_shim(cfg.path()).unwrap();
    let rcfile = vec!["--rcfile".to_string(), shim.display().to_string()];
    let mut cmd = CommandBuilder::new("/bin/bash");
    assert_eq!(
        apply_shell_integration_env(&mut cmd, "/bin/bash", cfg.path(), Vec::new()),
        rcfile
    );
    assert_eq!(cmd.get_env("CROFT_BASH_LOGIN"), None);
    let mut cmd = CommandBuilder::new("/bin/bash");
    assert_eq!(
        apply_shell_integration_env(&mut cmd, "/bin/bash", cfg.path(), vec!["-l".to_string()]),
        rcfile
    );
    assert_eq!(
        cmd.get_env("CROFT_BASH_LOGIN"),
        Some(std::ffi::OsStr::new("1"))
    );
}

#[test]
fn a_bash_pane_runs_its_own_croft_when_the_user_rc_prepends_another() {
    // The shim is read through `--rcfile` here, which every bash honours,
    // so macOS's 3.2 runs the same script `$ENV` hands a modern bash.
    let bash = "/bin/bash";
    if !std::path::Path::new(bash).exists() {
        return;
    }
    let crofts = two_crofts();
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join(".bashrc"),
        format!("export PATH=\"{}:$PATH\"\n", crofts.other_dir.display()),
    )
    .unwrap();
    let cfg_dir = tempfile::tempdir().unwrap();
    let shim = crate::shell_integration::ensure_bash_shim(cfg_dir.path()).unwrap();
    let mut cmd = CommandBuilder::new(bash);
    cmd.arg("--rcfile");
    cmd.arg(&shim);
    cmd.arg("-i");
    cmd.cwd(home.path());
    cmd.env("HOME", home.path());
    cmd.env("CROFT_BASH_INJECT", "1");
    apply_pane_bin(&mut cmd, Some(&crofts.pane_bin));
    let mut term = PtyTerminal::spawn_with(cmd, None).unwrap();
    assert_eq!(croft_that_answers(&mut term), "running");
}

#[test]
fn a_fish_pane_runs_its_own_croft_when_the_user_config_prepends_another() {
    let Some(fish) = find_fish() else {
        return;
    };
    let crofts = two_crofts();
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join(".config");
    std::fs::create_dir_all(config.join("fish")).unwrap();
    std::fs::write(
        config.join("fish").join("config.fish"),
        format!("set -gx PATH {} $PATH\n", crofts.other_dir.display()),
    )
    .unwrap();
    let cfg_dir = tempfile::tempdir().unwrap();
    let dir = crate::shell_integration::ensure_fish_integration(cfg_dir.path()).unwrap();
    let mut cmd = CommandBuilder::new(&fish);
    cmd.arg("-i");
    cmd.arg("-l");
    cmd.cwd(home.path());
    cmd.env("HOME", home.path());
    cmd.env("XDG_CONFIG_HOME", &config);
    cmd.env(
        "XDG_DATA_DIRS",
        crate::shell_integration::fish_xdg_data_dirs(&dir, None),
    );
    cmd.env("CROFT_FISH_XDG_DATA_DIR", &dir);
    apply_pane_bin(&mut cmd, Some(&crofts.pane_bin));
    let mut term = PtyTerminal::spawn_with(cmd, None).unwrap();
    assert_eq!(croft_that_answers(&mut term), "running");
}

#[test]
fn prompt_jump_targets_walk_previous_and_next() {
    // Pure chooser used by Cmd+Up / Cmd+Down: pick the nearest prompt
    // line above / below the current viewport top, if any.
    let prompts = [-30, -12, 0];
    assert_eq!(pick_prompt_jump(&prompts, 0, false), Some(-12));
    assert_eq!(pick_prompt_jump(&prompts, -12, false), Some(-30));
    assert_eq!(pick_prompt_jump(&prompts, -30, false), None);
    assert_eq!(pick_prompt_jump(&prompts, -30, true), Some(-12));
    assert_eq!(pick_prompt_jump(&prompts, -12, true), Some(0));
    assert_eq!(pick_prompt_jump(&prompts, 0, true), None);
}

#[test]
fn bell_sets_a_flag_the_app_can_drain() {
    let tmp = tempfile::tempdir().unwrap();
    let script = "printf 'DONE\\007\\n'";
    let term =
        PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script.into()], tmp.path())
            .unwrap();
    // Wait for the child's output row, not the run header echoing the
    // command text — the BEL byte arrives with the output.
    wait_for_grid(&term, |ls| {
        ls.iter()
            .any(|l| l.contains("DONE") && !l.contains("printf"))
    });
    assert!(
        term.take_bell(),
        "BEL from the child must set the bell flag for the app to drain"
    );
    assert!(
        !term.take_bell(),
        "take_bell drains the flag — a second read is false"
    );
}

/// #678: a program in the pane copying through OSC 52 (Claude Code over
/// SSH has no other way) must reach the app, once, and only for the
/// clipboard: a primary-selection store is not a copy the user made.
#[test]
fn osc52_clipboard_store_is_latched_for_the_app_and_selection_is_not() {
    let tmp = tempfile::tempdir().unwrap();
    // "aGVsbG8=" is "hello"; the primary-selection store ("p") comes
    // first so a leak of it would be overwritten into a false pass only
    // if it were accepted AND ordered last.
    let script = "printf '\\033]52;c;aGVsbG8=\\007\\033]52;p;bm9wZQ==\\007DONE\\n'";
    let term =
        PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script.into()], tmp.path())
            .unwrap();
    wait_for_grid(&term, |ls| {
        ls.iter()
            .any(|l| l.contains("DONE") && !l.contains("printf"))
    });
    assert_eq!(
        term.take_clipboard_store().as_deref(),
        Some("hello"),
        "the clipboard store must be latched, and the selection store ignored"
    );
    assert_eq!(
        term.take_clipboard_store(),
        None,
        "take_clipboard_store drains the latch"
    );
}

/// #257: the colour export round-trips through the log parser: the
/// text is what the pane showed and the coloured run keeps its colour
/// and weight, as the 16 symbolic slots the theme resolves.
#[test]
fn grid_lines_ansi_carries_the_pane_colours_as_sgr() {
    let tmp = tempfile::tempdir().unwrap();
    // The sentinel is assembled at runtime so the pane's run-label header
    // can never match the scan below.
    let term = PtyTerminal::new_running(
        "/bin/sh",
        &[
            String::from("-c"),
            String::from("s=QQ; printf \"\\033[1;31m${s}RED\\033[0m plain\\n\"; sleep 30"),
        ],
        tmp.path(),
    )
    .unwrap();
    wait_for_grid(&term, |ls| ls.iter().any(|l| l.contains("QQRED plain")));
    let exported = term.grid_lines_ansi();
    let (plain, raw) = exported
        .iter()
        .find(|(p, _)| p.contains("QQRED"))
        .expect("the coloured row is exported");
    let mut style = crate::ansi_text::AnsiStyle::default();
    let parsed = crate::ansi_text::parse_line(raw, &mut style);
    assert_eq!(
        parsed.text.trim_end(),
        plain.trim_end(),
        "the plain twin is the coloured row with its escapes stripped"
    );
    assert!(
        parsed.text.starts_with("QQRED plain"),
        "escapes never reach the text: {:?}",
        parsed.text
    );
    let red = parsed
        .spans
        .iter()
        .find(|s| s.start == 0 && s.end >= 5)
        .unwrap_or_else(|| panic!("a span covers the red run: {:?} from {raw:?}", parsed.spans));
    assert_eq!(
        red.style.fg,
        Some(crate::ansi_text::AnsiColor::Indexed(1)),
        "SGR 31 comes back as the symbolic red slot"
    );
    assert!(red.style.bold, "SGR 1 comes back as bold");
    let plain = parsed.spans.iter().find(|s| s.start >= 5);
    assert!(
        plain.is_none_or(|s| s.style.fg.is_none() && !s.style.bold),
        "the reset ends the run: {:?}",
        parsed.spans
    );
}

/// #356: the recorder's screen read is the visible screen in colour,
/// with the cursor where the program put it, and no cursor once the
/// program hides it.
#[test]
fn screen_ansi_wrapped_carries_colour_and_the_cursor() {
    let tmp = tempfile::tempdir().unwrap();
    let term = PtyTerminal::new_running(
        "/bin/sh",
        &[
            String::from("-c"),
            String::from(
                "s=QQ; printf \"\\033[1;31m${s}RED\\033[0m plain\\n\\033[4;7H\"; sleep 30",
            ),
        ],
        tmp.path(),
    )
    .unwrap();
    wait_for_grid(&term, |ls| ls.iter().any(|l| l.contains("QQRED plain")));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let (rows, cursor) = loop {
        let (rows, cursor) = term.screen_ansi_wrapped();
        if cursor == Some((3, 6)) || std::time::Instant::now() > deadline {
            break (rows, cursor);
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    assert_eq!(cursor, Some((3, 6)), "CSI 4;7H is row 3, column 6");
    let (_, ansi, _) = rows
        .iter()
        .find(|(plain, _, _)| plain.contains("QQRED"))
        .expect("the coloured row is on screen");
    assert!(ansi.contains("\x1b[1;31mQQRED\x1b[0m plain"), "{ansi:?}");
    assert_eq!(
        rows.len(),
        term.term.lock().screen_lines(),
        "the screen, and none of the scrollback above it"
    );

    term.feed_bytes_for_test(b"\x1b[?25l");
    assert_eq!(
        term.screen_ansi_wrapped().1,
        None,
        "a hidden cursor is none"
    );
}

#[test]
fn grid_lines_exposes_output_for_terminal_find() {
    // The find bar searches the grid text this returns. Spawn a program
    // that prints a known needle, wait for it, and confirm the search
    // helpers locate it on the mapped grid line.
    let tmp = tempfile::tempdir().unwrap();
    let needle = "croft-find-needle-42";
    let term = PtyTerminal::new_running("/bin/echo", &[String::from(needle)], tmp.path()).unwrap();
    let mut waited_ms = 0u32;
    loop {
        let (lines, top) = term.grid_lines();
        if let Some(row) = lines.iter().position(|l| l.contains(needle)) {
            let hit = crate::widgets::editor_find::find_next_match(
                &lines,
                needle,
                crate::widgets::search::SearchOpts::default(),
                0,
                0,
                false,
            )
            .expect("search must find the printed needle");
            assert_eq!(hit.row, row, "match row must line up with the grid row");
            // `top + row` maps the hit back to an absolute grid line the
            // viewport can scroll to; for the live screen that is >= 0.
            assert!(top + hit.row as i32 >= top);
            break;
        }
        assert!(waited_ms < 4000, "echo output never reached the grid");
        std::thread::sleep(std::time::Duration::from_millis(20));
        waited_ms += 20;
    }
}

#[test]
fn pending_bytes_counts_advanced_output_and_resets_on_take_dirty() {
    let tmp = tempfile::tempdir().unwrap();
    let term = PtyTerminal::new_running(
        "/bin/echo",
        &[String::from("croft-pending-bytes-probe")],
        tmp.path(),
    )
    .unwrap();
    // The wait is load-scaled (#307): /bin/echo through a PTY is
    // milliseconds on a quiet box and seconds when the suite is spawning
    // dozens of shells at once, and no constant here can know which.
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(500),
        "/bin/echo to deliver a byte through the PTY",
        || term.peek_pending_bytes() > 0,
    );
    assert!(
        term.peek_pending_bytes() > 0,
        "the reader thread must accumulate the bytes it advanced so the main loop can tell echo from a bulk stream"
    );
    assert!(
        term.peek_pending_bytes() <= 4096,
        "a one-line echo must stay under the small-update threshold so it bypasses the redraw cap"
    );
    let _ = term.take_dirty();
    assert_eq!(
        term.peek_pending_bytes(),
        0,
        "take_dirty must reset the byte accumulator so the next window starts from zero"
    );
}

#[test]
fn new_running_renders_running_header_in_term_immediately() {
    let tmp = tempfile::tempdir().unwrap();
    let term = PtyTerminal::new_running(
        "/bin/echo",
        &[String::from("croft-header-probe")],
        tmp.path(),
    )
    .unwrap();
    let snapshot = term.visible_text();
    assert!(
        snapshot.contains("/bin/echo croft-header-probe"),
        "expected the run command in the header line; got:\n{snapshot}"
    );
    assert!(
        snapshot.starts_with('▶') || snapshot.contains("▶ "),
        "expected a ▶ marker on the header line; got:\n{snapshot}"
    );
}

#[test]
fn new_running_appends_exit_footer_when_child_finishes() {
    let tmp = tempfile::tempdir().unwrap();
    let term =
        PtyTerminal::new_running("/bin/echo", &[String::from("croft-exit-probe")], tmp.path())
            .unwrap();
    let mut waited_ms = 0u32;
    let footer_needle = "[Process exited]";
    loop {
        if term.visible_text().contains(footer_needle) {
            break;
        }
        if waited_ms >= 4000 {
            let snap = term.visible_text();
            panic!(
                "expected '{footer_needle}' to appear within 4s of the child exiting; got:\n{snap}"
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
        waited_ms += 20;
    }
}

#[test]
fn new_for_interactive_shell_does_not_render_running_header() {
    let tmp = tempfile::tempdir().unwrap();
    let term = PtyTerminal::new(tmp.path()).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));
    let snapshot = term.visible_text();
    assert!(
        !snapshot.contains("▶ "),
        "interactive shell must not render a one-shot script header; got:\n{snapshot}"
    );
    assert!(
        !snapshot.contains("[Process exited]"),
        "interactive shell must not render an exit footer; got:\n{snapshot}"
    );
}

#[test]
fn selection_normalised_handles_anchor_after_head() {
    let s = Selection {
        anchor: (5, 4),
        head: (2, 1),
        block: false,
    };
    assert_eq!(s.normalised(), (2, 1, 5, 4));
}

#[test]
fn selection_normalised_handles_same_row() {
    let s = Selection {
        anchor: (3, 9),
        head: (3, 2),
        block: false,
    };
    assert_eq!(s.normalised(), (3, 2, 3, 9));
}

#[test]
fn selection_has_area_only_when_endpoints_differ() {
    let s = Selection::new(2, 5);
    assert!(!s.has_area());
    let s2 = Selection {
        anchor: (2, 5),
        head: (2, 6),
        block: false,
    };
    assert!(s2.has_area());
}

#[test]
fn cell_in_selection_within_single_row() {
    assert!(cell_in_selection(2, 5, 2, 3, 2, 7));
    assert!(!cell_in_selection(2, 2, 2, 3, 2, 7));
    assert!(!cell_in_selection(2, 8, 2, 3, 2, 7));
    assert!(!cell_in_selection(1, 5, 2, 3, 2, 7));
    assert!(!cell_in_selection(3, 5, 2, 3, 2, 7));
}

#[test]
fn cell_in_selection_spans_multiple_rows() {
    assert!(!cell_in_selection(1, 4, 1, 5, 3, 2));
    assert!(cell_in_selection(1, 5, 1, 5, 3, 2));
    assert!(cell_in_selection(1, 9, 1, 5, 3, 2));
    assert!(cell_in_selection(2, 0, 1, 5, 3, 2));
    assert!(cell_in_selection(2, 9, 1, 5, 3, 2));
    assert!(cell_in_selection(3, 2, 1, 5, 3, 2));
    assert!(!cell_in_selection(3, 3, 1, 5, 3, 2));
    assert!(!cell_in_selection(4, 0, 1, 5, 3, 2));
}

#[test]
fn osc52_copy_seq_wraps_with_correct_envelope() {
    let bytes = osc52_copy_seq("hello");
    assert_eq!(bytes, b"\x1b]52;c;aGVsbG8=\x07");
}

#[test]
fn osc52_copy_seq_handles_empty() {
    assert_eq!(osc52_copy_seq(""), b"\x1b]52;c;\x07");
}

#[test]
fn osc52_copy_seq_handles_unicode() {
    let bytes = osc52_copy_seq("héllo");
    assert!(bytes.starts_with(b"\x1b]52;c;"));
    assert_eq!(*bytes.last().unwrap(), 0x07);
    let body = &bytes[7..bytes.len() - 1];
    use base64::Engine;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(body)
        .unwrap();
    assert_eq!(decoded, "héllo".as_bytes());
}

#[test]
fn select_word_at_in_term_brackets_the_alphanumeric_run_around_the_pivot() {
    let mut t = fresh_term(40, 5);
    feed(&mut t, b"hello world from croft");
    let got = select_word_at_in_term(&t, 0, 0, 8);
    assert_eq!(
        got,
        Some(((0, 6), (0, 10))),
        "pivot inside 'world' must select cols 6..=10"
    );
}

#[test]
fn select_word_at_in_term_returns_none_when_pivot_is_on_whitespace() {
    let mut t = fresh_term(40, 5);
    feed(&mut t, b"hello world");
    assert_eq!(
        select_word_at_in_term(&t, 0, 0, 5),
        None,
        "pivot on the space between 'hello' and 'world' must yield no selection"
    );
}

#[test]
fn select_word_at_in_term_treats_underscores_as_word_chars_and_dots_as_boundaries() {
    let mut t = fresh_term(40, 5);
    feed(&mut t, b"foo_bar.baz");
    assert_eq!(
        select_word_at_in_term(&t, 0, 0, 2),
        Some(((0, 0), (0, 6))),
        "'foo_bar' is one word (underscore is a word char), '.baz' is separate"
    );
    assert_eq!(
        select_word_at_in_term(&t, 0, 0, 9),
        Some(((0, 8), (0, 10))),
        "pivot inside 'baz' must select cols 8..=10"
    );
}

/// Double-click on a word that lives in scrollback must still
/// word-select. Pre-fix, `select_word_at_in_term` ignored the
/// scrollback offset and read the live grid at the same viewport
/// row, which was usually blank — the lookup returned None and
/// the user saw "double-click doesn't auto-select."
#[test]
fn select_word_at_in_term_word_selects_in_scrollback_when_display_offset_is_nonzero() {
    let mut t = fresh_term(20, 3);
    // Push three lines into scrollback by feeding six rows into a
    // three-row viewport.
    feed(
        &mut t,
        b"scroll-A row\r\nscroll-B row\r\nscroll-C row\r\nlive-D row\r\nlive-E row\r\nlive-F row",
    );
    t.scroll_display(alacritty_terminal::grid::Scroll::Delta(2));
    let display_offset = t.grid().display_offset();
    assert_eq!(
        display_offset, 2,
        "precondition: Scroll::Delta(2) must move the viewport into scrollback"
    );
    // Viewport row 0 with display_offset=2 = grid Line(-2) =
    // "scroll-B row". Pivot at column 2 sits inside "scroll-B".
    let got = select_word_at_in_term(&t, display_offset, 0, 2);
    assert!(
        got.is_some(),
        "double-click on a scrolled-back row must return a real anchor/head, not None - if this is None the user sees the double-click 'do nothing'"
    );
    let (anchor, head) = got.unwrap();
    // "scroll-B" spans cols 0..=7 (s=0, c=1, r=2, o=3, l=4, l=5,
    // -=… actually '-' is not a word char, so the word is
    // "scroll" at cols 0..=5). The pivot is at col 2, which
    // lives inside "scroll". The selection must cover that run.
    assert_eq!(
        anchor,
        (0, 0),
        "anchor must mark the start of the word run on viewport row 0"
    );
    assert_eq!(
        head,
        (0, 5),
        "head must mark the last char of 'scroll' before the hyphen breaks the word run"
    );
}

#[test]
fn extract_selection_text_single_line() {
    let mut t = fresh_term(20, 5);
    feed(&mut t, b"hello world");
    let txt = extract_selection_text(&t, 0, 6, 0, 10);
    assert_eq!(txt, "world");
}

#[test]
fn copying_keeps_a_space_at_a_wrap_and_combining_marks() {
    let mut t = fresh_term(10, 5);
    feed(&mut t, b"echo abcd efg");
    assert_eq!(extract_selection_text(&t, 0, 0, 1, 9), "echo abcd efg");
    let mut t = fresh_term(20, 5);
    feed(&mut t, "cafe\u{301}".as_bytes());
    assert_eq!(extract_selection_text(&t, 0, 0, 0, 19), "cafe\u{301}");
}

#[test]
fn extract_selection_text_multi_line_trims_trailing_spaces() {
    let mut t = fresh_term(20, 5);
    feed(&mut t, b"first line\r\nsecond line");
    let txt = extract_selection_text(&t, 0, 6, 1, 5);
    assert_eq!(txt, "line\nsecond");
}

/// `extract_selection_text` now addresses absolute grid lines, so a
/// negative line index reads scrollback directly and a given line
/// returns the same content no matter how the viewport is scrolled.
/// This is the property that lets a selection survive scrolling: the
/// endpoints name content, not screen rows.
#[test]
fn extract_selection_text_addresses_absolute_grid_lines() {
    let mut t = fresh_term(20, 3);
    // Six lines into a 3-row viewport. Each `\r\n` past the bottom
    // pushes the topmost row into scrollback. End-state:
    //   scrollback: scroll-A = Line(-3), scroll-B = Line(-2), scroll-C = Line(-1)
    //   live:       live-D = Line(0), live-E = Line(1), live-F = Line(2)
    feed(
        &mut t,
        b"scroll-A row\r\nscroll-B row\r\nscroll-C row\r\nlive-D row\r\nlive-E row\r\nlive-F row",
    );

    assert_eq!(
        extract_selection_text(&t, 0, 0, 0, 19),
        "live-D row",
        "Line(0) is the first live row"
    );
    assert_eq!(
        extract_selection_text(&t, -2, 0, -2, 19),
        "scroll-B row",
        "Line(-2) reads the scrollback row directly, no display_offset needed"
    );

    // Scrolling the viewport must NOT change what an absolute line
    // returns - that invariant is why a selection no longer drifts
    // when the user scrolls mid-drag.
    t.scroll_display(alacritty_terminal::grid::Scroll::Delta(2));
    assert_eq!(t.grid().display_offset(), 2);
    assert_eq!(
        extract_selection_text(&t, -2, 0, -2, 19),
        "scroll-B row",
        "Line(-2) still reads 'scroll-B row' after scrolling - absolute lines are scroll-invariant"
    );
}

/// The whole point of the fix: a selection that spans from scrollback
/// history into the live screen extracts every line, even though it
/// is far taller than the 3-row viewport. Before the absolute-
/// coordinate model the selection was capped at the visible pane
/// height and the off-screen lines were simply unreachable.
#[test]
fn selection_stays_glued_to_its_content_while_output_streams() {
    // A selection made while a program keeps printing (Claude Code, any
    // streaming TUI) must track its content into scrollback, not stay at
    // fixed grid lines while the text slides underneath.
    let tmp = tempfile::tempdir().unwrap();
    let go = tmp.path().join("go");
    // Sentinel assembled at runtime so the `▶ sh -c …` run-label header
    // (which echoes the script text) can never match it.
    let script = format!(
        "t=TAR; echo \"${{t}}GET-line\"; until [ -e {} ]; do sleep 0.05; done; seq 1 60; sleep 30",
        go.display()
    );
    let mut term =
        PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script], tmp.path()).unwrap();
    let mut waited = 0u32;
    let line = loop {
        let (lines, top) = term.grid_lines();
        if let Some(idx) = lines.iter().position(|l| l.starts_with("TARGET-line")) {
            break top + idx as i32;
        }
        assert!(waited < 8000, "sentinel never arrived");
        std::thread::sleep(std::time::Duration::from_millis(20));
        waited += 20;
    };
    term.set_selection(Some(Selection {
        anchor: (line, 0),
        head: (line, 30),
        block: false,
    }));
    assert_eq!(term.selection_text(), "TARGET-line");

    // Release the second phase: 60 more lines scroll the buffer.
    std::fs::File::create(&go).unwrap();
    let mut waited = 0u32;
    while !term.grid_lines().0.iter().any(|l| l == "60") {
        assert!(waited < 8000, "streamed output never arrived");
        std::thread::sleep(std::time::Duration::from_millis(20));
        waited += 20;
    }
    assert_eq!(
        term.selection_text(),
        "TARGET-line",
        "the selection must follow its content as output streams past it"
    );
    let sel = term.selection().expect("selection survives streaming");
    assert!(
        sel.anchor.0 < line,
        "reported endpoints re-anchor upward as lines scroll into history"
    );
}

#[test]
fn selection_survives_streaming_past_a_saturated_scrollback() {
    // Once a pane has scrolled more than SCROLLBACK_LINES, history_size
    // saturates at the cap while content keeps rotating through the
    // ring. A selection made in that state must still track its content
    // as more output streams (the live Claude Code pane: long-running,
    // scrollback full since forever).
    let tmp = tempfile::tempdir().unwrap();
    let go = tmp.path().join("go");
    let script = format!(
        "seq 1 5200; t=TAR; echo \"${{t}}GET-line\"; until [ -e {} ]; do sleep 0.05; done; seq 1 60; sleep 30",
        go.display()
    );
    let mut term =
        PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script], tmp.path()).unwrap();
    let mut waited = 0u32;
    let line = loop {
        let (lines, top) = term.grid_lines();
        if let Some(idx) = lines.iter().position(|l| l.starts_with("TARGET-line")) {
            break top + idx as i32;
        }
        assert!(waited < 8000, "sentinel never arrived");
        std::thread::sleep(std::time::Duration::from_millis(20));
        waited += 20;
    };
    let hist_at_select = term.term.lock().grid().history_size();
    term.set_selection(Some(Selection {
        anchor: (line, 0),
        head: (line, 30),
        block: false,
    }));
    assert_eq!(term.selection_text(), "TARGET-line");

    std::fs::File::create(&go).unwrap();
    let mut waited = 0u32;
    while !term.grid_lines().0.iter().any(|l| l == "60") {
        assert!(waited < 8000, "streamed output never arrived");
        std::thread::sleep(std::time::Duration::from_millis(20));
        waited += 20;
    }
    // Premise of this test: the scrollback was already full, so
    // history_size never moved even though 60 lines scrolled past.
    assert_eq!(
        term.term.lock().grid().history_size(),
        hist_at_select,
        "test setup must saturate the scrollback before streaming"
    );
    assert_eq!(
        term.selection_text(),
        "TARGET-line",
        "the selection must follow its content even with history_size saturated"
    );
}

#[test]
fn alt_screen_selection_stays_partially_visible_when_app_chrome_covers_its_edge() {
    // Scrolling Claude Code slides the selected block toward its input
    // box; the rows that reach it are overdrawn by the box while the
    // rest stay visible. The surviving rows must keep their highlight
    // (matched as a partial block), and copy must still yield the whole
    // remembered selection.
    let (_tmp, mut t) = quiet_pty();
    feed_pty(
        &t,
        b"\x1b[?1049h\x1b[H\x1b[2Jalpha one\r\nbravo two\r\ncharlie three\r\ndelta four\r\necho five",
    );
    t.set_selection(Some(Selection {
        anchor: (1, 0),
        head: (3, 9),
        block: false,
    }));
    assert_eq!(t.selection_text(), "bravo two\ncharlie three\ndelta four");

    // The app scrolls one line; the block's last row slides under the
    // input box, which repaints over it. Rows 0-1 of the block survive
    // at lines 0-1; line 2 onward is app chrome.
    feed_pty(
        &t,
        b"\x1b[H\x1b[2Jbravo two\r\ncharlie three\r\n> INPUT BOX\r\n> more chrome\r\n> even more",
    );
    t.rebase_selection();
    let anchor = t.alt_sel.as_ref().expect("anchor survives");
    assert!(
        !anchor.dormant,
        "two of three selected rows are still on screen: not dormant"
    );
    assert_eq!(
        anchor.visible,
        Some(vec![(0, 0, u16::MAX), (1, 0, u16::MAX)]),
        "only the surviving rows keep their highlight"
    );
    assert_eq!(
        t.selection_text(),
        "bravo two\ncharlie three\ndelta four",
        "copy yields the whole remembered selection while partially covered"
    );

    // Scrolled back: the whole block is visible again, fully live.
    feed_pty(
        &t,
        b"\x1b[H\x1b[2Jalpha one\r\nbravo two\r\ncharlie three\r\ndelta four\r\necho five",
    );
    t.rebase_selection();
    let anchor = t.alt_sel.as_ref().expect("anchor survives");
    assert!(!anchor.dormant);
    assert_eq!(anchor.visible, None, "fully visible again: no clip");
    assert_eq!(t.selection_text(), "bravo two\ncharlie three\ndelta four");
}

#[test]
fn a_single_row_selection_on_a_repeated_row_reanchors_to_its_own_copy() {
    // A one-row selection has a one-row fingerprint, and full-screen
    // apps repeat rows verbatim (divider rules, continuation markers).
    // After a repaint the nearest identical copy may be a DIFFERENT
    // copy: the neighbours captured with the anchor must break the tie
    // so the highlight follows its own row, not the closest lookalike.
    let (_tmp, mut t) = quiet_pty();
    feed_pty(
        &t,
        b"\x1b[?1049h\x1b[H\x1b[2Jalpha\r\n-----\r\nbravo\r\n-----\r\ncharlie",
    );
    // Select the SECOND divider (row 3), between bravo and charlie.
    t.set_selection(Some(Selection {
        anchor: (3, 0),
        head: (3, 4),
        block: false,
    }));
    assert_eq!(t.selection_text(), "-----");

    // The app scrolls two rows: the selected divider moves to row 1
    // (still between bravo and charlie), while a new divider appears
    // at row 4 — NEARER to the old position than the real one.
    feed_pty(
        &t,
        b"\x1b[H\x1b[2Jbravo\r\n-----\r\ncharlie\r\ndelta\r\n-----",
    );
    t.rebase_selection();
    let anchor = t.alt_sel.as_ref().expect("anchor survives");
    assert_eq!(
        anchor.top, 1,
        "the highlight must follow its own copy (neighbours bravo/charlie), not the nearest lookalike"
    );
    assert_eq!(
        t.selection.map(|s| (s.anchor, s.head)),
        Some(((1, 0), (1, 4))),
        "the selection coordinates must move with the content"
    );
    assert_eq!(t.selection_text(), "-----");
}

/// Drive a PtyTerminal's grid directly: parse `bytes` into its term as
/// if the child had printed them. The child (`sleep`) stays silent, so
/// the grid is exactly what the test painted.
fn feed_pty(t: &PtyTerminal, bytes: &[u8]) {
    let mut p = Processor::<StdSyncHandler>::new();
    let mut term = t.term.lock();
    p.advance(&mut *term, bytes);
}

fn quiet_pty() -> (tempfile::TempDir, PtyTerminal) {
    let tmp = tempfile::tempdir().unwrap();
    let t = PtyTerminal::new_running("/bin/sleep", &[String::from("30")], tmp.path()).unwrap();
    (tmp, t)
}

/// A TAB leaves `'\t'` in the cell it started from (so copy can give
/// the tab back). Painted raw, the host terminal jumped to its own tab
/// stop while ratatui counted one cell, so the gap kept the previous
/// frame's text and the rest of the row landed too far right (#1287).
#[test]
fn a_tab_paints_as_blank_cells_and_the_next_text_lands_on_the_tab_stop() {
    let (_tmp, mut t) = quiet_pty();
    let area = Rect::new(0, 0, 40, 12);
    let mut buf = Buffer::empty(area);
    (&mut t).render(area, &mut buf);
    t.feed_bytes_for_test(b"\x1b[2J\x1b[Ha\tb\r\n");
    for cell in buf.content.iter_mut() {
        cell.set_symbol("X");
    }
    (&mut t).render(area, &mut buf);
    let row: Vec<String> = (0..area.width)
        .map(|x| buf[(x, 1)].symbol().to_string())
        .collect();
    let a = row.iter().position(|c| c == "a").expect("a is painted");
    assert!(
        row.iter().all(|c| !c.chars().any(char::is_control)),
        "no control char reaches the host terminal: {row:?}"
    );
    assert_eq!(
        row[a + 1..a + 8].concat(),
        " ".repeat(7),
        "the tab's gap is blank, not the old frame: {row:?}"
    );
    assert_eq!(row[a + 8], "b", "b sits on the next tab stop: {row:?}");
}

/// The text views of the grid (find, `croft record` casts, the web
/// client) send no raw TAB either, while copy still gives it back.
#[test]
fn grid_text_views_blank_a_tab_but_copy_keeps_it() {
    let (_tmp, mut t) = quiet_pty();
    t.resize(40, 10);
    t.feed_bytes_for_test(b"\x1b[2J\x1b[Ha\tb\r\n");
    let row = {
        let term = t.term.lock();
        row_text_and_cols(&term, 0).0
    };
    assert!(row.starts_with("a       b"), "{row:?}");
    let ansi = t.grid_lines_ansi();
    let first = ansi
        .iter()
        .find(|(plain, _)| plain.starts_with('a'))
        .unwrap();
    assert!(
        !first.0.contains('\t') && !first.1.contains('\t'),
        "{first:?}"
    );
    assert!(first.0.starts_with("a       b"), "{first:?}");

    let line = {
        let term = t.term.lock();
        (0..term.screen_lines() as i32)
            .find(|&l| row_text_and_cols(&term, l).0.starts_with('a'))
            .unwrap()
    };
    let mut sel = Selection::new(line, 0);
    sel.head = (line, 8);
    t.set_selection(Some(sel));
    assert!(
        t.selection_text().starts_with("a\t"),
        "copy keeps the tab: {:?}",
        t.selection_text()
    );
}

/// An alt-screen round trip (`git log`, vim, htop) used to wipe every
/// arrival stamp (the alternate grid has no history, so the cursor's
/// absolute id collapsed and looked like an ED 3 wipe) and then
/// re-stamp the whole scrollback with the exit time. Stamps are keyed
/// on the scroll clock now, which the alternate screen freezes.
#[test]
fn timestamps_survive_an_alt_screen_round_trip() {
    let (_tmp, t) = quiet_pty();
    let mut prev = 0i64;
    feed_pty(&t, b"one\r\ntwo\r\nthree\r\n");
    t.stamp_chunk_for_test(&mut prev, 1000);
    let before = t.line_time_entries_for_test();
    assert!(!before.is_empty(), "rows got stamped");
    let (first_id, first_ms) = before[0];
    assert_eq!(first_ms, 1000);
    feed_pty(&t, b"\x1b[?1049h\x1b[Hpager-screen");
    t.stamp_chunk_for_test(&mut prev, 2000);
    feed_pty(&t, b"\x1b[?1049l");
    t.stamp_chunk_for_test(&mut prev, 3000);
    let after = t.line_time_entries_for_test();
    assert_eq!(
        before.iter().map(|&(k, _)| k).collect::<Vec<_>>(),
        after.iter().map(|&(k, _)| k).collect::<Vec<_>>(),
        "an alt-screen trip must not wipe or invent stamped rows"
    );
    let kept = after.iter().find(|&&(k, _)| k == first_id).unwrap().1;
    assert_eq!(
        kept, 1000,
        "an old row must keep its true arrival time, not the pager's exit time"
    );
}

/// Once the scrollback ring saturates, `history_size` stops moving, so
/// history-keyed row ids froze to screen positions: new rows overwrote
/// the same ids and the gutter looked up unrelated rows' times. Clock
/// ids keep advancing one per scrolled line forever.
#[test]
fn timestamps_keep_advancing_past_a_saturated_scrollback() {
    let (_tmp, t) = quiet_pty();
    let mut prev = 0i64;
    let mut chunk = String::new();
    for i in 0..5300 {
        chunk.push_str(&format!("s-{i}\r\n"));
    }
    feed_pty(&t, chunk.as_bytes());
    t.stamp_chunk_for_test(&mut prev, 1000);
    let max1 = t
        .line_time_entries_for_test()
        .last()
        .map(|&(k, _)| k)
        .unwrap();
    let mut more = String::new();
    for i in 0..100 {
        more.push_str(&format!("t-{i}\r\n"));
    }
    feed_pty(&t, more.as_bytes());
    t.stamp_chunk_for_test(&mut prev, 2000);
    let max2 = t
        .line_time_entries_for_test()
        .last()
        .map(|&(k, _)| k)
        .unwrap();
    assert!(
        max2 >= max1 + 100,
        "row ids must keep advancing after saturation (got {max1} then {max2})"
    );
}

/// An inline image captured after the scrollback saturated must ride
/// the ring rotation like the content it anchors: `history_size` stops
/// growing there, so a history-delta anchor freezes onto a viewport
/// row and the picture sits on top of every later command's output.
#[test]
fn an_inline_image_rides_scrollback_past_saturation() {
    let (_tmp, t) = quiet_pty();
    let mut chunk = String::new();
    for i in 0..5200 {
        chunk.push_str(&format!("pre-{i}\r\n"));
    }
    feed_pty(&t, chunk.as_bytes());
    feed_pty(&t, b"image-anchor-row");
    t.push_image_for_test(vec![9, 9, 9]);
    feed_pty(&t, b"\r\n");
    let find_anchor = |t: &PtyTerminal| {
        let (lines, top) = t.grid_lines();
        top + lines
            .iter()
            .position(|l| l.starts_with("image-anchor-row"))
            .unwrap() as i32
    };
    assert_eq!(
        t.pane_images().pop().unwrap().line,
        find_anchor(&t),
        "the anchor starts on its row"
    );
    let mut more = String::new();
    for i in 0..60 {
        more.push_str(&format!("post-{i}\r\n"));
    }
    feed_pty(&t, more.as_bytes());
    assert_eq!(
        t.pane_images().pop().unwrap().line,
        find_anchor(&t),
        "the anchor must follow its content through ring rotation"
    );
}

/// Typing `clear` reaches the grid as ED 3 through the PTY reader
/// thread (modern terminfo wipes scrollback), never through the pane's
/// own Clear method: the reader must drop captured images too, or the
/// picture floats over the fresh prompt.
#[test]
fn a_program_emitted_scrollback_wipe_drops_captured_images() {
    use base64::Engine;
    let mut png_buf = Vec::new();
    image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 255, 0, 255]))
        .write_to(
            &mut std::io::Cursor::new(&mut png_buf),
            image::ImageFormat::Png,
        )
        .expect("encode test png");
    let b64 = base64::engine::general_purpose::STANDARD.encode(&png_buf);
    let tmp = tempfile::tempdir().unwrap();
    // Fill lines push real content into scrollback first (ED 3 can only
    // shrink a nonempty history); the wipe then waits on stdin so the
    // test can observe the capture before it lands.
    let script = format!(
        "i=0; while [ $i -lt 60 ]; do echo fill-$i; i=$((i+1)); done; printf '\\033]1337;File=inline=1:{b64}\\007\\n'; read x; printf '\\033[3J\\033[2J'; printf 'wiped\\n'; sleep 30"
    );
    let mut term =
        PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script], tmp.path()).unwrap();
    let mut waited = 0u32;
    while term.pane_images().is_empty() {
        assert!(waited < 8000, "inline image never captured");
        std::thread::sleep(std::time::Duration::from_millis(40));
        waited += 40;
    }
    term.write_input(b"\n");
    wait_for_grid(&term, |ls| ls.iter().any(|l| l.starts_with("wiped")));
    assert!(
        term.pane_images().is_empty(),
        "an ED 3 scrollback wipe from the program must drop the pane's images"
    );
}

/// A wipe with EMPTY scrollback (picture captured on the first screen,
/// nothing scrolled into history yet) must drop the image too: the
/// history-shrink heuristic saw 0 → 0 and kept the stale overlay.
#[test]
fn a_zero_history_screen_wipe_drops_captured_images() {
    use base64::Engine;
    let mut png_buf = Vec::new();
    image::RgbaImage::from_pixel(1, 1, image::Rgba([255, 0, 0, 255]))
        .write_to(
            &mut std::io::Cursor::new(&mut png_buf),
            image::ImageFormat::Png,
        )
        .expect("encode test png");
    let b64 = base64::engine::general_purpose::STANDARD.encode(&png_buf);
    let tmp = tempfile::tempdir().unwrap();
    let script = format!(
        "printf '\\033]1337;File=inline=1:{b64}\\007\\n'; read x; printf '\\033[2J\\033[3J\\033[H'; printf 'wiped\\n'; sleep 30"
    );
    let mut term =
        PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script], tmp.path()).unwrap();
    let mut waited = 0u32;
    while term.pane_images().is_empty() {
        assert!(waited < 8000, "inline image never captured");
        std::thread::sleep(std::time::Duration::from_millis(40));
        waited += 40;
    }
    term.write_input(b"\n");
    wait_for_grid(&term, |ls| ls.iter().any(|l| l.starts_with("wiped")));
    assert!(
        term.pane_images().is_empty(),
        "a screen wipe with empty history must still drop the pane's images"
    );
}

/// CAN/SUB cancel an incomplete CSI and an ESC inside one begins a new
/// escape (VT100): the wipe sniffer's CSI walk must not swallow the
/// sequence that follows a cancelled CSI, or a wipe (or alt entry)
/// right after one goes unseen.
#[test]
fn a_cancelled_csi_does_not_hide_a_wipe_from_the_sniffer() {
    let mut w = WipeSniffer::default();
    // ESC inside an incomplete CSI: the ED 3 after it must be seen.
    let wipes = w.scan(b"\x1b[12\x1b[3J", false);
    assert!(
        wipes.iter().any(|w| w.hist),
        "ESC-in-CSI swallowed the scrollback wipe"
    );
    // CAN aborts; the alt entry after it must be seen, so the ED 2
    // inside the alt screen is correctly ignored.
    let mut w = WipeSniffer::default();
    let wipes = w.scan(b"\x1b[\x18\x1b[?1049h\x1b[2J", false);
    assert!(
        wipes.is_empty(),
        "CAN-cancelled CSI hid the alt entry from the sniffer"
    );
}

/// #876: each wipe is reported where it ends in the chunk, a sequence
/// split across reads included, so the reader can stop the parser
/// right there.
#[test]
fn the_wipe_sniffer_reports_where_each_wipe_ends() {
    let mut w = WipeSniffer::default();
    let wipes = w.scan(b"ab\x1b[2J\x1b[3", false);
    assert_eq!(
        wipes,
        vec![Wipe {
            end: 6,
            screen: true,
            hist: false
        }]
    );
    let wipes = w.scan(b"Jx\x1b", false);
    assert_eq!(
        wipes,
        vec![Wipe {
            end: 1,
            screen: false,
            hist: true
        }]
    );
    let wipes = w.scan(b"c", false);
    assert_eq!(
        wipes,
        vec![Wipe {
            end: 1,
            screen: true,
            hist: true
        }]
    );
}

/// `clear && vim`: the wipe and the alt-screen entry land in ONE PTY
/// chunk. Judging the wipe by the chunk's FINAL mode attributed it to
/// the alt screen and kept the invalidated primary images, which then
/// reappeared over unrelated content when the app exited.
#[test]
fn a_wipe_followed_by_alt_entry_in_one_chunk_still_drops_images() {
    use base64::Engine;
    let mut png_buf = Vec::new();
    image::RgbaImage::from_pixel(1, 1, image::Rgba([255, 255, 0, 255]))
        .write_to(
            &mut std::io::Cursor::new(&mut png_buf),
            image::ImageFormat::Png,
        )
        .expect("encode test png");
    let b64 = base64::engine::general_purpose::STANDARD.encode(&png_buf);
    let tmp = tempfile::tempdir().unwrap();
    // The wipe and the alt ENTRY share one write (one PTY chunk, the
    // pane still in the alt screen when it ends); the exit comes later.
    let script = format!(
        "printf '\\033]1337;File=inline=1:{b64}\\007\\n'; read x; printf '\\033[2J\\033[3J\\033[?1049halt-owns-this'; sleep 1; printf '\\033[?1049l'; printf 'back\\n'; sleep 30"
    );
    let mut term =
        PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script], tmp.path()).unwrap();
    let mut waited = 0u32;
    while term.pane_images().is_empty() {
        assert!(waited < 8000, "inline image never captured");
        std::thread::sleep(std::time::Duration::from_millis(40));
        waited += 40;
    }
    term.write_input(b"\n");
    wait_for_grid(&term, |ls| ls.iter().any(|l| l.starts_with("back")));
    assert!(
        term.pane_images().is_empty(),
        "the wipe preceded the alt entry, so the primary images are gone"
    );
}

/// Clearing the pane erases the content the OSC 133 marks anchored to:
/// keeping them used to invert their drift math (history_size zeroed)
/// so stale decoration dots later resurfaced on unrelated output rows,
/// where clicking one copied or re-ran the wrong text.
#[test]
fn clearing_the_terminal_drops_its_marks() {
    use crate::shell_integration::OscEvent;
    let (_tmp, mut t) = quiet_pty();
    feed_pty(&t, b"\r\n$ ");
    t.push_mark_for_test(OscEvent::PromptStart, 0);
    t.push_mark_for_test(OscEvent::PromptEnd, 2);
    feed_pty(&t, b"cmd\r\nout\r\n");
    t.push_mark_for_test(OscEvent::CommandStart, 0);
    t.push_mark_for_test(OscEvent::CommandEnd(Some(0)), 0);
    assert_eq!(
        pair_decorations(&t.marks_snapshot()).len(),
        1,
        "staging: one finished command"
    );
    t.clear_screen_and_scrollback();
    assert!(
        pair_decorations(&t.marks_snapshot()).is_empty(),
        "cleared content keeps no command decorations"
    );
}

/// A decoration's id names the same command for as long as that command
/// exists, and a positional index does not (#440).
///
/// Asserted as a CONTRAST, because that is the actual claim: the same
/// eviction is shown moving the index while leaving the id alone. An
/// assertion that only checked "id 7 is still 7" would pass against a
/// collector that renumbered from zero, since nothing would have moved.
///
/// Driven through `pair_decorations` rather than a live pane: both
/// eviction paths (the scrollback GC in `marks_snapshot`, the `MARKS_MAX`
/// drain in the reader thread) end in the same observable state — a
/// prefix of the mark stream is gone — and constructing that state
/// directly tests the property at the level it actually holds, instead of
/// depending on how a PTY happens to schedule its scrollback.
#[test]
fn a_decoration_id_outlives_the_eviction_that_moves_its_index() {
    use crate::shell_integration::OscEvent as E;
    let cycle = |line: i32| {
        [
            (E::PromptStart, line, 0, None),
            (E::PromptEnd, line, 2, None),
            (E::CommandStart, line + 1, 0, None),
            (E::CommandEnd(Some(0)), line + 2, 0, None),
        ]
    };
    let mut marks = Vec::new();
    for (n, line) in [-12, -8, -4].iter().enumerate() {
        marks.extend(cycle(*line));
        assert_eq!(marks.len(), (n + 1) * 4, "staging: four marks per cycle");
    }
    let views = views(&marks);

    let before = pair_decorations(&views);
    assert_eq!(before.len(), 3, "staging: three finished commands");
    assert!(
        before[0].id < before[1].id && before[1].id < before[2].id,
        "ids ascend with command order: {:?}",
        before.iter().map(|d| d.id).collect::<Vec<_>>()
    );

    // Evict the oldest command: its four marks leave the front of the
    // stream, exactly as the scrollback GC and the MARKS_MAX drain both
    // leave it. Ids are NOT reassigned, because they belong to the marks.
    let after = pair_decorations(&views[4..]);
    assert_eq!(after.len(), 2, "staging: one command was evicted");

    // The index has moved. The command that was last is still last here,
    // so the telling case is the MIDDLE one: it was at index 1 and is now
    // at index 0, which is what silently hands a caller the wrong command.
    assert_eq!(
        before[1].id, after[0].id,
        "the survivor moved down one slot"
    );
    assert_ne!(
        before[1].id, before[0].id,
        "staging: the slots must hold different commands"
    );
    assert_ne!(
        after[0].id,
        before.first().map(|d| d.id).unwrap(),
        "index 0 now names a different command than it did"
    );

    // The id still names the SAME command: same span, same exit.
    let found = after
        .iter()
        .find(|d| d.id == before[2].id)
        .expect("a surviving command keeps its id");
    assert_eq!(
        (found.exit, found.output_start, found.output_end),
        (before[2].exit, before[2].output_start, before[2].output_end),
        "the id must name the same command, not merely some decoration"
    );

    // And an evicted id reads as gone rather than as its neighbour, which
    // is the whole difference from a positional index.
    assert!(
        after.iter().all(|d| d.id != before[0].id),
        "an evicted command's id must never be reused by a survivor"
    );
}

/// A command outlives its own prompt marks, and the id says so (#440).
///
/// The GC retains per MARK, not per command, and the prompt marks sit on
/// earlier rows than the command's own — so the floor takes them first.
/// The interesting case is therefore not the clean boundary the test
/// above cuts, but this one: the same command, still identified, with its
/// presentation fields degraded. Pinning it here so the limit documented
/// on `CommandDecoration::id` cannot quietly stop being true.
#[test]
fn a_command_can_outlive_its_prompt_marks() {
    use crate::shell_integration::OscEvent as E;
    let marks = [
        (E::PromptStart, -10, 0, None),
        (E::PromptEnd, -10, 5, None),
        (E::CommandStart, -9, 0, None),
        (E::CommandEnd(Some(0)), -7, 0, None),
    ];
    let views = views(&marks);
    let whole = pair_decorations(&views);
    assert_eq!(whole.len(), 1, "staging: one finished command");

    // The floor rises past the prompt row but not past the command's.
    let survivors: Vec<MarkView> = views.iter().filter(|m| m.line >= -9).cloned().collect();
    assert_eq!(survivors.len(), 2, "staging: only the prompt marks evict");
    let after = pair_decorations(&survivors);
    assert_eq!(after.len(), 1, "the command itself survives");

    // Identity and the output span are untouched.
    assert_eq!(after[0].id, whole[0].id, "the id names the same command");
    assert_eq!(
        (after[0].exit, after[0].output_start, after[0].output_end),
        (whole[0].exit, whole[0].output_start, whole[0].output_end),
        "the fields taken from CommandStart/CommandEnd do not drift"
    );

    // The presentation fields DO degrade, exactly as documented.
    assert_eq!(
        whole[0].input,
        Some((-10, 5)),
        "staging: the typed-text position existed before the eviction"
    );
    assert_eq!(
        after[0].input, None,
        "the typed-text position goes with the prompt mark"
    );
    assert_eq!(whole[0].line, -10, "staging: the row was the prompt's");
    assert_eq!(
        after[0].line, after[0].output_start,
        "the row falls back to the command's own line"
    );
}

/// Ids are unique across panes, and a wipe does not restart them (#440).
///
/// The counter is process-wide precisely so an id means one thing
/// wherever it travels; a per-pane counter would hand two panes the same
/// id and a wipe would hand the same pane its old ids back. Both are the
/// kind of thing that looks fine until a consumer keys on the id, so both
/// are pinned rather than left to the comment.
#[test]
fn ids_are_unique_across_panes_and_survive_a_wipe() {
    use crate::shell_integration::OscEvent;
    let run = |t: &PtyTerminal| {
        feed_pty(t, b"\r\n$ ");
        t.push_mark_for_test(OscEvent::PromptStart, 0);
        t.push_mark_for_test(OscEvent::PromptEnd, 2);
        feed_pty(t, b"cmd\r\nout\r\n");
        t.push_mark_for_test(OscEvent::CommandStart, 0);
        t.push_mark_for_test(OscEvent::CommandEnd(Some(0)), 0);
    };

    let (_a, mut a) = quiet_pty();
    let (_b, b) = quiet_pty();
    run(&a);
    run(&b);
    run(&a);
    let mut ids: Vec<u64> = a
        .command_decorations()
        .iter()
        .chain(b.command_decorations().iter())
        .map(|d| d.id)
        .collect();
    assert_eq!(ids.len(), 3, "staging: three commands across two panes");
    let before = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), before, "ids must not collide between panes");

    // A wipe clears the pane's marks; the counter is not reset, so the
    // next command must not be handed an id the wipe just freed.
    let highest = *ids.last().unwrap();
    a.clear_screen_and_scrollback();
    assert!(
        a.command_decorations().is_empty(),
        "staging: the wipe must actually clear the marks"
    );
    run(&a);
    let after = a.command_decorations();
    assert_eq!(after.len(), 1, "staging: one command since the wipe");
    assert!(
        after[0].id > highest,
        "an id must not be reused after a wipe: {} came after {highest}",
        after[0].id
    );
}

/// A prompt mark recorded after the scrollback saturated must ride the
/// ring rotation like its content: under the history-size model the
/// delta froze, so the mark (and its decoration dot) drifted off its
/// row as output streamed.
#[test]
fn a_prompt_mark_rides_scrollback_past_saturation() {
    use crate::shell_integration::OscEvent;
    let (_tmp, mut t) = quiet_pty();
    let mut chunk = String::new();
    for i in 0..5200 {
        chunk.push_str(&format!("pre-{i}\r\n"));
    }
    feed_pty(&t, chunk.as_bytes());
    feed_pty(&t, b"$ marker-prompt");
    t.push_mark_for_test(OscEvent::PromptStart, 0);
    feed_pty(&t, b"\r\n");
    let find = |t: &mut PtyTerminal| {
        let (lines, top) = t.grid_lines();
        top + lines
            .iter()
            .position(|l| l.starts_with("$ marker-prompt"))
            .unwrap() as i32
    };
    assert_eq!(
        t.marks_snapshot()[0].line,
        find(&mut t),
        "the mark starts on its prompt row"
    );
    let mut more = String::new();
    for i in 0..60 {
        more.push_str(&format!("post-{i}\r\n"));
    }
    feed_pty(&t, more.as_bytes());
    assert_eq!(
        t.marks_snapshot()[0].line,
        find(&mut t),
        "the mark must follow its prompt through ring rotation"
    );
}

/// The pane's Clear wipes screen AND scrollback: the content an inline
/// image anchored to is gone, so the picture must go with it instead of
/// floating over the fresh prompt at a now-meaningless row.
#[test]
fn clearing_the_terminal_drops_its_captured_images() {
    let (_tmp, mut t) = quiet_pty();
    feed_pty(&t, b"\r\nsome output");
    t.push_image_for_test(vec![1, 2, 3]);
    feed_pty(&t, b"\r\n");
    assert_eq!(t.pane_images().len(), 1);
    t.clear_screen_and_scrollback();
    assert!(
        t.pane_images().is_empty(),
        "a cleared pane keeps no image anchored to erased content"
    );
}

/// A captured line wider than the pane appears in the grid as a WRAPPED
/// first row (a prefix of the needle): the jump must find it, must not
/// be fooled by an unrelated short row sharing the prefix (only rows
/// that really soft-wrap qualify for the reverse arm), and must skip
/// continuation rows so the newest-first scan can't land mid-line.
#[test]
fn capture_jump_matches_wrapped_lines_without_false_prefixes() {
    let (_tmp, mut t) = quiet_pty();
    let area = Rect::new(0, 0, 40, 12);
    let mut buf = Buffer::empty(area);
    (&mut t).render(area, &mut buf);
    let needle: String = format!("error[E0308]: {}", "x".repeat(60))
        .chars()
        .take(60)
        .collect();
    // An unrelated NON-wrapped row that happens to share the prefix.
    feed_pty(&t, b"\r\nerror[E0308]: unrelated short\r\n");
    assert_eq!(
        t.find_captured_line(&needle, None),
        None,
        "a short non-wrapped lookalike must not match"
    );
    // The real captured line, wrapping across two grid rows.
    let full = format!("error[E0308]: {}", "x".repeat(60));
    feed_pty(&t, full.as_bytes());
    let start = {
        let term = t.term.lock();
        term.grid().cursor.point.line.0 - 1
    };
    assert_eq!(
        t.find_captured_line(&needle, None),
        Some(start),
        "the wrapped line's FIRST row is the jump target, not its continuation"
    );
}

/// A soft-wrapped logical line copies as ONE line: the WRAPLINE flag on
/// the wrapping row's last cell tells a continuation from a real row
/// break, so selection copy, the durable command history, and the
/// sticky header all see the text the user actually typed. A '\n' at
/// the wrap corrupted stored commands and re-ran only the first
/// fragment on paste. Hard breaks keep their newline.
#[test]
fn selection_over_a_soft_wrapped_line_copies_one_logical_line() {
    let (_tmp, mut t) = quiet_pty();
    let area = Rect::new(0, 0, 40, 10);
    let mut buf = Buffer::empty(area);
    (&mut t).render(area, &mut buf);
    let cols = t.term.lock().columns();
    // A fresh row below any spawn banner, then a line long enough to wrap.
    feed_pty(&t, b"\r\n");
    let start = t.term.lock().grid().cursor.point.line.0;
    let long: String = "abcdefghij".repeat(6); // 60 chars > pane width
    feed_pty(&t, long.as_bytes());
    t.set_selection(Some(Selection {
        anchor: (start, 0),
        head: (start + 1, cols.saturating_sub(1) as u16),
        block: false,
    }));
    assert_eq!(
        t.selection_text(),
        long,
        "the wrapped rows must rejoin without a newline"
    );
    // A hard row break keeps its newline.
    feed_pty(&t, b"\r\nsecond");
    t.set_selection(Some(Selection {
        anchor: (start + 1, 0),
        head: (start + 2, 6),
        block: false,
    }));
    assert!(
        t.selection_text().contains('\n'),
        "a real row break still copies as two lines: {:?}",
        t.selection_text()
    );
}

/// The annotate prompt's existing-note lookup compares the selection
/// line captured at snapshot time against annotation lines; both must
/// translate at the SAME clock reading, or output scrolling between the
/// capture and the lookup shifts the annotations away from the fixed
/// selection line and the same span duplicates instead of editing.
#[test]
fn annotation_lookup_translates_at_the_callers_clock() {
    let (_tmp, mut t) = quiet_pty();
    feed_pty(&t, b"existing span\r\n");
    let clock = t.scroll_clock();
    t.add_annotation(0, clock, 0, 13, String::from("existing note"));
    // The prompt captures selection + clock in one snapshot...
    let (_, snap_clock) = t.selection_and_clock();
    // ...then 30 rows land before the existing-note lookup runs.
    let mut fill = String::new();
    for i in 0..30 {
        fill.push_str(&format!("late-{i}\r\n"));
    }
    feed_pty(&t, fill.as_bytes());
    let cur = t.annotations_at_clock(snap_clock);
    assert_eq!(
        cur[0].0, 0,
        "translated at the captured clock, the note still names the captured line"
    );
}

/// Annotations name PRIMARY-screen rows; while vim/htop own the
/// viewport the same line numbers describe unrelated alternate-grid
/// content, so neither the amber paint nor click hit-testing may
/// resolve there — and both come back when the app exits.
#[test]
fn annotations_hide_while_an_alt_screen_app_owns_the_viewport() {
    let (_tmp, mut t) = quiet_pty();
    t.last_inner = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 24,
    };
    feed_pty(&t, b"annotated content row\r\n");
    let clock = t.scroll_clock();
    t.add_annotation(0, clock, 0, 15, String::from("note"));
    assert!(
        t.annotation_at(3, 0).is_some(),
        "the note resolves on the primary screen"
    );
    feed_pty(&t, b"\x1b[?1049h\x1b[Hvim-owns-this-screen");
    assert!(
        t.annotation_at(3, 0).is_none(),
        "no hit while the alternate screen is up"
    );
    feed_pty(&t, b"\x1b[?1049l");
    assert!(
        t.annotation_at(3, 0).is_some(),
        "back on the primary screen the note resolves again"
    );
}

/// The Annotate prompt captures its span when it OPENS; rows that
/// stream while the user types the note must not shift the pin. The
/// commit anchors with the open-time (line, clock) pair — sampling a
/// fresh baseline at commit pinned the note N rows below its text
/// after N rows of output.
#[test]
fn an_annotation_committed_after_output_streamed_pins_where_it_was_captured() {
    let (_tmp, mut t) = quiet_pty();
    feed_pty(&t, b"target-line alpha\r\n");
    let (lines, top) = t.grid_lines();
    let line = top
        + lines
            .iter()
            .position(|l| l.starts_with("target-line"))
            .unwrap() as i32;
    let clock = t.scroll_clock();
    // 40 rows stream while the note is being typed.
    let mut more = String::new();
    for i in 0..40 {
        more.push_str(&format!("while-typing-{i}\r\n"));
    }
    feed_pty(&t, more.as_bytes());
    t.add_annotation(line, clock, 0, 11, String::from("note"));
    let cur = t.annotations_current();
    assert_eq!(cur.len(), 1, "the annotation survives");
    let term = t.term.lock();
    let text = extract_selection_text(&term, cur[0].0, 0, cur[0].0, 20);
    assert!(
        text.starts_with("target-line alpha"),
        "the note must pin to the text selected at open, got row: {text:?}"
    );
}

/// Past `SCROLLBACK_LINES` the ring rotates while `history_size` stops
/// moving, so a history-growth anchor freezes to a screen position.
/// Annotations ride the scroll clock (like selections) and must keep
/// following their content in exactly the long-lived panes (Claude
/// Code, `tail -f`) the feature targets.
#[test]
fn an_annotation_survives_streaming_past_a_saturated_scrollback() {
    let (_tmp, mut t) = quiet_pty();
    let mut chunk = String::new();
    for i in 0..5200 {
        chunk.push_str(&format!("pre-{i}\r\n"));
    }
    feed_pty(&t, chunk.as_bytes());
    feed_pty(&t, b"note-worthy-marker here\r\n");
    let (lines, top) = t.grid_lines();
    let line = top
        + lines
            .iter()
            .position(|l| l.starts_with("note-worthy-marker"))
            .unwrap() as i32;
    let clock = t.scroll_clock();
    t.add_annotation(line, clock, 0, 18, String::from("pinned"));
    let mut more = String::new();
    for i in 0..60 {
        more.push_str(&format!("post-{i}\r\n"));
    }
    feed_pty(&t, more.as_bytes());
    let cur = t.annotations_current();
    assert_eq!(cur.len(), 1, "the annotation survives the rotation");
    let term = t.term.lock();
    let text = extract_selection_text(&term, cur[0].0, 0, cur[0].0, 25);
    assert!(
        text.starts_with("note-worthy-marker"),
        "the note must follow its content through ring rotation, got row: {text:?}"
    );
}

/// A command spanning several grid rows (a quoted/heredoc newline, or a
/// soft wrap shown in a later-widened pane) joins its rows with '\n' in
/// `extract_selection_text`; the sticky header used to print that byte
/// into a cell verbatim, corrupting the frame on the host terminal (the
/// screen-corruption class the pdftoppm/DAP capture fixes exist to stop).
#[test]
fn sticky_header_never_paints_a_control_char_for_a_multi_row_command() {
    use crate::shell_integration::OscEvent;
    let (_tmp, mut t) = quiet_pty();
    // First render sizes the grid to the pane (78 cols), so the long
    // command genuinely wraps.
    let area = Rect::new(0, 0, 80, 20);
    let mut buf = Buffer::empty(area);
    (&mut t).render(area, &mut buf);
    feed_pty(&t, b"$ ");
    t.push_mark_for_test(OscEvent::PromptEnd, 2);
    // A multi-line typed command (heredoc / quoted newline): the input
    // span covers two HARD rows, so the joined text carries a newline
    // well inside the label's painted width.
    feed_pty(&t, b"echo \"build-it-99\r\nsecond-line\"");
    feed_pty(&t, b"\r\n");
    t.push_mark_for_test(OscEvent::CommandStart, 0);
    for i in 0..40 {
        feed_pty(&t, format!("out-{i}\r\n").as_bytes());
    }
    t.push_mark_for_test(OscEvent::CommandEnd(Some(0)), 0);
    assert!(
        !t.command_decorations().is_empty(),
        "the marks must pair into a decoration"
    );
    t.scroll_to_top();
    for _ in 0..5 {
        t.scroll_down(1);
    }
    let mut buf = Buffer::empty(area);
    (&mut t).render(area, &mut buf);
    let inner = t.last_inner;
    let top: String = (inner.x..inner.x + inner.width)
        .map(|x| buf[(x, inner.y)].symbol().chars().next().unwrap_or(' '))
        .collect();
    assert!(
        top.contains("build-it-99"),
        "the wrapped command must still pin: {top:?}"
    );
    for y in 0..area.height {
        for x in 0..area.width {
            let sym = buf[(x, y)].symbol();
            assert!(
                !sym.chars().any(|c| c.is_control()),
                "cell ({x},{y}) holds a control char: {sym:?}"
            );
        }
    }
}

/// Croft Loch pins a scrolled command on the raised surface in body
/// text, not bold, with the scroll depth muted: a quiet card rather than
/// a bright banner over the output.
#[test]
fn croft_loch_pins_the_sticky_command_on_the_raised_surface() {
    use crate::shell_integration::OscEvent;
    let (_tmp, mut t) = quiet_pty();
    t.theme = crate::theme::Theme::LOCH;
    let tones = crate::widgets::surface::tones(t.theme).unwrap();
    let area = Rect::new(0, 0, 80, 20);
    let mut buf = Buffer::empty(area);
    (&mut t).render(area, &mut buf);
    feed_pty(&t, b"$ ");
    t.push_mark_for_test(OscEvent::PromptEnd, 2);
    feed_pty(&t, b"make all\r\n");
    t.push_mark_for_test(OscEvent::CommandStart, 0);
    for i in 0..40 {
        feed_pty(&t, format!("out-{i}\r\n").as_bytes());
    }
    t.push_mark_for_test(OscEvent::CommandEnd(Some(0)), 0);
    t.scroll_to_top();
    for _ in 0..5 {
        t.scroll_down(1);
    }
    let mut buf = Buffer::empty(area);
    (&mut t).render(area, &mut buf);
    let inner = t.last_inner;
    let at = |sym: &str| {
        (inner.x..inner.right())
            .find(|&x| buf[(x, inner.y)].symbol() == sym)
            .unwrap_or_else(|| panic!("{sym:?} on the pinned row"))
    };
    let cell = &buf[(at("m"), inner.y)];
    assert_eq!(cell.bg, tones.raised());
    assert_eq!(cell.fg, tones.text());
    assert!(!cell.modifier.contains(Modifier::BOLD));
    assert_eq!(buf[(at("\u{2191}"), inner.y)].fg, tones.muted());
}

/// A prompt row rewritten SHORTER than the recorded 133;B column (a
/// backgrounded `\r\x1b[K` progress writer, or a pane shrunk under the
/// prompt) used to panic click-to-move: the clamp bounds inverted
/// (`min > max`) and `u16::clamp` asserts. A click is never allowed to
/// take the app down; the degraded gesture just clamps sanely.
#[test]
fn a_prompt_click_after_the_row_was_rewritten_shorter_does_not_panic() {
    let (_tmp, mut t) = quiet_pty();
    t.last_inner = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 24,
    };
    feed_pty(&t, b"user@host ~/Documents/croft % ");
    t.push_mark_for_test(crate::shell_integration::OscEvent::PromptEnd, 30);
    // The rewrite leaves 7 columns of text and the cursor at column 7,
    // both left of the recorded prompt-end column 30.
    feed_pty(&t, b"\r\x1b[Kworking");
    let row = t.term.lock().grid().cursor.point.line.0 as u16;
    // Live bounds: the text ends at column 7 and the cursor sits at
    // column 7, both left of the recorded 133;B column 30. The click at
    // column 5 clamps into [30.min(7), 7] = 7, which equals the cursor,
    // so the gesture degrades to no motion instead of panicking.
    assert_eq!(
        t.prompt_click_arrows(5, row),
        None,
        "a click on a shortened prompt row must degrade, not panic"
    );
}

#[test]
fn alt_screen_selection_follows_content_across_repaints() {
    // Alt-screen apps (Claude Code) never scroll the grid — they
    // repaint it in place. The selection must re-find its text after
    // each repaint, hide while it is scrolled out of the app's view,
    // and come back when the content does.
    let (_tmp, mut t) = quiet_pty();
    feed_pty(
        &t,
        b"\x1b[?1049h\x1b[H\x1b[2Jalpha one\r\nbravo two\r\ncharlie three\r\ndelta four",
    );
    t.set_selection(Some(Selection {
        anchor: (1, 0),
        head: (1, 8),
        block: false,
    }));
    assert_eq!(t.selection_text(), "bravo two");

    // The app scrolls its transcript up one line and repaints.
    feed_pty(
        &t,
        b"\x1b[H\x1b[2Jbravo two\r\ncharlie three\r\ndelta four\r\necho five",
    );
    t.rebase_selection();
    assert_eq!(t.selection_text(), "bravo two");
    assert_eq!(
        t.selection().unwrap().anchor.0,
        0,
        "the highlight must move to where the app repainted its text"
    );

    // The selected content scrolls out of the app's view entirely:
    // highlight goes dormant but copy still yields the remembered text.
    feed_pty(
        &t,
        b"\x1b[H\x1b[2Jfoxtrot six\r\ngolf seven\r\nhotel eight\r\nindia nine",
    );
    t.rebase_selection();
    assert!(
        t.alt_sel.as_ref().is_some_and(|a| a.dormant),
        "no matching content anywhere in the grid: dormant"
    );
    assert_eq!(t.selection_text(), "bravo two");

    // The app scrolls the content back into view at a new position.
    feed_pty(
        &t,
        b"\x1b[H\x1b[2Jalpha one\r\nbravo two\r\ncharlie three\r\ndelta four",
    );
    t.rebase_selection();
    assert!(t.alt_sel.as_ref().is_some_and(|a| !a.dormant));
    assert_eq!(t.selection().unwrap().anchor.0, 1);
    assert_eq!(t.selection_text(), "bravo two");
}

#[test]
fn alt_screen_row_half_covered_by_a_floating_pill_keeps_its_surviving_prefix() {
    // Claude Code floats a "Jump to bottom" pill ON TOP of a content
    // row. That row's text no longer equals the remembered row, but
    // its left part is untouched — it must keep a highlight clipped to
    // the surviving columns instead of dropping out entirely.
    let (_tmp, mut t) = quiet_pty();
    feed_pty(
        &t,
        b"\x1b[?1049h\x1b[H\x1b[2Jalpha one\r\nbravo two words here\r\ncharlie three words here\r\ndelta four words here\r\necho five",
    );
    t.set_selection(Some(Selection {
        anchor: (1, 0),
        head: (3, 20),
        block: false,
    }));
    // The app repaints the same content but overlays a pill over the
    // right half of the block's middle row.
    feed_pty(
        &t,
        b"\x1b[H\x1b[2Jalpha one\r\nbravo two words here\r\ncharlie three[ PILL ]here\r\ndelta four words here\r\necho five",
    );
    t.rebase_selection();
    let anchor = t.alt_sel.as_ref().expect("anchor survives");
    assert!(!anchor.dormant);
    let clips = anchor.visible.as_ref().expect("partially covered");
    assert!(
        clips.contains(&(1, 0, u16::MAX)) && clips.contains(&(3, 0, u16::MAX)),
        "intact rows keep their full highlight: {clips:?}"
    );
    assert!(
        clips
            .iter()
            .any(|&(l, lo, hi)| l == 2 && lo == 0 && (12..14).contains(&hi)),
        "the covered row keeps its surviving prefix ('charlie three'): {clips:?}"
    );
    assert!(
        clips
            .iter()
            .any(|&(l, lo, hi)| l == 2 && lo == 21 && hi == 24),
        "the surviving suffix ('here') stays lit too, even under half the row: {clips:?}"
    );
}

#[test]
fn alt_screen_bottom_up_drag_from_a_blank_row_selects_and_survives_release() {
    // Bottom-up selections usually start on a blank row or an animated
    // status row: the anchor captured at mouse-down mismatches on the
    // next frame, and before drag-awareness that turned the whole drag
    // dormant (invisible). While the button is held the selection must
    // follow the pointer over whatever is on screen; release captures
    // the definitive anchor.
    let (_tmp, mut t) = quiet_pty();
    feed_pty(
        &t,
        b"\x1b[?1049h\x1b[H\x1b[2Jalpha one\r\nbravo two\r\ncharlie three\r\n\r\ntokens 42",
    );
    t.last_inner = Rect::new(0, 0, 40, 10);
    // Mouse-down on the blank row (row 3), then an app repaint changes
    // the animated status row so the blank-row anchor can't match as a
    // block, then drag upward.
    t.start_selection_at(5, 3);
    feed_pty(&t, b"\x1b[5;1Htokens 43\x1b[K");
    t.extend_selection_to(0, 1);
    let anchor = t.alt_sel.as_ref().expect("anchor present");
    assert!(
        !anchor.dormant,
        "a held drag never hides itself, whatever the anchor matched"
    );
    assert_eq!(t.selection_text(), "bravo two\ncharlie three\n");
    t.end_drag();
    // After release the anchor is definitive: the app scrolls its
    // content down one row and the highlight follows.
    feed_pty(
        &t,
        b"\x1b[H\x1b[2Jnew line\r\nalpha one\r\nbravo two\r\ncharlie three\r\n\r\ntokens 44",
    );
    t.rebase_selection();
    assert_eq!(t.selection().expect("survives").normalised().0, 2);
    assert_eq!(t.selection_text(), "bravo two\ncharlie three\n");
}

#[test]
fn alt_screen_edge_autoscroll_forwards_wheel_to_the_tracking_app() {
    // On the alternate screen a drag held past the pane edge cannot
    // scroll croft scrollback (there is none); a mouse-tracking app
    // gets a wheel report at the edge cell so IT scrolls, and the
    // selection head re-pins to the edge row.
    let tmp = tempfile::tempdir().unwrap();
    // `cat -v` echoes whatever the pty receives back as printable
    // text, so the wheel report's arrival is observable in the grid.
    let mut t = PtyTerminal::new_running("/bin/cat", &[String::from("-v")], tmp.path()).unwrap();
    feed_pty(&t, b"\x1b[?1049h\x1b[?1002h\x1b[?1006h\x1b[H\x1b[2J");
    t.last_inner = Rect::new(0, 0, 40, 10);
    t.set_selection(Some(Selection {
        anchor: (2, 0),
        head: (2, 5),
        block: false,
    }));
    t.autoscroll_select(1, 5);
    assert_eq!(
        t.selection().unwrap().head.0,
        9,
        "the head re-pins to the bottom edge row"
    );
    let mut waited = 0u32;
    // SGR wheel-down at 1-based cell (6, 10): `ESC[<65;6;10M`.
    while !t.grid_lines().0.iter().any(|l| l.contains("[<65;6;10M")) {
        assert!(waited < 8000, "the app never received the wheel report");
        std::thread::sleep(std::time::Duration::from_millis(20));
        waited += 20;
    }
}

#[test]
fn primary_selection_survives_alt_round_trip_but_alt_selection_dies_on_exit() {
    let (_tmp, mut t) = quiet_pty();
    feed_pty(&t, b"hello world\r\n");
    // The `▶ /bin/sleep 30` run-label header owns the top row(s), so
    // find where the fed line actually landed.
    let (lines, top) = t.grid_lines();
    let line = top
        + lines
            .iter()
            .position(|l| l.starts_with("hello"))
            .expect("fed line present") as i32;
    t.set_selection(Some(Selection {
        anchor: (line, 0),
        head: (line, 4),
        block: false,
    }));
    assert_eq!(t.selection_text(), "hello");
    // vim-style round trip: enter and leave the alternate screen.
    feed_pty(&t, b"\x1b[?1049h\x1b[H\x1b[2Jvim vim vim");
    t.rebase_selection();
    feed_pty(&t, b"\x1b[?1049l");
    t.rebase_selection();
    assert_eq!(
        t.selection_text(),
        "hello",
        "a primary-screen selection is frozen across an alt-screen trip"
    );
    // A selection made ON the alt screen names alternate-grid content;
    // leaving the alt screen destroys that content and the selection.
    feed_pty(&t, b"\x1b[?1049h\x1b[H\x1b[2Jquick brown fox");
    t.set_selection(Some(Selection {
        anchor: (0, 0),
        head: (0, 4),
        block: false,
    }));
    assert_eq!(t.selection_text(), "quick");
    feed_pty(&t, b"\x1b[?1049l");
    t.rebase_selection();
    assert!(
        t.selection().is_none(),
        "an alt-screen selection dies with the alternate grid"
    );
}

#[test]
fn selection_spanning_scrollback_into_live_extracts_every_line() {
    let mut t = fresh_term(20, 3);
    feed(&mut t, b"AAAA\r\nBBBB\r\nCCCC\r\nDDDD\r\nEEEE\r\nFFFF");
    // Select from the oldest scrollback line (AAAA = Line(-3)) all
    // the way to the last live line (FFFF = Line(2)).
    let all = extract_selection_text(&t, -3, 0, 2, 19);
    assert_eq!(all, "AAAA\nBBBB\nCCCC\nDDDD\nEEEE\nFFFF");
}

#[test]
fn process_name_resolves_a_live_pid() {
    let me = std::process::id() as i32;
    let name = process_name(me);
    assert!(
        name.as_deref().is_some_and(|n| !n.is_empty()),
        "the test process resolves to a non-empty name: {name:?}"
    );
}

#[cfg(unix)]
#[test]
fn process_cmdline_reads_a_live_process_argv() {
    // #1846: the plain refresh never read the command line, so this was
    // always `None` and no pane running `ssh box` read as an ssh session.
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("sleep runs");
    // `spawn` can return while the child still reads as the test binary
    // that forked it, before its exec lands; a loaded runner caught that.
    // Read until the argv is sleep's: a process_cmdline that never reads
    // the command line still fails here, when the budget runs out.
    let mut argv = None;
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(200),
        "the child's argv to read as sleep's",
        || {
            argv = process_cmdline(child.id() as i32);
            argv.as_ref()
                .is_some_and(|a| a.last().map(String::as_str) == Some("30"))
        },
    );
    let _ = child.kill();
    let _ = child.wait();
    let argv = argv.expect("a live child's argv");
    assert!(argv[0].ends_with("sleep"), "{argv:?}");
    assert_eq!(argv.last().map(String::as_str), Some("30"), "{argv:?}");
}

#[test]
fn pick_pane_label_prefers_a_manual_name() {
    assert_eq!(pick_pane_label(Some("server"), "zsh"), "server");
    assert_eq!(pick_pane_label(None, "vim"), "vim");
    assert_eq!(pick_pane_label(None, ""), "");
}

#[test]
fn parse_shells_skips_comments_and_dedupes_by_basename() {
    let s =
        "# /etc/shells\n/bin/zsh\n/bin/bash\n/usr/local/bin/bash\n\nnot-a-path\n/usr/bin/fish\n";
    let p = parse_shells(s);
    let bases: Vec<&str> = p.iter().map(|(_, b)| b.as_str()).collect();
    assert_eq!(
        bases,
        vec!["zsh", "bash", "fish"],
        "comments/blanks/non-paths skipped, basenames deduped"
    );
    assert_eq!(p[0].0, "/bin/zsh", "full path retained");
}
#[test]
fn finished_command_carries_its_output_block() {
    let tmp = tempfile::tempdir().unwrap();
    let script = "printf '\\033]133;A\\007$ \\033]133;B\\007make\\n\\033]133;C\\007'; printf 'main.c:7:3: error: boom\\nsecond line\\n'; printf '\\033]133;D;1\\007'";
    let term =
        PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script.into()], tmp.path())
            .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut finished = Vec::new();
    while std::time::Instant::now() < deadline {
        finished.extend(term.drain_finished_commands());
        if !finished.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(40));
    }
    let f = finished
        .first()
        .expect("the 133;D must yield a FinishedCommand");
    assert!(
        f.output.contains("main.c:7:3: error: boom") && f.output.contains("second line"),
        "the output block must ride the completion; got {:?}",
        f.output
    );
}

/// Run `script` under `/bin/sh` in a pane and return the first `n`
/// commands its marks finish.
fn finished_commands(script: &str, n: usize) -> Vec<FinishedCommand> {
    let tmp = tempfile::tempdir().unwrap();
    let term =
        PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script.into()], tmp.path())
            .unwrap();
    let mut finished = Vec::new();
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(1500),
        "the script's 133;D marks",
        || {
            finished.extend(term.drain_finished_commands());
            finished.len() >= n
        },
    );
    finished
}

/// #876: `clear` erases the rows the typed command and its output were
/// read back from at `133;D`, so the history, the build matchers and
/// the run notices all got an empty command. A full reset (`reset`,
/// RIS) erases them in place, without scrolling, so the output start
/// has to move to the top of the screen.
#[test]
fn a_command_that_clears_the_screen_keeps_its_text_and_output() {
    let script = "printf '\\033]133;A\\007$ \\033]133;B\\007clear; echo hi\\n\\033]133;C\\007\\033[H\\033[2J\\033[3Jhi\\n\\033]133;D;0\\007\\033]133;A\\007$ \\033]133;B\\007echo bye; clear\\n\\033]133;C\\007bye\\n\\033[H\\033[2J\\033[3J\\033]133;D;0\\007\\033]133;A\\007$ \\033]133;B\\007reset; echo again\\n\\033]133;C\\007\\033cagain\\n\\033]133;D;0\\007'";
    let f = finished_commands(script, 3);
    assert_eq!(f[0].cmd, "clear; echo hi");
    assert_eq!(f[0].output, "hi");
    assert_eq!(f[1].cmd, "echo bye; clear");
    assert_eq!(f[1].output, "", "the clear erased the output");
    assert_eq!(f[2].cmd, "reset; echo again");
    assert_eq!(f[2].output, "again");
}

/// #876: a clear in its own read, after an earlier command whose marks
/// survive in the scrollback (ED 2 alone pushes the screen there), must
/// not hand the earlier command's text or output to this one.
#[test]
fn a_clear_in_its_own_read_does_not_pick_up_the_previous_command() {
    let script = "printf '\\033]133;A\\007$ \\033]133;B\\007echo one\\n\\033]133;C\\007one\\n\\033]133;D;0\\007\\033]133;A\\007$ \\033]133;B\\007clear; echo two\\n\\033]133;C\\007'; sleep 0.3; printf '\\033[H\\033[2J'; sleep 0.3; printf 'two\\n\\033]133;D;0\\007'";
    let f = finished_commands(script, 2);
    assert_eq!(
        (f[0].cmd.as_str(), f[0].output.as_str()),
        ("echo one", "one")
    );
    assert_eq!(
        (f[1].cmd.as_str(), f[1].output.as_str()),
        ("clear; echo two", "two")
    );
}

/// #876: output that scrolls past the screen right after a clear, in
/// the same read, is captured from its first line.
#[test]
fn output_scrolling_after_a_clear_is_captured_whole() {
    let script = "printf '\\033]133;A\\007$ \\033]133;B\\007clear; seq 60\\n\\033]133;C\\007\\033[H\\033[2J\\033[3J%s\\n\\033]133;D;0\\007' \"$(seq 60)\"";
    let f = finished_commands(script, 1);
    assert_eq!(f[0].cmd, "clear; seq 60");
    let want: Vec<String> = (1..=60).map(|i| i.to_string()).collect();
    assert_eq!(f[0].output, want.join("\n"));
}

/// Run `typed` as the input between a `133;B` and `133;C` mark, and
/// return what the finished command recorded and what Re-run reads.
fn recorded_command(typed: &str) -> (String, String) {
    let tmp = tempfile::tempdir().unwrap();
    let script = format!(
        "printf '\\033]133;A\\007$ \\033]133;B\\007{typed}\\n\\033]133;C\\007out\\n\\033]133;D;0\\007'"
    );
    let term =
        PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script], tmp.path()).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut finished = Vec::new();
    while std::time::Instant::now() < deadline {
        finished.extend(term.drain_finished_commands());
        if !finished.is_empty() && !term.command_decorations().is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(40));
    }
    let f = finished
        .first()
        .expect("the 133;D must yield a FinishedCommand");
    let deco = term.command_decorations()[0];
    (f.cmd.clone(), term.command_input_text(&deco))
}

/// The `printf` formats fish runs in `func` of croft's fish hooks, in
/// order, rewritten for `/bin/sh`'s printf (which spells ESC `\033`).
fn fish_hook_printfs(script: &str, func: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut lines = script.lines();
    let Some(head) = lines.find(|l| {
        l.trim_start().starts_with(&format!("function {func} "))
            || l.trim() == format!("function {func}")
    }) else {
        return out;
    };
    let indent = head.len() - head.trim_start().len();
    for line in lines {
        let t = line.trim_start();
        if t == "end" && line.len() - t.len() == indent {
            break;
        }
        if let Some(rest) = t.strip_prefix("printf '") {
            let fmt = &rest[..rest.find('\'').unwrap()];
            out.push(fmt.replace("\\e", "\\033"));
        }
    }
    out
}

/// #1514: replay what croft's fish 3 hooks print around one command
/// (the prompt event, the user's prompt, the wrapped prompt's end mark,
/// the typed line, preexec, postexec) through a real pane, and the
/// command is recorded with its text. Without an end-of-prompt mark it
/// was recorded empty, so the history never got it.
#[test]
fn a_fish_3_pane_records_the_typed_command() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = crate::shell_integration::ensure_fish_integration(tmp.path()).unwrap();
    let script = std::fs::read_to_string(dir.join("fish/vendor_conf.d/croft.fish")).unwrap();
    let fmts = |func: &str| {
        fish_hook_printfs(&script, func)
            .into_iter()
            .filter(|f| !f.contains("]7;"))
            .map(|f| f.replace("%s", "0"))
            .collect::<String>()
    };
    // The prompt event's own `133;D` closes a cycle a command left
    // open; with none open here, it does not fire.
    let prompt_start = fmts("_croft_mark_prompt").replace("\\033]133;D\\a", "");
    assert!(prompt_start.contains("133;A"), "{prompt_start:?}");
    let stream = format!(
        "{prompt_start}~> {}ls -d /tmp\\n{}/tmp\\n{}",
        fmts("_croft_prompt_end"),
        fmts("_croft_mark_output_start"),
        fmts("_croft_mark_output_end"),
    );
    let term = PtyTerminal::new_running(
        "/bin/sh",
        &[String::from("-c"), format!("printf '{stream}'")],
        tmp.path(),
    )
    .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut finished = Vec::new();
    while std::time::Instant::now() < deadline {
        finished.extend(term.drain_finished_commands());
        if !finished.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(40));
    }
    let f = finished.first().expect("the hooks' 133;D ends a command");
    assert_eq!(f.cmd, "ls -d /tmp", "stream {stream:?}");
}

/// #1514 negative: fish 4 marks prompts itself, so croft's hooks, the
/// prompt wrap among them, stay behind the old-fish check and never
/// add a second set of marks.
#[test]
fn the_fish_prompt_wrap_is_only_for_old_fish() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = crate::shell_integration::ensure_fish_integration(tmp.path()).unwrap();
    let script = std::fs::read_to_string(dir.join("fish/vendor_conf.d/croft.fish")).unwrap();
    let gate = script.find("forward-char-passive").unwrap();
    let wrap = script.find("function _croft_wrap_prompt").unwrap();
    let end_mark = script.find("133;B").unwrap();
    assert!(gate < wrap && gate < end_mark);
    // The wrap leaves an already wrapped prompt alone, so a redrawn
    // prompt does not stack wrappers or print the mark twice.
    let body = &script[wrap..];
    let body = &body[..body.find("\n        end\n").unwrap()];
    assert!(body.contains("string match -q '*_croft_prompt_end*'; and return"));
}

/// #1514: `funcsave fish_prompt` (and `fish_config prompt save`, which
/// calls it) and `funced` must see the user's prompt, not croft's
/// wrapper, or the saved prompt would call croft-only helpers in every
/// later fish session. Both are wrapped to unwrap the prompt first, and
/// only on old fish, where the prompt wrap exists.
#[test]
fn saving_the_fish_prompt_saves_the_users_prompt_not_the_wrapper() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = crate::shell_integration::ensure_fish_integration(tmp.path()).unwrap();
    let script = std::fs::read_to_string(dir.join("fish/vendor_conf.d/croft.fish")).unwrap();
    let gate = script.find("forward-char-passive").unwrap();
    let unwrap = script.find("function _croft_unwrap_prompt").unwrap();
    assert!(gate < unwrap);
    let body = &script[unwrap..];
    let body = &body[..body.find("\n        end\n").unwrap()];
    assert!(body.contains("functions --copy _croft_user_prompt fish_prompt"));
    for cmd in ["funcsave", "funced"] {
        assert!(script.contains(&format!("functions --copy {cmd} _croft_{cmd}")));
        let def = format!("function {cmd}\n");
        let at = script
            .find(&def)
            .unwrap_or_else(|| panic!("{cmd} is wrapped"));
        assert!(gate < at);
        let wrapper = &script[at..];
        let wrapper = &wrapper[..wrapper.find("\n            end\n").unwrap()];
        let un = wrapper.find("_croft_unwrap_prompt").unwrap();
        let orig = wrapper.find(&format!("_croft_{cmd} $argv")).unwrap();
        assert!(un < orig, "{wrapper}");
    }
}

#[test]
fn a_multi_line_command_is_recorded_without_its_continuation_prompt() {
    // #1250: bash's PS2 `> ` was stored as typed input, and recalling
    // the command ran `> cat notes` as a redirect.
    let (cmd, rerun) = recorded_command("for f in a b; do\\n> cat notes; done");
    assert_eq!(cmd, "for f in a b; do\ncat notes; done");
    assert_eq!(rerun, cmd, "Re-run reads the same text");
    let (cmd, _) = recorded_command("ls \\\\\\n> -la");
    assert_eq!(cmd, "ls \\\n-la");
}

#[test]
fn zsh_continuation_prompts_are_dropped_too() {
    let (cmd, _) = recorded_command("for f in a b\\nfor> do echo $f\\nfor> done");
    assert_eq!(cmd, "for f in a b\ndo echo $f\ndone");
    let (cmd, _) = recorded_command("echo \"a\\ndquote> b\"");
    assert_eq!(cmd, "echo \"a\nb\"");
}

#[test]
fn redirects_typed_by_the_user_stay_in_the_command() {
    // Negative: only the prompt goes. A `>` the user typed, on the first
    // line or at the start of a continuation line, is kept.
    let (cmd, _) = recorded_command("echo hi > out.txt");
    assert_eq!(cmd, "echo hi > out.txt");
    let (cmd, _) = recorded_command("{ echo a\\n> > log; }");
    assert_eq!(cmd, "{ echo a\n> log; }");
}

#[test]
fn an_empty_continuation_line_keeps_no_prompt() {
    // The grid drops a row's trailing blanks, so an empty line after
    // `> ` reads as a bare `>`.
    assert_eq!(
        without_continuation_prompts("ls \\\n>\n> -la"),
        "ls \\\n\n-la"
    );
    assert_eq!(without_continuation_prompts("for x\nfor>"), "for x\n");
    assert_eq!(without_continuation_prompts("a\n>> log"), "a\n>> log");
}

#[test]
fn a_soft_wrapped_line_is_not_a_continuation() {
    // Negative: a long line wraps with no newline and no PS2, so text
    // that happens to sit at the start of the next row is kept.
    let long = format!("echo {}> x", "y".repeat(78));
    let (cmd, _) = recorded_command(&long);
    assert_eq!(cmd, long);
}

#[test]
fn finished_output_keeps_a_final_line_without_trailing_newline() {
    // `133;D` moves no cursor: a diagnostic printed WITHOUT a trailing
    // newline leaves the mark on that very row, and excluding the
    // cursor row dropped exactly the line the matchers needed (#120
    // review).
    let tmp = tempfile::tempdir().unwrap();
    let script = "printf '\\033]133;A\\007$ \\033]133;B\\007cc\\n\\033]133;C\\007'; printf 'lib.c:2:1: error: no newline here'; printf '\\033]133;D;1\\007'";
    let term =
        PtyTerminal::new_running("/bin/sh", &[String::from("-c"), script.into()], tmp.path())
            .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut finished = Vec::new();
    while std::time::Instant::now() < deadline {
        finished.extend(term.drain_finished_commands());
        if !finished.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(40));
    }
    let f = finished.first().expect("completion expected");
    assert!(
        f.output.contains("lib.c:2:1: error: no newline here"),
        "the unterminated final row must be captured; got {:?}",
        f.output
    );
}
