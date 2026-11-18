//! The Source Control panel's own tests, moved out of source_control.rs (#2062).

use super::*;

#[test]
fn insert_char_appends_and_moves_cursor() {
    let mut p = SourceControlPanel::new();
    p.insert_char('h');
    p.insert_char('i');
    assert_eq!(p.message, "hi");
    assert_eq!(p.message_cursor, 2);
}

#[test]
fn backspace_deletes_previous_char() {
    let mut p = SourceControlPanel::new();
    p.insert_str("abc");
    p.backspace();
    assert_eq!(p.message, "ab");
    assert_eq!(p.message_cursor, 2);
}

#[test]
fn backspace_at_start_is_noop() {
    let mut p = SourceControlPanel::new();
    p.backspace();
    assert_eq!(p.message, "");
    assert_eq!(p.message_cursor, 0);
}

#[test]
fn hovering_an_unselected_change_row_lifts_it() {
    use ratatui::buffer::Buffer;
    let mut p = SourceControlPanel::new();
    p.focus_gradient = false; // Croft Dark
    p.theme = crate::theme::Theme::DARK_BLUE;
    p.status.in_repo = true;
    p.selected_change = None; // nothing selected
    p.entries = vec![
        ChangeEntry {
            path: "a.rs".into(),
            kind: ChangeKind::Modified,
            ..Default::default()
        },
        ChangeEntry {
            path: "b.rs".into(),
            kind: ChangeKind::Modified,
            ..Default::default()
        },
    ];
    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 24,
    };
    let mut buf = Buffer::empty(area);
    (&mut p).render(area, &mut buf);
    // list_layout = [Header(Changes), Entry(0), Entry(1)]; hover Entry(1).
    let row_y = p.last_list_area.y + 2;
    p.hover_pointer = Some((p.last_list_area.x, row_y));
    let mut buf = Buffer::empty(area);
    (&mut p).render(area, &mut buf);
    assert_eq!(
        buf[(p.last_list_area.x, row_y)].bg,
        Color::Rgb(0x2b, 0x31, 0x42),
        "the hovered unselected change row wears the Croft Dark hover lift"
    );
}

#[test]
fn header_refresh_pill_renders_in_repo_and_hit_tests() {
    use ratatui::buffer::Buffer;
    let mut p = SourceControlPanel::new();
    p.status.in_repo = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 24,
    };
    let mut buf = Buffer::empty(area);
    (&mut p).render(area, &mut buf);
    let btn = p.header_refresh_btn;
    assert!(btn.width > 0, "the refresh pill must render inside a repo");
    // The cod-refresh glyph lands in the header row.
    let header_row: String = (0..area.width)
        .map(|x| buf[(x, btn.y)].symbol().chars().next().unwrap_or(' '))
        .collect();
    assert!(
        header_row.contains(crate::widgets::header_pill::REFRESH_GLYPH),
        "header row must paint the cod-refresh glyph (got: {header_row:?})",
    );
    // Hit-test: a click on the pill registers, one column left of it does not.
    assert!(p.click_header_refresh(btn.x, btn.y));
    assert!(!p.click_header_refresh(btn.x.saturating_sub(2), btn.y));
}

#[test]
fn long_commit_feedback_wraps_inside_the_panel() {
    // A refusal is only useful if it can be read: it ran on past the
    // panel's edge into the editor, where the editor painted over it.
    use ratatui::buffer::Buffer;
    let mut p = SourceControlPanel::new();
    p.status.in_repo = true;
    let msg = "Resolve 1 merge conflict (README.md) and stage it before committing";
    p.commit_feedback = Some(String::from(msg));
    p.commit_feedback_is_error = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 32,
        height: 30,
    };
    let mut buf = Buffer::empty(Rect { width: 80, ..area });
    (&mut p).render(area, &mut buf);
    let row = |y: u16, xs: std::ops::Range<u16>| -> String {
        xs.map(|x| buf[(x, y)].symbol().to_string()).collect()
    };
    for y in 0..area.height {
        assert_eq!(row(y, 32..80).trim(), "", "row {y} spills past the panel");
    }
    let words: Vec<String> = (0..area.height)
        .flat_map(|y| {
            row(y, 1..31)
                .split_whitespace()
                .map(String::from)
                .collect::<Vec<_>>()
        })
        .collect();
    let text = words.join(" ");
    assert!(text.contains(msg), "the whole message is on screen: {text}");
}

#[test]
fn feedback_wrapping_splits_long_words_and_caps_the_rows() {
    assert_eq!(
        wrap_feedback("abcdefghij kl", 4, 4),
        vec!["abcd", "efgh", "ij", "kl"]
    );
    let rows = wrap_feedback(&"word ".repeat(40), 10, 2);
    assert_eq!(rows.len(), 2);
    assert!(
        rows[1].ends_with('…') && rows[1].chars().count() <= 10,
        "{rows:?}"
    );
    assert!(wrap_feedback("anything", 0, 3).is_empty());
}

#[test]
fn short_commit_feedback_stays_on_one_line() {
    // Negative: the usual one-line summary keeps its single row, so the
    // change list below does not move down.
    use ratatui::buffer::Buffer;
    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 24,
    };
    let list_top = |feedback: Option<&str>| {
        let mut p = SourceControlPanel::new();
        p.status.in_repo = true;
        p.commit_feedback = feedback.map(String::from);
        let mut buf = Buffer::empty(area);
        (&mut p).render(area, &mut buf);
        p.last_list_area.y
    };
    assert_eq!(list_top(Some("[main 548c72d] Merge")), list_top(Some("x")));
}

#[test]
fn header_refresh_pill_absent_without_a_repo() {
    use ratatui::buffer::Buffer;
    let mut p = SourceControlPanel::new();
    p.status.in_repo = false;
    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 24,
    };
    let mut buf = Buffer::empty(area);
    (&mut p).render(area, &mut buf);
    assert_eq!(
        p.header_refresh_btn.width, 0,
        "no refresh pill in the no-repo empty state",
    );
}

#[test]
fn list_layout_orders_conflicts_staged_changes_untracked() {
    let mut p = SourceControlPanel::new();
    p.entries = vec![
        ChangeEntry {
            path: "u.txt".into(),
            kind: ChangeKind::Untracked,
            ..Default::default()
        },
        ChangeEntry {
            path: "m.txt".into(),
            kind: ChangeKind::Modified,
            ..Default::default()
        },
        ChangeEntry {
            path: "s.txt".into(),
            kind: ChangeKind::StagedAdded,
            ..Default::default()
        },
        ChangeEntry {
            path: "c.txt".into(),
            kind: ChangeKind::Conflicted,
            ..Default::default()
        },
    ];
    let lines = p.list_layout();
    assert!(matches!(
        lines[0],
        ListLine::Header(ChangeSection::Conflicts)
    ));
    assert!(matches!(lines[2], ListLine::Header(ChangeSection::Staged)));
    assert!(matches!(lines[4], ListLine::Header(ChangeSection::Changes)));
    assert!(matches!(
        lines[6],
        ListLine::Header(ChangeSection::Untracked)
    ));
}

fn buffer_to_string(buf: &Buffer) -> String {
    let mut out = String::new();
    for y in buf.area.y..buf.area.y + buf.area.height {
        for x in buf.area.x..buf.area.x + buf.area.width {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

fn dummy_status_with_branch(name: &str) -> GitStatus {
    GitStatus {
        in_repo: true,
        branch: Some(name.to_string()),
        detached_hash: None,
        ahead: 0,
        behind: 0,
        head_oid: None,
        ignored: std::sync::Arc::default(),
        decorations: std::sync::Arc::default(),
        dirty: false,
        repo_root: None,
        changed_count: 0,
        prepared_message: None,
        operation: None,
        rebasing_branch: None,
    }
}

#[test]
fn header_pills_stay_clear_of_a_translated_title() {
    use ratatui::buffer::Buffer;
    let title = "QUELLCODEVERWALTUNG";
    // From the narrowest panel whose inner row holds the whole title.
    for width in 21..40u16 {
        let mut p = SourceControlPanel::new();
        p.status.in_repo = true;
        let area = Rect {
            x: 0,
            y: 0,
            width,
            height: 24,
        };
        let mut buf = Buffer::empty(area);
        crate::i18n::with_test_catalog(&format!(r#"{{"SOURCE CONTROL": "{title}"}}"#), || {
            (&mut p).render(area, &mut buf);
        });
        let inner = p.last_inner;
        let header: String = (inner.x..inner.x + inner.width)
            .map(|x| buf[(x, inner.y)].symbol())
            .collect();
        assert!(
            header.contains(title),
            "width {width}: header pills painted over the title: {header:?}"
        );
        for btn in [p.header_refresh_btn, p.header_more_btn] {
            assert!(
                btn.width == 0 || btn.x > inner.x + title.len() as u16,
                "width {width}: pill {btn:?} overlaps the title"
            );
        }
    }
}

#[test]
fn header_row_says_source_control_inside_the_panel_not_on_the_outer_border() {
    use ratatui::buffer::Buffer;
    let mut p = SourceControlPanel::new();
    p.set_status(dummy_status_with_branch("main"), Vec::new());
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 30,
    };
    let mut buf = Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut p, area, &mut buf);
    let dump = buffer_to_string(&buf);
    // Header lives on the first inner row in light-grey-bold.
    let inner_top = dump.lines().nth(1).expect("inner row 0");
    assert!(
        inner_top.contains("SOURCE CONTROL"),
        "first inner row must carry SOURCE CONTROL: {inner_top:?}"
    );
    // Outer border row carries no chip / title.
    let outer = dump.lines().next().expect("outer top border");
    assert!(
        !outer.contains("SOURCE CONTROL"),
        "outer border must not carry the title chip: {outer:?}"
    );
}

#[test]
fn placeholder_text_never_overflows_past_the_input_box_right_border() {
    use ratatui::buffer::Buffer;
    // Render the SC panel in a region that is *narrower* than the
    // buffer, leaving an empty band to the right where leaks would
    // show up. The placeholder is 41 chars; the input-box inner is
    // only ~22 cells wide at a 28-col panel, so a missing width cap
    // would write 'r = push)' into the empty band — exactly the
    // 'leak past the right border into the editor pane' the user
    // reported.
    let mut p = SourceControlPanel::new();
    p.set_status(dummy_status_with_branch("main"), Vec::new());
    let panel_area = Rect {
        x: 0,
        y: 0,
        width: 28,
        height: 30,
    };
    let buf_area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 30,
    };
    let mut buf = Buffer::empty(buf_area);
    ratatui::widgets::Widget::render(&mut p, panel_area, &mut buf);
    let placeholder_row = p.last_input_area.y + 1;
    // Every cell past the panel's right edge must be blank — the
    // panel area is 0..28, so columns 28..80 are the editor band.
    for x in panel_area.right()..buf_area.right() {
        let sym = buf[(x, placeholder_row)].symbol().to_string();
        assert!(
            sym == " " || sym.is_empty(),
            "column {x} (past the SC panel's right edge at {}) carries content: {sym:?} — the placeholder string overflowed the input box and bled into the editor band, exactly the bug the user reported. Use set_stringn with a width cap.",
            panel_area.right(),
        );
    }
}

