//! How the editor draws a buffer: minimap, folding, wrap, gutter, scrolling
//! and inline decorations.

use super::*;

#[test]
fn minimap_rgba_paints_chars_skips_whitespace() {
    let mut e = crate::widgets::editor::Editor::new();
    // Two leading spaces then a glyph: indentation must read as bg, the glyph
    // in fg. Second (blank) line keeps the file at 2 rows so each maps to one
    // pixel row in a height-2 canvas.
    e.lines = vec![String::from("  x"), String::new()];
    let bg = (0u8, 0, 0);
    let fg = (200u8, 200, 200);
    let buf = e.minimap_rgba(4, 2, 2, bg, fg);
    let px = |x: u32, y: u32| {
        let i = ((y * 4 + x) * 4) as usize;
        (buf[i], buf[i + 1], buf[i + 2])
    };
    assert_eq!(px(0, 0), bg, "indentation stays background");
    assert_eq!(px(1, 0), bg, "indentation stays background");
    assert_eq!(px(2, 0), fg, "the glyph is painted in the default fg");
}

#[test]
fn minimap_goto_moves_the_cursor() {
    // The editor force-follows the cursor on render, so minimap navigation must
    // move the caret to the target line or the scroll would snap straight back
    // (the bug behind "clicking the minimap does nothing").
    let mut e = crate::widgets::editor::Editor::new();
    e.lines = (0..200).map(|i| format!("line {i}")).collect();
    e.goto_line_centered(100);
    assert_eq!(e.cursor_row, 100, "caret jumps to the clicked line");
    assert!(e.scroll <= 100, "viewport scrolls toward the target");
}

#[test]
fn minimap_short_file_does_not_stretch() {
    // Two lines into a tall (h=10) canvas with content_h=4: the lines occupy
    // the top 4 rows; the rest of the strip stays background (no stretching a
    // short file down the whole column).
    let mut e = crate::widgets::editor::Editor::new();
    e.lines = vec![String::from("x"), String::from("y")];
    let bg = (0u8, 0, 0);
    let fg = (200u8, 200, 200);
    let buf = e.minimap_rgba(1, 10, 4, bg, fg);
    let px = |y: u32| {
        let i = (y * 4) as usize;
        (buf[i], buf[i + 1], buf[i + 2])
    };
    assert_eq!(px(0), fg, "first line sits at the top");
    assert_eq!(
        px(8),
        bg,
        "rows past the content stay background, not stretched"
    );
}

#[test]
fn minimap_viewport_box_lightens_visible_rows_only() {
    // 1px-wide, 4px-tall black raster; viewport = top 2 of 4 lines.
    let base = vec![0u8; 4 * 4];
    let out = composite_minimap_viewport(
        &base,
        1,
        4,
        MinimapOverlay {
            top: 0,
            rows: 2,
            total: 4,
            light: false,
            selection: None,
            sel_rgb: (0, 0, 0),
            content_h: 4,
        },
    );
    let row = |y: usize| out[y * 4]; // R channel
    assert!(row(0) > 0, "a visible row is lightened toward white");
    assert!(row(1) > 0, "a visible row is lightened toward white");
    assert_eq!(row(3), 0, "an off-screen row is untouched");
}

#[test]
fn minimap_selection_band_tints_selected_rows() {
    // 1px-wide, 4px-tall black raster; rows 2..=3 selected in red, viewport is
    // the top row only, so the selection band shows on its own.
    let base = vec![0u8; 4 * 4];
    let out = composite_minimap_viewport(
        &base,
        1,
        4,
        MinimapOverlay {
            top: 0,
            rows: 1,
            total: 4,
            light: false,
            selection: Some((2, 3)),
            sel_rgb: (255, 0, 0),
            content_h: 4,
        },
    );
    let r = |y: usize| out[y * 4];
    assert!(r(2) > 0, "selected row picks up the selection color");
    assert!(r(3) > 0, "selected row picks up the selection color");
}

#[test]
fn minimap_rgba_to_png_roundtrips() {
    let png = rgba_to_png(vec![1u8; 2 * 2 * 4], 2, 2).expect("encode");
    assert_eq!(
        &png[1..4],
        b"PNG",
        "emits a PNG the inline-image path can use"
    );
}

/// #1544 negative: the line number and the text body keep their clicks:
/// they move the caret and set no breakpoint.
#[test]
fn a_click_on_the_line_number_or_the_text_moves_the_caret_only() {
    let (mut app, _tmp, f) = app_with_painted_file("l1\nl2\nl3\nl4\nl5\n");
    let number = app.editor.last_inner.x + 2;
    let text_x = app.editor.last_inner.x + app.editor.last_gutter_width + 1;
    let (row2, row3) = (app.editor.last_inner.y + 2, app.editor.last_inner.y + 3);
    click_and_release(&mut app, number, row2);
    assert_eq!(app.editor.cursor_row, 2, "the number click moves the caret");
    app.editor_click.clear();
    click_and_release(&mut app, text_x + 1, row3);
    assert_eq!(app.editor.cursor_row, 3, "the text click moves the caret");
    assert!(app.editor.breakpoint_lines(&f).is_empty());
}

/// #1544 negative: the fold chevron still folds, and a lane click past the
/// last line or with Shift held sets nothing.
#[test]
fn the_chevron_still_folds_and_off_line_or_shift_lane_clicks_set_nothing() {
    use crossterm::event::{MouseButton, MouseEventKind};
    let (mut app, _tmp, f) = app_with_painted_file("def f():\n    a = 1\n    b = 2\n");
    let x = app.editor.last_inner.x;
    let y = app.editor.last_inner.y;
    click_and_release(&mut app, x + 1, y);
    assert!(app.editor.is_line_hidden(1), "the chevron folds the body");
    assert!(app.editor.breakpoint_lines(&f).is_empty());
    app.editor_click.clear();
    click_and_release(&mut app, x, y + 20);
    assert!(app.editor.breakpoint_lines(&f).is_empty(), "no line there");
    app.editor_click.clear();
    let mut shift = mouse(MouseEventKind::Down(MouseButton::Left), x, y);
    shift.modifiers = KeyModifiers::SHIFT;
    app.handle_mouse(shift);
    assert!(
        app.editor.breakpoint_lines(&f).is_empty(),
        "Shift+click keeps extending the selection"
    );
}

/// #1563: with word wrap on, the edges are the first and last wrapped rows:
/// Down on the last row goes to the end of the line, Up on the first row to
/// its start.
#[test]
fn wrapped_arrows_reach_the_ends_of_the_first_and_last_rows() {
    use ratatui::widgets::Widget as _;
    let (_tmp, mut app) = app_with_run_script();
    let last = app.editor.lines.len() - 1;
    let long = "done # ".to_string() + &"word ".repeat(30);
    app.editor.lines[last] = long.clone();
    app.editor.toggle_wrap();
    assert!(app.editor.wrap_enabled());
    let area = Rect::new(0, 0, 40, 20);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut *app.editor).render(area, &mut buf);

    app.editor.cursor_row = last;
    app.editor.cursor_col = 2;
    for _ in 0..10 {
        app.handle_key(key(KeyCode::Down, KeyModifiers::NONE))
            .unwrap();
    }
    assert_eq!(
        (app.editor.cursor_row, app.editor.cursor_col),
        (last, long.chars().count())
    );

    app.editor.cursor_row = 0;
    app.editor.cursor_col = 6;
    app.handle_key(key(KeyCode::Up, KeyModifiers::NONE))
        .unwrap();
    assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (0, 0));
}

#[test]
fn dragging_editor_horizontal_scrollbar_pans_the_buffer() {
    // The editor paints a horizontal scrollbar on its bottom row when the
    // longest line overflows the text column. Clicking and dragging its thumb
    // must move scroll_col, and releasing must end the drag.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let f = tmp.path().join("long.txt");
    let long: String = std::iter::repeat_n('z', 500).collect();
    std::fs::write(&f, format!("{long}\n")).unwrap();
    app.editor.open(&f).unwrap();
    app.focus_pane(Pane::Editor);

    let backend = ratatui::backend::TestBackend::new(80, 24);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();

    let bar = app.editor.last_hscrollbar;
    assert!(
        bar.width > 0 && bar.height == 1,
        "an overflowing line must paint a horizontal scrollbar"
    );

    // Click-drag the thumb to the far-right end of the track.
    let right_x = bar.x + bar.width - 1;
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: right_x,
        row: bar.y,
        modifiers: KeyModifiers::NONE,
    });
    assert!(
        app.editor.scroll_col > 0,
        "clicking the right of the track must pan right"
    );
    let after_down = app.editor.scroll_col;

    // Dragging back to the left edge pans back to column 0.
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left),
        column: bar.x,
        row: bar.y,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(
        app.editor.scroll_col, 0,
        "dragging to the left edge pans home"
    );
    assert_ne!(after_down, app.editor.scroll_col);

    // Release ends the drag so later moves don't keep panning.
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left),
        column: bar.x,
        row: bar.y,
        modifiers: KeyModifiers::NONE,
    });
    assert!(
        !app.editor_hscrollbar_drag,
        "releasing must clear the drag flag"
    );
}

/// #1352: F7 in an editor tab walks its change bars (line 5, then the
/// deletion boundary at line 9) and wraps; Shift+F7 goes back.
#[test]
fn next_change_in_the_editor_walks_the_change_bars_and_wraps() {
    let (mut app, _t) = app_with_change_bars();
    app.editor.cursor_row = 0;
    let f7 = |app: &mut App, m: KeyModifiers| {
        app.handle_key(key(KeyCode::F(7), m)).unwrap();
        app.editor.cursor_row
    };
    assert_eq!(f7(&mut app, KeyModifiers::NONE), 4);
    assert_eq!(f7(&mut app, KeyModifiers::NONE), 8);
    assert_eq!(f7(&mut app, KeyModifiers::NONE), 4, "wraps to the first");
    assert_eq!(f7(&mut app, KeyModifiers::SHIFT), 8, "Shift+F7 wraps back");
    assert_eq!(f7(&mut app, KeyModifiers::SHIFT), 4);
}

