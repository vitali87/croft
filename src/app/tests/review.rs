//! Code review: pull request review, review comments and sticky notes.

use super::*;

/// The agent review diff must not leave another file's "Restore Snapshot"
/// armed (#345): it opens a snapshot diff like the TIMELINE does, so it
/// owes the same disarm, or restore silently overwrites a file the user is
/// not looking at.
#[test]
fn the_review_diff_disarms_another_files_restore() {
    let tmp = tempfile::tempdir().unwrap();
    let hist = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.txt");
    let b = tmp.path().join("b.txt");
    std::fs::write(&a, "a-disk\n").unwrap();
    std::fs::write(&b, "b-disk\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.history_root = hist.path().to_path_buf();

    // a.txt's timeline snapshot is on screen: restore is armed for a.txt.
    crate::history::record_in(&app.history_root, &a, b"a-old\n", 1000).unwrap();
    app.editor.open(&a).unwrap();
    app.open_timeline_diff(a.clone(), "local:1000".into());
    assert!(app.history_restore.is_some(), "staging: a's snapshot armed");

    // b.txt is in an agent lane and has never been reviewed, so the diff
    // REFUSES - and a refusal leaves a's diff on screen, which is exactly
    // when a stale restore is most dangerous.
    let working = vec![String::from("claude")];
    app.agent_ledger.record_write(&b, 7, &working);
    app.diff_agent_lane_row("claude", &b);
    assert!(
        app.history_restore.is_none(),
        "the review diff left the previous file's restore armed"
    );
    app.restore_history_snapshot();
    assert_eq!(
        std::fs::read_to_string(&a).unwrap(),
        "a-disk\n",
        "restore wrote a stale snapshot over an unrelated file"
    );
}

/// #1585: an Esc that leaves a comment box's reply field or vim's Insert
/// mode did something, so it is not half of Esc Esc.
#[test]
fn an_esc_leaving_a_comment_box_or_insert_mode_is_not_half_of_esc_esc() {
    let (_tmp, mut app) = zen_app();
    app.toggle_zen_mode();
    app.editor.comment_focus = Some(crate::widgets::editor::CommentFocus {
        id: 1,
        reply: String::new(),
        cursor: 0,
    });
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert!(
        app.editor.comment_focus.is_none(),
        "the first Esc left the box"
    );
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert!(app.zen_mode, "the Esc that left the box did not count");

    app.zen_esc_at = None;
    app.vim.enabled = true;
    app.vim.enter_insert_mode();
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.vim.mode(), crate::vim::VimMode::Normal);
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert!(app.zen_mode, "the Esc that left Insert mode did not count");
}

#[test]
fn cmd_k_n_annotates_the_selection_and_a_click_shows_the_note() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.terminals[0] = crate::widgets::terminal::PtyTerminal::new_running(
        "/bin/sh",
        &[
            String::from("-c"),
            String::from("m=mark; echo ${m}-this-output; sleep 30"),
        ],
        tmp.path(),
    )
    .unwrap();
    app.focus_pane(Pane::Terminal);
    crate::test_budget::await_spawned(
        crate::test_budget::tests::SHELL_PAINT_BASE,
        "the pane output \"mark-this-output\"",
        || {
            app.terminals[0]
                .grid_lines()
                .0
                .iter()
                .any(|l| l.starts_with("mark-this-output"))
        },
    );
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let inner = app.terminals[0].last_inner;
    let (lines, top) = app.terminals[0].grid_lines();
    let vrow = (top
        + lines
            .iter()
            .position(|l| l.starts_with("mark-this-output"))
            .unwrap() as i32) as u16;

    // Select the word and annotate it through the prompt.
    app.terminals[0].start_selection_at(inner.x, inner.y + vrow);
    app.terminals[0].extend_selection_to(inner.x + 15, inner.y + vrow);
    assert!(app.handle_cmd_k_chord(key(KeyCode::Char('n'), KeyModifiers::NONE)));
    assert!(
        app.prompt.is_some(),
        "Cmd+K N must open the annotation prompt"
    );
    app.prompt.as_mut().unwrap().buffer = String::from("broke here");
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert!(app.prompt.is_none());
    let (_, text) = app.terminals[0]
        .annotation_at(inner.x + 3, inner.y + vrow)
        .expect("the annotation must exist over the selected span");
    assert_eq!(text, "broke here");

    // The span renders amber + underlined.
    term.draw(|f| app.render(f)).unwrap();
    let style = term.backend().buffer()[(inner.x + 3, inner.y + vrow)].style();
    assert_eq!(
        style.fg,
        Some(ratatui::style::Color::Rgb(0xe5, 0xc0, 0x7b)),
        "annotated cells are tinted amber"
    );
    assert!(
        style
            .add_modifier
            .contains(ratatui::style::Modifier::UNDERLINED),
        "annotated cells are underlined"
    );

    // A plain click on the span pops the note.
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        inner.x + 3,
        inner.y + vrow,
    ));
    app.handle_mouse(mouse(
        MouseEventKind::Up(MouseButton::Left),
        inner.x + 3,
        inner.y + vrow,
    ));
    let popup = app
        .hover_popup
        .as_ref()
        .expect("clicking the span shows the note");
    assert!(
        popup.lines.iter().any(|l| l.contains("broke here")),
        "popup carries the note text"
    );

    // The note must actually be PAINTED, not just set. Regression: the
    // hover popup rendered only inside the editor-tabs branch, so with the
    // welcome screen up (no file open — exactly a fresh croft) the click
    // set the popup but nothing ever drew it.
    term.draw(|f| app.render(f)).unwrap();
    let buf = term.backend().buffer();
    let painted = (0..buf.area.height).any(|y| {
        (0..buf.area.width)
            .map(|x| buf[(x, y)].symbol())
            .collect::<String>()
            .contains("broke here")
    });
    assert!(
        painted,
        "the note popup must be painted on screen even with the welcome screen up"
    );

    // A real mouse drifts right after the click: the note must survive
    // pointer motion while it stays on the annotated span, and dismiss
    // once the pointer leaves it (regression: any move outside the editor
    // used to kill the popup instantly).
    app.handle_mouse(mouse(MouseEventKind::Moved, inner.x + 4, inner.y + vrow));
    assert!(
        app.hover_popup.is_some(),
        "drifting within the span must keep the note open"
    );
    app.handle_mouse(mouse(MouseEventKind::Moved, inner.x + 60, inner.y + vrow));
    assert!(
        app.hover_popup.is_none(),
        "leaving the span dismisses the note"
    );

    // Cmd+K Shift+N over the span deletes it.
    app.terminals[0].start_selection_at(inner.x + 2, inner.y + vrow);
    app.terminals[0].extend_selection_to(inner.x + 5, inner.y + vrow);
    assert!(app.handle_cmd_k_chord(key(KeyCode::Char('N'), KeyModifiers::SHIFT)));
    assert!(
        app.terminals[0]
            .annotation_at(inner.x + 3, inner.y + vrow)
            .is_none(),
        "Cmd+K Shift+N deletes the annotation under the selection"
    );
}

/// F4 visits notes in file (row) order and wraps, regardless of the order
/// they streamed in. Landing order [(10,..),(2,..)] must not strand the
/// row-2 note or stick on row 10.
#[test]
fn next_note_by_row_walks_rows_in_order_and_wraps() {
    let notes = vec![
        (1u64, 10usize, String::from("late")),
        (2u64, 2usize, String::from("early")),
        (3u64, 6usize, String::from("mid")),
    ];
    // From the top: nearest row strictly greater.
    assert_eq!(notes[super::next_note_by_row(&notes, (0, usize::MAX))].1, 2);
    assert_eq!(notes[super::next_note_by_row(&notes, (2, usize::MAX))].1, 6);
    assert_eq!(
        notes[super::next_note_by_row(&notes, (6, usize::MAX))].1,
        10
    );
    // Past the last note: wrap to the smallest row (not stick on row 10).
    assert_eq!(
        notes[super::next_note_by_row(&notes, (10, usize::MAX))].1,
        2
    );
    assert_eq!(
        notes[super::next_note_by_row(&notes, (99, usize::MAX))].1,
        2
    );
}

/// Replying anchors the navigator's answer on the answered note's own row,
/// so two notes on one row is the NORMAL state after one reply. The index
/// tie-break must step through both and wrap - a row-only walk went
/// first-note → first-note forever and the answer was unreachable by F4.
#[test]
fn next_note_by_row_steps_through_co_anchored_notes() {
    let notes = vec![
        (1u64, 5usize, String::from("original")),
        (2u64, 5usize, String::from("answer")),
    ];
    // Focused on the first note at row 5: F4 reaches its co-anchored answer.
    assert_eq!(super::next_note_by_row(&notes, (5, 0)), 1);
    // Focused on the answer: F4 wraps back to the first.
    assert_eq!(super::next_note_by_row(&notes, (5, 1)), 0);
    // From a bare caret on that row, neither is "past" it: wrap to first.
    assert_eq!(super::next_note_by_row(&notes, (5, usize::MAX)), 0);
}