#[test]
fn typed_message_text_never_overflows_past_the_input_box_right_border() {
    use ratatui::buffer::Buffer;
    let mut p = SourceControlPanel::new();
    p.set_status(dummy_status_with_branch("main"), Vec::new());
    p.message = "this is a very long commit message that should not overflow the input box at all"
        .to_string();
    let panel_area = Rect {
        x: 0,
        y: 0,
        width: 28,
        height: 30,
    };
    let buf_area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 30,
    };
    let mut buf = Buffer::empty(buf_area);
    ratatui::widgets::Widget::render(&mut p, panel_area, &mut buf);
    let row = p.last_input_area.y + 1;
    for x in panel_area.right()..buf_area.right() {
        let sym = buf[(x, row)].symbol().to_string();
        assert!(
            sym == " " || sym.is_empty(),
            "typed message overflowed past the SC panel's right edge at column {x}: {sym:?} — same root cause as the placeholder leak"
        );
    }
}

#[test]
fn commit_feedback_shows_its_first_line_and_stays_inside_the_panel() {
    // #858: git's commit output (subject, diffstat, one `create mode`
    // row per new file) was drawn uncapped and ran on into the editor.
    use ratatui::buffer::Buffer;
    let mut p = SourceControlPanel::new();
    p.set_status(dummy_status_with_branch("main"), Vec::new());
    p.commit_feedback = Some(
        "[main 5a6cd5c] Add per-category report with a subject longer than the panel\n \
             3 files changed, 38 insertions(+)\n create mode 100644 report.py"
            .to_string(),
    );
    let panel_area = Rect {
        x: 0,
        y: 0,
        width: 28,
        height: 30,
    };
    let buf_area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 30,
    };
    let mut buf = Buffer::empty(buf_area);
    ratatui::widgets::Widget::render(&mut p, panel_area, &mut buf);
    for y in buf_area.top()..buf_area.bottom() {
        for x in panel_area.right()..buf_area.right() {
            let sym = buf[(x, y)].symbol();
            assert!(
                sym == " " || sym.is_empty(),
                "cell ({x}, {y}) past the panel's right edge carries {sym:?}"
            );
        }
    }
    let panel_text: String = (panel_area.top()..panel_area.bottom())
        .flat_map(|y| (panel_area.left()..panel_area.right()).map(move |x| (x, y)))
        .map(|pos| buf[pos].symbol().to_string())
        .collect();
    assert!(panel_text.contains("[main 5a6cd5c] Add"), "{panel_text:?}");
    assert!(
        !panel_text.contains("changed") && !panel_text.contains("create mode"),
        "only git's first line belongs in the panel: {panel_text:?}"
    );
}

/// #858 negative: a one-line feedback that fits is drawn whole, and an
/// error one is clipped at the panel's edge like a success.
#[test]
fn a_short_feedback_is_drawn_whole_and_an_error_stays_inside_the_panel() {
    use ratatui::buffer::Buffer;
    let draw = |feedback: &str, is_error: bool| -> (Buffer, Rect) {
        let mut p = SourceControlPanel::new();
        p.set_status(dummy_status_with_branch("main"), Vec::new());
        p.commit_feedback = Some(feedback.to_string());
        p.commit_feedback_is_error = is_error;
        let panel_area = Rect::new(0, 0, 28, 30);
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 30));
        ratatui::widgets::Widget::render(&mut p, panel_area, &mut buf);
        (buf, panel_area)
    };
    let text = |buf: &Buffer, area: Rect| -> String {
        (area.top()..area.bottom())
            .map(|y| {
                (area.left()..area.right())
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let (buf, area) = draw("Empty commit message", true);
    assert!(text(&buf, area).contains("Empty commit message"));
    let (buf, area) = draw(
        "error: pathspec 'a-very-long-file-name-that-runs-on.rs' did not match",
        true,
    );
    for y in 0..30 {
        for x in area.right()..80 {
            let sym = buf[(x, y)].symbol();
            assert!(
                sym == " " || sym.is_empty(),
                "cell ({x}, {y}) past the panel carries {sym:?}"
            );
        }
    }
    assert!(text(&buf, area).contains("error: pathspec"));
}

#[test]
fn long_commit_message_keeps_the_caret_visible_and_scrolls_to_show_the_tail() {
    use ratatui::buffer::Buffer;
    let mut p = SourceControlPanel::new();
    p.set_status(dummy_status_with_branch("main"), Vec::new());
    p.focused = true;
    // A workspace temp path is far longer than the ~22-cell input box.
    // Typing it leaves the cursor at the end.
    let long = "/var/folders/8r/rz52gwd56wz0yw3sqc7xprgm0000gn/T/croft";
    p.insert_str(long);
    let panel_area = Rect {
        x: 0,
        y: 0,
        width: 28,
        height: 30,
    };
    let mut buf = Buffer::empty(panel_area);
    ratatui::widgets::Widget::render(&mut p, panel_area, &mut buf);
    assert!(
        p.cursor_screen_pos().is_some(),
        "the caret must stay visible when the message is longer than the input box; otherwise the user can't see what they're editing and the box feels frozen"
    );
    let row = p.last_input_area.y + 1;
    let rendered: String = (p.last_input_area.x..p.last_input_area.right())
        .map(|x| buf[(x, row)].symbol().to_string())
        .collect();
    assert!(
        rendered.contains("croft"),
        "the box must scroll horizontally to reveal the tail near the cursor; rendered window was {rendered:?}"
    );
}

/// #1334: Home and End stay on the caret's line of a multi-line
/// message, as in the editor.
#[test]
fn home_and_end_move_within_the_carets_line() {
    let mut p = SourceControlPanel::new();
    p.insert_str("Add rounding\n\nRounds to cents.\nFixes 42");
    assert!(p.move_cursor_up());
    p.home();
    p.insert_char('[');
    p.end();
    p.insert_char(']');
    assert_eq!(p.message, "Add rounding\n\n[Rounds to cents.]\nFixes 42");
    // The empty line between subject and body: both stay on it.
    let mut p = SourceControlPanel::new();
    p.insert_str("a\n\nbody");
    assert!(p.move_cursor_up());
    p.end();
    assert_eq!(p.message_cursor, 2);
    p.home();
    assert_eq!(p.message_cursor, 2);
}

/// Negative (#1334): on a one-line message Home and End reach its start
/// and end as before, and the whole-message moves still reach index 0
/// and the end from any line.
#[test]
fn single_line_home_end_and_whole_message_moves_are_unchanged() {
    let mut p = SourceControlPanel::new();
    p.insert_str("one line");
    p.home();
    assert_eq!(p.message_cursor, 0);
    p.end();
    assert_eq!(p.message_cursor, 8);
    let mut p = SourceControlPanel::new();
    p.insert_str("subject\n\nbody\nlast");
    assert!(p.move_cursor_up());
    p.message_start();
    assert_eq!(p.message_cursor, 0);
    assert!(p.move_cursor_down());
    p.message_end();
    assert_eq!(p.message_cursor, p.message.chars().count());
}

#[test]
fn delete_key_forward_deletes_in_the_commit_message() {
    let mut p = SourceControlPanel::new();
    p.insert_str("abc");
    p.home();
    p.delete_char();
    assert_eq!((p.message.as_str(), p.message_cursor), ("bc", 0));
}

#[test]
fn input_box_is_three_rows_tall_with_a_focus_aware_border() {
    use ratatui::buffer::Buffer;
    let mut p = SourceControlPanel::new();
    p.set_status(dummy_status_with_branch("main"), Vec::new());
    p.focused = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 30,
    };
    let mut buf = Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut p, area, &mut buf);
    assert_eq!(
        p.last_input_area.height, 3,
        "commit-message input must be 3 rows tall to match the chunky aesthetic"
    );
    assert!(p.last_input_area.width >= 20);
}

#[test]
fn commit_button_is_chunky_three_row_full_width_block() {
    use ratatui::buffer::Buffer;
    let mut p = SourceControlPanel::new();
    p.set_status(dummy_status_with_branch("main"), Vec::new());
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 30,
    };
    let mut buf = Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut p, area, &mut buf);
    assert_eq!(
        p.last_button_area.height, 3,
        "Commit button must be 3 rows tall (chunky) like the mockup"
    );
    // Button must be wider than the old single-glyph label — close to
    // the inner width.
    assert!(
        p.last_button_area.width >= 20,
        "Commit button must be near-full-width; got {}",
        p.last_button_area.width
    );
}

#[test]
fn branch_row_uses_the_source_control_codicon_in_cyan() {
    use ratatui::buffer::Buffer;
    // The cyan glyph is the boxed themes'; Croft Loch mutes it (below).
    let mut p = SourceControlPanel::new();
    p.theme = crate::theme::Theme::BLACK;
    p.set_status(dummy_status_with_branch("main"), Vec::new());
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 20,
    };
    let mut buf = Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut p, area, &mut buf);
    let inner = p.last_inner;
    let row_y = inner.y + 2;
    // The branch glyph is cod-source-control (U+EA68) in cyan.
    // Verified against the upstream codicon mapping.json and the
    // Nerd Fonts CSS on 2026-05-09. Two prior mis-pins (U+EB14 =
    // link-external, U+EA84 = github) put the wrong shape on the
    // user's branch row; this test guards against either coming
    // back.
    let mut hit: Option<u16> = None;
    for x in inner.x..inner.x + inner.width {
        if buf[(x, row_y)].symbol() == "\u{ea68}" {
            hit = Some(x);
            break;
        }
    }
    let x = hit.expect("branch row must carry the cod-source-control (U+EA68) Y-fork glyph");
    let style = buf[(x, row_y)].style();
    let expected = p.theme.ui(ratatui::style::Color::Rgb(0x88, 0xc0, 0xd0));
    assert_eq!(
        style.fg,
        Some(expected),
        "branch glyph must render in cyan, matching the VS Code mockup"
    );
    // Regression guards against the two wrong codepoints that
    // shipped before:
    //   U+EB14 → cod-link-external (the share-arrow the user spotted)
    //   U+EA84 → cod-github (the octocat the user spotted before that)
    for x in inner.x..inner.x + inner.width {
        let s = buf[(x, row_y)].symbol();
        assert_ne!(
            s, "\u{eb14}",
            "branch row must not carry cod-link-external (U+EB14)"
        );
        assert_ne!(
            s, "\u{ea84}",
            "branch row must not carry cod-github (U+EA84)"
        );
    }
}