#[test]
fn inline_blame_annotation_paints_on_the_cursor_line() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("m.rs");
    std::fs::write(&f, "fn main() {}\nlet x = 1;\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&f).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.cursor_row = 0;
    app.editor.set_blame(
        f.clone(),
        Some(crate::git::Blame::new(
            vec![
                std::sync::Arc::new(crate::git::BlameCommit {
                    short_hash: "abc12345".into(),
                    summary: "init".into(),
                    author: "Vitali".into(),
                    age_secs: 3600,
                    uncommitted: false,
                }),
                std::sync::Arc::new(crate::git::BlameCommit {
                    short_hash: "abc12345".into(),
                    summary: "add x".into(),
                    author: "Alice".into(),
                    age_secs: 60,
                    uncommitted: false,
                }),
            ],
            None,
        )),
    );
    let backend = ratatui::backend::TestBackend::new(120, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|frame| app.render(frame)).unwrap();
    let buf = term.backend().buffer();
    let mut whole = String::new();
    for y in 0..30 {
        for x in 0..120 {
            whole.push_str(buf[(x, y)].symbol());
        }
        whole.push('\n');
    }
    assert!(
        whole.contains("Vitali, 1 hour ago"),
        "the cursor line's blame annotation must render; got:\n{whole}"
    );
    assert!(
        !whole.contains("Alice"),
        "only the cursor line is annotated, not every line"
    );
}

/// `Editor: Toggle Provenance` (#349) flips the lens on the active editor
/// and keeps it across the per-tick sync; off again on a second toggle.
#[test]
fn toggle_provenance_command_flips_the_editor_lens() {
    let mut app = editor_app_with_lines(&["one", "two"]);
    assert!(!app.editor.provenance_overlay, "the lens is off by default");
    app.run_command(crate::widgets::command_palette::Command::ToggleProvenance);
    assert!(app.editor.provenance_overlay, "the command turns it on");
    assert_eq!(app.status, "Provenance: on");
    app.sync_blame();
    assert!(
        app.editor.provenance_overlay,
        "and the tick sync keeps it on"
    );
    app.run_command(crate::widgets::command_palette::Command::ToggleProvenance);
    app.sync_blame();
    assert!(
        !app.editor.provenance_overlay,
        "toggling again turns it off"
    );
}