/// The TurnDone hint names a file only when every note landed in the SAME
/// file: "3 comments in b.rs" was a lie when two of them sat in a.rs.
#[test]
fn turn_note_summary_only_names_a_single_file() {
    use std::collections::BTreeSet;
    let one: BTreeSet<String> = [String::from("a.rs")].into();
    assert_eq!(super::turn_note_summary(1, &one), "1 comment in a.rs");
    assert_eq!(super::turn_note_summary(3, &one), "3 comments in a.rs");
    let two: BTreeSet<String> = [String::from("a.rs"), String::from("b.rs")].into();
    assert_eq!(
        super::turn_note_summary(3, &two),
        "3 comments across 2 files"
    );
    assert_eq!(super::turn_note_summary(2, &BTreeSet::new()), "2 comments");
}

/// A clean turn that left comments must SAY so: the TurnDone status names
/// the comment count and the F4 review key. The fake's turn leaves TWO
/// boxes: its NOTE fence, plus its prose commentary ("Reviewing."), which
/// lands as a comment box at the turn's origin — never in OUTPUT (boxes are
/// the navigator's single voice).
#[test]
fn turn_done_status_keeps_the_notes_visible() {
    use std::time::{Duration, Instant};
    if !crate::lsp::manager::is_on_path("python3") {
        eprintln!("SKIPPED: python3 not on PATH");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("demo.txt");
    std::fs::write(&file, "a\nb\nc").unwrap();
    let socket = tmp.path().join("collab.sock");
    {
        let s = socket.clone();
        std::thread::spawn(move || {
            let _ = crate::collab::relay_serve(&s);
        });
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while !crate::session::is_alive(&socket) {
        assert!(Instant::now() < deadline, "relay never came up");
        std::thread::sleep(Duration::from_millis(10));
    }
    let script = tmp.path().join("fake_claude.py");
    std::fs::write(&script, crate::pair::FAKE_CLAUDE).unwrap();
    let log = tmp.path().join("stdin.log");

    // Full seating (record + owner self-appointment): the app-side owner
    // answers the pilot's SnapshotRequest so the NOTE can anchor.
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.open_file_at_launch(&file);
    app.pair_record_path = tmp.path().join("x.pair.json");
    app.pair_socket = socket.clone();
    app.pair_host_lock_path = tmp.path().join("x.pair-host.lock");
    let (script2, log2, socket2) = (script.clone(), log.clone(), socket.clone());
    app.pair_spawn_override = Some(Box::new(move |cfg| {
        let mut cmd = std::process::Command::new("python3");
        cmd.arg(&script2).arg(&log2).arg("notes");
        crate::pair_host::PairHost::spawn_cmd(&socket2, &cfg.name, cfg.task.as_deref(), cmd)
    }));
    crate::session::write_pair_record(
        &app.pair_record_path,
        &crate::session::PairRecord {
            model: None,
            name: "nav".into(),
            enabled: true,
            task: None,
            provider: None,
            base_url: None,
        },
    )
    .unwrap();
    app.last_pair_check = None;
    assert!(app.maybe_seat_navigator());
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.collab.is_none() {
        assert!(Instant::now() < deadline, "owner seat never connected");
        app.poll_collab();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(app.handle_cmd_k_chord(key(KeyCode::Char('y'), KeyModifiers::NONE)));

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        app.poll_collab();
        app.poll_pair();
        if app.status.contains("finished") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "turn never finished; status: {}",
            app.status
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        app.status.contains("2 comments") && app.status.contains("F4"),
        "TurnDone status must surface the comments, got: {}",
        app.status
    );
    // The prose landed as an anchored box (poll_pair's match makes box vs
    // OUTPUT mutually exclusive; the fallback path has its own test, and
    // the OUTPUT channel is process-global so it cannot be asserted empty
    // under the parallel suite).
    let host = app.pair_host.as_ref().expect("still seated");
    let snap = host.notes_snapshot("demo.txt");
    assert!(
        snap.iter().any(|(_, _, body)| body.contains("Reviewing.")),
        "commentary must land as a comment box; snapshot: {snap:?}"
    );
}

/// The navigator note popup: opens when the caret LANDS on a noted line
/// (staying put never re-triggers), Esc dismisses it without eating the
/// selection, F4 cycles through the file's notes (wrapping) and jumps the
/// caret, and moving off a noted line closes it.
#[test]
fn f4_focuses_comment_boxes_typing_edits_the_draft_and_esc_releases() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("f.txt");
    std::fs::write(&file, "a\nb\nc\nd\ne").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.open_file_at_launch(&file);
    app.navigator_notes.insert(
        "f.txt".into(),
        vec![(1, 1, "first note".into()), (2, 3, "second note".into())],
    );

    // F4 focuses the next box from the caret (id 1 on row 1) and jumps to
    // its anchor; a second F4 hops to the next box; a third wraps.
    app.handle_key(key(KeyCode::F(4), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.editor.cursor_row, 1);
    assert_eq!(app.editor.comment_focus.as_ref().map(|f| f.id), Some(1));
    app.handle_key(key(KeyCode::F(4), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.editor.cursor_row, 3);
    assert_eq!(app.editor.comment_focus.as_ref().map(|f| f.id), Some(2));
    app.handle_key(key(KeyCode::F(4), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.editor.comment_focus.as_ref().map(|f| f.id), Some(1));

    // Typing edits the reply draft, never the buffer.
    let before = app.editor.lines.clone();
    for c in ['w', 'h', 'y'] {
        app.handle_key(key(KeyCode::Char(c), KeyModifiers::NONE))
            .unwrap();
    }
    assert_eq!(
        app.editor.comment_focus.as_ref().map(|f| f.reply.as_str()),
        Some("why")
    );
    assert_eq!(app.editor.lines, before, "the buffer is untouched");
    app.handle_key(key(KeyCode::Backspace, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(
        app.editor.comment_focus.as_ref().map(|f| f.reply.as_str()),
        Some("wh")
    );

    // Esc releases the keyboard back to the buffer.
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert!(app.editor.comment_focus.is_none());

    // Shift+F4 ignores the nearest box: it leaves the snapshot immediately.
    app.editor.cursor_row = 0;
    app.handle_key(key(KeyCode::F(4), KeyModifiers::SHIFT))
        .unwrap();
    assert_eq!(
        app.navigator_notes.get("f.txt").map(|n| n.len()),
        Some(1),
        "the ignored box is gone from the snapshot"
    );
}

/// A failed ask submit must not eat the typed draft: the navigator can go
/// away (or busy) between opening the box and Enter, and the instruction
/// the user typed is unrecoverable once the prompt closes. The failure
/// keeps the box open for a retry, like a failed comment reply keeps its
/// draft.
#[test]
fn a_failed_ask_submit_keeps_the_typed_draft() {
    use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.input_prompt = Some(
        InputPrompt::new(
            InputPurpose::AskNavigator {
                file: String::from("f.rs"),
                range: (0, 0),
                selection: String::new(),
            },
            "Ask nav",
            "",
        )
        .with_value("check the loop bounds"),
    );
    app.submit_input_prompt();
    let p = app
        .input_prompt
        .as_ref()
        .expect("the failed submit must keep the prompt open");
    assert_eq!(p.value, "check the loop bounds", "the draft survives");
    assert!(
        app.status.to_lowercase().contains("not active"),
        "the failure is announced: {}",
        app.status
    );
}

/// Enter in a focused comment box sends the reply turn: the composed
/// message (note context, the reply, the edit-on-request rule, numbered
/// buffer) reaches the pilot's claude stdin, and the focus releases.
#[test]
fn enter_in_a_focused_comment_box_sends_the_reply_turn() {
    use std::time::{Duration, Instant};
    if !crate::lsp::manager::is_on_path("python3") {
        eprintln!("SKIPPED: python3 not on PATH");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("f.txt");
    std::fs::write(&file, "a\nb\nc").unwrap();
    let socket = tmp.path().join("collab.sock");
    {
        let s = socket.clone();
        std::thread::spawn(move || {
            let _ = crate::collab::relay_serve(&s);
        });
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while !crate::session::is_alive(&socket) {
        assert!(Instant::now() < deadline, "relay never came up");
        std::thread::sleep(Duration::from_millis(10));
    }
    let script = tmp.path().join("fake_claude.py");
    std::fs::write(&script, crate::pair::FAKE_CLAUDE).unwrap();
    let log = tmp.path().join("stdin.log");

    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.open_file_at_launch(&file);
    let mut cmd = std::process::Command::new("python3");
    cmd.arg(&script).arg(&log).arg("notes");
    app.pair_host = Some(crate::pair_host::PairHost::spawn_cmd(&socket, "nav", None, cmd).unwrap());
    app.navigator_notes
        .insert("f.txt".into(), vec![(1, 1, "seed note".into())]);
    app.editor.comment_focus = Some(crate::widgets::editor::CommentFocus {
        id: 1,
        reply: String::from("is this hot?"),
        cursor: 12,
    });

    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert!(
        app.editor.comment_focus.is_none(),
        "a sent reply releases the focus; status: {}",
        app.status
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let sent = std::fs::read_to_string(&log).unwrap_or_default();
        if sent.contains("replied to your note on f.txt at line 1")
            && sent.contains("seed note")
            && sent.contains("is this hot?")
            && sent.contains("make it with EDIT fences")
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "reply turn never reached claude; log: {sent}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// #366: review boxes survive a render, which rebuilds the navigator's.
///
/// `render` reassigns `editor.comment_boxes` from `navigator_notes` every
/// frame, so review threads written straight into that field were wiped
/// before the user saw one — while the status line still reported having
/// loaded them. The boxes therefore live in their own field and are merged
/// in, and this asserts the merge rather than the assignment.
#[test]
fn review_boxes_survive_the_render_that_rebuilds_navigator_notes() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("a.rs");
    std::fs::write(&file, "one\ntwo\nthree\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&file).unwrap();

    app.review_boxes = Some((
        file.clone(),
        vec![crate::review_threads::Thread {
            id: 77,
            author: String::from("ada"),
            body: String::from("this moved"),
            path: String::from("a.rs"),
            anchor: crate::review_threads::Anchor::Outdated(1),
            resolved: false,
        }],
    ));

    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();

    assert!(
        app.editor.comment_boxes.iter().any(|b| b.id == 77),
        "the render wiped the review box: {:?}",
        app.editor
            .comment_boxes
            .iter()
            .map(|b| b.id)
            .collect::<Vec<_>>()
    );

    // A second frame must not duplicate it — the merge appends each time,
    // so it has to append to a freshly rebuilt list rather than accumulate.
    term.draw(|f| app.render(f)).unwrap();
    assert_eq!(
        app.editor
            .comment_boxes
            .iter()
            .filter(|b| b.id == 77)
            .count(),
        1,
        "the box was duplicated across frames"
    );
}

/// #366: another file's review comments do not follow the user to this one.
///
/// The boxes are merged into `editor.comment_boxes` on every frame. Keyed
/// only by "whatever was loaded last", `a.rs`'s objections would hang off
/// `b.rs`'s line numbers, against unrelated code — the same harm
/// `threads_are_filtered_to_the_file_being_viewed` prevents on the filter
/// axis, re-entering through the lifetime axis. So the field carries the
/// path it was loaded for and the merge checks it.
#[test]
fn review_boxes_do_not_follow_the_user_to_another_file() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.rs");
    let b = tmp.path().join("b.rs");
    std::fs::write(&a, "one\ntwo\nthree\n").unwrap();
    std::fs::write(&b, "alpha\nbeta\ngamma\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&a).unwrap();
    app.review_boxes = Some((
        a.clone(),
        vec![crate::review_threads::Thread {
            id: 77,
            author: String::from("ada"),
            body: String::from("objection"),
            path: String::from("a.rs"),
            anchor: crate::review_threads::Anchor::At(1),
            resolved: false,
        }],
    ));

    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    assert!(
        app.editor.comment_boxes.iter().any(|b| b.id == 77),
        "the box should be on the file it was loaded for"
    );

    // Switch files: the boxes belong to a.rs and must not appear here.
    app.editor.open(&b).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    assert!(
        !app.editor.comment_boxes.iter().any(|bx| bx.id == 77),
        "a.rs's review comments followed the user to b.rs: {:?}",
        app.editor
            .comment_boxes
            .iter()
            .map(|bx| (bx.id, bx.line))
            .collect::<Vec<_>>()
    );
}

/// #366: box geometry tracks the live buffer, not the buffer at load time.
///
/// `box_line` clamps so a box past the end stays visible. Computed once at
/// load, that clamp is stale the instant the user edits: delete most of the
/// file and the boxes keep rows that no longer exist, which is exactly the
/// invisibility clamping exists to prevent. So the field keeps `Thread`s and
/// the geometry is derived per frame.
#[test]
fn review_box_geometry_follows_the_buffer_when_it_shrinks() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("a.rs");
    std::fs::write(&file, "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&file).unwrap();
    app.review_boxes = Some((
        file.clone(),
        vec![crate::review_threads::Thread {
            id: 88,
            author: String::from("ada"),
            body: String::from("late"),
            path: String::from("a.rs"),
            anchor: crate::review_threads::Anchor::At(9),
            resolved: false,
        }],
    ));

    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let before = app
        .editor
        .comment_boxes
        .iter()
        .find(|b| b.id == 88)
        .map(|b| b.line);
    assert_eq!(before, Some(9), "the box should sit on its own line first");

    // The user deletes most of the file; line 9 no longer exists.
    app.editor.lines.truncate(3);
    term.draw(|f| app.render(f)).unwrap();
    let after = app
        .editor
        .comment_boxes
        .iter()
        .find(|b| b.id == 88)
        .map(|b| b.line);
    assert_eq!(
        after,
        Some(2),
        "the box kept a row past the end of the shrunken buffer"
    );
}

/// #366: Ignore actually dismisses a review box.
///
/// Every box renders a live `\u{2715} Ignore` footer, and the hit-test is
/// driven off `comment_boxes` indices with no notion of a box's origin. If
/// `ignore_comment_box` only knows about navigator notes, it reports
/// "Comment ignored" and the box returns on the next frame — a dismiss that
/// claims success and visibly does nothing.
#[test]
fn ignoring_a_review_box_actually_dismisses_it() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("a.rs");
    std::fs::write(&file, "one\ntwo\nthree\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&file).unwrap();
    app.review_boxes = Some((
        file.clone(),
        vec![crate::review_threads::Thread {
            id: 99,
            author: String::from("ada"),
            body: String::from("dismiss me"),
            path: String::from("a.rs"),
            anchor: crate::review_threads::Anchor::At(0),
            resolved: false,
        }],
    ));

    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    assert!(app.editor.comment_boxes.iter().any(|b| b.id == 99));

    app.ignore_comment_box(99);
    term.draw(|f| app.render(f)).unwrap();
    assert!(
        !app.editor.comment_boxes.iter().any(|b| b.id == 99),
        "the ignored review box came back on the next frame"
    );
}

/// #366: F4 reaches review boxes, not only navigator notes.
///
/// The boxes render on a shared surface with a shared hit-test, so a
/// keyboard walk that reads `navigator_notes` alone leaves them visible but
/// unreachable — and reports "No navigator comments in this file" while
/// several are on screen.
#[test]
fn the_comment_walk_reaches_review_boxes() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("a.rs");
    std::fs::write(&file, "one\ntwo\nthree\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&file).unwrap();
    app.review_boxes = Some((
        file.clone(),
        vec![crate::review_threads::Thread {
            id: 55,
            author: String::from("ada"),
            body: String::from("reachable?"),
            path: String::from("a.rs"),
            anchor: crate::review_threads::Anchor::At(2),
            resolved: false,
        }],
    ));

    // Deliberately NO render first: `comment_boxes` is a render output, so
    // a walk that read it would depend on a frame having been drawn and
    // would find nothing on an F4 pressed before the first one.
    app.editor.cursor_row = 0;
    assert_eq!(
        app.next_comment_from_caret().map(|(id, _)| id),
        Some(55),
        "F4 skipped the review box entirely"
    );
}

/// #366: review boxes follow the open file through a rename.
///
/// Both guards compare the stored path against `editor.path`, so a rename
/// that repoints the tab without repointing the field makes the boxes
/// vanish from the buffer the user is still editing.
#[test]
fn review_boxes_follow_the_open_file_through_a_rename() {
    let tmp = tempfile::tempdir().unwrap();
    let old = tmp.path().join("a.rs");
    let new = tmp.path().join("renamed.rs");
    std::fs::write(&old, "one\ntwo\nthree\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&old).unwrap();
    app.review_boxes = Some((
        old.clone(),
        vec![crate::review_threads::Thread {
            id: 66,
            author: String::from("ada"),
            body: String::from("still mine"),
            path: String::from("a.rs"),
            anchor: crate::review_threads::Anchor::At(1),
            resolved: false,
        }],
    ));

    // Driven through the real rename prompt rather than by calling the
    // helpers directly: hand-calling them tests the helper's logic while
    // leaving the WIRING untested, so deleting the call from the rename
    // handler would keep passing.
    app.prompt = Some(super::Prompt {
        label: String::from("Rename a.rs"),
        buffer: String::from("renamed.rs"),
        kind: super::PromptKind::Rename(old.clone()),
        target_dir: tmp.path().to_path_buf(),
        error: None,
    });
    app.commit_prompt();
    assert!(
        app.prompt.is_none(),
        "the rename prompt should have committed"
    );
    assert_eq!(
        app.editor.path.as_deref(),
        Some(new.as_path()),
        "the tab should have followed the rename"
    );

    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    assert!(
        app.editor.comment_boxes.iter().any(|b| b.id == 66),
        "the rename orphaned the review boxes: {:?}",
        app.editor
            .comment_boxes
            .iter()
            .map(|b| b.id)
            .collect::<Vec<_>>()
    );
}

/// #345: a re-root leaves no dots from the workspace the user left.
///
/// The paths in the set are ABSOLUTE, and Make Root's documented use is
/// re-rooting into a CHILD — where the new tree's paths are exactly the ones
/// still in the set. Left behind, every stale dot re-renders on the new tree
/// describing a lane that is gone, which is the precise staleness this
/// decoration exists to avoid.
#[test]
fn a_re_root_drops_the_previous_workspaces_review_queue() {
    let tmp = tempfile::tempdir().unwrap();
    let child = tmp.path().join("child");
    std::fs::create_dir(&child).unwrap();
    let inside = child.join("in_child.rs");
    std::fs::write(&inside, "fn a() {}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();

    app.terminals[0].set_agent(Some(crate::agents::AgentLane {
        name: String::from("claude"),
        status: crate::agents::AgentStatus::Working,
    }));
    let mut changed = std::collections::BTreeSet::new();
    changed.insert(inside.clone());
    assert!(app.attribute_writes_to_working_agents(&changed));
    app.sync_agent_lane_decorations();
    assert!(
        app.tree.agent_touched.contains(&inside),
        "staging: the file is decorated before the re-root"
    );

    // Re-root INTO the child: the file's absolute path is unchanged and it
    // is still on screen, so a surviving set would re-decorate it.
    app.change_workspace_root(child.clone());
    assert!(
        app.tree.agent_touched.is_empty(),
        "the Explorer kept the old workspace's dots: {:?}",
        app.tree.agent_touched
    );
    assert_eq!(
        app.agent_ledger.unreviewed_files(),
        0,
        "the ledger kept the old workspace's queue"
    );
    assert!(!app.tree.is_agent_touched(&inside));
}

#[test]
fn the_review_queue_names_the_lane_each_agents_files_are_in() {
    // #348: with a worktree lane in the workspace the queue groups by root,
    // naming a lane by its BRANCH and any other root by its display label,
    // so two files of the same basename are told apart.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    init_lane_repo(&repo);
    let mut app = App::new(repo.clone()).unwrap();
    app.terminal_session_path = tmp.path().join("sessions.json");
    app.lane_agent = None;
    app.create_worktree_lane("fix login");
    let repo = app.roots.primary().to_path_buf();
    let lane = tmp.path().join("repo-fix-login").canonicalize().unwrap();

    let working = vec![String::from("claude")];
    app.agent_ledger
        .record_write(&repo.join("src/mod.rs"), 1, &working);
    app.agent_ledger
        .record_write(&lane.join("src/mod.rs"), 2, &working);
    app.agent_ledger
        .record_write(&lane.join("src/other.rs"), 3, &working);

    let rows = app.agent_lane_rows();
    assert_eq!(rows.len(), 1, "one row per agent: {rows:?}");
    let row = &rows[0];
    assert!(row.starts_with("claude: 3 to review \u{2014} "), "{row}");
    let repo_label = repo
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap();
    assert!(
        row.contains(&format!("{repo_label} 1")),
        "the primary root is named by its folder: {row}"
    );
    assert!(
        row.contains("agent/fix-login 2"),
        "the lane is named by its branch: {row}"
    );
    assert!(
        !row.contains("mod.rs"),
        "a grouped row carries labels and counts, not file names, so the \
         status bar cannot elide the labels away: {row}"
    );
    assert!(
        row.find(&repo_label) < row.find("agent/fix-login"),
        "root order, primary first: {row}"
    );

    // A file outside every root still appears, under its own heading. Only
    // reachable defensively: the ledger's one production feeder drops paths
    // under no root before recording them.
    app.agent_ledger
        .record_write(&tmp.path().join("stray.rs"), 4, &working);
    let row = app.agent_lane_rows().remove(0);
    assert!(
        row.contains("outside the workspace 1"),
        "an unowned file is counted, not dropped: {row}"
    );
}

#[test]
fn a_grouped_review_queue_row_survives_the_status_bar() {
    // The bar is one row and elides the MIDDLE (`elide_middle`), so a row
    // long enough to be cut loses its group labels — the one thing grouping
    // adds. Labels and counts keep it short enough to land whole. Same
    // shape as `status_transient_keeps_its_tail_when_longer_than_the_bar`.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    init_lane_repo(&repo);
    let mut app = App::new(repo.clone()).unwrap();
    app.terminal_session_path = tmp.path().join("sessions.json");
    app.lane_agent = None;
    app.create_worktree_lane("fix login");
    let repo = app.roots.primary().to_path_buf();
    let lane = tmp.path().join("repo-fix-login").canonicalize().unwrap();
    let working = vec![String::from("claude")];
    for i in 0..4 {
        app.agent_ledger
            .record_write(&repo.join(format!("src/r{i}.rs")), i as u64, &working);
        app.agent_ledger
            .record_write(&lane.join(format!("src/l{i}.rs")), 100 + i as u64, &working);
    }
    app.run_command(crate::widgets::command_palette::Command::ShowAgentLane);
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let buf = term.backend().buffer().clone();
    let a = buf.area;
    let last_row: String = (a.x..a.x + a.width)
        .map(|x| buf[(x, a.y + a.height - 1)].symbol().to_string())
        .collect();
    assert!(
        last_row.contains("agent/fix-login 4"),
        "the lane's group must reach the bar unelided: {last_row:?}"
    );
    assert!(
        last_row.contains("Ln 1, Col 1"),
        "the right cluster must still paint: {last_row:?}"
    );
}

#[test]
fn a_single_root_review_queue_reads_as_it_always_did() {
    // Grouping is for workspaces with more than one root: with one, the
    // label would name the only place the files could be and the group
    // count would repeat the total, so the row keeps its old flat shape.
    // A truncated sample says so, and an empty queue has no dangling tail.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let working = vec![String::from("claude")];
    app.agent_ledger
        .record_write(&tmp.path().join("a.rs"), 1, &working);
    app.agent_ledger
        .record_write(&tmp.path().join("b.rs"), 2, &working);
    assert_eq!(
        app.agent_lane_rows(),
        vec![String::from("claude: 2 to review (b.rs, a.rs)")],
        "the pre-grouping shape, unchanged"
    );
    for i in 0..3 {
        app.agent_ledger.record_write(
            &tmp.path().join(format!("f{i}.rs")),
            10 + i as u64,
            &working,
        );
    }
    let row = app.agent_lane_rows().remove(0);
    assert_eq!(
        row, "claude: 5 to review (f2.rs, f1.rs, f0.rs, \u{2026})",
        "a sample of three says it is a sample"
    );
    // Nothing unreviewed: no trailing separator, no empty group.
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let f = tmp.path().join("z.rs");
    std::fs::write(&f, "x").unwrap();
    app.agent_ledger.record_write(&f, 1, &working);
    let hash = app.agent_ledger.lane("claude")[0].current_hash;
    app.agent_ledger.mark_reviewed("claude", &f, hash, None);
    assert_eq!(
        app.agent_lane_rows(),
        vec![String::from("claude: 0 to review ()")],
        "an emptied lane keeps the old shape too"
    );
}

/// A git repo holding `a.rs` and a fake `gh` that answers the review
/// calls (#366, #368): (tempdir, repo root, a.rs, gh's call log, the last
/// review payload, the gh script).
fn review_fixture() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    std::path::PathBuf,
    std::path::PathBuf,
    std::path::PathBuf,
    std::path::PathBuf,
) {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("repo");
    std::fs::create_dir(&root).unwrap();
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["config", "user.email", "a@b"],
        vec!["config", "user.name", "a"],
    ] {
        let _ = std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(&args)
            .status();
    }
    let f = root.join("a.rs");
    std::fs::write(&f, "fn a() {}\nfn b() {}\n").unwrap();
    for args in [vec!["add", "."], vec!["commit", "-m", "init", "--quiet"]] {
        let _ = std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(&args)
            .status();
    }
    let log = tmp.path().join("gh.log");
    let stdin = tmp.path().join("gh.stdin");
    let gh = tmp.path().join("gh");
    std::fs::write(
        &gh,
        format!(
            r#"#!/bin/sh
echo "$@" >> '{log}'
case "$*" in
  "pr view"*) echo 7 ;;
  "pr diff"*) printf 'diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,1 +1,2 @@\n fn a() {{}}\n+fn b() {{}}\n' ;;
  *graphql*reviewThreads*) echo '{{"data":{{"repository":{{"pullRequest":{{"reviewThreads":{{"nodes":[{{"id":"PRRT_1","isResolved":false,"comments":{{"nodes":[{{"databaseId":5}}]}}}}]}}}}}}}}}}' ;;
  *graphql*) echo '{{}}' ;;
  *"/comments --paginate"*) echo '[{{"id":5,"path":"a.rs","line":1,"user":{{"login":"ada"}},"body":"why?"}}]' ;;
  *reviews*) cat > '{stdin}'; echo '{{}}' ;;
  *) echo '{{}}' ;;
esac
"#,
            log = log.display(),
            stdin = stdin.display(),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();

    (tmp, root, f, log, stdin, gh)
}

/// An app on the review fixture, with git's view of the repo in place.
fn review_app(root: &std::path::Path, f: &std::path::Path, gh: &std::path::Path) -> App {
    let mut app = App::new(root.to_path_buf()).unwrap();
    app.review_gh = gh.display().to_string();
    app.editor.open_pinned(f).unwrap();
    crate::test_budget::await_spawned(
        std::time::Duration::from_secs(5),
        "git to find the repository",
        || {
            app.git.drain_into(None);
            app.git.status().repo_root.is_some()
        },
    );
    app
}

/// #366's write side end to end against a fake `gh`: threads load with
/// their GraphQL resolution, a reply and a resolve go to GitHub off the
/// frame loop, and a pending comment is submitted with the chosen verdict.
#[test]
fn review_threads_can_be_replied_to_resolved_and_a_review_submitted() {
    let (_tmp, root, f, log, stdin, gh) = review_fixture();
    let mut app = review_app(&root, &f, &gh);
    app.load_review_threads();
    crate::test_budget::await_spawned(std::time::Duration::from_secs(5), "the threads", || {
        app.drain_review_ops();
        app.review_boxes.is_some()
    });
    let threads = app.review_boxes.as_ref().map(|(_, t)| t.clone()).unwrap();
    assert_eq!(threads.len(), 1);
    assert!(!threads[0].resolved);
    assert_eq!(app.review_nodes.get(&5).map(String::as_str), Some("PRRT_1"));

    let wait_for = |app: &mut App, what: &str, done: &dyn Fn(&App) -> bool| {
        crate::test_budget::await_spawned(std::time::Duration::from_secs(5), what, || {
            app.drain_review_ops();
            done(app)
        });
    };

    app.editor.cursor_row = 0;
    app.toggle_review_thread_resolved();
    wait_for(&mut app, "the resolve", &|a| {
        a.review_boxes.as_ref().is_some_and(|(_, t)| t[0].resolved)
    });

    assert!(app.reply_to_review_thread(5, "done"));
    wait_for(&mut app, "the reply", &|a| {
        a.review_boxes
            .as_ref()
            .is_some_and(|(_, t)| t[0].body.ends_with("you: done"))
    });

    app.editor.cursor_row = 1;
    app.open_review_comment_prompt();
    let rel = app.prompt.as_ref().unwrap().target_dir.clone();
    app.prompt = None;
    app.add_pending_review_comment("add a test", rel);
    app.review_verdict = Some(crate::review_threads::ReviewEvent::RequestChanges);
    app.submit_review(String::from("Close."));
    wait_for(&mut app, "the review", &|a| a.review_pending.is_empty());

    let sent: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&stdin).unwrap()).unwrap();
    assert_eq!(sent["event"], "REQUEST_CHANGES");
    assert_eq!(sent["comments"][0]["path"], "a.rs");
    assert_eq!(sent["comments"][0]["line"], 2);
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(
        calls.contains("pulls/7/comments/5/replies -f body=done"),
        "{calls}"
    );
    assert!(calls.contains("resolveReviewThread"), "{calls}");
}

/// Pending comments belong to the PR they were written on: once the
/// branch's PR changes, they neither post there nor mix with new ones, and
/// a failed export settles none of the notes it carried.
#[test]
fn pending_comments_stay_with_their_pull_request() {
    let (_tmp, root, f, _log, stdin, gh) = review_fixture();
    let mut app = review_app(&root, &f, &gh);
    app.editor.cursor_row = 0;
    app.open_review_comment_prompt();
    let rel = app.prompt.take().unwrap().target_dir;
    app.add_pending_review_comment("on seven", rel.clone());
    assert_eq!(app.review_pending.len(), 1);

    // The branch now has another PR.
    app.review_pr = Some((root.clone(), String::from("8")));
    app.add_pending_review_comment("on eight", rel);
    assert_eq!(app.review_pending.len(), 1, "not mixed in");
    assert!(app.status.contains("PR #7"), "{}", app.status);
    app.submit_review(String::from("LGTM"));
    assert!(!stdin.exists(), "nothing posted to #8");

    // A failed export keeps the navigator notes it carried.
    app.review_pending.clear();
    app.navigator_notes
        .insert(String::from("a.rs"), vec![(1, 1, String::from("n"))]);
    app.drain_review_ops();
    app.review_tx
        .send(crate::review_ops::Outcome::Failed(String::from("nope")))
        .unwrap();
    app.drain_review_ops();
    app.review_tx
        .send(crate::review_ops::Outcome::Submitted {
            inline: 0,
            folded: 0,
            settles: crate::review_ops::Settles::default(),
        })
        .unwrap();
    app.drain_review_ops();
    assert_eq!(app.navigator_notes["a.rs"].len(), 1);
}

/// A failed reply or thread load while a review is being submitted does not
/// end the submission: only the submission's own answer does.
#[test]
fn only_the_submissions_own_failure_ends_it() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.review_submitting = true;
    app.review_tx
        .send(crate::review_ops::Outcome::Failed(String::from(
            "Reply failed",
        )))
        .unwrap();
    app.drain_review_ops();
    assert!(app.review_submitting);
    app.review_tx
        .send(crate::review_ops::Outcome::SubmitFailed(String::from("no")))
        .unwrap();
    app.drain_review_ops();
    assert!(!app.review_submitting);
}

/// #368: the navigator's notes and pending comments are previewed, then
/// posted as one review with the navigator's marked as AI-authored; a note
/// off the diff lands in the summary, and posted notes leave the editor.
#[test]
fn comments_export_to_the_pull_request_after_a_preview() {
    let (_tmp, root, f, log, stdin, gh) = review_fixture();
    let mut app = review_app(&root, &f, &gh);
    app.navigator_notes.insert(
        String::from("a.rs"),
        vec![
            (1, 1, String::from("b is unused")),
            (2, 40, String::from("far")),
        ],
    );
    app.review_pending
        .push(crate::review_threads::PendingComment {
            path: String::from("a.rs"),
            line: 0,
            body: String::from("mine"),
        });
    app.open_export_comments();
    crate::test_budget::await_spawned(std::time::Duration::from_secs(5), "the preview", || {
        app.drain_review_ops();
        app.list_picker.is_some()
    });
    let picker = app.list_picker.as_ref().expect("the preview is shown");
    assert_eq!(
        picker.rows[0].label,
        "Post to PR #7: 2 inline, 1 in the summary"
    );
    assert_eq!(picker.rows.len(), 4);
    // A thread load answering for another PR meanwhile does not redirect
    // the post away from the PR the preview named.
    app.review_pr = Some((root.clone(), String::from("8")));
    app.post_exported_comments();
    crate::test_budget::await_spawned(std::time::Duration::from_secs(5), "the post", || {
        app.drain_review_ops();
        app.navigator_notes.get("a.rs").is_some_and(Vec::is_empty)
    });
    let sent: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&stdin).unwrap()).unwrap();
    assert_eq!(sent["event"], "COMMENT");
    let bodies: Vec<&str> = sent["comments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["body"].as_str().unwrap())
        .collect();
    assert_eq!(bodies, vec!["mine", "[AI, croft navigator] b is unused"]);
    assert!(
        sent["body"]
            .as_str()
            .unwrap()
            .contains("`a.rs:41`: [AI, croft navigator] far")
    );
    assert!(app.review_pending.is_empty());
    let log = std::fs::read_to_string(&log).unwrap();
    assert!(
        log.contains("pulls/7/reviews") && !log.contains("pulls/8/"),
        "{log}"
    );
}

/// #367: a sticky note hangs under its line as a box, follows the line when
/// lines are added above, takes replies, resolves from its ✕, is counted in
/// the Explorer while open, and is reached by F4.
#[test]
fn a_sticky_note_is_a_box_that_follows_its_line_and_takes_replies() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("a.rs");
    std::fs::write(&f, "fn a() {}\nfn b() {}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&f).unwrap();
    app.editor.cursor_row = 1;
    app.open_sticky_note_prompt();
    assert!(matches!(
        app.prompt.as_ref().map(|p| &p.kind),
        Some(PromptKind::StickyNote)
    ));
    app.prompt = None;
    app.add_sticky_note("why b?");

    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|fr| app.render(fr)).unwrap();
    let b = app
        .editor
        .comment_boxes
        .iter()
        .find(|b| b.body == "why b?")
        .cloned()
        .expect("the note is drawn as a box");
    assert_eq!(b.line, 1);
    assert_eq!(
        app.tree.note_counts.get(&f).copied(),
        Some(1),
        "the Explorer counts it"
    );

    // Two lines land above it: the box follows fn b.
    app.editor.lines.insert(0, String::from("// one"));
    app.editor.lines.insert(0, String::from("// two"));
    term.draw(|fr| app.render(fr)).unwrap();
    let moved = app
        .editor
        .comment_boxes
        .iter()
        .find(|x| x.id == b.id)
        .unwrap();
    assert_eq!(moved.line, 3);

    app.editor.cursor_row = 0;
    assert_eq!(app.next_comment_from_caret().map(|(id, _)| id), Some(b.id));

    app.editor.comment_focus = Some(crate::widgets::editor::CommentFocus {
        id: b.id,
        reply: String::from("it is used"),
        cursor: 0,
    });
    app.submit_comment_reply();
    app.ignore_comment_box(b.id);
    term.draw(|fr| app.render(fr)).unwrap();
    let after = app
        .editor
        .comment_boxes
        .iter()
        .find(|x| x.id == b.id)
        .unwrap();
    assert!(after.body.ends_with(": it is used"), "{}", after.body);
    assert!(after.author.ends_with("resolved"));
    assert!(
        app.tree.note_counts.is_empty(),
        "a resolved note isn't counted"
    );

    app.editor.comment_focus = None;
    app.editor.cursor_row = 3;
    app.delete_sticky_note_here();
    term.draw(|fr| app.render(fr)).unwrap();
    assert!(!app.editor.comment_boxes.iter().any(|x| x.id == b.id));
}