#[test]
fn commit_button_uses_rounded_border_corners() {
    use ratatui::buffer::Buffer;
    // The line-art corners are the boxed themes'; Croft Loch's soft
    // split button has its own test.
    let mut p = SourceControlPanel::new();
    p.theme = crate::theme::Theme::BLACK;
    p.set_status(dummy_status_with_branch("main"), Vec::new());
    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 20,
    };
    let mut buf = Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut p, area, &mut buf);
    let b = p.last_button_area;
    assert!(b.height >= 3 && b.width >= 4, "button must be laid out");
    let tl = buf[(b.x, b.y)].symbol().to_string();
    let tr = buf[(b.x + b.width - 1, b.y)].symbol().to_string();
    let bl = buf[(b.x, b.y + b.height - 1)].symbol().to_string();
    let br = buf[(b.x + b.width - 1, b.y + b.height - 1)]
        .symbol()
        .to_string();
    assert_eq!(
        tl, "╭",
        "top-left commit-button corner must be rounded; got {tl:?}"
    );
    assert_eq!(
        tr, "╮",
        "top-right commit-button corner must be rounded; got {tr:?}"
    );
    assert_eq!(
        bl, "╰",
        "bottom-left commit-button corner must be rounded; got {bl:?}"
    );
    assert_eq!(
        br, "╯",
        "bottom-right commit-button corner must be rounded; got {br:?}"
    );
}

#[test]
fn change_section_header_carries_a_count_pill() {
    use ratatui::buffer::Buffer;
    let mut p = SourceControlPanel::new();
    p.set_status(
        dummy_status_with_branch("main"),
        vec![
            ChangeEntry {
                path: "a.py".into(),
                kind: ChangeKind::Modified,
                ..Default::default()
            },
            ChangeEntry {
                path: "b.py".into(),
                kind: ChangeKind::Modified,
                ..Default::default()
            },
            ChangeEntry {
                path: "c.py".into(),
                kind: ChangeKind::Modified,
                ..Default::default()
            },
        ],
    );
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 40,
    };
    let mut buf = Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut p, area, &mut buf);
    let dump = buffer_to_string(&buf);
    assert!(
        dump.contains("CHANGES"),
        "section header must say CHANGES:\n{dump}"
    );
    assert!(
        dump.contains('3'),
        "section header must include a count badge of 3:\n{dump}"
    );
    assert!(
        dump.contains('▾')
            || dump.contains('▿')
            || dump.contains(crate::widgets::surface::CHEVRON_UNFOLDED),
        "section header must carry a down chevron:\n{dump}"
    );
}

/// Croft Loch: the commit message is a raised well with a one-eighth
/// block edge (the rim at rest, the accent while it has the keyboard),
/// never the rounded line box, and its inner rect is the box's, so the
/// caret lands where it always has. The placeholder is "Message" in the
/// muted ink, with the keys as a faint hint at the field's right edge.
#[test]
fn croft_loch_commit_message_is_a_raised_well_with_a_hairline_edge() {
    use ratatui::buffer::Buffer;
    let t = crate::widgets::surface::tones(crate::theme::Theme::LOCH).unwrap();
    let mut p = SourceControlPanel::new();
    p.theme = crate::theme::Theme::LOCH;
    p.set_status(dummy_status_with_branch("main"), Vec::new());
    let area = Rect::new(0, 0, 40, 30);
    let render = |p: &mut SourceControlPanel| {
        let mut buf = Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut *p, area, &mut buf);
        buf
    };
    let buf = render(&mut p);
    let r = p.last_input_area;
    assert_eq!(r.height, 3);
    assert_eq!(buf[(r.x + 3, r.y)].symbol(), "\u{2581}", "top edge");
    assert_eq!(buf[(r.x + 3, r.y)].fg, t.rim(), "the rim at rest");
    assert_eq!(buf[(r.x, r.y + 1)].symbol(), "\u{2595}", "left edge");
    assert_eq!(buf[(r.right() - 1, r.y + 1)].symbol(), "\u{258f}");
    assert_eq!(buf[(r.x + 3, r.bottom() - 1)].symbol(), "\u{2594}");
    assert_eq!(buf[(r.x + 3, r.y + 1)].bg, t.raised(), "a raised well");
    let row: String = (r.x..r.right())
        .map(|x| buf[(x, r.y + 1)].symbol())
        .collect();
    assert!(row.contains("Message"), "{row:?}");
    assert!(row.contains("\u{23ce} commit"), "{row:?}");
    assert_eq!(buf[(r.x + 2, r.y + 1)].fg, t.muted());
    let hint_x = r.x + row.chars().position(|c| c == '\u{23ce}').unwrap() as u16;
    assert_eq!(buf[(hint_x, r.y + 1)].fg, t.faint(), "the hint is faint");
    for y in r.top()..r.bottom() {
        for x in r.left()..r.right() {
            assert!(
                !"\u{256d}\u{256e}\u{2570}\u{256f}\u{2500}\u{2502}".contains(buf[(x, y)].symbol()),
                "a line-box glyph at ({x},{y})"
            );
        }
    }
    p.focused = true;
    let buf = render(&mut p);
    assert_eq!(buf[(r.x + 3, r.y)].fg, t.accent(), "the accent with focus");
    assert_eq!(p.cursor_screen_pos(), Some((r.x + 2, r.y + 1)));
}

/// Croft Loch's empty commit field fits every side-bar width: "Message"
/// whole, then the longest key hint that fits whole, ending on the
/// field's last text cell, or no hint at all. Nothing is ever cut
/// mid-word, and the caret keeps its cell.
#[test]
fn croft_loch_commit_placeholder_is_whole_at_every_side_bar_width() {
    use ratatui::buffer::Buffer;
    let hints = [
        "\u{23ce} commit \u{b7} \u{21e7}\u{23ce} newline",
        "\u{23ce} commit",
    ];
    let mut seen = [false; 3];
    for width in 16u16..=64 {
        let mut p = SourceControlPanel::new();
        p.theme = crate::theme::Theme::LOCH;
        p.set_status(dummy_status_with_branch("main"), Vec::new());
        let area = Rect::new(0, 0, width, 30);
        let mut buf = Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut p, area, &mut buf);
        let r = p.last_input_area;
        // The text cells: a cell of padding inside each edge.
        let (x0, x1) = (r.x + 2, r.right() - 2);
        let text: String = (x0..x1).map(|x| buf[(x, r.y + 1)].symbol()).collect();
        let rest = text
            .strip_prefix("Message")
            .unwrap_or_else(|| panic!("\"Message\" whole at width {width}: {text:?}"));
        let hint = rest.trim_start();
        match hints.iter().position(|h| *h == hint) {
            Some(i) => {
                seen[i] = true;
                assert!(
                    rest.len() - hint.len() >= 3,
                    "the hint stands clear of the word at width {width}: {text:?}"
                );
            }
            None => {
                assert!(hint.is_empty(), "a cut hint at width {width}: {text:?}");
                seen[2] = true;
            }
        }
    }
    assert!(seen.iter().all(|s| *s), "every tier is exercised: {seen:?}");
    // A normal side bar (the 30-column one the screenshots use) has
    // room for the commit key at least.
    let mut p = SourceControlPanel::new();
    p.theme = crate::theme::Theme::LOCH;
    p.set_status(dummy_status_with_branch("main"), Vec::new());
    let area = Rect::new(0, 0, 30, 30);
    let mut buf = Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut p, area, &mut buf);
    let r = p.last_input_area;
    let row: String = (r.x..r.right())
        .map(|x| buf[(x, r.y + 1)].symbol())
        .collect();
    assert!(row.contains("\u{23ce} commit"), "{row:?}");
}

/// Croft Loch's Commit is one calm split pill: the face is only the
/// middle row of the click targets, capped with half blocks on its outer
/// ends, the rows above and below left as chrome (no half-block bands,
/// no quadrant corners); the caret half joins it across the gap behind a
/// thin darker seam, and each half lifts under the pointer. The click
/// targets are the boxed theme's rects.
#[test]
fn croft_loch_commit_is_a_one_row_split_pill() {
    use ratatui::buffer::Buffer;
    let t = crate::widgets::surface::tones(crate::theme::Theme::LOCH).unwrap();
    let mut p = SourceControlPanel::new();
    p.theme = crate::theme::Theme::LOCH;
    p.set_status(dummy_status_with_branch("main"), Vec::new());
    let area = Rect::new(0, 0, 40, 30);
    let mut buf = Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut p, area, &mut buf);
    let (m, c) = (p.last_button_area, p.last_commit_caret_area);
    assert_eq!((m.height, c.x), (3, m.x + m.width + 1), "same targets");
    let teal = crate::gradient::rgb_color(crate::gradient::PRIMARY_BTN_BG);
    let face_y = m.y + 1;
    // The rows above and below the face are plain chrome.
    for y in [m.y, m.bottom() - 1] {
        for x in m.x..c.right() {
            assert_eq!(buf[(x, y)].symbol(), " ", "no band at ({x}, {y})");
            assert_eq!(buf[(x, y)].bg, t.chrome(), "chrome at ({x}, {y})");
        }
    }
    // The face: half-block caps on the outer ends, keeping the chrome
    // behind their empty half, the teal face between them.
    assert_eq!(buf[(m.x, face_y)].symbol(), "\u{2590}", "the left cap");
    assert_eq!(buf[(m.x, face_y)].fg, teal);
    assert_eq!(buf[(m.x, face_y)].bg, t.chrome());
    assert_eq!(buf[(c.right() - 1, face_y)].symbol(), "\u{258c}");
    assert_eq!(buf[(c.right() - 1, face_y)].fg, teal);
    assert_eq!(buf[(c.right() - 1, face_y)].bg, t.chrome());
    for x in m.x + 1..c.right() - 1 {
        assert_eq!(buf[(x, face_y)].bg, teal, "the face at {x}");
    }
    assert_eq!(buf[(m.right(), face_y)].symbol(), "\u{258f}", "the seam");
    assert_ne!(buf[(m.right(), face_y)].fg, teal, "a darker seam");
    let label: String = (m.x..m.right())
        .map(|x| buf[(x, face_y)].symbol())
        .collect();
    assert!(label.contains("Commit"), "{label:?}");
    assert_eq!(
        buf[(c.x, face_y)].symbol(),
        crate::widgets::surface::CHEVRON_UNFOLDED.to_string(),
        "the chevron centred right of the seam"
    );
    p.hover_pointer = Some((m.x + 1, m.y));
    let mut buf = Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut p, area, &mut buf);
    assert_ne!(buf[(m.x + 1, face_y)].bg, teal, "the hovered half lifts");
    assert_eq!(buf[(c.x + 1, face_y)].bg, teal, "the other half stays");
    assert_eq!(buf[(m.x + 1, m.y)].bg, t.chrome(), "only the face lifts");
    p.hover_pointer = Some((c.x + 1, c.bottom() - 1));
    let mut buf = Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut p, area, &mut buf);
    assert_eq!(buf[(m.x + 1, face_y)].bg, teal);
    assert_ne!(buf[(c.x, face_y)].bg, teal, "the caret half lifts");
    assert_eq!(
        buf[(m.right(), face_y)].bg,
        buf[(c.x, face_y)].bg,
        "the seam cell lifts with the caret it belongs to"
    );
}