/// The group-by-seat lens on a Source Control diff (#349, slice four): the
/// command loads the file's persisted map and reports who wrote the lines
/// the change adds, so a review can be read as "the agent's part" and "my
/// part". Driven through the real command, not by setting the flag.
#[test]
fn the_group_by_seat_lens_reports_who_wrote_the_added_lines() {
    use crate::provenance::Seat;
    let tmp = tempfile::tempdir().unwrap();
    let hist = tempfile::tempdir().unwrap();
    let f = tmp.path().join("note.txt");
    // The working tree: one line HEAD already had, two the change adds.
    std::fs::write(&f, "kept\nmine\nnobody's\n").unwrap();
    let mut seats = crate::provenance::Provenance::new();
    seats.record(1..2, Seat::Me);
    crate::history::record_with_seats_in(
        hist.path(),
        &f,
        &std::fs::read(&f).unwrap(),
        1_000,
        &seats,
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.history_root = hist.path().to_path_buf();
    app.focus_pane(Pane::Editor);
    app.editor
        .open_head_diff_with_text(
            std::path::PathBuf::from("note.txt (HEAD)"),
            "kept\n",
            &f,
            true,
        )
        .unwrap();
    assert!(app.editor.diff.is_some(), "staging: a diff view is open");
    assert!(
        !app.editor.diff.as_ref().unwrap().group_by_seat,
        "staging: the lens is off by default"
    );

    app.run_command(crate::widgets::command_palette::Command::DiffToggleGroupBySeat);
    let diff = app.editor.diff.as_ref().unwrap();
    assert!(diff.group_by_seat, "the command turns the lens on");
    assert_eq!(
        diff.seat_summary(),
        "you 1 \u{2022} unrecorded 1",
        "the added lines are reported per seat, the unwatched one as unrecorded"
    );
    assert_eq!(
        app.status, "Diff: group by seat \u{2022} you 1 \u{2022} unrecorded 1",
        "and the status line says the same"
    );

    app.run_command(crate::widgets::command_palette::Command::DiffToggleGroupBySeat);
    assert!(
        !app.editor.diff.as_ref().unwrap().group_by_seat,
        "toggling again turns it off"
    );
}

/// The lens survives a rebuild of the view under it (#349 review): the
/// real refresh re-reads a Source Control diff whose file moved, and the
/// lens must stay on with a map reloaded against the NEW rows, rather than
/// going dark or keeping a map that named lines these rows renumbered.
#[test]
fn the_group_by_seat_lens_survives_a_rebuild_of_the_view() {
    use crate::provenance::Seat;
    let (tmp, f) = repo_with_seed("kept\n");
    let hist = tempfile::tempdir().unwrap();
    std::fs::write(&f, "kept\nmine\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.history_root = hist.path().to_path_buf();
    app.focus_pane(Pane::Editor);
    let record = |millis: u64, seats: &crate::provenance::Provenance| {
        crate::history::record_with_seats_in(
            hist.path(),
            &f,
            &std::fs::read(&f).unwrap(),
            millis,
            seats,
        )
        .unwrap();
    };
    let mut seats = crate::provenance::Provenance::new();
    seats.record(1..2, Seat::Me);
    record(1_000, &seats);
    app.editor
        .open_head_diff_with_text(
            std::path::PathBuf::from("seed.txt (HEAD)"),
            "kept\n",
            &f,
            true,
        )
        .unwrap();
    app.tag_open_diff(crate::widgets::diff::DiffSource::HeadVsWorking {
        root: tmp.path().to_path_buf(),
        rel: String::from("seed.txt"),
    });
    app.run_command(crate::widgets::command_palette::Command::DiffToggleGroupBySeat);
    assert_eq!(
        app.editor.diff.as_ref().unwrap().seat_summary(),
        "you 1",
        "staging: the lens is on and reports the one added line"
    );

    // An agent appends a line; the real refresh re-reads the moved file.
    rewrite_later(&f, "kept\nmine\ntheirs\n");
    let mut seats2 = crate::provenance::Provenance::new();
    seats2.record(1..2, Seat::Me);
    seats2.record(2..3, Seat::Agent(String::from("pane 2")));
    record(500_000, &seats2);
    let no_paths = std::collections::BTreeSet::new();
    assert!(
        app.refresh_open_diff_views(false, &[], &no_paths),
        "staging: the refresh rebuilt the view"
    );
    let diff = app.editor.diff.as_ref().unwrap();
    assert_eq!(
        diff.right_lines.len(),
        3,
        "staging: the rebuilt view shows the new line"
    );
    assert!(
        diff.group_by_seat,
        "the lens stays on across the rebuild rather than going dark"
    );
    assert_eq!(
        diff.seat_summary(),
        "you 1 \u{2022} agent (pane 2) 1",
        "and its map is reloaded against the rebuilt rows: {}",
        diff.seat_summary()
    );
}

/// A map is never joined onto rows it does not describe (#349 review): if
/// the file moved on disk since the view was built, the lens reports
/// nothing rather than crediting the view's stale lines from a newer map.
#[test]
fn the_group_by_seat_lens_refuses_a_map_for_lines_the_view_does_not_show() {
    use crate::provenance::Seat;
    let tmp = tempfile::tempdir().unwrap();
    let hist = tempfile::tempdir().unwrap();
    let f = tmp.path().join("note.txt");
    std::fs::write(&f, "kept\nmine\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.history_root = hist.path().to_path_buf();
    app.focus_pane(Pane::Editor);
    app.editor
        .open_head_diff_with_text(
            std::path::PathBuf::from("note.txt (HEAD)"),
            "kept\n",
            &f,
            true,
        )
        .unwrap();
    // Same LENGTH, different content ("else" for "mine"), so the (mtime,
    // len) stamp gate genuinely cannot see this rewrite: the view still
    // shows the old lines while disk and its snapshot have moved.
    std::fs::write(&f, "kept\nelse\n").unwrap();
    let mut seats = crate::provenance::Provenance::new();
    seats.record(1..2, Seat::Me);
    crate::history::record_with_seats_in(
        hist.path(),
        &f,
        &std::fs::read(&f).unwrap(),
        1_000,
        &seats,
    )
    .unwrap();
    app.run_command(crate::widgets::command_palette::Command::DiffToggleGroupBySeat);
    let diff = app.editor.diff.as_ref().unwrap();
    assert_eq!(
        diff.right_lines,
        vec![String::from("kept"), String::from("mine")],
        "staging: the view still shows the pre-rewrite lines"
    );
    assert_eq!(
        diff.seat_summary(),
        "unrecorded 1",
        "the newer map is refused rather than joined onto lines it never described"
    );
}

/// A lone carriage return is one line to the diff opener (#349 review):
/// the comparison that binds a map to the view's rows has to split the same
/// way, or the lens reports every line unrecorded on such a file.
#[test]
fn the_group_by_seat_lens_handles_a_lone_carriage_return() {
    use crate::provenance::Seat;
    let tmp = tempfile::tempdir().unwrap();
    let hist = tempfile::tempdir().unwrap();
    let f = tmp.path().join("note.txt");
    // "a\rb" is ONE line to `str::lines`, which is how the view is built.
    std::fs::write(&f, "kept\na\rb\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.history_root = hist.path().to_path_buf();
    app.focus_pane(Pane::Editor);
    let mut seats = crate::provenance::Provenance::new();
    seats.record(1..2, Seat::Me);
    crate::history::record_with_seats_in(
        hist.path(),
        &f,
        &std::fs::read(&f).unwrap(),
        1_000,
        &seats,
    )
    .unwrap();
    app.editor
        .open_head_diff_with_text(
            std::path::PathBuf::from("note.txt (HEAD)"),
            "kept\n",
            &f,
            true,
        )
        .unwrap();
    assert_eq!(
        app.editor.diff.as_ref().unwrap().right_lines.len(),
        2,
        "staging: the opener made two lines, the lone CR inside the second"
    );
    app.run_command(crate::widgets::command_palette::Command::DiffToggleGroupBySeat);
    assert_eq!(
        app.editor.diff.as_ref().unwrap().seat_summary(),
        "you 1",
        "the map is applied rather than refused over a splitting difference"
    );
}

/// The lens refuses a diff whose right side is not the working tree (#349):
/// a diff parsed from raw git output numbers its rows by position in that
/// text, so joining a line-keyed seat map onto it would credit lines at
/// random. Refusing is the only safe answer.
#[test]
fn the_group_by_seat_lens_refuses_a_diff_that_is_not_the_working_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("note.txt");
    std::fs::write(&f, "one\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Editor);
    // A Timeline snapshot diff: same opener, left_is_git_head false.
    app.editor
        .open_head_diff_with_text(
            std::path::PathBuf::from("note.txt (snapshot)"),
            "old\n",
            &f,
            false,
        )
        .unwrap();
    app.run_command(crate::widgets::command_palette::Command::DiffToggleGroupBySeat);
    assert!(
        !app.editor.diff.as_ref().unwrap().group_by_seat,
        "the lens stays off on a diff it cannot key"
    );
    assert!(
        app.status.contains("working tree"),
        "and says why: {}",
        app.status
    );
}

/// Provenance survives a restart (#349, criterion 2): a save records who
/// typed each line beside the history snapshot, and a fresh app that opens
/// the unchanged file gets the map back. Distinguishable seats per line, or
/// a seat on the wrong line would be invisible.
#[test]
fn provenance_survives_a_restart_when_the_file_is_unchanged() {
    use crate::provenance::Seat;
    let tmp = tempfile::tempdir().unwrap();
    let hist = tempfile::tempdir().unwrap();
    let f = tmp.path().join("note.txt");
    std::fs::write(&f, "one\ntwo\nthree\n").unwrap();
    {
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.history_root = hist.path().to_path_buf();
        app.editor.open(&f).unwrap();
        app.focus_pane(Pane::Editor);
        // Recorded directly rather than typed: a typed newline splits the
        // line below and credits it to the typist (the shipped rule), and
        // this test needs one line nobody touched.
        app.editor.provenance.record(0..1, Seat::Navigator);
        app.editor
            .provenance
            .record(1..2, Seat::Agent(String::from("pane 2")));
        app.editor.dirty = true;
        app.save();
        let snaps = wait_for_snapshots(&app.history_root, &f);
        assert_eq!(snaps.len(), 1, "staging: the save recorded a snapshot");
        assert_eq!(
            std::fs::read(&snaps[0].file).unwrap(),
            std::fs::read(&f).unwrap(),
            "staging: the snapshot holds the saved bytes"
        );
        // The sidecar lands after the snapshot, on the same thread; wait for
        // it rather than for the snapshot it follows.
        wait_for_seats(&app.history_root, &f, snaps[0].millis);
    }
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.history_root = hist.path().to_path_buf();
    app.editor.open(&f).unwrap();
    assert_eq!(
        app.editor.provenance.attributed(),
        0,
        "staging: a fresh open knows nothing yet"
    );
    app.sync_provenance();
    assert_eq!(
        app.editor.provenance.seat(0),
        Some(&Seat::Navigator),
        "the navigator's line comes back: {:?}",
        app.editor.provenance
    );
    assert_eq!(
        app.editor.provenance.seat(1),
        Some(&Seat::Agent(String::from("pane 2"))),
        "and the agent's: {:?}",
        app.editor.provenance
    );
    assert_eq!(
        app.editor.provenance.seat(2),
        None,
        "the untouched line stays unknown"
    );
}

/// A whole-buffer swap under the same path clears the map and is reconsidered
/// on its own (#349 review): the reload behind an FS-sync sweep or a
/// disk-conflict popup, a revert, "Reopen as Text" and a re-decode all go
/// through an opener, and none of them has to reset anything by hand. Driven
/// through the openers themselves; the marker is never touched.
#[test]
fn provenance_comes_back_after_every_same_path_swap() {
    use crate::provenance::Seat;
    let tmp = tempfile::tempdir().unwrap();
    let hist = tempfile::tempdir().unwrap();
    let f = tmp.path().join("note.txt");
    std::fs::write(&f, "one\ntwo\n").unwrap();
    let mut seats = crate::provenance::Provenance::new();
    seats.record(0..1, Seat::Navigator);
    crate::history::record_with_seats_in(
        hist.path(),
        &f,
        &std::fs::read(&f).unwrap(),
        1_000,
        &seats,
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.history_root = hist.path().to_path_buf();
    app.editor.open(&f).unwrap();
    app.sync_provenance();
    assert_eq!(
        app.editor.provenance, seats,
        "staging: the first sync restores"
    );

    // The revert every reload path ends in.
    app.editor.revert_to_disk().unwrap();
    assert_eq!(
        app.editor.provenance.attributed(),
        0,
        "staging: the revert cleared the map"
    );
    app.sync_provenance();
    assert_eq!(
        app.editor.provenance, seats,
        "the map comes back after a revert of the same path"
    );

    // "Reopen as Text": `open` on the path already open.
    app.editor.open(&f).unwrap();
    assert_eq!(
        app.editor.provenance.attributed(),
        0,
        "staging: the reopen cleared the map"
    );
    app.sync_provenance();
    assert_eq!(
        app.editor.provenance, seats,
        "and after a reopen of the same path"
    );
}

/// Two tabs on one file are two buffers (#349 review): a split opens the
/// same path in a fresh group, whose tab sits at the same `edit_seq` as the
/// first one did when it was synced. The marker is per buffer, so the split
/// pane gets the map too rather than staying blank for the session.
#[test]
fn a_split_of_a_synced_file_gets_the_map_in_the_new_group() {
    use crate::provenance::Seat;
    let tmp = tempfile::tempdir().unwrap();
    let hist = tempfile::tempdir().unwrap();
    let f = tmp.path().join("note.txt");
    std::fs::write(&f, "one\n").unwrap();
    let mut seats = crate::provenance::Provenance::new();
    seats.record(0..1, Seat::Navigator);
    crate::history::record_with_seats_in(
        hist.path(),
        &f,
        &std::fs::read(&f).unwrap(),
        1_000,
        &seats,
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.history_root = hist.path().to_path_buf();
    app.editor.open(&f).unwrap();
    app.focus_pane(Pane::Editor);
    app.sync_provenance();
    assert_eq!(
        app.editor.provenance, seats,
        "staging: the first tab is synced"
    );
    app.split_editor();
    assert_eq!(
        app.editor.path.as_deref(),
        Some(f.as_path()),
        "staging: the split opened the same file in the focused group"
    );
    assert_eq!(
        app.editor.provenance.attributed(),
        0,
        "staging: the new group's tab knows nothing yet"
    );
    app.sync_provenance();
    assert_eq!(
        app.editor.provenance, seats,
        "the split pane's buffer is synced on its own"
    );
}

/// The lens paints in every pane, so the map is restored onto every open
/// buffer (#349 review), not only the focused one: a split whose other
/// group holds the file must not show the lens on with every line blank.
#[test]
fn provenance_is_synced_onto_the_inactive_group_too() {
    use crate::provenance::Seat;
    let tmp = tempfile::tempdir().unwrap();
    let hist = tempfile::tempdir().unwrap();
    let f = tmp.path().join("note.txt");
    std::fs::write(&f, "one\n").unwrap();
    let mut seats = crate::provenance::Provenance::new();
    seats.record(0..1, Seat::Navigator);
    crate::history::record_with_seats_in(
        hist.path(),
        &f,
        &std::fs::read(&f).unwrap(),
        1_000,
        &seats,
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.history_root = hist.path().to_path_buf();
    app.editor.open(&f).unwrap();
    app.focus_pane(Pane::Editor);
    // Split before any sync: both groups hold the file, neither knows its map.
    app.split_editor();
    let inactive = app.editor_layout.inactive_groups();
    assert_eq!(inactive.len(), 1, "staging: one inactive group");
    assert_eq!(
        inactive[0].path.as_deref(),
        Some(f.as_path()),
        "staging: the inactive group holds the file"
    );
    app.sync_provenance();
    assert_eq!(
        app.editor.provenance, seats,
        "staging: the focused tab is synced"
    );
    assert_eq!(
        app.editor_layout.inactive_groups()[0].provenance,
        seats,
        "the inactive group's tab is synced too"
    );
}

/// A save records the saved tab's own map (#349 review): with the same file
/// open in two groups, an auto save of the inactive group's buffer must not
/// persist the focused group's map against bytes it never described.
#[test]
fn an_inactive_group_save_records_the_map_of_that_buffer() {
    use crate::provenance::Seat;
    let tmp = tempfile::tempdir().unwrap();
    let hist = tempfile::tempdir().unwrap();
    let f = tmp.path().join("note.txt");
    std::fs::write(&f, "one\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.history_root = hist.path().to_path_buf();
    app.editor.open(&f).unwrap();
    app.focus_pane(Pane::Editor);
    app.split_editor();
    // The focused group's buffer: clean, and wearing a map of its own.
    app.editor
        .provenance
        .record(0..1, Seat::Peer(String::from("ana")));
    // The inactive group's buffer: the navigator types, so it is dirty and
    // due, and its map says navigator.
    {
        let mut groups = app.editor_layout.inactive_groups_mut();
        let other: &mut crate::widgets::editor::Editor = &mut *groups[0];
        assert_eq!(
            other.path.as_deref(),
            Some(f.as_path()),
            "staging: same file"
        );
        other.cursor_row = 0;
        other.cursor_col = 0;
        other.insert_str_as("nav\n", Seat::Navigator);
        assert!(other.dirty, "staging: the inactive buffer is dirty");
        age_last_edit(other);
    }
    app.auto_save = true;
    assert!(
        app.tick_auto_save(),
        "staging: the sweep saved the dirty buffer"
    );
    assert!(
        std::fs::read_to_string(&f).unwrap().starts_with("nav\n"),
        "staging: the inactive buffer's text is what hit the disk"
    );
    let snaps = wait_for_snapshots(&app.history_root, &f);
    assert_eq!(snaps.len(), 1, "staging: the save recorded a snapshot");
    wait_for_seats(&app.history_root, &f, snaps[0].millis);
    let recorded = crate::history::seats_for(&app.history_root, &f, snaps[0].millis).unwrap();
    assert_eq!(
        recorded.seat(0),
        Some(&Seat::Navigator),
        "the sidecar holds the saved buffer's map, not the focused tab's: {recorded:?}"
    );
}

/// A clean buffer can still be stale (#349 review): when one tab of a split
/// saves, the other keeps the pre-save text and its own disk stamp. The
/// newest snapshot then matches disk while describing text that tab never
/// held, so the map must NOT be applied to it.
#[test]
fn a_stale_tab_is_not_credited_with_the_map_of_the_saving_tab() {
    use crate::provenance::Seat;
    let tmp = tempfile::tempdir().unwrap();
    let hist = tempfile::tempdir().unwrap();
    let f = tmp.path().join("note.txt");
    std::fs::write(&f, "one\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.history_root = hist.path().to_path_buf();
    app.editor.open(&f).unwrap();
    app.focus_pane(Pane::Editor);
    app.split_editor();
    // The inactive group types and saves; the focused tab is untouched, so
    // it stays clean, unattributed, and holding the pre-save text.
    {
        let mut groups = app.editor_layout.inactive_groups_mut();
        let other: &mut crate::widgets::editor::Editor = &mut *groups[0];
        other.cursor_row = 0;
        other.cursor_col = 0;
        other.insert_str_as("nav\n", Seat::Navigator);
        age_last_edit(other);
    }
    app.auto_save = true;
    assert!(
        app.tick_auto_save(),
        "staging: the sweep saved the other buffer"
    );
    let snaps = wait_for_snapshots(&app.history_root, &f);
    wait_for_seats(&app.history_root, &f, snaps[0].millis);
    // Positive control: there IS a map beside a snapshot matching disk, so a
    // sync that ignored staleness would have something to apply.
    assert_eq!(
        std::fs::read(&snaps[0].file).unwrap(),
        std::fs::read(&f).unwrap(),
        "staging: the newest snapshot matches the file on disk"
    );
    assert!(!app.editor.dirty, "staging: the focused tab is clean");
    assert_eq!(
        app.editor.lines,
        vec![String::from("one")],
        "staging: and still holds the pre-save text"
    );
    app.sync_provenance();
    assert_eq!(
        app.editor.provenance.attributed(),
        0,
        "a stale buffer is not credited from a snapshot of text it never held: {:?}",
        app.editor.provenance
    );
}

/// An empty file gets its map back too (#349 review): `open` gives an empty
/// buffer one sentinel line, so a comparison against the snapshot's bare
/// (zero-line) split would withhold the map from every emptied file.
#[test]
fn the_map_of_an_emptied_file_survives_a_restart() {
    use crate::provenance::Seat;
    let tmp = tempfile::tempdir().unwrap();
    let hist = tempfile::tempdir().unwrap();
    let f = tmp.path().join("note.txt");
    std::fs::write(&f, "").unwrap();
    let mut seats = crate::provenance::Provenance::new();
    seats.record(0..1, Seat::Navigator);
    crate::history::record_with_seats_in(hist.path(), &f, b"", 1_000, &seats).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.history_root = hist.path().to_path_buf();
    app.editor.open(&f).unwrap();
    assert_eq!(
        app.editor.lines,
        vec![String::new()],
        "staging: an empty file opens as one empty line"
    );
    app.sync_provenance();
    assert_eq!(
        app.editor.provenance, seats,
        "the emptied file's map comes back: {:?}",
        app.editor.provenance
    );
}

/// A UTF-16 file's map comes back too (#349 review): the snapshot holds the
/// file's raw bytes, so the comparison must decode them the way `open` does,
/// through the BOM, rather than assuming UTF-8.
#[test]
fn the_map_of_a_utf16_file_survives_a_restart() {
    use crate::provenance::Seat;
    let tmp = tempfile::tempdir().unwrap();
    let hist = tempfile::tempdir().unwrap();
    let f = tmp.path().join("note.txt");
    let mut bytes = vec![0xFF, 0xFE];
    for unit in "one
two
"
    .encode_utf16()
    {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    std::fs::write(&f, &bytes).unwrap();
    let mut seats = crate::provenance::Provenance::new();
    seats.record(0..1, Seat::Navigator);
    crate::history::record_with_seats_in(hist.path(), &f, &bytes, 1_000, &seats).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.history_root = hist.path().to_path_buf();
    app.editor.open(&f).unwrap();
    assert_eq!(
        app.editor.lines,
        vec![String::from("one"), String::from("two")],
        "staging: the file opened as UTF-16 text"
    );
    app.sync_provenance();
    assert_eq!(
        app.editor.provenance, seats,
        "the UTF-16 file's map comes back: {:?}",
        app.editor.provenance
    );
}

/// The recorded map is bound to the bytes it describes (#349 review): the
/// worker re-reads the file, so a save of the same path landing in between
/// must not get the earlier buffer's map attached to its bytes. The snapshot
/// is still recorded; only the attribution is withheld.
#[test]
fn the_map_is_not_recorded_against_another_save_s_bytes() {
    use crate::provenance::Seat;
    let tmp = tempfile::tempdir().unwrap();
    let hist = tempfile::tempdir().unwrap();
    let f = tmp.path().join("note.txt");
    std::fs::write(&f, "one\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.history_root = hist.path().to_path_buf();
    app.editor.open(&f).unwrap();
    app.focus_pane(Pane::Editor);
    let mut seats = crate::provenance::Provenance::new();
    seats.record(0..1, Seat::Navigator);
    // The map describes "one", but something else reaches the disk first:
    // exactly the race the worker's re-read opens.
    std::fs::write(&f, "someone else\n").unwrap();
    app.record_history_snapshot_of(&f, seats, Some(b"one\n".to_vec()));
    let snaps = wait_for_snapshots(&app.history_root, &f);
    assert_eq!(snaps.len(), 1, "the snapshot is still recorded");
    assert_eq!(
        std::fs::read(&snaps[0].file).unwrap(),
        b"someone else\n",
        "and holds the bytes that were actually on disk"
    );
    assert_eq!(
        crate::history::seats_for(&app.history_root, &f, snaps[0].millis),
        None,
        "but wears no map, rather than one describing text it does not hold"
    );
    // Positive control: the same call with matching bytes DOES record a map,
    // so the assertion above is about the binding and not about the plumbing.
    let mut seats2 = crate::provenance::Provenance::new();
    seats2.record(0..1, Seat::Peer(String::from("ana")));
    let on_disk = std::fs::read(&f).unwrap();
    app.record_history_snapshot_of(&f, seats2, Some(on_disk));
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(500),
        "the control's sidecar",
        || {
            crate::history::entries_in(&app.history_root, &f)
                .first()
                .is_some_and(|s| {
                    crate::history::seats_for(&app.history_root, &f, s.millis).is_some()
                })
        },
    );
}

/// A tab that is not eligible yet is not the same as one considered (#349
/// review): a sync that lands on a dirty buffer leaves it for a later tick
/// rather than burning the map for the session.
#[test]
fn a_dirty_tab_is_reconsidered_for_provenance_once_it_is_clean() {
    use crate::provenance::Seat;
    let tmp = tempfile::tempdir().unwrap();
    let hist = tempfile::tempdir().unwrap();
    let f = tmp.path().join("note.txt");
    std::fs::write(&f, "one\n").unwrap();
    let mut seats = crate::provenance::Provenance::new();
    seats.record(0..1, Seat::Peer(String::from("ana")));
    crate::history::record_with_seats_in(
        hist.path(),
        &f,
        &std::fs::read(&f).unwrap(),
        1_000,
        &seats,
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.history_root = hist.path().to_path_buf();
    app.editor.open(&f).unwrap();
    app.editor.dirty = true;
    app.sync_provenance();
    assert_eq!(
        app.editor.provenance.attributed(),
        0,
        "staging: a dirty buffer takes no map"
    );
    app.editor.dirty = false;
    app.sync_provenance();
    assert_eq!(
        app.editor.provenance, seats,
        "the same buffer, clean, gets the map on the next tick"
    );
}

/// A persisted map is restored only onto the text it described: a file
/// changed since the snapshot (by anything, croft or not) opens with every
/// line unknown rather than credited from a stale map.
#[test]
fn provenance_is_not_restored_onto_text_the_snapshot_never_saw() {
    use crate::provenance::Seat;
    let tmp = tempfile::tempdir().unwrap();
    let hist = tempfile::tempdir().unwrap();
    let f = tmp.path().join("note.txt");
    std::fs::write(&f, "v1\n").unwrap();
    {
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.history_root = hist.path().to_path_buf();
        app.editor.open(&f).unwrap();
        app.focus_pane(Pane::Editor);
        app.editor.cursor_row = 0;
        app.editor.cursor_col = 0;
        app.editor.insert_str_as("nav\n", Seat::Navigator);
        app.save();
        let snaps = wait_for_snapshots(&app.history_root, &f);
        assert_eq!(snaps.len(), 1, "staging: the save recorded a snapshot");
        // Positive control: there IS a map that a careless restore would apply.
        wait_for_seats(&app.history_root, &f, snaps[0].millis);
    }
    // Edited outside croft after the save: the snapshot's map describes text
    // that is no longer what is on disk.
    let mut on_disk = std::fs::read_to_string(&f).unwrap();
    on_disk.insert_str(0, "someone else\n");
    std::fs::write(&f, on_disk).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.history_root = hist.path().to_path_buf();
    app.editor.open(&f).unwrap();
    app.sync_provenance();
    assert_eq!(
        app.editor.provenance.attributed(),
        0,
        "a changed file is not credited from a stale map: {:?}",
        app.editor.provenance
    );
}

/// The lens reaches every tab of every group, not only the focused editor
/// (#349 review): the gutter bar paints in every pane, so `sync_focus_flags`
/// carries it the way it carries indent guides.
#[test]
fn the_provenance_lens_reaches_every_editor_after_the_flag_sync() {
    let mut app = editor_app_with_lines(&["one", "two"]);
    app.editor
        .push_editor(crate::widgets::editor::Editor::new());
    assert!(app.editor.editors.len() >= 2, "fixture: two tabs");
    // A split puts the first group out of focus; the lens must reach it too.
    app.editor.path = Some(std::path::PathBuf::from("split-anchor.txt"));
    app.split_editor();
    app.run_command(crate::widgets::command_palette::Command::ToggleProvenance);
    app.sync_focus_flags();
    let every_editor_on = app.editor.editors.iter().all(|e| e.provenance_overlay)
        && app
            .editor_layout
            .inactive_groups()
            .iter()
            .flat_map(|g| g.editors.iter())
            .all(|e| e.provenance_overlay);
    assert!(
        every_editor_on,
        "every tab in every group carries the lens after the sync"
    );
    app.run_command(crate::widgets::command_palette::Command::ToggleProvenance);
    app.sync_focus_flags();
    assert!(
        app.editor.editors.iter().all(|e| !e.provenance_overlay),
        "and every tab drops it"
    );
}

#[test]
fn toggle_inline_blame_command_flips_the_editor_flag_and_pref() {
    let mut app = editor_app_with_lines(&["one", "two"]);
    assert!(
        app.inline_blame_enabled,
        "inline blame is on by default (GitLens parity)"
    );
    app.run_command(crate::widgets::command_palette::Command::ToggleInlineBlame);
    assert!(!app.inline_blame_enabled, "the command turns it off");
    // sync_blame propagates the pref onto the active editor each tick.
    app.sync_blame();
    assert!(!app.editor.blame_enabled);
    app.run_command(crate::widgets::command_palette::Command::ToggleInlineBlame);
    assert!(app.inline_blame_enabled, "toggling again turns it back on");
    app.sync_blame();
    assert!(app.editor.blame_enabled);
}

#[test]
fn toggle_indent_guides_command_flips_every_editor_via_the_frame_sync() {
    let mut app = editor_app_with_lines(&["fn a() {", "    x();", "}"]);
    assert!(
        app.indent_guides_enabled,
        "indent guides are on by default (VS Code editor.guides.indentation)"
    );
    app.run_command(crate::widgets::command_palette::Command::ToggleIndentGuides);
    assert!(!app.indent_guides_enabled, "the command turns them off");
    // The per-frame focus sync pushes the flag onto every editor in every
    // group, so tabs opened after the toggle can never resurrect guides.
    app.sync_focus_flags();
    assert!(app.editor.editors.iter().all(|e| !e.show_indent_guides));
    // A tab opened AFTER the toggle starts on the constructor default (true)
    // and must be brought in line by the next frame's sync.
    app.editor
        .open_text_buffer(std::path::Path::new("later.rs"), "fn x() {\n    y();\n}")
        .unwrap();
    assert!(
        app.editor.show_indent_guides,
        "a fresh tab starts on the default, proving the sync below does the work"
    );
    app.sync_focus_flags();
    assert!(
        app.editor.editors.iter().all(|e| !e.show_indent_guides),
        "the frame sync disables guides on the tab opened after the toggle"
    );
    app.run_command(crate::widgets::command_palette::Command::ToggleIndentGuides);
    assert!(app.indent_guides_enabled);
    app.editor
        .open_text_buffer(std::path::Path::new("even-later.rs"), "fn z() {}")
        .unwrap();
    app.sync_focus_flags();
    assert!(
        app.editor.editors.iter().all(|e| e.show_indent_guides),
        "re-enabling reaches tabs opened before and after the toggle"
    );
}

#[test]
fn toggle_bracket_colors_command_flips_every_editor_via_the_frame_sync() {
    let mut app = editor_app_with_lines(&["fn a(x: u8) {}"]);
    assert!(
        app.bracket_colors_enabled,
        "bracket colorization is on by default (VS Code default since 1.67)"
    );
    app.run_command(crate::widgets::command_palette::Command::ToggleBracketColors);
    assert!(!app.bracket_colors_enabled, "the command turns it off");
    app.editor
        .open_text_buffer(std::path::Path::new("later.rs"), "g(1)")
        .unwrap();
    app.sync_focus_flags();
    assert!(
        app.editor.editors.iter().all(|e| !e.show_bracket_colors),
        "the frame sync disables the tab opened after the toggle too"
    );
    app.run_command(crate::widgets::command_palette::Command::ToggleBracketColors);
    app.sync_focus_flags();
    assert!(app.editor.editors.iter().all(|e| e.show_bracket_colors));
}

#[test]
fn toggle_render_whitespace_cycles_modes_across_every_editor() {
    use crate::widgets::editor::WhitespaceMode;
    let mut app = editor_app_with_lines(&["a b"]);
    let all_editors_in = |app: &App, mode: WhitespaceMode| {
        app.editor.editors.iter().all(|e| e.whitespace_mode == mode)
    };
    assert_eq!(
        app.whitespace_mode,
        WhitespaceMode::Selection,
        "selection is the default (VS Code editor.renderWhitespace)"
    );
    app.sync_focus_flags();
    assert!(all_editors_in(&app, WhitespaceMode::Selection));
    app.run_command(crate::widgets::command_palette::Command::ToggleRenderWhitespace);
    assert_eq!(app.whitespace_mode, WhitespaceMode::All);
    app.editor
        .open_text_buffer(std::path::Path::new("later.txt"), "c d")
        .unwrap();
    app.sync_focus_flags();
    assert!(
        all_editors_in(&app, WhitespaceMode::All),
        "the frame sync carries the mode to tabs opened after the toggle"
    );
    app.run_command(crate::widgets::command_palette::Command::ToggleRenderWhitespace);
    assert_eq!(app.whitespace_mode, WhitespaceMode::None);
    app.sync_focus_flags();
    assert!(all_editors_in(&app, WhitespaceMode::None));
    app.run_command(crate::widgets::command_palette::Command::ToggleRenderWhitespace);
    assert_eq!(app.whitespace_mode, WhitespaceMode::Selection, "full cycle");
    app.sync_focus_flags();
    assert!(all_editors_in(&app, WhitespaceMode::Selection));
}

#[test]
fn build_sticky_lines_pins_enclosing_headers_scrolled_above_the_viewport() {
    use crate::lsp::manager::OutlineKind;
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("m.rs");
    std::fs::write(&f, "x\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&f).unwrap();
    app.outline.set_symbols(
        f.clone(),
        vec![
            sym("Widget", OutlineKind::Class, 0, 0, 40),
            sym("build", OutlineKind::Method, 1, 10, 30),
        ],
    );
    // Scrolled deep inside build: both the class and method headers are above.
    app.editor.scroll = 20;
    assert_eq!(app.build_sticky_lines(), vec![0, 10], "outermost first");
    // Scrolled inside the class but above the method: only the class pins.
    app.editor.scroll = 5;
    assert_eq!(app.build_sticky_lines(), vec![0]);
    // Parked on the class header itself: nothing is scrolled off, so no pins.
    app.editor.scroll = 0;
    assert!(app.build_sticky_lines().is_empty());
}

#[test]
fn minimap_edit_rebakes_in_place_but_a_moved_strip_arms_the_clear() {
    // Issue: every keystroke bumped `edit_seq`, the minimap layout signature
    // mismatched, and `update_minimap_overlay` armed the full-screen clear
    // latch - so the whole app blinked on every edit (a 2J + full redraw is
    // one visible round trip over SSH). A content-only change re-emits the
    // raster over the SAME cells (fixed Kitty id / same-cell OSC-1337 emit
    // replaces in place), so only a moved or resized strip may arm the clear.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.cell_pixel = Some((8, 16));
    app.inline_protocol = crate::iterm2_inline::InlineImageProtocol::Kitty;
    app.editor.lines = (0..80).map(|i| format!("line {i}")).collect();
    let strip = ratatui::layout::Rect {
        x: 100,
        y: 1,
        width: 6,
        height: 40,
    };
    app.update_minimap_overlay(strip);
    assert!(app.minimap_image_payload().is_some(), "minimap must bake");
    let _ = app.consume_minimap_image_clear();
    app.mark_minimap_image_displayed();

    app.editor.lines.push(String::from("one more line"));
    app.editor.edit_seq += 1;
    app.update_minimap_overlay(strip);
    assert!(
        !app.consume_minimap_image_clear(),
        "a content-only minimap change must replace the image in place, not blank the screen"
    );
    assert!(
        app.minimap_image_payload().is_some(),
        "the rebaked raster must still be there to re-emit"
    );

    app.mark_minimap_image_displayed();
    let moved = ratatui::layout::Rect { x: 60, ..strip };
    app.update_minimap_overlay(moved);
    assert!(
        app.consume_minimap_image_clear(),
        "a moved minimap strip must arm the eviction clear so no ghost remains"
    );
}

#[test]
fn timestamps_gutter_toggles_on_and_shows_arrival_times() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.terminals[0] = crate::widgets::terminal::PtyTerminal::new_running(
        "/bin/sh",
        &[
            String::from("-c"),
            String::from("g=gutter; echo ${g}-row-9; sleep 30"),
        ],
        tmp.path(),
    )
    .unwrap();
    app.focus_pane(Pane::Terminal);
    crate::test_budget::await_spawned(
        crate::test_budget::tests::SHELL_PAINT_BASE,
        "the pane output \"gutter-row-9\"",
        || {
            app.terminals[0]
                .grid_lines()
                .0
                .iter()
                .any(|l| l.starts_with("gutter-row-9"))
        },
    );
    fn screen_rows(term: &ratatui::Terminal<ratatui::backend::TestBackend>) -> Vec<String> {
        let buf = term.backend().buffer();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().chars().next().unwrap_or(' '))
                    .collect()
            })
            .collect()
    }
    fn gutter_of(rows: &[String], needle: &str) -> Option<String> {
        let row = rows.iter().find(|r| r.contains(needle))?;
        let inner = row.trim_end().trim_end_matches(['│', '┃']);
        Some(
            inner
                .chars()
                .rev()
                .take(8)
                .collect::<String>()
                .chars()
                .rev()
                .collect(),
        )
    }
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();

    // Off by default: the row ends with its own text, no clock.
    term.draw(|f| app.render(f)).unwrap();
    let g = gutter_of(&screen_rows(&term), "gutter-row-9").unwrap_or_default();
    assert!(
        !g.chars().filter(|c| *c == ':').count().eq(&2),
        "no gutter while the toggle is off: {g:?}"
    );

    app.toggle_terminal_timestamps();
    assert!(app.show_terminal_timestamps);
    term.draw(|f| app.render(f)).unwrap();
    let g = gutter_of(&screen_rows(&term), "gutter-row-9").expect("row still on screen");
    let looks_like_clock = g.len() == 8
        && g.as_bytes()[2] == b':'
        && g.as_bytes()[5] == b':'
        && g.chars().filter(|c| c.is_ascii_digit()).count() == 6;
    assert!(
        looks_like_clock,
        "the toggled gutter shows HH:MM:SS at the row's right edge: {g:?}"
    );

    app.toggle_terminal_timestamps();
    term.draw(|f| app.render(f)).unwrap();
    let g = gutter_of(&screen_rows(&term), "gutter-row-9").unwrap_or_default();
    assert!(
        !(g.len() == 8 && g.as_bytes()[2] == b':' && g.as_bytes()[5] == b':'),
        "toggling off removes the gutter: {g:?}"
    );
}

#[test]
fn sticky_command_header_pins_the_command_while_scrolled_into_its_output() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // Full OSC 133 marks around a runtime-assembled command (the pane's
    // `▶ …` run header echoes the script text, so the typed command must
    // never appear verbatim in it), then enough output to scroll.
    let script = "b=build; printf \"\\033]133;A\\007$ \\033]133;B\\007${b}-it-77 now\\n\\033]133;C\\007\"; i=0; while [ $i -lt 80 ]; do echo out-$i; i=$((i+1)); done; printf '\\033]133;D;0\\007'; sleep 30";
    app.terminals[0] = crate::widgets::terminal::PtyTerminal::new_running(
        "/bin/sh",
        &[String::from("-c"), String::from(script)],
        tmp.path(),
    )
    .unwrap();
    app.focus_pane(Pane::Terminal);
    crate::test_budget::await_spawned(
        crate::test_budget::tests::SHELL_PAINT_BASE,
        "the command to finish",
        || !app.terminals[0].command_decorations().is_empty(),
    );
    fn screen_rows(term: &ratatui::Terminal<ratatui::backend::TestBackend>) -> Vec<String> {
        let buf = term.backend().buffer();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().chars().next().unwrap_or(' '))
                    .collect()
            })
            .collect()
    }
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let inner = app.terminals[0].last_inner;

    // At the live bottom nothing pins.
    let rows = screen_rows(&term);
    assert!(
        !rows[inner.y as usize].contains("build-it-77"),
        "no header at the live bottom: {:?}",
        rows[inner.y as usize]
    );

    // Scroll so the viewport's top row sits inside the command's output,
    // with its prompt above the view: the typed command pins to row 0.
    app.terminals[0].scroll_to_top();
    for _ in 0..4 {
        app.terminals[0].scroll_down(1);
    }
    term.draw(|f| app.render(f)).unwrap();
    let rows = screen_rows(&term);
    assert!(
        rows[inner.y as usize].contains("build-it-77 now"),
        "the command must pin to the pane's top row: {:?}",
        rows[inner.y as usize]
    );

    // Back at the live bottom the header unpins.
    app.terminals[0].reset_scrollback();
    term.draw(|f| app.render(f)).unwrap();
    let rows = screen_rows(&term);
    assert!(
        !rows[inner.y as usize].contains("build-it-77"),
        "the header unpins at the live bottom: {:?}",
        rows[inner.y as usize]
    );
}

#[test]
fn the_wheel_still_scrolls_a_long_text_file() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("long.txt");
    let body: String = (0..500).map(|i| format!("line {i}\n")).collect();
    std::fs::write(&path, body).unwrap();

    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&path).unwrap();
    let (col, row) = place_editor_pane(&mut app);

    wheel(&mut app, MouseEventKind::ScrollDown, col, row);
    assert_eq!(app.editor.scroll, 3, "wheel down still scrolls text");
    wheel(&mut app, MouseEventKind::ScrollUp, col, row);
    assert_eq!(app.editor.scroll, 0, "wheel up still scrolls text back");
}

#[test]
fn drag_target_over_the_sticky_band_is_the_pinned_directory() {
    use crossterm::event::{MouseButton, MouseEventKind};
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("outer/inner")).unwrap();
    for i in 0..25 {
        std::fs::write(tmp.path().join(format!("outer/inner/f{i:02}.rs")), "x\n").unwrap();
    }
    std::fs::write(tmp.path().join("loose.txt"), "x\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let outer = app
        .tree
        .nodes
        .iter()
        .position(|n| n.path.ends_with("outer"))
        .unwrap();
    app.tree.selected = outer;
    app.tree.expand_selected();
    let inner = app
        .tree
        .nodes
        .iter()
        .position(|n| n.path.ends_with("inner"))
        .unwrap();
    app.tree.selected = inner;
    app.tree.expand_selected();
    // Scroll deep so the band pins ancestors, selection far below.
    let deep = app
        .tree
        .nodes
        .iter()
        .position(|n| n.path.ends_with("f20.rs"))
        .unwrap();
    app.tree.selected = deep;
    app.tree.scroll = deep.saturating_sub(3);
    let backend = ratatui::backend::TestBackend::new(120, 20);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let (band_y, pinned_idx) = {
        let rows = app.tree.sticky_rows_for_test();
        // The MIDDLE pinned row ("outer") is where the behaviors diverge:
        // the outermost is the workspace root (dropping a root-level file
        // into its own parent is correctly refused), and the deepest
        // ("inner") coincides with the covered rows' parent, where the old
        // sticky-blind resolution accidentally agreed.
        assert!(rows.len() >= 3, "band must pin root/outer/inner here");
        rows[1]
    };
    let loose = tmp.path().join("loose.txt");
    let target = drag_target_index(&app.tree, band_y, std::slice::from_ref(&loose))
        .expect("a pinned directory is a valid drop target");
    assert_eq!(
        target, pinned_idx,
        "the drop target under the band is the PINNED directory, not the covered row"
    );
    let covered = app.tree.node_at_y(band_y);
    assert_ne!(
        covered,
        Some(target),
        "precondition: the band actually covers a different row there"
    );
    let _ = MouseEventKind::Down(MouseButton::Left); // silence unused import paths on some cfgs
}

#[test]
fn fold_all_regions_command_works_with_no_language_server_attached() {
    // #254's fallback criterion: region folding must work when no LSP
    // ever answers — here the file's language has no server at all.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("notes.txt"),
        "#region setup\nline a\nline b\n#endregion\nrest\n",
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor
        .open_pinned(&tmp.path().join("notes.txt"))
        .unwrap();
    app.focus_pane(Pane::Editor);
    app.run_command(crate::widgets::command_palette::Command::FoldAllRegions);
    assert!(app.editor.is_line_hidden(1) && app.editor.is_line_hidden(2));
    assert!(!app.editor.is_line_hidden(0) && !app.editor.is_line_hidden(4));
    app.run_command(crate::widgets::command_palette::Command::UnfoldAllRegions);
    assert!(!app.editor.is_line_hidden(1), "regions expanded again");
}

#[test]
fn expand_selection_falls_back_to_tree_sitter_when_the_lsp_cannot_route() {
    // #254 item 2's serverless criterion, end to end: the worker answers
    // `unsupported` for an untracked doc, and the drained verdict grows
    // the selection from tree-sitter node ancestry — the gesture never
    // hangs and never needs a real server.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("lib.rs"),
        "fn main() {\n    let value = 1;\n}\n",
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&tmp.path().join("lib.rs")).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.cursor_row = 1;
    app.editor.cursor_col = 9; // inside `value`
    app.run_command(crate::widgets::command_palette::Command::ExpandSelection);
    // The request is in flight; the worker's unsupported verdict lands
    // asynchronously. Drain until it resolves the queued press.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while app.editor.selection.is_none() && std::time::Instant::now() < deadline {
        app.drain_lsp_selection_ranges();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let sel = app
        .editor
        .selection
        .expect("the fallback grew a selection")
        .normalised();
    assert!(
        sel.0 <= (1, 8) && sel.1 >= (1, 13),
        "the identifier (or an enclosing node) is selected: {sel:?}"
    );
    // Shrink retraces locally, no LSP involved.
    app.run_command(crate::widgets::command_palette::Command::ShrinkSelection);
    assert!(app.editor.selection.is_none(), "back to the bare caret");
}

#[test]
fn cmd_k_shift_e_runs_the_caret_lines_only_lens() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.rs", "fn main() {}\n");
    app.editor.code_lenses = vec![lens_at(
        0,
        "fn main() {}",
        "Frobnicate",
        "vendor.frobnicate",
    )];
    assert!(app.handle_cmd_k_chord(key(KeyCode::Char('E'), KeyModifiers::SHIFT)));
    assert!(app.status.starts_with("Code lens:"), "{}", app.status);
    app.editor
        .code_lenses
        .push(lens_at(0, "fn main() {}", "Other", "vendor.other"));
    assert!(app.handle_cmd_k_chord(key(KeyCode::Char('e'), KeyModifiers::SHIFT)));
    assert!(
        app.context_menu.is_some(),
        "two lenses open a menu to pick from"
    );
}