/// An App on a 120-line `mod.py` with one note on line 113 (row 112), drawn
/// once so the note has its box.
fn app_with_a_note_far_down() -> (
    tempfile::TempDir,
    App,
    ratatui::Terminal<ratatui::backend::TestBackend>,
) {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("mod.py");
    let src: String = (0..60)
        .map(|i| format!("def f{i}(x):\n    return x + {i}\n"))
        .collect();
    std::fs::write(&f, src).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&f).unwrap();
    app.editor.cursor_row = 112;
    app.add_sticky_note("TODO before release, f56 must handle negative x");
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|fr| app.render(fr)).unwrap();
    (tmp, app, term)
}

fn live_note_bodies(app: &App) -> Vec<String> {
    app.notes.live().map(|n| n.body.clone()).collect()
}

/// #1131: Delete Note with the caret on line 1 must not reach down to a
/// note on line 113 the user can't see.
#[test]
fn delete_note_leaves_a_note_further_down_the_file_alone() {
    let (_tmp, mut app, _term) = app_with_a_note_far_down();
    app.editor.comment_focus = None;
    app.editor.cursor_row = 0;
    app.delete_sticky_note_here();
    assert_eq!(
        live_note_bodies(&app),
        vec!["TODO before release, f56 must handle negative x"],
        "the note on line 113 survives"
    );
    assert_eq!(app.status, "No note on this line");
}