/// Croft Loch's change list: a hairline above it, the file name in body
/// text with its folder after it in the muted ink, the selected row as
/// the band with the accent bar while the list has the keyboard, a
/// quiet count chip, and the branch glyph muted rather than the accent.
#[test]
fn croft_loch_change_rows_lead_with_the_name_and_wear_the_band() {
    use crate::widgets::surface::{ROW_BAR, tones};
    use ratatui::buffer::Buffer;
    let t = tones(crate::theme::Theme::LOCH).unwrap();
    let mut p = SourceControlPanel::new();
    p.theme = crate::theme::Theme::LOCH;
    p.set_status(
        dummy_status_with_branch("main"),
        vec![
            ChangeEntry {
                path: "src/deep/a.py".into(),
                kind: ChangeKind::Modified,
                ..Default::default()
            },
            ChangeEntry {
                path: "b.py".into(),
                kind: ChangeKind::Modified,
                ..Default::default()
            },
        ],
    );
    p.focused = true;
    assert!(p.focus_list());
    let area = Rect::new(0, 0, 40, 30);
    let mut buf = Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut p, area, &mut buf);
    let rows: Vec<String> = (0..area.height)
        .map(|y| (0..area.width).map(|x| buf[(x, y)].symbol()).collect())
        .collect();
    let y = rows
        .iter()
        .position(|r| r.contains("a.py src/deep"))
        .expect("the name, then its folder") as u16;
    let x = rows[y as usize].find("a.py").unwrap() as u16;
    let x = rows[y as usize][..x as usize].chars().count() as u16;
    assert_eq!(buf[(x, y)].fg, t.text());
    assert_eq!(buf[(x + 5, y)].fg, t.muted(), "the folder is muted");
    let selected = p.selected_change.expect("a row is selected");
    let sel_y = rows
        .iter()
        .position(|r| r.contains(if selected == 0 { "a.py" } else { "b.py" }))
        .unwrap() as u16;
    assert_eq!(buf[(0, sel_y)].symbol(), ROW_BAR, "the bar in the padding");
    assert_eq!(buf[(3, sel_y)].bg, t.band_on(t.chrome));
    let list = p.last_list_area;
    assert_eq!(
        buf[(list.x + 2, list.y - 1)].symbol(),
        "\u{2581}",
        "hairline"
    );
    assert_eq!(buf[(list.x + 2, list.y - 1)].fg, t.hairline());
    let branch_y = p.last_inner.y + 2;
    let glyph = (0..area.width)
        .find(|&x| buf[(x, branch_y)].symbol() == "\u{ea68}")
        .unwrap();
    assert_eq!(buf[(glyph, branch_y)].fg, t.muted(), "a muted branch glyph");
}

/// Croft Loch's REPOSITORIES rows stay on the chrome: no row paints the
/// terminal's default background across the side bar.
#[test]
fn croft_loch_repository_rows_keep_the_chrome() {
    use ratatui::buffer::Buffer;
    let t = crate::widgets::surface::tones(crate::theme::Theme::LOCH).unwrap();
    let mut p = SourceControlPanel::new();
    p.theme = crate::theme::Theme::LOCH;
    p.status = dummy_status_with_branch("trunk");
    p.repositories = ["gitb", "gitc"]
        .iter()
        .enumerate()
        .map(|(i, label)| RepoRow {
            label: String::from(*label),
            branch: Some(String::from("trunk")),
            changes: 0,
            active: i == 0,
            workspace_root: std::path::PathBuf::from(format!("/w/{label}")),
        })
        .collect();
    let area = Rect::new(0, 0, 34, 20);
    let mut buf = Buffer::empty(area);
    // The side bar paints its chrome under every view first.
    buf.set_style(area, Style::default().bg(t.chrome()));
    (&mut p).render(area, &mut buf);
    let (rect, _) = p.repo_row_areas[1].clone();
    for x in rect.left()..rect.right() {
        assert_eq!(buf[(x, rect.y)].bg, t.chrome(), "({x},{})", rect.y);
    }
    let (active, _) = p.repo_row_areas[0].clone();
    assert_eq!(buf[(active.x + 2, active.y)].fg, t.text());
}

#[test]
fn section_header_shows_green_plus_additions_and_red_minus_deletions() {
    use ratatui::buffer::Buffer;
    let mut p = SourceControlPanel::new();
    p.set_status(
        dummy_status_with_branch("main"),
        vec![
            ChangeEntry {
                path: "a.py".into(),
                kind: ChangeKind::Modified,
                additions: 12,
                deletions: 3,
            },
            ChangeEntry {
                path: "b.py".into(),
                kind: ChangeKind::Modified,
                additions: 5,
                deletions: 7,
            },
        ],
    );
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 40,
    };
    let mut buf = Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut p, area, &mut buf);
    let dump = buffer_to_string(&buf);
    assert!(
        dump.contains("+17"),
        "section header must show total additions +17:\n{dump}"
    );
    assert!(
        dump.contains("-10"),
        "section header must show total deletions -10:\n{dump}"
    );

    let mut plus_cell_is_green = false;
    let mut minus_cell_is_red = false;
    for y in 0..area.height {
        for x in 0..area.width {
            let cell = &buf[(x, y)];
            let sym = cell.symbol();
            let Some(fg) = cell.style().fg else { continue };
            let Color::Rgb(r, g, b) = fg else { continue };
            if sym == "+" && g > r && g > b {
                plus_cell_is_green = true;
            }
            if sym == "-" && r > g && r > b {
                minus_cell_is_red = true;
            }
        }
    }
    assert!(
        plus_cell_is_green,
        "the '+' glyph in the section header must be painted green"
    );
    assert!(
        minus_cell_is_red,
        "the '-' glyph in the section header must be painted red"
    );
}

#[test]
fn render_in_non_repo_hides_commit_input_and_button_and_list() {
    use crate::git::GitStatus;
    let mut p = SourceControlPanel::new();
    p.status = GitStatus::default(); // in_repo = false
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 30,
    };
    let mut buf = Buffer::empty(area);
    (&mut p).render(area, &mut buf);
    assert_eq!(
        p.last_input_area,
        Rect::default(),
        "input area must stay empty in non-repo"
    );
    assert_eq!(
        p.last_button_area,
        Rect::default(),
        "commit button area must stay empty in non-repo"
    );
    assert_eq!(
        p.last_list_area,
        Rect::default(),
        "list area must stay empty in non-repo"
    );
}

#[test]
fn empty_state_renders_no_repository_detected_title_and_description() {
    use crate::git::GitStatus;
    let mut p = SourceControlPanel::new();
    p.status = GitStatus::default();
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 30,
    };
    let mut buf = Buffer::empty(area);
    (&mut p).render(area, &mut buf);
    let dump = buffer_to_string(&buf);
    assert!(
        dump.contains("No repository detected"),
        "empty-state title missing:\n{dump}"
    );
    assert!(
        dump.contains("Initialize Repository"),
        "primary button label missing:\n{dump}"
    );
    assert!(
        dump.contains("Open a folder under Git"),
        "description must mention 'Open a folder under Git':\n{dump}"
    );
    assert!(
        dump.contains("tracking changes"),
        "description must mention 'tracking changes':\n{dump}"
    );
    assert!(
        !dump.contains("Open Folder"),
        "Open Folder button must be gone:\n{dump}"
    );
    assert!(
        !dump.contains("Learn more about Git"),
        "Learn more link must be gone:\n{dump}"
    );
}

/// #1227: the workspace sits one folder above its repo. Initialize
/// would `git init` the parent around the existing repo, so the card
/// names the subfolder repos and offers to open them instead.
#[test]
fn empty_state_offers_subfolder_repos_instead_of_initialize() {
    use crate::git::GitStatus;
    let mut p = SourceControlPanel::new();
    p.status = GitStatus::default();
    p.nested_repos = vec![(String::from("sub/"), std::path::PathBuf::from("/w/sub"))];
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 30,
    };
    let mut buf = Buffer::empty(area);
    (&mut p).render(area, &mut buf);
    let dump = buffer_to_string(&buf);
    assert!(
        dump.contains("Repositories found in subfolders"),
        "title must name the subfolder repos:\n{dump}"
    );
    assert!(dump.contains("Open sub/"), "open button missing:\n{dump}");
    assert!(
        !dump.contains("Initialize") && !dump.contains("No repository detected"),
        "Initialize must not be offered above a repo:\n{dump}"
    );
    assert_eq!(p.last_init_repo_button_area, Rect::default());
    assert_eq!(p.nested_repo_button_areas.len(), 1);
    let (btn, _) = p.nested_repo_button_areas[0].clone();
    assert_eq!(
        p.click_nested_repo(btn.x + 1, btn.y + 1),
        Some(std::path::PathBuf::from("/w/sub"))
    );
    assert_eq!(p.click_nested_repo(btn.x + 1, btn.y + btn.height), None);
}

#[test]
fn empty_state_lists_one_open_button_per_subfolder_repo() {
    use crate::git::GitStatus;
    let mut p = SourceControlPanel::new();
    p.status = GitStatus::default();
    p.nested_repos = vec![
        (String::from("api/"), std::path::PathBuf::from("/w/api")),
        (String::from("web/"), std::path::PathBuf::from("/w/web")),
    ];
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 40,
    };
    let mut buf = Buffer::empty(area);
    (&mut p).render(area, &mut buf);
    let dump = buffer_to_string(&buf);
    assert!(
        dump.contains("Open api/") && dump.contains("Open web/"),
        "{dump}"
    );
    let paths: Vec<_> = p
        .nested_repo_button_areas
        .iter()
        .map(|(_, path)| path.clone())
        .collect();
    assert_eq!(
        paths,
        [
            std::path::PathBuf::from("/w/api"),
            std::path::PathBuf::from("/w/web")
        ]
    );
}

#[test]
fn empty_state_without_subfolder_repos_still_offers_initialize() {
    use crate::git::GitStatus;
    let mut p = SourceControlPanel::new();
    p.status = GitStatus::default();
    p.nested_repo_button_areas = vec![(
        Rect {
            x: 1,
            y: 1,
            width: 5,
            height: 3,
        },
        std::path::PathBuf::from("/stale"),
    )];
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 30,
    };
    let mut buf = Buffer::empty(area);
    (&mut p).render(area, &mut buf);
    assert!(p.last_init_repo_button_area.width > 0);
    assert!(
        p.nested_repo_button_areas.is_empty(),
        "stale open buttons must not stay clickable"
    );
    assert_eq!(p.click_nested_repo(2, 2), None);
}

#[test]
fn a_repo_workspace_never_shows_subfolder_open_buttons() {
    use crate::git::GitStatus;
    let mut p = SourceControlPanel::new();
    p.status = GitStatus {
        in_repo: true,
        ..GitStatus::default()
    };
    p.nested_repos = vec![(String::from("sub/"), std::path::PathBuf::from("/w/sub"))];
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 30,
    };
    let mut buf = Buffer::empty(area);
    (&mut p).render(area, &mut buf);
    assert!(p.nested_repo_button_areas.is_empty());
    assert!(!buffer_to_string(&buf).contains("Open sub/"));
}