/// #682: typing re-bakes the minimap at most every `MINIMAP_EDIT_REBAKE`,
/// and the deferred bake still lands once the wait is over.
#[test]
fn minimap_rebakes_for_typing_at_most_every_few_hundred_ms() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.cell_pixel = Some((8, 16));
    app.inline_protocol = crate::iterm2_inline::InlineImageProtocol::Kitty;
    app.editor.lines = (0..80).map(|i| format!("line {i}")).collect();
    let strip = ratatui::layout::Rect {
        x: 100,
        y: 1,
        width: 6,
        height: 40,
    };
    app.update_minimap_overlay(strip);
    let first = app.minimap_image_payload().map(|(o, _)| o.to_string());
    assert!(first.is_some());
    app.editor.lines[0] = String::from("changed");
    app.editor.edit_seq += 1;
    app.update_minimap_overlay(strip);
    assert_eq!(
        app.minimap_image_payload().map(|(o, _)| o.to_string()),
        first,
        "a keystroke right after a bake waits"
    );
    assert!(!app.tick_minimap(), "not due yet");
    app.minimap_baked_at = Some(std::time::Instant::now() - super::MINIMAP_EDIT_REBAKE);
    assert!(app.tick_minimap(), "the wait is over: redraw");
    assert!(
        !app.tick_minimap(),
        "one redraw per deferred bake, even if the minimap is gone by then"
    );
    app.update_minimap_overlay(strip);
    assert_ne!(
        app.minimap_image_payload().map(|(o, _)| o.to_string()),
        first,
        "the deferred edit is baked"
    );
    assert!(!app.tick_minimap());
    // Scrolling is not held back.
    app.editor.scroll = 5;
    let before = app.minimap_image_payload().map(|(o, _)| o.to_string());
    app.update_minimap_overlay(strip);
    assert_ne!(
        app.minimap_image_payload().map(|(o, _)| o.to_string()),
        before
    );
}