/// The note on the caret's own line still goes, and the status names it.
#[test]
fn delete_note_removes_the_note_on_the_caret_line() {
    let (_tmp, mut app, _term) = app_with_a_note_far_down();
    app.editor.comment_focus = None;
    app.editor.cursor_row = 112;
    app.delete_sticky_note_here();
    assert!(live_note_bodies(&app).is_empty());
    assert_eq!(
        app.status,
        "Note deleted on line 113: TODO before release, f56 must handle"
    );
}

/// A note whose box has focus goes wherever the caret is.
#[test]
fn delete_note_removes_the_focused_note_whatever_the_caret_line() {
    let (_tmp, mut app, _term) = app_with_a_note_far_down();
    let id = app
        .editor
        .comment_boxes
        .iter()
        .find(|b| b.body.starts_with("TODO before release"))
        .map(|b| b.id)
        .expect("the note is drawn as a box");
    app.editor.cursor_row = 0;
    app.editor.comment_focus = Some(crate::widgets::editor::CommentFocus {
        id,
        reply: String::new(),
        cursor: 0,
    });
    app.delete_sticky_note_here();
    assert!(live_note_bodies(&app).is_empty());
    assert!(
        app.status.starts_with("Note deleted on line 113"),
        "{}",
        app.status
    );
}