#[test]
fn empty_state_records_button_rect_for_hit_testing() {
    use crate::git::GitStatus;
    let mut p = SourceControlPanel::new();
    p.status = GitStatus::default();
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 30,
    };
    let mut buf = Buffer::empty(area);
    (&mut p).render(area, &mut buf);
    assert!(
        p.last_init_repo_button_area.width > 0 && p.last_init_repo_button_area.height > 0,
        "Initialize Repository button rect must be tracked"
    );
}

#[test]
fn empty_state_init_button_paints_a_visible_label_at_narrow_widths() {
    // Regression for "WTF is this": a narrow side panel produced a
    // blank blue rectangle because render_rounded_button used to
    // silently drop the label when label.chars().count() > area.width.
    // The button must always carry SOME visible label.
    use crate::git::GitStatus;
    let mut p = SourceControlPanel::new();
    p.status = GitStatus::default();
    let area = Rect {
        x: 0,
        y: 0,
        width: 30,
        height: 40,
    };
    let mut buf = Buffer::empty(area);
    (&mut p).render(area, &mut buf);
    let btn = p.last_init_repo_button_area;
    assert!(
        btn.width > 0 && btn.height > 0,
        "init-repo button must be tracked"
    );
    let label_y = btn.y + btn.height / 2;
    let mut row = String::new();
    for x in btn.x..btn.x + btn.width {
        row.push_str(buf[(x, label_y)].symbol());
    }
    let trimmed = row.trim();
    assert!(
        !trimmed.is_empty() && trimmed.chars().any(|c| c.is_alphabetic()),
        "init-repo button row must carry a visible label, got {row:?}"
    );
}

/// Croft Loch's Initialize Repository button is the same one-row pill
/// as Commit: a teal face between half-block caps on its middle row, on
/// the raised card, whose rows above and below stay the card; the whole
/// three-row rect still answers the click and lifts the face on hover.
#[test]
fn croft_loch_initialize_button_is_a_one_row_pill_on_the_card() {
    use crate::git::GitStatus;
    let t = crate::widgets::surface::tones(crate::theme::Theme::LOCH).unwrap();
    let teal = crate::gradient::rgb_color(crate::gradient::PRIMARY_BTN_BG);
    let mut p = SourceControlPanel::new();
    p.theme = crate::theme::Theme::LOCH;
    p.status = GitStatus::default();
    let area = Rect::new(0, 0, 40, 40);
    let mut buf = Buffer::empty(area);
    (&mut p).render(area, &mut buf);
    let b = p.last_init_repo_button_area;
    assert_eq!(b.height, 3, "the three-row target");
    let face_y = b.y + 1;
    assert_eq!(buf[(b.x, face_y)].symbol(), "\u{2590}");
    assert_eq!(buf[(b.x, face_y)].bg, t.raised(), "the cap on the card");
    assert_eq!(buf[(b.right() - 1, face_y)].symbol(), "\u{258c}");
    assert_eq!(buf[(b.x + 1, face_y)].bg, teal);
    let label: String = (b.x..b.right())
        .map(|x| buf[(x, face_y)].symbol())
        .collect();
    assert!(label.contains("Initialize"), "{label:?}");
    for y in [b.y, b.bottom() - 1] {
        for x in b.x..b.right() {
            assert_eq!(buf[(x, y)].symbol(), " ", "no band at ({x}, {y})");
            assert_eq!(buf[(x, y)].bg, t.raised(), "the card at ({x}, {y})");
        }
    }
    assert!(
        p.click_init_repo_button(b.x, b.y),
        "the top row still clicks"
    );
    p.hover_pointer = Some((b.x + 2, b.bottom() - 1));
    let mut buf = Buffer::empty(area);
    (&mut p).render(area, &mut buf);
    assert_ne!(buf[(b.x + 1, face_y)].bg, teal, "the face lifts");
    assert_eq!(buf[(b.x + 1, b.y)].bg, t.raised(), "only the face");
}

#[test]
fn empty_state_description_wraps_to_card_width_no_mid_word_truncation() {
    // Regression for "text inside is not even adjusted": the
    // description used to be hard-truncated at card_inner.width
    // chars, producing fragments like "Open a folder under G" /
    // "repository to start t" when the card was narrow. With
    // ratatui's Paragraph + Wrap, words must stay whole.
    use crate::git::GitStatus;
    let mut p = SourceControlPanel::new();
    p.status = GitStatus::default();
    // Width 30 puts the card_inner at ~24 - too narrow for
    // "Open a folder under Git or create a new" but plenty for
    // word-wrapped fragments.
    let area = Rect {
        x: 0,
        y: 0,
        width: 30,
        height: 40,
    };
    let mut buf = Buffer::empty(area);
    (&mut p).render(area, &mut buf);
    let dump = buffer_to_string(&buf);
    assert!(
        !dump.contains("Open a folder under G\n"),
        "description must NOT be hard-truncated mid-word ('under G'):\n{dump}"
    );
    assert!(
        !dump.contains("repository to start t\n"),
        "description must NOT be hard-truncated mid-word ('start t'):\n{dump}"
    );
}

#[test]
fn empty_state_illustration_does_not_use_arrow_up_codicon() {
    // Regression: the earlier illustration used U+EAA1 (cod-arrow-up)
    // which rendered a literal up-arrow glyph. The new rich
    // illustration uses ● bullets for git nodes; the compact fallback
    // uses U+EA68 (cod-source-control). Whichever variant renders,
    // U+EAA1 must NEVER appear.
    use crate::git::GitStatus;
    let mut p = SourceControlPanel::new();
    p.status = GitStatus::default();
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 30,
    };
    let mut buf = Buffer::empty(area);
    (&mut p).render(area, &mut buf);
    let dump = buffer_to_string(&buf);
    assert!(
        !dump.contains('\u{eaa1}'),
        "empty-state must NOT carry U+EAA1 (cod-arrow-up); that was the broken glyph"
    );
    // The rich illustration must surface git nodes (●).
    assert!(
        dump.contains('\u{25cf}'),
        "rich illustration must paint ● git nodes"
    );
}

#[test]
fn click_init_repo_button_returns_true_only_inside_the_rect() {
    let mut p = SourceControlPanel::new();
    p.last_init_repo_button_area = Rect {
        x: 5,
        y: 10,
        width: 30,
        height: 3,
    };
    assert!(p.click_init_repo_button(20, 11));
    assert!(!p.click_init_repo_button(40, 11));
    assert!(!p.click_init_repo_button(20, 20));
}

#[test]
fn change_rows_use_a_subtle_row_tint_not_darker_than_panel_bg() {
    // User report: the rows were darker than the panel bg, the
    // opposite of what the mockup shows. Drop the heavy dark tint —
    // either no row bg at all, or a tint LIGHTER than the editor bg
    // (Color::Reset / 0x1e222e). Rgb(0x16,0x1b,0x25) is darker and
    // must not appear on entry rows.
    use ratatui::buffer::Buffer;
    let mut p = SourceControlPanel::new();
    p.set_status(
        dummy_status_with_branch("main"),
        vec![ChangeEntry {
            path: "a.py".into(),
            kind: ChangeKind::Modified,
            ..Default::default()
        }],
    );
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 30,
    };
    let mut buf = Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut p, area, &mut buf);
    let bad = ratatui::style::Color::Rgb(0x16, 0x1b, 0x25);
    for y in 0..area.height {
        for x in 0..area.width {
            assert_ne!(
                buf[(x, y)].style().bg,
                Some(bad),
                "no cell may carry the darker-than-panel row bg ({bad:?}) — that's the colour the user complained about; cell at ({x},{y})"
            );
        }
    }
}

#[test]
fn clicking_a_change_entry_highlights_it_with_a_visible_row_bg() {
    use ratatui::buffer::Buffer;
    let mut p = SourceControlPanel::new();
    p.set_status(
        dummy_status_with_branch("main"),
        vec![
            ChangeEntry {
                path: "a.py".into(),
                kind: ChangeKind::Modified,
                ..Default::default()
            },
            ChangeEntry {
                path: "b.py".into(),
                kind: ChangeKind::Modified,
                ..Default::default()
            },
        ],
    );
    // Render once to learn the row coordinates.
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 30,
    };
    let mut buf = Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut p, area, &mut buf);
    // Pretend the user clicked the second change row.
    let target_y = (0..area.height)
        .find(|y| {
            let mut row = String::new();
            for x in 0..area.width {
                row.push_str(buf[(x, *y)].symbol());
            }
            row.contains("b.py")
        })
        .expect("b.py row must render");
    p.select_change_at(target_y);
    // Re-render with the new selection.
    let mut buf = Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut p, area, &mut buf);
    // The selected row must carry a non-Reset bg distinct from the
    // unselected rows so the user can tell which entry is active.
    let mut selected_has_bg = false;
    for x in 0..area.width {
        if let Some(bg) = buf[(x, target_y)].style().bg
            && bg != ratatui::style::Color::Reset
        {
            selected_has_bg = true;
            break;
        }
    }
    assert!(
        selected_has_bg,
        "the row of the clicked entry must carry a visible bg highlight"
    );
}

#[test]
fn rendering_at_narrow_widths_does_not_panic() {
    use ratatui::buffer::Buffer;
    for width in 0u16..40 {
        for height in 0u16..30 {
            let mut p = SourceControlPanel::new();
            p.set_status(
                dummy_status_with_branch("main"),
                vec![
                    ChangeEntry {
                        path: "a.py".into(),
                        kind: ChangeKind::Modified,
                        ..Default::default()
                    },
                    ChangeEntry {
                        path: ".idea/".into(),
                        kind: ChangeKind::Untracked,
                        ..Default::default()
                    },
                ],
            );
            let area = Rect {
                x: 0,
                y: 0,
                width,
                height,
            };
            if area.width == 0 || area.height == 0 {
                continue;
            }
            let mut buf = Buffer::empty(area);
            ratatui::widgets::Widget::render(&mut p, area, &mut buf);
        }
    }
}

#[test]
fn repositories_overview_renders_rows_and_maps_clicks() {
    let mut p = SourceControlPanel::new();
    p.status = dummy_status_with_branch("trunk");
    p.repositories = vec![
        RepoRow {
            label: String::from("gitb"),
            branch: Some(String::from("trunk")),
            changes: 1,
            active: true,
            workspace_root: std::path::PathBuf::from("/w/gitb"),
        },
        RepoRow {
            label: String::from("gitc"),
            branch: Some(String::from("dev")),
            changes: 1,
            active: false,
            workspace_root: std::path::PathBuf::from("/w/gitc"),
        },
    ];
    let area = Rect {
        x: 0,
        y: 0,
        width: 34,
        height: 20,
    };
    let mut buf = Buffer::empty(area);
    (&mut p).render(area, &mut buf);
    let dump = buffer_to_string(&buf);
    assert!(dump.contains("REPOSITORIES"), "{dump}");
    assert!(dump.contains("gitb") && dump.contains("gitc"), "{dump}");
    let (rect, root) = p.repo_row_areas[1].clone();
    assert_eq!(root, std::path::PathBuf::from("/w/gitc"));
    assert_eq!(
        p.click_repository(rect.x + 2, rect.y),
        Some(std::path::PathBuf::from("/w/gitc")),
        "a click inside the second row maps to its folder"
    );
    assert_eq!(p.click_repository(rect.x + 2, rect.y + 10), None);
}