/// #850: a syntax pass that outlives its keystroke lands through the tick,
/// in the focused group and in a split's other group alike, with the colors
/// a whole-buffer pass gives, and the minimap strip re-bakes in them.
#[test]
fn a_syntax_pass_that_outlives_the_keystroke_lands_on_a_later_tick() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "main.rs", "fn main() {\n    let x = 1;\n}\n");
    app.split_editor();
    let path = tmp.path().join("main.rs");
    let type_into = |ed: &mut crate::widgets::editor::Editor| {
        ed.defer_highlights_for_test();
        ed.cursor_row = 1;
        ed.cursor_col = 4;
        ed.insert_str("let y = \"s\"; // (note)\n    ");
        assert!(ed.highlight_pending(), "the keystroke did not wait");
    };
    type_into(&mut app.editor);
    type_into(app.editor_layout.inactive_groups_mut()[0]);
    app.cell_pixel = Some((8, 16));
    app.inline_protocol = crate::iterm2_inline::InlineImageProtocol::Kitty;
    let strip = ratatui::layout::Rect {
        x: 100,
        y: 1,
        width: 6,
        height: 40,
    };
    app.update_minimap_overlay(strip);
    let carried = app.minimap_image_payload().map(|(o, _)| o.to_string());
    assert!(carried.is_some());

    let pending = |app: &mut App| {
        app.editor.highlight_pending() || app.editor_layout.inactive_groups()[0].highlight_pending()
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut landed = false;
    while pending(&mut app) {
        landed |= app.poll_highlights();
        assert!(
            std::time::Instant::now() < deadline,
            "the passes never landed"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(
        landed,
        "the tick reported the landing, so the frame repaints"
    );
    let want = |lines: &[String]| {
        let fresh =
            crate::widgets::editor::Editor::historical(&path, &lines.join("\n"), Vec::new());
        format!("{:?}", fresh.highlights_for_test())
    };
    let inactive = &app.editor_layout.inactive_groups()[0];
    assert_eq!(
        format!("{:?}", inactive.highlights_for_test()),
        want(&inactive.lines),
        "the other group's tab"
    );
    assert_eq!(
        format!("{:?}", app.editor.highlights_for_test()),
        want(&app.editor.lines),
        "the focused tab"
    );

    app.minimap_baked_at = Some(std::time::Instant::now() - super::MINIMAP_EDIT_REBAKE);
    app.update_minimap_overlay(strip);
    assert_ne!(
        app.minimap_image_payload().map(|(o, _)| o.to_string()),
        carried,
        "the strip re-bakes in the landed colors"
    );
}

/// #682: the minimap PNG is compressed hard; a typical strip encodes to a
/// fraction of its default size.
#[test]
fn the_minimap_png_is_small() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.lines = (0..400)
        .map(|i| format!("    let value_{i} = compute(a, b, {i}); // note"))
        .collect();
    let (w, h) = (6 * 9, 45 * 18);
    let rgba = app
        .editor
        .minimap_rgba(w, h, h, (30, 30, 30), (200, 200, 200));
    let png = super::rgba_to_png(rgba, w, h).unwrap();
    assert!(png.len() < 1500, "{} bytes", png.len());
    assert!(image::load_from_memory(&png).is_ok());
}

#[test]
fn home_after_scrolling_right_brings_the_view_back() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, _term) = wide_line_app(tmp.path());
    app.handle_key(key(KeyCode::End, KeyModifiers::NONE))
        .unwrap();
    app.editor.scroll_col = 500;
    app.editor.cursor_col = 600;
    app.handle_key(key(KeyCode::Home, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.editor.cursor_col, 0);
    assert_eq!(app.editor.scroll_col, 0);
}