/// A caret below every note doesn't wrap round to delete one above it.
#[test]
fn delete_note_below_the_last_note_deletes_nothing() {
    let (_tmp, mut app, _term) = app_with_a_note_far_down();
    app.editor.comment_focus = None;
    app.editor.cursor_row = 119;
    app.delete_sticky_note_here();
    assert_eq!(live_note_bodies(&app).len(), 1);
    assert_eq!(app.status, "No note on this line");
}

/// A comment written while a review is being submitted stays pending, and a
/// second submit while the first runs is refused rather than posted twice.
#[test]
fn a_submit_in_flight_keeps_new_comments_and_refuses_a_second_submit() {
    let (_tmp, root, f, _log, _stdin, gh) = review_fixture();
    let mut app = review_app(&root, &f, &gh);
    app.editor.cursor_row = 0;
    app.open_review_comment_prompt();
    let rel = app.prompt.take().unwrap().target_dir;
    app.add_pending_review_comment("first", rel.clone());
    app.submit_review(String::from("Looks good"));
    assert!(app.review_submitting);
    app.editor.cursor_row = 1;
    app.add_pending_review_comment("written meanwhile", rel);
    app.submit_review(String::from("again"));
    assert!(app.status.contains("already"), "{}", app.status);
    crate::test_budget::await_spawned(std::time::Duration::from_secs(5), "the submit", || {
        app.drain_review_ops();
        !app.review_submitting
    });
    let left: Vec<_> = app.review_pending.iter().map(|c| c.body.clone()).collect();
    assert_eq!(left, vec![String::from("written meanwhile")]);
}