fn merging(message: Option<&str>) -> GitStatus {
    GitStatus {
        prepared_message: message.map(String::from),
        ..dummy_status_with_branch("main")
    }
}

/// A merge in progress fills the empty box with git's message (#1282),
/// caret at its end, and the box empties again once the merge is over.
#[test]
fn a_merge_in_progress_fills_an_empty_message_box() {
    let mut p = SourceControlPanel::new();
    p.set_status(merging(Some("Merge branch 'other'")), Vec::new());
    assert_eq!(p.message, "Merge branch 'other'");
    assert_eq!(p.message_cursor, p.message.chars().count());
    p.set_status(merging(None), Vec::new());
    assert_eq!(p.message, "", "the merge was committed or aborted");
}

/// #1336: the whole prepared message goes in, body and all, so a revert
/// keeps "This reverts commit …" and a cherry-pick its "(cherry picked
/// from commit …)". The caret lands at its end, and an untouched seed
/// still goes once the revert is over.
#[test]
fn a_prepared_message_with_a_body_fills_the_whole_message() {
    let mut p = SourceControlPanel::new();
    let prepared = "Revert \"x\"\n\nThis reverts commit abc.";
    p.set_status(merging(Some(prepared)), Vec::new());
    assert_eq!(p.message, prepared);
    assert_eq!(p.message_cursor, p.message.chars().count());
    p.set_status(merging(Some(prepared)), Vec::new());
    assert_eq!(p.message, prepared, "a refresh doesn't seed it twice");
    p.set_status(merging(None), Vec::new());
    assert_eq!(p.message, "", "the revert was committed or aborted");
}

/// Negative (#1336): an edited body is the user's text, so the end of
/// the revert leaves it in the box.
#[test]
fn an_edited_body_is_kept_when_the_revert_ends() {
    let mut p = SourceControlPanel::new();
    p.set_status(
        merging(Some("Revert \"x\"\n\nThis reverts commit abc.")),
        Vec::new(),
    );
    p.insert_str(" It broke checkout.");
    p.set_status(merging(None), Vec::new());
    assert_eq!(
        p.message,
        "Revert \"x\"\n\nThis reverts commit abc. It broke checkout."
    );
}

#[test]
fn a_prepared_message_never_replaces_typed_text() {
    // Negative: what the user typed stays, during and after the merge.
    let mut p = SourceControlPanel::new();
    p.insert_str("my own words");
    p.set_status(merging(Some("Merge branch 'other'")), Vec::new());
    assert_eq!(p.message, "my own words");
    p.set_status(merging(None), Vec::new());
    assert_eq!(p.message, "my own words");
    // An edited seed is the user's text too.
    let mut p = SourceControlPanel::new();
    p.set_status(merging(Some("Merge branch 'other'")), Vec::new());
    p.insert_str(" into main");
    p.set_status(merging(None), Vec::new());
    assert_eq!(p.message, "Merge branch 'other' into main");
}

/// #1620: after a commit from the box, the template fills it again for
/// the next commit.
#[test]
fn the_template_comes_back_after_each_commit() {
    let mut p = SourceControlPanel::new();
    p.set_status(merging(Some("PROJ-:\n\nWhy:")), Vec::new());
    assert_eq!(p.message, "PROJ-:\n\nWhy:");
    p.insert_str(" because");
    p.clear_after_commit();
    assert_eq!(p.message, "");
    p.set_status(merging(Some("PROJ-:\n\nWhy:")), Vec::new());
    assert_eq!(p.message, "PROJ-:\n\nWhy:", "seeded for the next commit");
}

/// #1620: an untouched seed is replaced when another repository (or a
/// changed template) prepares a different message, so A's template is
/// never committed in B; an edited seed is kept.
#[test]
fn an_untouched_seed_follows_the_repository() {
    let mut p = SourceControlPanel::new();
    p.set_status(merging(Some("A-:")), Vec::new());
    p.set_status(merging(Some("B-:")), Vec::new());
    assert_eq!(p.message, "B-:");
    p.insert_str(" edited");
    p.set_status(merging(Some("A-:")), Vec::new());
    assert_eq!(p.message, "B-: edited");
}

/// Negative (#1620): text the user typed is kept when the template
/// changes, and a template they cleared is not put back until a commit.
#[test]
fn a_changed_template_never_replaces_typed_text() {
    let mut p = SourceControlPanel::new();
    p.insert_str("fix: my words");
    p.set_status(merging(Some("PROJ-:")), Vec::new());
    p.set_status(merging(Some("TEAM-:")), Vec::new());
    assert_eq!(p.message, "fix: my words");
    let mut p = SourceControlPanel::new();
    p.set_status(merging(Some("PROJ-:")), Vec::new());
    p.clear_message();
    p.set_status(merging(Some("PROJ-:")), Vec::new());
    assert_eq!(p.message, "");
}

#[test]
fn a_cleared_seed_is_not_refilled_on_the_next_refresh() {
    let mut p = SourceControlPanel::new();
    p.set_status(merging(Some("Merge branch 'other'")), Vec::new());
    p.clear_message();
    p.set_status(merging(Some("Merge branch 'other'")), Vec::new());
    assert_eq!(p.message, "", "the user emptied it on purpose");
}

#[test]
fn no_merge_leaves_an_empty_box_empty() {
    let mut p = SourceControlPanel::new();
    p.set_status(merging(None), Vec::new());
    assert_eq!(p.message, "");
}

#[test]
fn render_in_repo_paints_input_and_button() {
    use crate::git::GitStatus;
    let mut p = SourceControlPanel::new();
    p.status = GitStatus {
        in_repo: true,
        branch: Some("main".into()),
        ..Default::default()
    };
    // Need at least: header + blank + branch + blank + 3-row input +
    // blank + 3-row button = 11 rows of inner area, so 13 rows of
    // outer area to clear the borders.
    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 16,
    };
    let mut buf = Buffer::empty(area);
    (&mut p).render(area, &mut buf);
    assert!(
        p.last_input_area.height > 0,
        "input area must paint when in_repo"
    );
    assert!(
        p.last_button_area.height > 0,
        "button area must paint when in_repo"
    );
}

#[test]
fn cursor_screen_pos_is_none_when_not_focused() {
    let mut p = SourceControlPanel::new();
    p.status = GitStatus {
        in_repo: true,
        branch: Some("main".into()),
        ..Default::default()
    };
    p.focused = false;
    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 16,
    };
    let mut buf = Buffer::empty(area);
    (&mut p).render(area, &mut buf);
    assert_eq!(
        p.cursor_screen_pos(),
        None,
        "caret must hide when the panel is not focused"
    );
}

#[test]
fn cursor_screen_pos_is_none_when_no_repo() {
    let mut p = SourceControlPanel::new();
    p.status = GitStatus::default();
    p.focused = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 30,
    };
    let mut buf = Buffer::empty(area);
    (&mut p).render(area, &mut buf);
    assert_eq!(
        p.cursor_screen_pos(),
        None,
        "caret must hide when the panel is in the no-repo empty state (no input box painted)"
    );
}

#[test]
fn cursor_screen_pos_lands_at_first_text_cell_when_message_is_empty() {
    let mut p = SourceControlPanel::new();
    p.status = GitStatus {
        in_repo: true,
        branch: Some("main".into()),
        ..Default::default()
    };
    p.focused = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 16,
    };
    let mut buf = Buffer::empty(area);
    (&mut p).render(area, &mut buf);
    let r = p.last_input_area;
    assert!(r.width >= 3 && r.height == 3, "input must be laid out");
    let (cx, cy) = p
        .cursor_screen_pos()
        .expect("focused, in repo → caret must report a cell");
    assert_eq!(
        (cx, cy),
        (r.x + 2, r.y + 1),
        "empty message caret sits at the input's first inner text cell (one pad cell after the rounded left border)"
    );
}

#[test]
fn cursor_screen_pos_advances_one_cell_per_inserted_char() {
    let mut p = SourceControlPanel::new();
    p.status = GitStatus {
        in_repo: true,
        branch: Some("main".into()),
        ..Default::default()
    };
    p.focused = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 16,
    };
    let mut buf = Buffer::empty(area);
    (&mut p).render(area, &mut buf);
    p.insert_str("fix");
    let r = p.last_input_area;
    let (cx, cy) = p.cursor_screen_pos().expect("caret must report a cell");
    assert_eq!(
        (cx, cy),
        (r.x + 2 + 3, r.y + 1),
        "after typing 3 chars the caret sits 3 cells past the first text cell"
    );
}

#[test]
fn cursor_screen_pos_is_none_before_first_render() {
    let mut p = SourceControlPanel::new();
    p.status = GitStatus {
        in_repo: true,
        branch: Some("main".into()),
        ..Default::default()
    };
    p.focused = true;
    assert_eq!(
        p.cursor_screen_pos(),
        None,
        "caret must hide before the first render (last_input_area is still default)"
    );
}

#[test]
fn selected_unstaged_row_paints_discard_and_stage_icons_before_the_badge() {
    let mut p = SourceControlPanel::new();
    p.set_status(
        dummy_status_with_branch("main"),
        vec![ChangeEntry {
            path: "ch.py".into(),
            kind: ChangeKind::Modified,
            ..Default::default()
        }],
    );
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 20,
    };
    let mut buf = Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut p, area, &mut buf);
    // First render learns the row coordinate; click-select it.
    let row_y = (area.y..area.y + area.height)
        .find(|y| {
            let mut row = String::new();
            for x in area.x..area.x + area.width {
                row.push_str(buf[(x, *y)].symbol());
            }
            row.contains("ch.py")
        })
        .expect("ch.py row must render");
    p.select_change_at(row_y);
    let mut buf = Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut p, area, &mut buf);
    let actions = p
        .last_row_actions
        .expect("selected unstaged row must record action hit rects");
    assert_eq!(actions.entry_idx, 0);
    assert_eq!(
        buf[(actions.discard.x, actions.discard.y)].symbol(),
        DISCARD_GLYPH.to_string().as_str(),
        "discard icon must paint at the recorded hit cell"
    );
    assert_eq!(
        buf[(actions.stage.x, actions.stage.y)].symbol(),
        STAGE_GLYPH.to_string().as_str(),
        "stage icon must paint at the recorded hit cell"
    );
    // Stage sits to the right of discard, both before the badge.
    assert!(actions.discard.x < actions.stage.x);
}