/// After Reopen with Encoding, the gutter baseline is HEAD decoded the same
/// way, so the added line gets its bar and the old lines none.
#[test]
fn a_windows_1252_file_gets_gutter_marks_after_reopen_with_encoding() {
    let (tmp, f) = encoded_repo("latin1.txt", LATIN1_HEAD, LATIN1_ADDED);
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&f).unwrap();
    app.sync_git_gutters();
    app.editor
        .reopen_with_encoding(encoding_rs::WINDOWS_1252)
        .unwrap();
    app.sync_git_gutters();
    assert_eq!(
        app.editor.git_head_lines,
        Some(crate::packed_lines::PackedLines::from(
            &["name;city", "Müller;Köln", "José;Málaga"][..]
        ))
    );
    draw(&mut app, 100, 30);
    assert_eq!(
        app.editor.git_mark_at(3),
        Some(crate::widgets::editor::GitMark::Added)
    );
    assert_eq!(app.editor.git_mark_at(1), None);
}

/// A UTF-16LE file with a BOM is decoded as UTF-16 on both sides.
#[test]
fn a_utf16_file_gets_gutter_marks() {
    let utf16 = |s: &str| -> Vec<u8> { s.encode_utf16().flat_map(u16::to_le_bytes).collect() };
    let head = [&[0xff, 0xfe][..], &utf16("one\ntwo\n")].concat();
    let (tmp, f) = encoded_repo("app.rc", &head, &utf16("three\n"));
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&f).unwrap();
    app.sync_git_gutters();
    assert_eq!(
        app.editor.git_head_lines,
        Some(crate::packed_lines::PackedLines::from(&["one", "two"][..]))
    );
    draw(&mut app, 100, 30);
    assert_eq!(
        app.editor.git_mark_at(2),
        Some(crate::widgets::editor::GitMark::Added)
    );
}