fn pr_screen(app: &mut App) -> String {
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(150, 32)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let buf = term.backend().buffer();
    let mut s = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            s.push_str(buf[(x, y)].symbol());
        }
        s.push('\n');
    }
    s
}

#[test]
fn a_pr_review_tab_lists_files_checks_and_a_summary() {
    // The cache-dir override is process-global; serialize with the
    // other tests that redirect it.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    // #365: the PULL REQUEST tab shows every changed file with +n -m, the
    // checks, and a summary line.
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.open_pr_review(crate::widgets::pr_review::tests::sample(), "o/r#579".into());
        assert!(app.editor.pr_review.is_some(), "the PR tab is active");
        let screen = pr_screen(&mut app);
        for want in [
            "PR #579",
            "feat(sarif): model",
            "src/sarif/view.rs",
            "+1048",
            "0 of 2 viewed",
            "docs",
        ] {
            assert!(screen.contains(want), "{want:?} on screen:\n{screen}");
        }
    });
}

#[test]
fn space_marks_a_file_viewed_and_it_persists() {
    // The cache-dir override is process-global; serialize with the
    // other tests that redirect it.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.open_pr_review(crate::widgets::pr_review::tests::sample(), "o/r#579".into());
        app.handle_pr_review_key(key(KeyCode::Char(' '), KeyModifiers::NONE));
        let view = app.editor.pr_review.as_ref().unwrap();
        assert!(view.viewed.contains("src/sarif/view.rs"));
        assert!(pr_screen(&mut app).contains("1 of 2 viewed"));
        // A new session sees the mark.
        let mut again = App::new(tmp.path().to_path_buf()).unwrap();
        again.open_pr_review(crate::widgets::pr_review::tests::sample(), "o/r#579".into());
        assert!(
            again
                .editor
                .pr_review
                .as_ref()
                .unwrap()
                .viewed
                .contains("src/sarif/view.rs"),
            "viewed marks persist"
        );
    });
}

#[test]
fn review_pull_request_is_a_palette_command_with_a_number_prompt() {
    use crate::widgets::command_palette::Command;
    assert_eq!(
        Command::from_id("review_pull_request"),
        Some(Command::ReviewPullRequest)
    );
    assert_eq!(
        Command::ReviewPullRequest.title(),
        "Source Control: Review Pull Request"
    );
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.submit_pr_number("not a number");
    assert!(
        app.status.contains("not a pull request number"),
        "{}",
        app.status
    );
}

/// #365: the review tab's `gh` calls run off the UI thread, and a pasted
/// URL from another repository opens that repository's pull request.
#[test]
fn a_pr_review_loads_off_the_ui_thread_and_a_url_keeps_its_repository() {
    use std::os::unix::fs::PermissionsExt;
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("gh-args");
        let gh = tmp.path().join("gh");
        std::fs::write(
            &gh,
            format!(
                r#"#!/bin/sh
echo "$@" >> '{}'
sleep 0.3
case "$2" in
  view) echo '{{"number": 42, "title": "t", "url": "https://github.com/x/y/pull/42", "author": {{"login": "a"}}, "files": [{{"path": "a.rs", "additions": 1, "deletions": 0, "changeType": "ADDED"}}], "statusCheckRollup": []}}' ;;
  diff) printf 'diff --git a/a.rs b/a.rs\nnew file mode 100644\n--- /dev/null\n+++ b/a.rs\n@@ -0,0 +1 @@\n+fn a() {{}}\n' ;;
esac
"#,
                log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.review_gh = gh.display().to_string();

        let started = std::time::Instant::now();
        app.submit_pr_number("https://github.com/x/y/pull/42/files");
        assert!(
            started.elapsed() < std::time::Duration::from_millis(250),
            "the prompt returns before gh does"
        );
        assert!(app.status.contains("Loading PR"), "{}", app.status);
        crate::test_budget::await_spawned(std::time::Duration::from_secs(5), "gh pr view", || {
            app.poll_pr_gh();
            app.editor.pr_review.is_some()
        });
        assert_eq!(app.editor.pr_review.as_ref().unwrap().pr.number, 42);

        app.handle_pr_review_key(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.status.contains("Fetching the diff"), "{}", app.status);
        crate::test_budget::await_spawned(std::time::Duration::from_secs(5), "gh pr diff", || {
            app.poll_pr_gh();
            app.editor.diff.is_some()
        });
        let args = std::fs::read_to_string(&log).unwrap();
        // The viewed-state read (`gh api graphql`) runs beside these.
        let lines: Vec<&str> = args.lines().filter(|l| l.starts_with("pr ")).collect();
        assert!(
            lines[0].starts_with("pr view https://github.com/x/y/pull/42 --json"),
            "the URL, not a bare 42 this repository would resolve: {args}"
        );
        assert_eq!(lines[1], "pr diff https://github.com/x/y/pull/42");
    });
}

/// #917: a PR review that cannot load because gh is missing or signed out
/// says how to fix that, not "No such file or directory (os error 2)".
#[test]
fn a_pr_review_without_a_usable_gh_says_how_to_fix_it() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let signed_out = tmp.path().join("signed-out-gh");
    std::fs::write(&signed_out, "#!/bin/sh\necho 'gh auth login' >&2\nexit 4\n").unwrap();
    std::fs::set_permissions(&signed_out, std::fs::Permissions::from_mode(0o755)).unwrap();
    for (gh, why) in [
        (tmp.path().join("gh"), crate::gh::MISSING),
        (signed_out, crate::gh::SIGNED_OUT),
    ] {
        app.review_gh = gh.display().to_string();
        app.submit_pr_number("42");
        crate::test_budget::await_spawned(std::time::Duration::from_secs(5), "gh pr view", || {
            app.poll_pr_gh();
            app.pr_gh.is_none()
        });
        assert_eq!(app.status, format!("Could not load PR 42: {why}"));
    }
}