#[test]
fn unselected_rows_paint_no_action_icons() {
    let mut p = SourceControlPanel::new();
    p.set_status(
        dummy_status_with_branch("main"),
        vec![
            ChangeEntry {
                path: "a.py".into(),
                kind: ChangeKind::Modified,
                ..Default::default()
            },
            ChangeEntry {
                path: "b.py".into(),
                kind: ChangeKind::Modified,
                ..Default::default()
            },
        ],
    );
    // No selection ⇒ no row should sprout action icons.
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 20,
    };
    let mut buf = Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut p, area, &mut buf);
    assert!(
        p.last_row_actions.is_none(),
        "no selection ⇒ no action icons"
    );
    let dump = buffer_to_string(&buf);
    assert!(
        !dump.contains(DISCARD_GLYPH),
        "discard glyph must not paint on any row when nothing is selected: {dump}"
    );
}

#[test]
fn selected_staged_row_exposes_only_an_unstage_action() {
    let mut p = SourceControlPanel::new();
    p.set_status(
        dummy_status_with_branch("main"),
        vec![ChangeEntry {
            path: "s.py".into(),
            kind: ChangeKind::StagedModified,
            ..Default::default()
        }],
    );
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 20,
    };
    let mut buf = Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut p, area, &mut buf);
    let row_y = (area.y..area.y + area.height)
        .find(|y| {
            let mut row = String::new();
            for x in area.x..area.x + area.width {
                row.push_str(buf[(x, *y)].symbol());
            }
            row.contains("s.py")
        })
        .expect("s.py row must render");
    p.select_change_at(row_y);
    let mut buf = Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut p, area, &mut buf);
    let actions = p
        .last_row_actions
        .expect("a selected staged row exposes the unstage action");
    assert!(actions.unstage.width > 0, "staged rows offer Unstage");
    assert_eq!(
        actions.stage,
        Rect::default(),
        "a staged row never offers Stage (it is already staged)"
    );
    assert_eq!(
        actions.discard,
        Rect::default(),
        "Discard lives on the Changes section, not Staged"
    );
    // The minus glyph lands in the unstage cell.
    assert_eq!(
        buf[(actions.unstage.x, actions.unstage.y)].symbol(),
        UNSTAGE_GLYPH.to_string(),
    );
}

#[test]
fn click_discard_action_returns_entry_idx_only_inside_the_rect() {
    let mut p = SourceControlPanel::new();
    p.last_row_actions = Some(RowActionAreas {
        entry_idx: 7,
        discard: Rect {
            x: 10,
            y: 5,
            width: 1,
            height: 1,
        },
        stage: Rect {
            x: 12,
            y: 5,
            width: 1,
            height: 1,
        },
        unstage: Rect::default(),
    });
    assert_eq!(p.click_discard_action(10, 5), Some(7));
    assert_eq!(
        p.click_discard_action(12, 5),
        None,
        "stage cell is not discard"
    );
    assert_eq!(
        p.click_discard_action(11, 5),
        None,
        "gap cell is not discard"
    );
    assert_eq!(p.click_discard_action(10, 6), None, "wrong row");
}

#[test]
fn click_stage_action_returns_entry_idx_only_inside_the_rect() {
    let mut p = SourceControlPanel::new();
    p.last_row_actions = Some(RowActionAreas {
        entry_idx: 3,
        discard: Rect {
            x: 10,
            y: 5,
            width: 1,
            height: 1,
        },
        stage: Rect {
            x: 12,
            y: 5,
            width: 1,
            height: 1,
        },
        unstage: Rect::default(),
    });
    assert_eq!(p.click_stage_action(12, 5), Some(3));
    assert_eq!(
        p.click_stage_action(10, 5),
        None,
        "discard cell is not stage"
    );
    assert_eq!(p.click_stage_action(13, 5), None, "outside stage rect");
}

#[test]
fn click_unstage_action_returns_entry_idx_only_inside_the_rect() {
    let mut p = SourceControlPanel::new();
    p.last_row_actions = Some(RowActionAreas {
        entry_idx: 4,
        discard: Rect::default(),
        stage: Rect::default(),
        unstage: Rect {
            x: 12,
            y: 5,
            width: 1,
            height: 1,
        },
    });
    assert_eq!(p.click_unstage_action(12, 5), Some(4));
    assert_eq!(
        p.click_unstage_action(11, 5),
        None,
        "gap cell is not unstage"
    );
    assert_eq!(p.click_unstage_action(12, 6), None, "wrong row");
}

#[test]
fn action_icons_are_suppressed_at_narrow_widths_so_the_path_stays_legible() {
    let mut p = SourceControlPanel::new();
    p.set_status(
        dummy_status_with_branch("main"),
        vec![ChangeEntry {
            path: "a.py".into(),
            kind: ChangeKind::Modified,
            ..Default::default()
        }],
    );
    // Width 10 leaves only a few cells for the path; reserving 4 cells
    // for icons would clobber it. The widget must skip icons rather
    // than overlap. Force selection directly since the row may sit
    // below the visible viewport at this tiny size.
    p.selected_change = Some(0);
    let area = Rect {
        x: 0,
        y: 0,
        width: 10,
        height: 60,
    };
    let mut buf = Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut p, area, &mut buf);
    assert!(
        p.last_row_actions.is_none(),
        "narrow rows must skip the action icons rather than overlap the path"
    );
}

#[test]
fn select_all_changes_loads_every_entry_into_multi_selection() {
    let mut p = SourceControlPanel::new();
    p.set_status(
        dummy_status_with_branch("main"),
        vec![
            ChangeEntry {
                path: "a".into(),
                kind: ChangeKind::Modified,
                ..Default::default()
            },
            ChangeEntry {
                path: "b".into(),
                kind: ChangeKind::Modified,
                ..Default::default()
            },
            ChangeEntry {
                path: "c".into(),
                kind: ChangeKind::Untracked,
                ..Default::default()
            },
        ],
    );
    let n = p.select_all_changes();
    assert_eq!(n, 3);
    assert!(p.multi_selection.contains(&0));
    assert!(p.multi_selection.contains(&1));
    assert!(p.multi_selection.contains(&2));
}

#[test]
fn clicking_a_row_resets_multi_selection_to_just_that_row() {
    let mut p = SourceControlPanel::new();
    p.set_status(
        dummy_status_with_branch("main"),
        vec![
            ChangeEntry {
                path: "a".into(),
                kind: ChangeKind::Modified,
                ..Default::default()
            },
            ChangeEntry {
                path: "b".into(),
                kind: ChangeKind::Modified,
                ..Default::default()
            },
        ],
    );
    p.select_all_changes();
    assert_eq!(p.multi_selection.len(), 2);
    // Render to populate hit areas, then "click" the second row.
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 20,
    };
    let mut buf = Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut p, area, &mut buf);
    let row_y = (area.y..area.y + area.height)
        .find(|y| {
            let mut row = String::new();
            for x in area.x..area.x + area.width {
                row.push_str(buf[(x, *y)].symbol());
            }
            row.contains("b") && !row.contains("a")
        })
        .expect("b row must render");
    p.select_change_at(row_y);
    assert_eq!(
        p.multi_selection.len(),
        1,
        "single click narrows to one row"
    );
}

fn change(path: &str, kind: ChangeKind) -> ChangeEntry {
    ChangeEntry {
        path: path.into(),
        kind,
        ..Default::default()
    }
}

/// #871: Up/Down walk the rows as drawn (Staged, Changes, Untracked,
/// not `entries` order) and keep the selection in view; Up from the
/// first row reports the end so the App hands back the message box.
#[test]
fn arrows_walk_the_change_list_in_drawn_order_and_keep_it_in_view() {
    let mut p = SourceControlPanel::new();
    p.set_status(
        dummy_status_with_branch("main"),
        vec![
            change("new.txt", ChangeKind::Untracked),
            change("edit.txt", ChangeKind::Modified),
            change("added.txt", ChangeKind::StagedAdded),
        ],
    );
    p.focused = true;
    p.last_input_area = Rect::new(0, 0, 20, 3);
    p.last_list_area = Rect::new(0, 10, 20, 2);
    assert!(p.cursor_screen_pos().is_some());
    assert!(p.focus_list());
    assert!(p.cursor_screen_pos().is_none(), "the list hides the caret");
    assert_eq!(p.selected_change, Some(2), "the staged row is drawn first");
    let mut walked = Vec::new();
    for _ in 0..3 {
        assert!(p.move_selection(false));
        walked.push((p.selected_change, p.scroll));
    }
    // Six lines (a header above each row) in a two-row viewport.
    assert_eq!(walked, [(Some(1), 2), (Some(0), 4), (Some(0), 4)]);
    assert!(p.move_selection(true) && p.move_selection(true));
    assert_eq!((p.selected_change, p.scroll), (Some(2), 0));
    assert!(!p.move_selection(true), "Up from the first row leaves");
}

/// #871: staging an untracked file moves it up git's order; the
/// selection follows the file, so the next key acts on the same row.
#[test]
fn a_refresh_keeps_the_selection_on_its_file() {
    let mut p = SourceControlPanel::new();
    let main = || dummy_status_with_branch("main");
    p.set_status(
        main(),
        vec![
            change("seed.txt", ChangeKind::Modified),
            change("scratch.txt", ChangeKind::Untracked),
        ],
    );
    p.selected_change = Some(1);
    p.list_focused = true;
    p.set_status(
        main(),
        vec![
            change("scratch.txt", ChangeKind::StagedAdded),
            change("seed.txt", ChangeKind::Modified),
        ],
    );
    assert_eq!(p.selected_change, Some(0));
    // A partly staged file stays on the row in the same section.
    let seed = |kind| change("seed.txt", kind);
    let rows = vec![seed(ChangeKind::StagedModified), seed(ChangeKind::Modified)];
    p.set_status(main(), rows.clone());
    p.selected_change = Some(1);
    p.set_status(
        main(),
        [vec![change("a", ChangeKind::Untracked)], rows].concat(),
    );
    assert_eq!(p.selected_change, Some(2));
    p.set_status(main(), Vec::new());
    assert!(!p.list_focused, "an empty list gives the keyboard back");
}

/// #871: the multi-selection follows its files through a refresh that
/// re-orders them, as the selection does, so Ctrl+S stages the rows
/// still highlighted; a file that left the list leaves it.
#[test]
fn a_refresh_keeps_the_multi_selection_on_its_files() {
    let mut p = SourceControlPanel::new();
    let main = || dummy_status_with_branch("main");
    p.set_status(
        main(),
        vec![
            change("seed.txt", ChangeKind::Modified),
            change("scratch.txt", ChangeKind::Untracked),
            change("gone.txt", ChangeKind::Untracked),
        ],
    );
    p.multi_selection = [1, 2].into_iter().collect();
    p.set_status(
        main(),
        vec![
            change("scratch.txt", ChangeKind::StagedAdded),
            change("seed.txt", ChangeKind::Modified),
        ],
    );
    assert_eq!(p.multi_selection, [0].into_iter().collect());
}