// --- Minimap without inline images (#1231) --------------------------------

/// An app with a 300-line Rust file open, in a terminal that cannot draw
/// inline images (tmux, Alacritty, GNOME Terminal, plain xterm).
fn text_minimap_app() -> (tempfile::TempDir, App) {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("long.rs");
    let body: String = (0..300)
        .map(|i| format!("fn f{i}() {{ let value = {i}; }}\n"))
        .collect();
    std::fs::write(&path, body).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.inline_protocol = crate::iterm2_inline::InlineImageProtocol::None;
    app.cell_pixel = None;
    app.editor.open(&path).unwrap();
    (tmp, app)
}

fn is_braille(s: &str) -> bool {
    s.chars()
        .next()
        .is_some_and(|c| ('\u{2801}'..='\u{28ff}').contains(&c))
}

/// The braille cells drawn inside `r`.
fn braille_in(term: &ratatui::Terminal<ratatui::backend::TestBackend>, r: Rect) -> usize {
    let buf = term.backend().buffer();
    let mut n = 0;
    for y in r.y..r.y + r.height {
        for x in r.x..r.x + r.width {
            if is_braille(buf[(x, y)].symbol()) {
                n += 1;
            }
        }
    }
    n
}

#[test]
fn the_minimap_draws_as_braille_without_inline_images() {
    let (_tmp, mut app) = text_minimap_app();
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 40)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let strip = app.minimap_img_rect;
    assert_eq!(strip.width, 6, "the strip is carved: {strip:?}");
    assert!(
        braille_in(&term, strip) > 0,
        "the strip shows the file's shape:\n{}",
        screen_text(&term)
    );
    let text = app.editor.last_full_area;
    assert_eq!(
        strip.x,
        text.x + text.width,
        "the strip sits right of the text"
    );
}