/// #917: Review: Add Comment and Submit Review look the PR up on the UI
/// thread; a missing or signed-out gh says how to fix it there too, not
/// that the branch has no PR.
#[test]
fn the_review_comment_pr_lookup_without_a_usable_gh_says_how_to_fix_it() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let signed_out = tmp.path().join("signed-out-gh");
    std::fs::write(&signed_out, "#!/bin/sh\nexit 4\n").unwrap();
    std::fs::set_permissions(&signed_out, std::fs::Permissions::from_mode(0o755)).unwrap();
    let no_pr = tmp.path().join("no-pr-gh");
    std::fs::write(&no_pr, "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(&no_pr, std::fs::Permissions::from_mode(0o755)).unwrap();
    for (gh, status) in [
        (
            tmp.path().join("gh"),
            format!("Could not find this branch's PR: {}", crate::gh::MISSING),
        ),
        (
            signed_out,
            format!("Could not find this branch's PR: {}", crate::gh::SIGNED_OUT),
        ),
        (
            no_pr,
            String::from("No PR for this branch — check it out first"),
        ),
    ] {
        app.review_gh = gh.display().to_string();
        assert_eq!(app.review_pr_number(tmp.path()), None);
        assert_eq!(app.status, status);
    }
}

/// Load x/y#42 through `gh` and wait for its viewed-state read to settle.
fn open_synced_pr(app: &mut App) {
    app.submit_pr_number("42");
    crate::test_budget::await_spawned(std::time::Duration::from_secs(5), "gh pr view", || {
        app.poll_pr_gh();
        app.editor.pr_review.is_some() && app.pr_viewed_fetch.is_none()
    });
}

/// #365: GitHub's viewed state (paged) is the truth when it loads: VIEWED
/// ticks, DISMISSED (changed since viewed) and UNVIEWED untick even a
/// local mark, and the local store follows. Toggling sends the matching
/// mutation; a failed one keeps the local mark and says so once.
#[test]
fn pr_viewed_state_syncs_with_github() {
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let page = |nodes: &str, next: &str| {
            format!(
                r#"echo '{{"data":{{"repository":{{"pullRequest":{{"id":"PR_kw42","files":{{"nodes":[{nodes}],"pageInfo":{next}}}}}}}}}}}'"#
            )
        };
        let graphql = format!(
            "  *unmarkFileAsViewed*) echo '{{}}' ;;\n  *markFileAsViewed*) echo 'Resource not accessible' >&2; exit 1 ;;\n  *after=CUR1*) {} ;;\n  *graphql*) {} ;;",
            page(
                r#"{"path":"c.rs","viewerViewedState":"UNVIEWED"}"#,
                r#"{"hasNextPage":false,"endCursor":"CUR2"}"#
            ),
            page(
                r#"{"path":"a.rs","viewerViewedState":"VIEWED"},{"path":"b.rs","viewerViewedState":"DISMISSED"}"#,
                r#"{"hasNextPage":true,"endCursor":"CUR1"}"#
            ),
        );
        let (gh, log) = viewed_sync_gh(tmp.path(), &graphql);
        // Marked locally before; GitHub says otherwise for both.
        let store_path = App::pr_viewed_path();
        let mut store = crate::pr_review::ViewedStore::default();
        store.set("x/y#42", "b.rs", true);
        store.set("x/y#42", "c.rs", true);
        store.save(&store_path).unwrap();

        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.review_gh = gh.display().to_string();
        open_synced_pr(&mut app);
        let view = app.editor.pr_review.as_ref().unwrap();
        assert_eq!(view.github_id.as_deref(), Some("PR_kw42"));
        assert_eq!(
            view.viewed.iter().map(String::as_str).collect::<Vec<_>>(),
            vec!["a.rs"],
            "both pages applied, GitHub wins"
        );
        assert!(pr_screen(&mut app).contains("1 of 3 viewed"));
        let stored = crate::pr_review::ViewedStore::load(&store_path).viewed("x/y#42");
        assert_eq!(
            stored.iter().map(String::as_str).collect::<Vec<_>>(),
            vec!["a.rs"],
            "the local store follows GitHub"
        );

        // a.rs is selected and viewed: Space unticks it at once and sends
        // unmarkFileAsViewed, which succeeds.
        app.status.clear();
        app.handle_pr_review_key(key(KeyCode::Char(' '), KeyModifiers::NONE));
        assert!(
            !app.editor
                .pr_review
                .as_ref()
                .unwrap()
                .viewed
                .contains("a.rs")
        );
        settle_viewed_writes(&mut app);
        assert!(app.status.is_empty(), "{}", app.status);
        let args = std::fs::read_to_string(&log).unwrap();
        let unmark = args.lines().last().unwrap();
        assert!(
            unmark.starts_with("api graphql -f query=mutation(")
                && unmark.contains("unmarkFileAsViewed(input: {pullRequestId: $id, path: $path})")
                && unmark.ends_with("-f id=PR_kw42 -f path=a.rs"),
            "{unmark}"
        );

        // b.rs: marking fails on GitHub; the mark stays, locally too, and
        // the reason shows once.
        app.handle_pr_review_key(key(KeyCode::Char(' '), KeyModifiers::NONE));
        assert!(
            app.editor
                .pr_review
                .as_ref()
                .unwrap()
                .viewed
                .contains("b.rs")
        );
        settle_viewed_writes(&mut app);
        assert_eq!(
            app.status,
            "Viewed state saved locally; GitHub could not be updated: Resource not accessible"
        );
        assert!(
            crate::pr_review::ViewedStore::load(&store_path)
                .viewed("x/y#42")
                .contains("b.rs")
        );
        // c.rs fails too, quietly this time.
        app.status.clear();
        app.handle_pr_review_key(key(KeyCode::Char(' '), KeyModifiers::NONE));
        settle_viewed_writes(&mut app);
        assert!(app.status.is_empty(), "said once: {}", app.status);
        assert!(
            app.editor
                .pr_review
                .as_ref()
                .unwrap()
                .viewed
                .contains("c.rs")
        );
        let args = std::fs::read_to_string(&log).unwrap();
        assert!(
            args.lines()
                .last()
                .unwrap()
                .ends_with("-f id=PR_kw42 -f path=c.rs"),
            "{args}"
        );
    });
}

/// #365: when gh cannot read the viewed state (signed out, not GitHub),
/// review behaves as before: local marks, no error, no mutations.
#[test]
fn pr_viewed_state_stays_local_without_github() {
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let (gh, log) = viewed_sync_gh(
            tmp.path(),
            "  *graphql*) echo 'gh auth login required' >&2; exit 4 ;;",
        );
        let store_path = App::pr_viewed_path();
        let mut store = crate::pr_review::ViewedStore::default();
        store.set("x/y#42", "b.rs", true);
        store.save(&store_path).unwrap();

        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.review_gh = gh.display().to_string();
        open_synced_pr(&mut app);
        assert_eq!(app.status, "Reviewing PR #42", "no error spam");
        let view = app.editor.pr_review.as_ref().unwrap();
        assert_eq!(view.github_id, None);
        assert!(view.viewed.contains("b.rs"), "local marks as before");
        app.handle_pr_review_key(key(KeyCode::Char(' '), KeyModifiers::NONE));
        settle_viewed_writes(&mut app);
        assert!(
            app.editor
                .pr_review
                .as_ref()
                .unwrap()
                .viewed
                .contains("a.rs")
        );
        assert!(
            crate::pr_review::ViewedStore::load(&store_path)
                .viewed("x/y#42")
                .contains("a.rs")
        );
        assert_eq!(app.status, "Reviewing PR #42");
        let args = std::fs::read_to_string(&log).unwrap();
        assert!(!args.contains("FileAsViewed"), "no mutation: {args}");
    });
}

/// #365: a file toggled while GitHub's viewed state is still loading keeps
/// the person's mark, and it is sent once the PR's id is known.
#[test]
fn a_toggle_before_github_answers_wins_and_is_sent() {
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let (gh, log) = viewed_sync_gh(tmp.path(), "  *FileAsViewed*) echo '{}' ;;");
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.review_gh = gh.display().to_string();
        app.open_pr_review(
            crate::pr_review::parse_pr(
                r#"{"number": 42, "url": "https://github.com/x/y/pull/42", "files": [{"path": "a.rs"}]}"#,
            )
            .unwrap(),
            String::from("x/y#42"),
        );
        // A read in flight that has not answered yet.
        let (tx, rx) = std::sync::mpsc::channel();
        app.pr_viewed_fetch = Some((String::from("x/y#42"), app.pr_viewed_seq, rx));
        app.handle_pr_review_key(key(KeyCode::Char(' '), KeyModifiers::NONE));
        let mut states = std::collections::BTreeMap::new();
        states.insert(
            String::from("a.rs"),
            crate::pr_review::ViewedState::Unviewed,
        );
        tx.send(Ok((String::from("PR_kw42"), states))).unwrap();
        settle_viewed_writes(&mut app);
        let view = app.editor.pr_review.as_ref().unwrap();
        assert!(view.viewed.contains("a.rs"), "the click is newer");
        assert!(view.unsynced.is_empty());
        let args = std::fs::read_to_string(&log).unwrap();
        assert!(
            args.contains(" markFileAsViewed(") && args.ends_with("-f id=PR_kw42 -f path=a.rs\n"),
            "{args}"
        );
    });
}