#[test]
fn multi_selected_rows_paint_a_visible_row_bg_even_when_not_primary() {
    let mut p = SourceControlPanel::new();
    p.set_status(
        dummy_status_with_branch("main"),
        vec![
            ChangeEntry {
                path: "a".into(),
                kind: ChangeKind::Modified,
                ..Default::default()
            },
            ChangeEntry {
                path: "b".into(),
                kind: ChangeKind::Modified,
                ..Default::default()
            },
        ],
    );
    p.select_all_changes();
    p.selected_change = Some(0); // only row 0 is primary
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 20,
    };
    let mut buf = Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut p, area, &mut buf);
    // Both rows must carry SOME bg distinct from panel bg.
    let row_b_y = (area.y..area.y + area.height)
        .find(|y| {
            let mut row = String::new();
            for x in area.x..area.x + area.width {
                row.push_str(buf[(x, *y)].symbol());
            }
            row.contains('b') && !row.contains('a')
        })
        .expect("b row must render");
    let mut secondary_has_bg = false;
    for x in 0..area.width {
        if let Some(bg) = buf[(x, row_b_y)].style().bg
            && bg != Color::Reset
        {
            secondary_has_bg = true;
            break;
        }
    }
    assert!(
        secondary_has_bg,
        "secondary multi-selected rows must paint a visible row bg so the Cmd+S scope is obvious"
    );
}

#[test]
fn split_commit_button_records_a_separate_caret_rect_at_default_widths() {
    let mut p = SourceControlPanel::new();
    p.set_status(dummy_status_with_branch("main"), Vec::new());
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 20,
    };
    let mut buf = Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut p, area, &mut buf);
    assert!(
        p.last_button_area.width > 0,
        "main commit button must paint"
    );
    assert!(
        p.last_commit_caret_area.width > 0,
        "split caret must paint alongside the main button at panel widths >= 20"
    );
    // Caret sits to the RIGHT of the main button with a 1-cell gap.
    assert_eq!(
        p.last_commit_caret_area.x,
        p.last_button_area.x + p.last_button_area.width + 1
    );
    // Hit-test rejects clicks landing on the caret cell.
    let caret_mid = (
        p.last_commit_caret_area.x + p.last_commit_caret_area.width / 2,
        p.last_commit_caret_area.y + 1,
    );
    assert!(!p.click_button(caret_mid.0, caret_mid.1));
    assert!(p.click_commit_caret(caret_mid.0, caret_mid.1));
}

#[test]
fn click_commit_menu_item_dispatches_on_recorded_rects() {
    let mut p = SourceControlPanel::new();
    p.commit_menu_item_areas = vec![
        (
            Rect {
                x: 10,
                y: 5,
                width: 20,
                height: 1,
            },
            CommitMenuItem::CommitAndPush,
        ),
        (
            Rect {
                x: 10,
                y: 6,
                width: 20,
                height: 1,
            },
            CommitMenuItem::Push,
        ),
        (
            Rect {
                x: 10,
                y: 7,
                width: 20,
                height: 1,
            },
            CommitMenuItem::ViewStagedDiff,
        ),
        (
            Rect {
                x: 10,
                y: 8,
                width: 20,
                height: 1,
            },
            CommitMenuItem::ViewDefaultBranchDiff,
        ),
    ];
    assert_eq!(
        p.click_commit_menu_item(15, 5),
        Some(CommitMenuItem::CommitAndPush)
    );
    assert_eq!(p.click_commit_menu_item(15, 6), Some(CommitMenuItem::Push));
    assert_eq!(
        p.click_commit_menu_item(15, 7),
        Some(CommitMenuItem::ViewStagedDiff)
    );
    assert_eq!(
        p.click_commit_menu_item(15, 8),
        Some(CommitMenuItem::ViewDefaultBranchDiff)
    );
    assert_eq!(p.click_commit_menu_item(5, 5), None, "outside the rects");
    assert_eq!(p.click_commit_menu_item(15, 9), None, "below the menu");
}

#[test]
fn commit_menu_item_label_inlines_default_branch_for_view_diff_item() {
    assert_eq!(
        CommitMenuItem::ViewDefaultBranchDiff.label("trunk"),
        "View Changes vs trunk"
    );
    assert_eq!(
        CommitMenuItem::CommitAndPush.label("anything"),
        "Commit & Push"
    );
    assert_eq!(CommitMenuItem::Push.label("anything"), "Push");
    assert_eq!(
        CommitMenuItem::ViewStagedDiff.label("anything"),
        "View Staged Changes"
    );
}

#[test]
fn commit_menu_items_carry_their_codicon_glyph() {
    // Nerd Fonts cod-* PUA values, verified against glyphnames.json
    // alongside cod-source-control (U+EA68) on the branch row.
    assert_eq!(CommitMenuItem::CommitAndPush.icon(), '\u{eac3}'); // cloud-upload
    assert_eq!(CommitMenuItem::Push.icon(), '\u{eaa1}'); // arrow-up
    assert_eq!(CommitMenuItem::ViewStagedDiff.icon(), '\u{eae1}'); // diff
    assert_eq!(CommitMenuItem::ViewPreviousCommitDiff.icon(), '\u{eafd}'); // git-compare
    assert_eq!(CommitMenuItem::ViewDefaultBranchDiff.icon(), '\u{eafd}'); // git-compare
}

#[test]
fn changes_count_returns_entry_total() {
    let mut p = SourceControlPanel::new();
    assert_eq!(p.changes_count(), 0);
    p.entries.push(ChangeEntry {
        path: "x".into(),
        kind: ChangeKind::Modified,
        ..Default::default()
    });
    p.entries.push(ChangeEntry {
        path: "y".into(),
        kind: ChangeKind::Untracked,
        ..Default::default()
    });
    assert_eq!(p.changes_count(), 2);
}

fn render_rows(p: &mut SourceControlPanel) -> Vec<String> {
    render_rows_in(p, 30)
}

fn render_rows_in(p: &mut SourceControlPanel, height: u16) -> Vec<String> {
    use ratatui::buffer::Buffer;
    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height,
    };
    let mut buf = Buffer::empty(area);
    ratatui::widgets::Widget::render(p, area, &mut buf);
    (0..area.height)
        .map(|y| (0..area.width).map(|x| buf[(x, y)].symbol()).collect())
        .collect()
}

/// Pasted line breaks are kept (CRLF and lone CR become LF), so a
/// message can have a body (#1241).
#[test]
fn pasted_line_breaks_are_kept_as_newlines() {
    let mut p = SourceControlPanel::new();
    p.insert_str("a\r\n\r\nb\rc\nd");
    assert_eq!(p.message, "a\n\nb\nc\nd");
    assert_eq!(p.message_cursor, p.message.chars().count());
}

/// The box grows a row per message line, so subject and body each show
/// on their own row, and the caret sits on its line.
#[test]
fn the_message_box_shows_each_line_on_its_own_row() {
    let mut p = SourceControlPanel::new();
    p.set_status(dummy_status_with_branch("main"), Vec::new());
    p.focused = true;
    p.insert_str("subject\n\nbody");
    let rows = render_rows(&mut p);
    let input = p.last_input_area;
    assert_eq!(input.height, 5, "three lines plus the border");
    let row = |n: u16| rows[(input.y + 1 + n) as usize].clone();
    assert!(row(0).contains("subject"), "{rows:#?}");
    assert!(!row(1).contains("body") && !row(1).contains("subject"));
    assert!(row(2).contains("body"), "{rows:#?}");
    assert_eq!(p.cursor_screen_pos(), Some((input.x + 2 + 4, input.y + 3)));
}

/// Past the row cap the box scrolls to keep the caret's line in view.
#[test]
fn a_long_message_scrolls_to_the_caret_line() {
    let mut p = SourceControlPanel::new();
    p.set_status(dummy_status_with_branch("main"), Vec::new());
    p.focused = true;
    let text: Vec<String> = (1..=12).map(|n| format!("line{n}")).collect();
    p.insert_str(&text.join("\n"));
    let rows = render_rows(&mut p);
    let input = p.last_input_area;
    assert_eq!(input.height, 2 + MESSAGE_MAX_ROWS as u16);
    let last = rows[(input.y + input.height - 2) as usize].clone();
    assert!(last.contains("line12"), "{rows:#?}");
    assert!(!rows.iter().any(|r| r.contains("line1 ")), "{rows:#?}");
    let (_, caret_y) = p.cursor_screen_pos().unwrap();
    assert_eq!(caret_y, input.y + input.height - 2);
}

fn long_message_panel(lines: usize) -> SourceControlPanel {
    let mut p = SourceControlPanel::new();
    p.set_status(dummy_status_with_branch("main"), Vec::new());
    p.focused = true;
    let text: Vec<String> = (1..=lines).map(|n| format!("line{n}")).collect();
    p.insert_str(&text.join("\n"));
    p
}

/// A tall message in a short panel shrinks its box instead of pushing
/// the Commit button off the bottom.
#[test]
fn a_long_message_leaves_room_for_the_commit_button() {
    let mut p = long_message_panel(5);
    let rows = render_rows_in(&mut p, 16);
    let button = p.last_button_area;
    assert_eq!(button.height, 3, "{rows:#?}");
    assert!(button.y + button.height <= 16, "{button:?}");
    assert!(
        rows[(button.y + 1) as usize].contains("Commit"),
        "{rows:#?}"
    );
    let input = p.last_input_area;
    assert!(input.y + input.height < button.y, "{input:?} {button:?}");
    let (_, caret_y) = p.cursor_screen_pos().unwrap();
    assert!(rows[caret_y as usize].contains("line5"), "{rows:#?}");
}

/// ...and leaves rows for the change list, so files can still be staged.
#[test]
fn a_long_message_leaves_room_for_the_change_list() {
    let mut p = long_message_panel(6);
    let rows = render_rows_in(&mut p, 20);
    assert!(
        p.last_list_area.height >= 2,
        "{:?} {rows:#?}",
        p.last_list_area
    );
    assert!(p.last_list_area.y + p.last_list_area.height <= 20);
}

/// Up and Down move the caret between message lines, keeping its
/// column where the line allows; at the first or last line they report
/// that nothing moved, so the panel can scroll its list as before.
#[test]
fn up_and_down_move_between_message_lines() {
    let mut p = SourceControlPanel::new();
    p.insert_str("abcdef\nxy\nlonger");
    assert!(p.move_cursor_up(), "from the last line up");
    assert_eq!(
        p.message_cursor,
        7 + 2,
        "column 6 clamps to the end of 'xy'"
    );
    assert!(p.move_cursor_up());
    assert_eq!(p.message_cursor, 2);
    assert!(!p.move_cursor_up(), "the first line has nothing above");
    assert!(p.move_cursor_down());
    assert!(p.move_cursor_down());
    assert_eq!(p.message_cursor, 10 + 2);
    assert!(!p.move_cursor_down(), "the last line has nothing below");
}

/// A one-line message keeps the three-row box.
#[test]
fn a_one_line_message_keeps_the_three_row_box() {
    let mut p = SourceControlPanel::new();
    p.set_status(dummy_status_with_branch("main"), Vec::new());
    p.insert_str("just a subject");
    render_rows(&mut p);
    assert_eq!(p.last_input_area.height, 3);
}