#[test]
fn a_click_on_the_text_minimap_jumps_to_that_part_of_the_file() {
    let (_tmp, mut app) = text_minimap_app();
    draw(&mut app, 160, 40);
    let strip = app.minimap_img_rect;
    assert!(strip.height > 0);
    assert_eq!(app.editor.scroll, 0);
    // 300 lines at one dot row each fill 75 cell rows; the strip is shorter,
    // so the whole file is folded into it and the bottom row is the end.
    left_click(&mut app, strip.x + 2, strip.y + strip.height - 1);
    assert!(
        app.editor.cursor_row > 250,
        "the bottom of the strip is the end of the file: row {}",
        app.editor.cursor_row
    );
}

#[test]
fn the_minimap_stays_on_the_active_group_in_a_split() {
    let (_tmp, mut app) = text_minimap_app();
    app.split_editor();
    assert!(app.editor_layout.is_split());
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(200, 40)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let strip = app.minimap_img_rect;
    let active = app.editor.last_full_area;
    assert_eq!(strip.width, 6, "a split keeps the minimap: {strip:?}");
    assert!(braille_in(&term, strip) > 0, "{}", screen_text(&term));
    assert!(
        strip.x == active.x + active.width || strip.x + strip.width == active.x,
        "the strip is beside the active group: strip {strip:?}, group {active:?}"
    );
}

#[test]
fn the_minimap_off_draws_no_strip() {
    let (_tmp, mut app) = text_minimap_app();
    app.toggle_minimap();
    assert_eq!(app.status, "Minimap off");
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 40)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    assert_eq!(app.minimap_img_rect, Rect::default());
    let screen = screen_text(&term);
    assert!(
        !screen.chars().any(|c| is_braille(&c.to_string())),
        "{screen}"
    );
}

#[test]
fn turning_the_minimap_on_in_a_narrow_editor_says_why_it_is_hidden() {
    let (_tmp, mut app) = text_minimap_app();
    app.toggle_minimap();
    draw(&mut app, 70, 30);
    app.toggle_minimap();
    assert!(
        app.status.starts_with("Minimap on") && app.status.contains("wide"),
        "{}",
        app.status
    );
    // With room, the toggle just says it is on.
    app.toggle_minimap();
    draw(&mut app, 160, 40);
    app.toggle_minimap();
    assert_eq!(app.status, "Minimap on");
}

#[test]
fn braille_minimap_folds_each_two_by_four_dot_block_into_a_cell() {
    let bg = (0, 0, 0);
    let ink = (200, 100, 50);
    // One cell: 2 dot columns of 4 source chars each, 4 dot rows.
    let (w, h) = (2 * MINIMAP_TEXT_CHARS_PER_DOT, 4);
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    for px in rgba.chunks_exact_mut(4) {
        px[3] = 0xff;
    }
    let cells = braille_minimap(&rgba, w, h, bg, 1, 1);
    assert_eq!(cells, vec![None], "a blank block is a blank cell");
    // Ink in the second char of the first dot column, top row: dot 1.
    let idx = 4;
    rgba[idx..idx + 3].copy_from_slice(&[ink.0, ink.1, ink.2]);
    assert_eq!(
        braille_minimap(&rgba, w, h, bg, 1, 1),
        vec![Some(('\u{2801}', ink))]
    );
    // Every dot lit is the full cell.
    for px in rgba.chunks_exact_mut(4) {
        px[..3].copy_from_slice(&[ink.0, ink.1, ink.2]);
    }
    assert_eq!(
        braille_minimap(&rgba, w, h, bg, 1, 1),
        vec![Some(('\u{28ff}', ink))]
    );
}

/// #1569: Shift+End in a wrapped paragraph selects to the end of the row the
/// caret is on, not the rest of the paragraph; a second Shift+End extends it
/// to the line's end.
#[test]
fn shift_end_in_a_wrapped_paragraph_selects_to_the_end_of_the_row() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("long.txt");
    let words: Vec<String> = (0..60).map(|i| format!("word{i}")).collect();
    std::fs::write(&f, format!("Notes: {}\n", words.join(" "))).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&f).unwrap();
    app.editor.toggle_wrap();
    assert!(app.editor.wrap_enabled());
    app.focus_pane(Pane::Editor);
    draw(&mut app, 100, 30);
    app.handle_editor_key(key(KeyCode::Down, KeyModifiers::NONE));
    let row_start = app.editor.cursor_col;
    assert!(row_start > 0, "Down moved onto row 2 of line 1");
    app.handle_editor_key(key(KeyCode::End, KeyModifiers::SHIFT));
    let selected = app.editor.selection_text();
    assert!(
        !selected.is_empty() && selected.len() < 60,
        "one row, not the paragraph: {selected:?}"
    );
    assert_eq!(
        app.editor.lines[0].chars().nth(app.editor.cursor_col),
        Some(' '),
        "up to the space the row breaks at: {selected:?}"
    );
    app.handle_editor_key(key(KeyCode::End, KeyModifiers::SHIFT));
    let selected = app.editor.selection_text();
    assert!(selected.ends_with("word59"), "{selected:?}");
}

/// #1569 negative: Ctrl+A / Ctrl+E stay readline's start / end of the
/// logical line in a wrapped paragraph, one press each.
#[test]
fn ctrl_a_and_ctrl_e_reach_the_line_ends_in_a_wrapped_paragraph() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("long.txt");
    let words: Vec<String> = (0..60).map(|i| format!("word{i}")).collect();
    std::fs::write(&f, format!("Notes: {}\n", words.join(" "))).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&f).unwrap();
    app.editor.toggle_wrap();
    assert!(app.editor.wrap_enabled());
    app.focus_pane(Pane::Editor);
    draw(&mut app, 100, 30);
    app.handle_editor_key(key(KeyCode::Down, KeyModifiers::NONE));
    assert!(app.editor.cursor_col > 0, "Down moved onto row 2 of line 1");
    let mid_row = app.editor.cursor_col + 3;
    app.handle_editor_key(key(KeyCode::Char('e'), KeyModifiers::CONTROL));
    assert_eq!(app.editor.cursor_col, app.editor.lines[0].chars().count());
    app.editor.cursor_col = mid_row;
    app.handle_editor_key(key(KeyCode::Char('a'), KeyModifiers::CONTROL));
    assert_eq!(app.editor.cursor_col, 0);
}

/// #1576: "View: Change Line Numbers" steps on, relative, interval, off
/// and back, on every open editor, including tabs opened afterwards.
#[test]
fn change_line_numbers_cycles_modes_across_every_editor() {
    use crate::widgets::editor::LineNumbers;
    let mut app = editor_app_with_lines(&["a b"]);
    let all_in =
        |app: &App, mode: LineNumbers| app.editor.editors.iter().all(|e| e.line_numbers == mode);
    assert_eq!(app.line_numbers, LineNumbers::On, "on by default");
    let mut seen = Vec::new();
    for _ in 0..4 {
        app.run_command(crate::widgets::command_palette::Command::CycleLineNumbers);
        app.editor
            .open_text_buffer(std::path::Path::new("later.txt"), "c d")
            .unwrap();
        app.sync_focus_flags();
        assert!(all_in(&app, app.line_numbers), "{:?}", app.line_numbers);
        seen.push(app.status.clone());
    }
    assert_eq!(
        seen,
        [
            "Line Numbers: relative",
            "Line Numbers: interval",
            "Line Numbers: off",
            "Line Numbers: on"
        ]
    );
}

/// #1576: the palette command and a settings reload both reach the split
/// beside the active one at once, not at the next focus change.
#[test]
fn line_numbers_reach_every_split_group() {
    use crate::widgets::editor::LineNumbers;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "a\nb\n");
    app.split_editor_dir(editor_layout::SplitDir::Horizontal, true);
    let modes = |app: &App| -> Vec<LineNumbers> {
        std::iter::once(&app.editor)
            .chain(app.editor_layout.inactive_groups())
            .flat_map(|group| group.editors.iter().map(|e| e.line_numbers))
            .collect()
    };
    assert_eq!(modes(&app), [LineNumbers::On, LineNumbers::On]);

    app.run_command(crate::widgets::command_palette::Command::CycleLineNumbers);
    assert_eq!(modes(&app), [LineNumbers::Relative, LineNumbers::Relative]);

    let prefs = crate::prefs::Prefs {
        line_numbers: String::from("off"),
        ..Default::default()
    };
    app.apply_merged_settings(&prefs);
    assert_eq!(modes(&app), [LineNumbers::Off, LineNumbers::Off]);
}