/// #365: an open review tab whose checks are still running re-reads them
/// in the background, and says when the last one finishes.
#[test]
fn running_pr_checks_refresh_themselves_until_they_finish() {
    use std::os::unix::fs::PermissionsExt;
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let gh = tmp.path().join("gh");
        std::fs::write(
            &gh,
            r#"#!/bin/sh
echo '{"number": 42, "title": "t", "url": "https://github.com/x/y/pull/42", "author": {"login": "a"}, "files": [], "statusCheckRollup": [{"__typename": "CheckRun", "name": "build", "status": "COMPLETED", "conclusion": "SUCCESS"}]}'
"#,
        )
        .unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.review_gh = gh.display().to_string();
        let mut pr = crate::pr_review::parse_pr(
            r#"{"number": 42, "title": "t", "url": "https://github.com/x/y/pull/42", "author": {"login": "a"}, "files": [], "statusCheckRollup": [{"__typename": "CheckRun", "name": "build", "status": "IN_PROGRESS", "conclusion": ""}]}"#,
        )
        .unwrap();
        pr.number = 42;
        app.open_pr_review(pr, String::from("x/y#42"));
        assert!(app.editor.pr_review.as_ref().unwrap().has_pending_checks());
        // The first pass only starts the clock; nothing is fetched yet.
        app.poll_pr_gh();
        assert!(app.pr_gh.is_none());
        app.pr_checks_polled = Some(std::time::Instant::now() - std::time::Duration::from_secs(31));
        // A due refresh runs gh. Not asserted through `pr_gh` straight after
        // the poll: that poll also reads the answer, and under load the stub
        // can answer before it does, leaving `pr_gh` already empty again.
        // The checks turning from running to passed, below, is what only a
        // gh run can do.
        app.poll_pr_gh();
        crate::test_budget::await_spawned(std::time::Duration::from_secs(5), "the refresh", || {
            app.poll_pr_gh();
            !app.editor.pr_review.as_ref().unwrap().has_pending_checks()
        });
        assert_eq!(
            app.editor.pr_review.as_ref().unwrap().pr.checks[0].state,
            crate::pr_review::CheckState::Pass
        );
        assert!(
            app.status.contains("every check has finished"),
            "{}",
            app.status
        );
        // Nothing pending: no more refreshes.
        app.pr_checks_polled = Some(std::time::Instant::now() - std::time::Duration::from_secs(31));
        app.poll_pr_gh();
        assert!(app.pr_gh.is_none());
    });
}

/// #365: Review Pull Request lists the open PRs to pick from (off the UI
/// thread), picking one opens it by URL, and the last row asks for a
/// number instead. When gh cannot list, the prompt opens directly.
#[test]
fn review_pull_request_picks_from_the_open_prs() {
    use crate::widgets::command_palette::Command;
    use std::os::unix::fs::PermissionsExt;
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("gh-args");
        let gh = tmp.path().join("gh");
        std::fs::write(
            &gh,
            format!(
                r#"#!/bin/sh
echo "$@" >> '{}'
case "$2" in
  list) echo '[{{"number": 9, "title": "Add it", "author": {{"login": "ada"}}, "url": "https://github.com/o/r/pull/9", "isDraft": false}}]' ;;
  *) exit 1 ;;
esac
"#,
                log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.review_gh = gh.display().to_string();
        app.run_command(Command::ReviewPullRequest);
        crate::test_budget::await_spawned(std::time::Duration::from_secs(5), "gh pr list", || {
            app.poll_pr_gh();
            app.list_picker.is_some()
        });
        let picker = app.list_picker.as_ref().unwrap();
        assert_eq!(picker.rows.len(), 2);
        assert!(picker.rows[0].label.contains("#9") && picker.rows[0].label.contains("Add it"));
        assert_eq!(picker.rows[1].id, crate::widgets::list_picker::PR_BY_NUMBER);
        app.confirm_list_picker();
        assert!(
            app.status
                .contains("Loading PR https://github.com/o/r/pull/9"),
            "{}",
            app.status
        );
        crate::test_budget::await_spawned(std::time::Duration::from_secs(5), "gh pr view", || {
            app.poll_pr_gh();
            app.pr_gh.is_none()
        });
        let args = std::fs::read_to_string(&log).unwrap();
        assert!(
            args.lines()
                .nth(1)
                .unwrap()
                .starts_with("pr view https://github.com/o/r/pull/9"),
            "{args}"
        );

        // gh unable to list: straight to the prompt, with the reason.
        app.review_gh = String::from("/nonexistent/gh");
        app.run_command(Command::ReviewPullRequest);
        crate::test_budget::await_spawned(
            std::time::Duration::from_secs(5),
            "the fallback",
            || {
                app.poll_pr_gh();
                app.input_prompt.is_some()
            },
        );
        assert!(
            app.status.contains("Could not list pull requests"),
            "{}",
            app.status
        );
    });
}

#[test]
fn review_mode_checks_the_pr_out_and_leaving_removes_a_clean_checkout() {
    // #365: `c` fetches the PR head into a sibling worktree added to the
    // workspace, and Esc takes it away again when nothing was changed.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let git = |dir: &Path, args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        // The "GitHub" repository, at a path ending in the PR's `o/r`, so
        // the remote naming it is found without a network.
        let up = tmp.path().join("o").join("r");
        std::fs::create_dir_all(&up).unwrap();
        git(&up, &["init", "-q", "-b", "main"]);
        git(&up, &["config", "user.email", "a@b"]);
        git(&up, &["config", "user.name", "a"]);
        std::fs::write(up.join("a.txt"), "base").unwrap();
        git(&up, &["add", "."]);
        git(&up, &["commit", "-q", "-m", "base"]);
        std::fs::write(up.join("a.txt"), "pr").unwrap();
        git(&up, &["commit", "-q", "-am", "pr"]);
        let head = git(&up, &["rev-parse", "HEAD"]);
        git(&up, &["update-ref", "refs/pull/579/head", &head]);
        git(&up, &["reset", "-q", "--hard", "HEAD~1"]);
        git(tmp.path(), &["clone", "-q", up.to_str().unwrap(), "app"]);
        let root = tmp.path().join("app").canonicalize().unwrap();

        let mut app = App::new(root.clone()).unwrap();
        app.open_pr_review(crate::widgets::pr_review::tests::sample(), "o/r#579".into());
        app.handle_pr_review_key(key(KeyCode::Char('c'), KeyModifiers::NONE));
        assert!(app.status.contains("Fetching PR #579"), "{}", app.status);
        crate::test_budget::await_spawned(
            std::time::Duration::from_secs(10),
            "PR checkout",
            || {
                app.poll_pr_checkout();
                app.pr_checkout_rx.is_none()
            },
        );
        let checkout = tmp.path().join("app-pr-579").canonicalize().unwrap();
        assert!(app.status.contains("checked out"), "{}", app.status);
        assert!(
            app.roots.iter().any(|r| r == checkout),
            "added to the workspace"
        );
        assert_eq!(
            std::fs::read_to_string(checkout.join("a.txt")).unwrap(),
            "pr"
        );

        app.handle_pr_review_key(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(
            app.status.contains("removed its checkout"),
            "{}",
            app.status
        );
        assert!(!checkout.exists(), "the worktree is gone");
        assert!(!app.roots.iter().any(|r| r == checkout));
        assert!(app.pr_checkout.is_none());

        // Leaving before the fetch lands: nobody owns the result, so it is
        // settled on arrival, never adopted.
        app.open_pr_review(crate::widgets::pr_review::tests::sample(), "o/r#579".into());
        app.handle_pr_review_key(key(KeyCode::Char('c'), KeyModifiers::NONE));
        app.handle_pr_review_key(key(KeyCode::Esc, KeyModifiers::NONE));
        crate::test_budget::await_spawned(
            std::time::Duration::from_secs(10),
            "PR checkout",
            || {
                app.poll_pr_checkout();
                app.pr_checkout_rx.is_none()
            },
        );
        assert!(
            app.status
                .contains("after its review closed; removed its checkout"),
            "{}",
            app.status
        );
        assert!(app.pr_checkout.is_none() && !checkout.exists());
        assert!(!app.roots.iter().any(|r| r == checkout));

        // An unsaved tab under the checkout keeps it.
        app.open_pr_review(crate::widgets::pr_review::tests::sample(), "o/r#579".into());
        app.handle_pr_review_key(key(KeyCode::Char('c'), KeyModifiers::NONE));
        crate::test_budget::await_spawned(
            std::time::Duration::from_secs(10),
            "PR checkout",
            || {
                app.poll_pr_checkout();
                app.pr_checkout_rx.is_none()
            },
        );
        let checkout = tmp.path().join("app-pr-579").canonicalize().unwrap();
        app.editor.open(&checkout.join("a.txt")).unwrap();
        app.editor.dirty = true;
        app.open_pr_review(crate::widgets::pr_review::tests::sample(), "o/r#579".into());
        app.handle_pr_review_key(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(
            app.status.contains("a.txt has unsaved changes"),
            "{}",
            app.status
        );
        assert!(checkout.exists(), "kept");
    });
}
