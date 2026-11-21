//! The search panel's own tests, moved out of search.rs (#2062).

use super::*;

#[test]
fn workspace_regex_replace_honours_line_anchors_and_group_suffixes() {
    let opts = SearchOpts {
        use_regex: true,
        ..SearchOpts::default()
    };
    assert_eq!(
        replace_in_text("foo\nfoo\n", "^foo", "x", opts),
        Some((String::from("x\nx\n"), 2))
    );
    assert_eq!(
        replace_in_text("a foo\r\nb foo\r\n", "foo$", "x", opts),
        Some((String::from("a x\r\nb x\r\n"), 2))
    );
    assert_eq!(
        replace_in_text("foo", "(foo)", "$1_old", opts).map(|r| r.0),
        Some(String::from("foo_old"))
    );
}
use std::fs;
use tempfile::TempDir;

fn write(p: &Path, s: &str) {
    fs::write(p, s).unwrap();
}

#[test]
fn collect_matches_substring_case_insensitive() {
    let mut out = Vec::new();
    let content = "Hello World\nfoo bar\nHELLO again\nno match here";
    collect_matches_in_text(
        Path::new("a.txt"),
        content,
        "hello",
        SearchOpts::default(),
        &mut out,
    );
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].line_no, 1);
    assert_eq!(out[0].line_text, "Hello World");
    assert_eq!(out[1].line_no, 3);
    assert_eq!(out[1].line_text, "HELLO again");
}

fn highlighted(hit: &SearchHit, query: &str, opts: SearchOpts) -> Vec<String> {
    split_for_highlight(&hit.line_text, as_typed(query), opts)
        .into_iter()
        .filter(|(_, m)| *m)
        .map(|(t, _)| t)
        .collect()
}

#[test]
fn a_match_starting_in_the_indent_keeps_its_highlight() {
    // The preview trimmed the indent a whitespace query matched in, so
    // the result row showed no highlight at all.
    for (content, query) in [
        ("\tleading word\n", "\tleading"),
        ("    spaced out\n", "  spaced"),
        ("x\n      \n", "   "),
    ] {
        let mut out = Vec::new();
        collect_matches_in_text(
            Path::new("a.txt"),
            content,
            query,
            SearchOpts::default(),
            &mut out,
        );
        assert_eq!(out.len(), 1, "{query:?}");
        assert_eq!(
            highlighted(&out[0], query, SearchOpts::default()).first(),
            Some(&as_typed(query).to_string()),
            "{query:?} in {:?}",
            out[0].line_text
        );
    }
}

#[test]
fn a_regex_match_starting_in_the_indent_keeps_its_highlight() {
    let opts = SearchOpts {
        use_regex: true,
        ..SearchOpts::default()
    };
    let mut out = Vec::new();
    collect_matches_in_text(
        Path::new("a.txt"),
        "    foo()\n",
        r"^\s+foo",
        opts,
        &mut out,
    );
    assert_eq!(highlighted(&out[0], r"^\s+foo", opts), ["    foo"]);
}

#[test]
fn the_indent_is_still_trimmed_when_the_match_starts_after_it() {
    // Negative: a match past the indent keeps the compact preview.
    let mut out = Vec::new();
    collect_matches_in_text(
        Path::new("a.txt"),
        "        let x = 1;\n\t\tx  y\n",
        "x",
        SearchOpts::default(),
        &mut out,
    );
    assert_eq!(out[0].line_text, "let x = 1;");
    assert_eq!(out[1].line_text, "x  y");
    let mut out = Vec::new();
    collect_matches_in_text(
        Path::new("a.txt"),
        "\tif a  b {\n",
        "  b",
        SearchOpts::default(),
        &mut out,
    );
    assert_eq!(out[0].line_text, "if a  b {");
}

#[test]
fn collect_matches_truncates_very_long_lines() {
    let mut out = Vec::new();
    let long_line = "x".repeat(MAX_LINE_LEN + 50) + "needle";
    collect_matches_in_text(
        Path::new("a.txt"),
        &long_line,
        "needle",
        SearchOpts::default(),
        &mut out,
    );
    assert_eq!(out.len(), 1);
    // Overlong lines are cut to MAX_LINE_LEN with no trailing ellipsis.
    assert!(!out[0].line_text.contains('…'));
    assert_eq!(out[0].line_text.chars().count(), MAX_LINE_LEN);
}

#[test]
fn search_workspace_finds_matches_across_files() {
    let tmp = TempDir::new().unwrap();
    write(&tmp.path().join("a.txt"), "alpha\nbeta\nbananas\n");
    write(&tmp.path().join("b.rs"), "fn beta() {}\nlet bananas = 1;\n");
    let hits = search_workspace(tmp.path(), "bananas", SearchOpts::default());
    assert_eq!(hits.len(), 2);
    let names: Vec<String> = hits
        .iter()
        .map(|h| h.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert!(names.contains(&"a.txt".to_string()));
    assert!(names.contains(&"b.rs".to_string()));
}

/// #1648's repro: a git repo with `SECRET_TOKEN` in a source file, in
/// dotfiles and under a dot-directory, and in gitignored copies.
fn dotfile_repo() -> TempDir {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let ok = crate::test_git::test_git_command()
        .args(["init", "-q"])
        .current_dir(root)
        .status()
        .unwrap()
        .success();
    assert!(ok);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join(".github/workflows")).unwrap();
    fs::create_dir_all(root.join("node_modules/x")).unwrap();
    write(&root.join(".gitignore"), "node_modules/\n*.log\n");
    write(&root.join("src/app.js"), "const SECRET_TOKEN = 1;\n");
    write(&root.join(".env"), "SECRET_TOKEN=abc\n");
    write(&root.join(".github/workflows/ci.yml"), "SECRET_TOKEN\n");
    write(&root.join("node_modules/x/index.js"), "SECRET_TOKEN\n");
    write(&root.join("debug.log"), "SECRET_TOKEN\n");
    write(&root.join(".git/SECRET_TOKEN_NOTE"), "SECRET_TOKEN\n");
    tmp
}

fn hit_paths(root: &Path, query: &str) -> Vec<String> {
    let mut paths: Vec<String> = search_workspace(root, query, SearchOpts::default())
        .into_iter()
        .map(|h| {
            h.path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    paths.sort();
    paths
}

/// #1648: dotfiles and dot-directories that are not gitignored are
/// searched, as in VS Code and as Quick Open already lists them.
#[test]
fn search_finds_matches_in_dotfiles_and_dot_directories() {
    let tmp = dotfile_repo();
    assert_eq!(
        hit_paths(tmp.path(), "SECRET_TOKEN"),
        [".env", ".github/workflows/ci.yml", "src/app.js"]
    );
}

/// #1648 negative: gitignored files and the VCS's own directory stay
/// out of the results.
#[test]
fn search_still_skips_gitignored_files_and_vcs_internals() {
    let tmp = dotfile_repo();
    for vcs in [".hg", ".svn"] {
        fs::create_dir_all(tmp.path().join(vcs)).unwrap();
        write(&tmp.path().join(vcs).join("store"), "SECRET_TOKEN\n");
    }
    let paths = hit_paths(tmp.path(), "SECRET_TOKEN");
    for skipped in ["node_modules", "debug.log", ".git/", ".hg/", ".svn/"] {
        assert!(
            paths.iter().all(|p| !p.starts_with(skipped)),
            "{skipped} must not be searched: {paths:?}"
        );
    }
    assert!(hit_paths(tmp.path(), "core.bare").is_empty());
}

#[test]
fn search_workspace_returns_empty_for_blank_query() {
    let tmp = TempDir::new().unwrap();
    write(&tmp.path().join("a.txt"), "anything");
    assert!(search_workspace(tmp.path(), "", SearchOpts::default()).is_empty());
    assert!(search_workspace(tmp.path(), "   ", SearchOpts::default()).is_empty());
}

#[test]
fn search_workspace_skips_binary_or_unreadable_files() {
    let tmp = TempDir::new().unwrap();
    write(&tmp.path().join("text.txt"), "hello world");
    // A non-utf8 file: read_to_string returns Err, we silently skip.
    std::fs::write(tmp.path().join("blob.bin"), [0u8, 159, 146, 150]).unwrap();
    let hits = search_workspace(tmp.path(), "hello", SearchOpts::default());
    assert_eq!(hits.len(), 1);
    assert!(hits[0].path.ends_with("text.txt"));
}

#[test]
fn search_workspace_returns_every_match_with_no_arbitrary_cap() {
    // Regression: the engine used to silently truncate at 200 matches,
    // which the user (correctly) called rubbish. Five files, each with
    // 100 lines containing the needle, must all show up.
    let tmp = TempDir::new().unwrap();
    for i in 0..5 {
        let many = (0..100).map(|_| "needle\n").collect::<String>();
        write(&tmp.path().join(format!("f{i}.txt")), &many);
    }
    let hits = search_workspace(tmp.path(), "needle", SearchOpts::default());
    assert_eq!(
        hits.len(),
        500,
        "engine must surface every match - silent truncation is unacceptable"
    );
}

#[test]
fn search_panel_run_query_populates_hits_and_resets_selection() {
    let tmp = TempDir::new().unwrap();
    write(&tmp.path().join("x.txt"), "one\ntwo\nthree\n");
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    panel.query = "two".into();
    panel.run_query();
    assert_eq!(panel.hits.len(), 1);
    assert_eq!(panel.selected, 0);
}

/// Layout x-offset for the typed-content cell, relative to `inner.x`:
/// chevron (1) + gap (1) + field bar (1) + 2-cell magnifier glyph
/// (codicon + 1 cell gap) = 5.
const INPUT_TYPED_COL: u16 = 5;
/// Layout y-offset for the input content row, relative to `inner.y`:
/// header + blank + content = 2. The single filled field has no top
/// border row, so the query sits one row higher than the old 3-row box.
const INPUT_CONTENT_ROW: u16 = 2;

#[test]
fn cursor_sits_at_input_start_when_query_empty_and_focused() {
    use ratatui::buffer::Buffer;
    let tmp = TempDir::new().unwrap();
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    panel.focused = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 12,
    };
    let mut buf = Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
    let inner_x = panel.last_inner.x;
    let inner_y = panel.last_inner.y;
    // The caret (driven as the host's blinking hardware caret, like the
    // commit box) sits at the start of the input content when empty.
    assert_eq!(
        panel.cursor_screen_pos(),
        Some((inner_x + INPUT_TYPED_COL, inner_y + INPUT_CONTENT_ROW)),
        "caret must sit at the start of the input content area when the query is empty"
    );
}

#[test]
fn cursor_sits_after_typed_text_when_query_non_empty_and_focused() {
    use ratatui::buffer::Buffer;
    let tmp = TempDir::new().unwrap();
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    panel.focused = true;
    panel.query = String::from("foo");
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 12,
    };
    let mut buf = Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
    let inner_x = panel.last_inner.x;
    let inner_y = panel.last_inner.y;
    // INPUT_TYPED_COL + "foo" (3 cells) → caret sits 3 cells past the start
    // of typed content.
    assert_eq!(
        panel.cursor_screen_pos(),
        Some((inner_x + INPUT_TYPED_COL + 3, inner_y + INPUT_CONTENT_ROW)),
        "caret must sit immediately after the typed query"
    );
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

#[test]
fn header_row_says_search_in_upper_case() {
    // Mockup-driven: an uppercase "SEARCH" header sits at the top-left
    // of the inner area, in light grey — replacing the old dark-blue
    // title bar that used to overlap the outer border.
    use ratatui::buffer::Buffer;
    let tmp = TempDir::new().unwrap();
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 10,
    };
    let mut buf = Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
    let inner_x = panel.last_inner.x;
    let inner_y = panel.last_inner.y;
    let mut header = String::new();
    for x in inner_x..inner_x + 8 {
        header.push_str(buf[(x, inner_y)].symbol());
    }
    assert!(
        header.starts_with("SEARCH"),
        "first inner row must start with the uppercase SEARCH header; got: {header:?}"
    );
    // Outer-border title row must NOT carry a SEARCH chip — the mockup
    // has no title bar; the header lives inside the panel.
    let mut top_border = String::new();
    for x in area.x..area.x + area.width {
        top_border.push_str(buf[(x, area.y)].symbol());
    }
    assert!(
        !top_border.contains("SEARCH"),
        "outer border row must not carry a SEARCH title chip: {top_border:?}"
    );
}

#[test]
fn input_box_renders_a_thin_focused_border_three_rows_tall() {
    use ratatui::buffer::Buffer;
    let tmp = TempDir::new().unwrap();
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    // The boxed input; Croft Loch draws it as an underlined field.
    panel.theme = crate::theme::Theme::BLACK;
    panel.focused = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 12,
    };
    let mut buf = Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
    let dump = buffer_to_string(&buf);
    // The input box must render a single-line top-left corner glyph
    // somewhere in the upper half of the panel.
    assert!(
        dump.contains('┌') || dump.contains('╭'),
        "input box must have a top-left corner; got buffer:\n{dump}"
    );
    assert!(
        dump.contains('└') || dump.contains('╰'),
        "input box must have a bottom-left corner so the border reads as a 3-row box, not a single rule:\n{dump}"
    );
}

#[test]
fn rendering_at_narrow_widths_does_not_panic() {
    use ratatui::buffer::Buffer;
    let tmp = TempDir::new().unwrap();
    for width in 0u16..40 {
        for height in 0u16..20 {
            let mut panel = SearchPanel::new(tmp.path().to_path_buf());
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
            ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
        }
    }
}

#[test]
fn asterisk_keeps_breathing_room_against_the_outer_right_border() {
    // User report: the asterisk in `.*` was sitting flush against the
    // outer right border, looking crowded into the corner. The fix
    // mirrors the gap that already exists between the input box and
    // `Aa` so the toggle cluster reads as a contained group on both
    // sides.
    use ratatui::buffer::Buffer;
    let tmp = TempDir::new().unwrap();
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 12,
    };
    let mut buf = Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
    let inner = panel.last_inner;
    // `.*` toggle is 2 cells wide starting at toggle_regex_x.
    let regex_end = panel.toggle_regex_x + 1;
    let inner_right = inner.x + inner.width - 1;
    let right_gap = inner_right.saturating_sub(regex_end);
    assert!(
        right_gap >= 1,
        "asterisk must not sit flush against the outer right border (right_gap={right_gap})"
    );
}

#[test]
fn input_row_carries_a_chevron_and_pipe_separated_toggles() {
    use ratatui::buffer::Buffer;
    let tmp = TempDir::new().unwrap();
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    // The boxed input; Croft Loch draws it as an underlined field.
    panel.theme = crate::theme::Theme::BLACK;
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 10,
    };
    let mut buf = Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
    let dump = buffer_to_string(&buf);
    // VS Code's expand-to-replace chevron points RIGHT (▸) while the
    // Replace row is collapsed and flips DOWN (▾) once expanded. A fresh
    // panel starts collapsed, so the right chevron must render.
    assert!(
        dump.contains('▸'),
        "collapsed panel must show a right-pointing expand chevron on the left:\n{dump}"
    );
    // Now expand the Replace row: the chevron flips to a down-arrow.
    panel.toggle_replace();
    let mut buf2 = Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut panel, area, &mut buf2);
    let dump2 = buffer_to_string(&buf2);
    assert!(
        dump2.contains('▾'),
        "expanded panel must flip the chevron to a down-arrow:\n{dump2}"
    );
    // Toggles separated by pipe '│' so the trio reads "Aa │ ab │ .*".
    assert!(
        dump.contains("Aa") && dump.contains("ab") && dump.contains(".*"),
        "all three toggle glyphs must render:\n{dump}"
    );
    let pipes = dump.matches('│').count();
    assert!(
        pipes >= 2,
        "at least two pipe separators must sit between the three toggles; got {pipes} pipes in:\n{dump}"
    );
}

#[test]
fn results_start_below_the_input_box_with_a_separator_in_between() {
    use ratatui::buffer::Buffer;
    let tmp = TempDir::new().unwrap();
    write(&tmp.path().join("a.txt"), "needle\n");
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    panel.query = String::from("needle");
    panel.run_query();
    assert_eq!(panel.hits.len(), 1);
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 14,
    };
    let mut buf = Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
    let dump = buffer_to_string(&buf);
    let row_with_needle = dump
        .lines()
        .position(|l| l.contains("needle") && l.contains("a.txt"))
        .expect("result row with needle must render");
    assert!(
        row_with_needle >= 5,
        "result row must sit below the input box (expected row >= 5, got {row_with_needle}):\n{dump}"
    );
}

#[test]
fn search_panel_does_not_render_paste_button_in_input_row() {
    use ratatui::buffer::Buffer;
    let tmp = TempDir::new().unwrap();
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    panel.focused = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 5,
    };
    let mut buf = Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
    assert_eq!(
        panel.paste_button_w, 0,
        "paste button should not reserve cells"
    );
    let mut collected = String::new();
    for x in area.x..area.x + area.width {
        collected.push_str(buf[(x, 1)].symbol());
    }
    assert!(
        !collected.to_lowercase().contains("paste"),
        "input row should not visibly contain a Paste button, got: {collected:?}"
    );
}

#[test]
fn paste_button_at_returns_false_when_button_hidden() {
    use ratatui::buffer::Buffer;
    let tmp = TempDir::new().unwrap();
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    panel.focused = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 5,
    };
    let mut buf = Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
    assert_eq!(panel.paste_button_w, 0);
    assert!(!panel.paste_button_at(0, 1));
    assert!(!panel.paste_button_at(40, 1));
}

#[test]
fn split_highlight_returns_whole_line_when_needle_empty() {
    let segs = split_for_highlight("hello world", "", SearchOpts::default());
    assert_eq!(segs, vec![(String::from("hello world"), false)]);
}

#[test]
fn split_highlight_returns_whole_line_when_no_match() {
    let segs = split_for_highlight("hello world", "xyz", SearchOpts::default());
    assert_eq!(segs, vec![(String::from("hello world"), false)]);
}

#[test]
fn split_highlight_marks_each_match_run_case_insensitive() {
    let segs = split_for_highlight(
        "The Quick brown fox jumps over the QUICK fence",
        "quick",
        SearchOpts::default(),
    );
    // Two runs of "quick" (mixed case), three non-match tails.
    let matches: Vec<&String> = segs
        .iter()
        .filter_map(|(s, m)| if *m { Some(s) } else { None })
        .collect();
    assert_eq!(matches.len(), 2);
    // Original-case slice must be preserved in the matched segment.
    assert_eq!(matches[0], "Quick");
    assert_eq!(matches[1], "QUICK");
    // Concatenating all segments must reproduce the original line.
    let joined: String = segs.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(joined, "The Quick brown fox jumps over the QUICK fence");
}

#[test]
fn split_highlight_handles_match_at_start_and_end() {
    let segs = split_for_highlight("foo bar foo", "foo", SearchOpts::default());
    let joined: String = segs.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(joined, "foo bar foo");
    assert_eq!(
        segs.first().map(|(_, m)| *m),
        Some(true),
        "first seg is match"
    );
    assert_eq!(
        segs.last().map(|(_, m)| *m),
        Some(true),
        "last seg is match"
    );
}

fn collect_until_done(
    rx: &std::sync::mpsc::Receiver<SearchEvent>,
    deadline: std::time::Duration,
) -> (Option<String>, Vec<SearchHit>, bool) {
    let until = std::time::Instant::now() + deadline;
    let mut last_q: Option<String> = None;
    let mut hits: Vec<SearchHit> = Vec::new();
    let mut saw_done = false;
    while std::time::Instant::now() < until {
        let remaining = until.saturating_duration_since(std::time::Instant::now());
        match rx.recv_timeout(remaining.max(std::time::Duration::from_millis(50))) {
            Ok(SearchEvent::Hits(q, _, batch)) => {
                last_q = Some(q);
                hits.extend(batch);
            }
            Ok(SearchEvent::Done(q, _)) => {
                last_q = Some(q);
                saw_done = true;
                break;
            }
            Err(_) => break,
        }
    }
    (last_q, hits, saw_done)
}

#[test]
fn search_worker_loop_searches_dirty_buffer_and_skips_stale_disk_copy() {
    // Disk holds the OLD name; the unsaved editor buffer holds the NEW
    // name. Dirty-aware search must find the new name (from the buffer)
    // and must NOT find the old name (the dirty file's stale disk copy is
    // skipped). This is the VS Code / Zed behavior the reported bug lacked.
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("a.txt");
    write(&path, "oldname\n");
    let (q_tx, q_rx) = std::sync::mpsc::channel::<SearchRequest>();
    let (e_tx, e_rx) = std::sync::mpsc::channel::<SearchEvent>();
    let root = tmp.path().to_path_buf();
    let join = std::thread::spawn(move || search_worker_loop(root, q_rx, e_tx));

    q_tx.send(SearchRequest::SetDirtyBuffers(vec![(
        path.clone(),
        "newname\n".into(),
    )]))
    .unwrap();

    q_tx.send(SearchRequest::Query(
        "newname".into(),
        SearchOpts::default(),
    ))
    .unwrap();
    let (_, hits, done) = collect_until_done(&e_rx, std::time::Duration::from_secs(2));
    assert!(done);
    assert_eq!(hits.len(), 1, "the unsaved buffer's new name must be found");
    assert_eq!(hits[0].line_text, "newname");

    q_tx.send(SearchRequest::Query(
        "oldname".into(),
        SearchOpts::default(),
    ))
    .unwrap();
    let (_, stale, done2) = collect_until_done(&e_rx, std::time::Duration::from_secs(2));
    assert!(done2);
    assert_eq!(
        stale.len(),
        0,
        "the stale on-disk copy of a dirty file must be skipped"
    );

    drop(q_tx);
    join.join().unwrap();
}

#[test]
fn search_worker_loop_returns_hits_for_a_typed_query() {
    let tmp = TempDir::new().unwrap();
    write(&tmp.path().join("a.txt"), "one\nneedle\nthree\n");
    let (q_tx, q_rx) = std::sync::mpsc::channel::<SearchRequest>();
    let (e_tx, e_rx) = std::sync::mpsc::channel::<SearchEvent>();
    let root = tmp.path().to_path_buf();
    let join = std::thread::spawn(move || search_worker_loop(root, q_rx, e_tx));
    q_tx.send(SearchRequest::Query("needle".into(), SearchOpts::default()))
        .unwrap();
    let (q, hits, done) = collect_until_done(&e_rx, std::time::Duration::from_secs(2));
    assert_eq!(q.as_deref(), Some("needle"));
    assert_eq!(hits.len(), 1);
    assert!(done, "worker must terminate the stream with a Done event");
    drop(q_tx);
    join.join().unwrap();
}

#[test]
fn search_worker_loop_streams_per_file_batches_for_a_typed_query() {
    // Three files each containing matches must surface as separate Hits
    // batches followed by exactly one Done. This pins the streaming
    // protocol so the UI can show "N matches (still searching)".
    let tmp = TempDir::new().unwrap();
    write(&tmp.path().join("a.txt"), "needle\n");
    write(&tmp.path().join("b.txt"), "needle\nneedle\n");
    write(&tmp.path().join("c.txt"), "needle\n");
    let (q_tx, q_rx) = std::sync::mpsc::channel::<SearchRequest>();
    let (e_tx, e_rx) = std::sync::mpsc::channel::<SearchEvent>();
    let root = tmp.path().to_path_buf();
    let join = std::thread::spawn(move || search_worker_loop(root, q_rx, e_tx));
    q_tx.send(SearchRequest::Query("needle".into(), SearchOpts::default()))
        .unwrap();
    let mut total = 0usize;
    let mut hits_events = 0usize;
    let mut done_events = 0usize;
    let until = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while std::time::Instant::now() < until {
        match e_rx.recv_timeout(std::time::Duration::from_millis(500)) {
            Ok(SearchEvent::Hits(_, _, batch)) => {
                hits_events += 1;
                total += batch.len();
            }
            Ok(SearchEvent::Done(_, _)) => {
                done_events += 1;
                break;
            }
            Err(_) => break,
        }
    }
    drop(q_tx);
    join.join().unwrap();
    assert_eq!(total, 4, "every match across all files must arrive");
    assert!(
        hits_events >= 3,
        "expected per-file batches, got {hits_events}"
    );
    assert_eq!(done_events, 1, "exactly one Done sentinel per request");
}

#[test]
fn search_worker_loop_coalesces_a_burst_of_keystrokes() {
    // Simulate a fast typist sending o, on, one. Worker must coalesce
    // and only run the latest, returning hits for "one" — not for the
    // intermediate prefixes.
    let tmp = TempDir::new().unwrap();
    write(&tmp.path().join("a.txt"), "alpha one beta\nzeta\n");
    let (q_tx, q_rx) = std::sync::mpsc::channel::<SearchRequest>();
    let (e_tx, e_rx) = std::sync::mpsc::channel::<SearchEvent>();
    let root = tmp.path().to_path_buf();
    let join = std::thread::spawn(move || search_worker_loop(root, q_rx, e_tx));
    q_tx.send(SearchRequest::Query("o".into(), SearchOpts::default()))
        .unwrap();
    q_tx.send(SearchRequest::Query("on".into(), SearchOpts::default()))
        .unwrap();
    q_tx.send(SearchRequest::Query("one".into(), SearchOpts::default()))
        .unwrap();
    let (q, hits, done) = collect_until_done(&e_rx, std::time::Duration::from_secs(2));
    assert_eq!(
        q.as_deref(),
        Some("one"),
        "coalesce must drop intermediate prefixes"
    );
    assert_eq!(hits.len(), 1);
    assert!(done);
    drop(q_tx);
    join.join().unwrap();
}

#[test]
fn search_worker_loop_resets_debounce_on_each_keystroke() {
    // Trailing-edge debounce: keys arriving within the quiet window
    // (200 ms) restart the timer, so the user typing "one" with
    // 100 ms between each keystroke must fire EXACTLY ONE search at
    // the end - for "one" - not three searches for "o", "on", "one".
    let tmp = TempDir::new().unwrap();
    write(&tmp.path().join("a.txt"), "alpha one beta\nzeta\n");
    let (q_tx, q_rx) = std::sync::mpsc::channel::<SearchRequest>();
    let (e_tx, e_rx) = std::sync::mpsc::channel::<SearchEvent>();
    let root = tmp.path().to_path_buf();
    let join = std::thread::spawn(move || search_worker_loop(root, q_rx, e_tx));
    // Each gap is well under SEARCH_DEBOUNCE (200 ms), so each new
    // send resets the timer and no search should fire mid-burst.
    q_tx.send(SearchRequest::Query("o".into(), SearchOpts::default()))
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(100));
    q_tx.send(SearchRequest::Query("on".into(), SearchOpts::default()))
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(100));
    q_tx.send(SearchRequest::Query("one".into(), SearchOpts::default()))
        .unwrap();
    // Now stop typing and let the debounce expire + the scan finish.
    let mut dones: Vec<String> = Vec::new();
    let until = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while std::time::Instant::now() < until {
        if let Ok(SearchEvent::Done(q, _)) =
            e_rx.recv_timeout(std::time::Duration::from_millis(200))
        {
            dones.push(q)
        }
    }
    drop(q_tx);
    join.join().unwrap();
    assert_eq!(
        dones,
        vec!["one".to_string()],
        "exactly one search should have fired - for the trailing query - not one per keystroke"
    );
}

#[test]
fn search_worker_loop_cancels_in_flight_scan_when_a_new_request_arrives() {
    // User-reported "0 matches" bug: a fast typist hits "c", which
    // matches almost every line of every file. The worker used to
    // block on that giant scan and ignore "cl"/"cla" until "c"
    // finished, so the user would stare at "0 matches" while typing.
    // Workspace: 200 files × 20000 lines = 4M matches. Without
    // cancellation, the "c" scan takes many seconds. With cancellation
    // wired up, switching to a no-match query must produce Done within
    // a tight window.
    let tmp = TempDir::new().unwrap();
    for i in 0..200 {
        let many = (0..20000)
            .map(|n| format!("ccc line {n}\n"))
            .collect::<String>();
        write(&tmp.path().join(format!("f{i}.txt")), &many);
    }
    let (q_tx, q_rx) = std::sync::mpsc::channel::<SearchRequest>();
    let (e_tx, e_rx) = std::sync::mpsc::channel::<SearchEvent>();
    let root = tmp.path().to_path_buf();
    let join = std::thread::spawn(move || search_worker_loop(root, q_rx, e_tx));
    q_tx.send(SearchRequest::Query("c".into(), SearchOpts::default()))
        .unwrap();
    // Stage on the scan observably RUNNING — the first Hits batch —
    // not on sleeping past the debounce: a sleep equal to the
    // debounce races the worker's own timer on a knife edge, and
    // when the send wins the new query displaces "c" in the debounce
    // loop, silently skipping the cancellation path this test exists
    // to cover (#68).
    let mut c_hits = 0usize;
    let staging = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while c_hits == 0 && std::time::Instant::now() < staging {
        if let Ok(SearchEvent::Hits(q, _, hits)) =
            e_rx.recv_timeout(std::time::Duration::from_millis(100))
            && q == "c"
        {
            c_hits += hits.len();
        }
    }
    assert!(
        c_hits > 0,
        "staging: the \"c\" scan never started streaming"
    );
    q_tx.send(SearchRequest::Query(
        "zzznoneatall".into(),
        SearchOpts::default(),
    ))
    .unwrap();
    // The cancellation proof is structural, not wall-clock (#68): a
    // cancelled "c" scan is cut short, streaming fewer than its full
    // 4M matches before its Done; an uncancelled one streams every
    // single match first. A tight arrival deadline on the second Done
    // measured the same fact through scheduler latency and flaked
    // under suite load — the long timeout here only guards a hang.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut second_done = false;
    while std::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        match e_rx.recv_timeout(remaining.max(std::time::Duration::from_millis(50))) {
            Ok(SearchEvent::Hits(q, _, hits)) if q == "c" => c_hits += hits.len(),
            Ok(SearchEvent::Done(q, _)) if q == "zzznoneatall" => {
                second_done = true;
                break;
            }
            _ => {}
        }
    }
    drop(q_tx);
    join.join().unwrap();
    assert!(
        second_done,
        "worker must emit Done for the new query after cancelling the in-flight scan"
    );
    assert!(
        c_hits < 4_000_000,
        "the in-flight scan must be cancelled, not run to completion; it streamed all {c_hits} matches"
    );
}

#[test]
fn search_worker_loop_short_circuits_empty_queries() {
    let tmp = TempDir::new().unwrap();
    write(&tmp.path().join("a.txt"), "anything\n");
    let (q_tx, q_rx) = std::sync::mpsc::channel::<SearchRequest>();
    let (e_tx, e_rx) = std::sync::mpsc::channel::<SearchEvent>();
    let root = tmp.path().to_path_buf();
    let join = std::thread::spawn(move || search_worker_loop(root, q_rx, e_tx));
    q_tx.send(SearchRequest::Query("".into(), SearchOpts::default()))
        .unwrap();
    let (q, hits, done) = collect_until_done(&e_rx, std::time::Duration::from_secs(2));
    assert_eq!(q.as_deref(), Some(""));
    assert!(hits.is_empty());
    assert!(done, "empty query still emits a Done sentinel");
    drop(q_tx);
    join.join().unwrap();
}

#[test]
fn search_worker_loop_targets_the_directory_set_by_the_latest_set_root() {
    let dir_a = TempDir::new().unwrap();
    write(&dir_a.path().join("a.txt"), "alpha only\n");
    let dir_b = TempDir::new().unwrap();
    write(&dir_b.path().join("b.txt"), "the needle is here\n");
    let (q_tx, q_rx) = std::sync::mpsc::channel::<SearchRequest>();
    let (e_tx, e_rx) = std::sync::mpsc::channel::<SearchEvent>();
    let start_root = dir_a.path().to_path_buf();
    let join = std::thread::spawn(move || search_worker_loop(start_root, q_rx, e_tx));
    q_tx.send(SearchRequest::SetRoot(dir_b.path().to_path_buf()))
        .unwrap();
    q_tx.send(SearchRequest::Query("needle".into(), SearchOpts::default()))
        .unwrap();
    let (q, hits, done) = collect_until_done(&e_rx, std::time::Duration::from_secs(2));
    assert_eq!(q.as_deref(), Some("needle"));
    assert_eq!(
        hits.len(),
        1,
        "after SetRoot, the worker must search the new directory, not the one captured at spawn"
    );
    assert!(
        hits[0].path.starts_with(dir_b.path()),
        "the hit must come from the directory the Explorer now points at"
    );
    assert!(done);
    drop(q_tx);
    join.join().unwrap();
}

#[test]
fn search_opts_default_matches_legacy_case_insensitive_substring() {
    let mut out = Vec::new();
    let opts = SearchOpts::default();
    collect_matches_in_text(
        Path::new("a.txt"),
        "Hello World\nfoo bar\nHELLO again\nno match",
        "hello",
        opts,
        &mut out,
    );
    assert_eq!(out.len(), 2, "default opts mirror the original behaviour");
}

#[test]
fn search_opts_case_sensitive_excludes_different_case() {
    let mut out = Vec::new();
    let opts = SearchOpts {
        case_sensitive: true,
        ..Default::default()
    };
    collect_matches_in_text(
        Path::new("a.txt"),
        "Hello World\nhello again\nHELLO loud",
        "hello",
        opts,
        &mut out,
    );
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].line_text, "hello again");
}

#[test]
fn search_opts_whole_word_excludes_partial_matches() {
    let mut out = Vec::new();
    let opts = SearchOpts {
        whole_word: true,
        ..Default::default()
    };
    collect_matches_in_text(
        Path::new("a.txt"),
        "cat sat\ncategory animal\ncat\nbobcat sleeps",
        "cat",
        opts,
        &mut out,
    );
    // Only the lines where 'cat' is a standalone word should match.
    let texts: Vec<&str> = out.iter().map(|h| h.line_text.as_str()).collect();
    assert_eq!(texts, vec!["cat sat", "cat"]);
}

#[test]
fn search_opts_regex_finds_pattern_class() {
    let mut out = Vec::new();
    let opts = SearchOpts {
        use_regex: true,
        ..Default::default()
    };
    collect_matches_in_text(
        Path::new("a.txt"),
        "let x = 42;\nlet y = abc;\nlet z = 13;\nno number",
        r"\d+",
        opts,
        &mut out,
    );
    let texts: Vec<&str> = out.iter().map(|h| h.line_text.as_str()).collect();
    assert_eq!(texts, vec!["let x = 42;", "let z = 13;"]);
}

#[test]
fn search_opts_regex_honours_case_sensitive_flag() {
    let mut out = Vec::new();
    let opts = SearchOpts {
        use_regex: true,
        case_sensitive: true,
        ..Default::default()
    };
    collect_matches_in_text(Path::new("a.txt"), "Foo\nfoo\nFOO", r"foo", opts, &mut out);
    let texts: Vec<&str> = out.iter().map(|h| h.line_text.as_str()).collect();
    assert_eq!(texts, vec!["foo"], "regex must respect case-sensitive flag");
}

#[test]
fn search_opts_invalid_regex_returns_empty_quietly() {
    let mut out = Vec::new();
    let opts = SearchOpts {
        use_regex: true,
        ..Default::default()
    };
    collect_matches_in_text(
        Path::new("a.txt"),
        "anything\nat all",
        r"[unclosed",
        opts,
        &mut out,
    );
    assert!(
        out.is_empty(),
        "invalid regex must not crash, just yield no hits"
    );
}

/// #1496: a regex that won't compile is an error, not "no matches".
/// #1552 negative: a typo is still one, though look-around now runs; it
/// keeps the fast engine's message, not the fallback's.
#[test]
fn a_regex_that_will_not_compile_names_the_problem() {
    let re = SearchOpts {
        use_regex: true,
        ..SearchOpts::default()
    };
    assert_eq!(
        query_error("foo(", re).as_deref(),
        Some("Invalid regex: unclosed group")
    );
    let typo = query_error(r"(?<=@)\w+(", re).unwrap();
    assert!(typo.starts_with("Invalid regex: "), "{typo}");
}

fn regex_opts() -> SearchOpts {
    SearchOpts {
        use_regex: true,
        ..SearchOpts::default()
    }
}

fn hit_lines(content: &str, query: &str, opts: SearchOpts) -> Vec<(usize, String)> {
    let mut hits = Vec::new();
    collect_matches_in_text(Path::new("a.py"), content, query, opts, &mut hits);
    hits.into_iter().map(|h| (h.line_no, h.line_text)).collect()
}

/// #1552: a look-ahead, a look-behind and a backreference, which VS Code
/// runs through PCRE2, run, and carry no error.
#[test]
fn look_around_and_backreferences_search() {
    let re = regex_opts();
    let prices = "price: 10 USD\nprice: 20 EUR\n";
    assert_eq!(query_error(r"\d+ (?!USD)", re), None);
    assert_eq!(
        hit_lines(prices, r"\d+ (?!USD)", re),
        [(2, String::from("price: 20 EUR"))]
    );
    assert_eq!(
        hit_lines("to: ada@host\nno at here\n", r"(?<=@)\w+", re),
        [(1, String::from("to: ada@host"))]
    );
    assert_eq!(query_error(r"(\w+) \1", re), None);
    assert_eq!(
        hit_lines("one two\nsaw the the cat\n", r"(\w+) \1", re),
        [(2, String::from("saw the the cat"))]
    );
}

/// #1552: the fallback keeps the case and whole-word toggles.
#[test]
fn look_around_keeps_case_and_whole_word() {
    let text = "a@Host\nb@hostname\n";
    let insensitive = regex_opts();
    assert_eq!(hit_lines(text, r"(?<=@)host", insensitive).len(), 2);
    let sensitive = SearchOpts {
        case_sensitive: true,
        ..insensitive
    };
    assert_eq!(
        hit_lines(text, r"(?<=@)host", sensitive),
        [(2, String::from("b@hostname"))]
    );
    let word = SearchOpts {
        whole_word: true,
        ..insensitive
    };
    assert_eq!(
        hit_lines(text, r"(?<=@)host", word),
        [(1, String::from("a@Host"))]
    );
}

/// #1552: a result row highlights what the look-ahead matched, and the
/// count is of those matches.
#[test]
fn look_ahead_matches_are_highlighted_and_counted() {
    let re = regex_opts();
    assert_eq!(
        split_for_highlight("10 USD 20 EUR 30 GBP", r"\d+ (?!USD)", re),
        [
            (String::from("10 USD "), false),
            (String::from("20 "), true),
            (String::from("EUR "), false),
            (String::from("30 "), true),
            (String::from("GBP"), false),
        ]
    );
    let mut hits = Vec::new();
    collect_matches_in_text(
        Path::new("a"),
        "10 USD 20 EUR 30 GBP\n",
        r"\d+ (?!USD)",
        re,
        &mut hits,
    );
    assert_eq!(hits[0].matches, 2);
}

/// #1552: with whole-word on, a fancy-only match that starts or ends
/// with punctuation is highlighted where the search counts it, by the
/// matcher's `(?<!\w)` / `(?!\w)` rule, not `\b`.
#[test]
fn whole_word_look_ahead_highlights_by_the_search_rule() {
    let word = SearchOpts {
        whole_word: true,
        ..regex_opts()
    };
    let mut hits = Vec::new();
    collect_matches_in_text(Path::new("a"), "- \n", "-(?= )", word, &mut hits);
    assert_eq!(hits.len(), 1);
    assert_eq!(
        split_for_highlight("- ", "-(?= )", word),
        [(String::from("-"), true), (String::from(" "), false)]
    );
    assert_eq!(
        split_for_highlight("a- ", "-(?= )", word),
        [(String::from("a- "), false)]
    );
}

/// #1552: Replace in Files runs a backreference too, `$1` and all, and
/// its preview shows the same.
#[test]
fn replace_with_a_backreference_pattern() {
    let re = regex_opts();
    assert_eq!(
        replace_in_text("saw the the cat\nok ok\none\n", r"\b(\w+) \1\b", "$1", re),
        Some((String::from("saw the cat\nok\none\n"), 2))
    );
    assert_eq!(
        replace_in_text("price: 10 USD 20 EUR\n", r"(\d+) (?!USD)", "${1}€ ", re),
        Some((String::from("price: 10 USD 20€ EUR\n"), 1))
    );
    assert_eq!(
        expand_replacement("the the", r"(\w+) \1", "[$1]", re),
        "[the]"
    );
}

/// #1552 negative: a pattern the fast engine takes still runs on it, and
/// literal mode never falls back: `(?!USD)` there is just text.
#[test]
fn the_fallback_is_only_for_patterns_that_need_it() {
    let re = regex_opts();
    assert!(matches!(
        compile_matcher(r"\d+ USD", re),
        Ok(SearchMatcher::Fast(_))
    ));
    assert!(matches!(
        compile_matcher(r"\d+ (?!USD)", re),
        Ok(SearchMatcher::Fancy(_))
    ));
    let literal = SearchOpts::default();
    assert!(hit_lines("price: 10 USD\n", "(?!USD)", literal).is_empty());
    assert_eq!(
        hit_lines("odd (?!USD) text\n", "(?!USD)", literal),
        [(1, String::from("odd (?!USD) text"))]
    );
}

/// #1496 negative: valid patterns, literal mode and an empty box carry
/// no error, so the count header still shows.
#[test]
fn valid_and_literal_queries_have_no_regex_error() {
    let re = SearchOpts {
        use_regex: true,
        ..SearchOpts::default()
    };
    assert_eq!(query_error(r"foo\(", re), None);
    assert_eq!(query_error(r"get_(user_\w+)", re), None);
    assert_eq!(query_error("", re), None);
    // The same `foo(` is a plain literal with the regex chip off.
    assert_eq!(query_error("foo(", SearchOpts::default()), None);
    let whole_word = SearchOpts {
        whole_word: true,
        ..re
    };
    assert_eq!(query_error("foo", whole_word), None);
}

/// #1496: the sidebar shows the error where the count would be, not
/// "0 results in 0 files" over a search that never ran.
#[test]
fn the_sidebar_shows_the_regex_error_in_place_of_the_count() {
    let tmp = TempDir::new().unwrap();
    write(&tmp.path().join("a.py"), "def foo(x):\n    return foo(1)\n");
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    panel.opts.use_regex = true;
    panel.query = "foo(".into();
    panel.run_query();
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 14,
    };
    let mut buf = Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
    let text = buffer_to_string(&buf);
    assert!(text.contains("Invalid regex: unclosed group"), "{text}");
    assert!(!text.contains("0 results"), "{text}");
    // Fixing the pattern brings the count back.
    panel.query = r"foo\(".into();
    panel.run_query();
    let mut buf = Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
    let text = buffer_to_string(&buf);
    assert!(text.contains("2 results in 1 file"), "{text}");
    assert!(!text.contains("Invalid regex"), "{text}");
}

/// #1496: the sidebar is narrow, so a long error (look-around's runs to
/// 70 columns) wraps onto the rows below instead of being cut off, and
/// the result list starts under its last line.
#[test]
fn a_long_regex_error_wraps_in_a_narrow_sidebar() {
    let tmp = TempDir::new().unwrap();
    write(&tmp.path().join("a.py"), "foo(1)\n");
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    panel.opts.use_regex = true;
    // Unclosed, so neither engine runs it (#1552): the fast engine's
    // look-around message is the long one this wraps.
    panel.query = r"foo(?=\()(".into();
    panel.run_query();
    let area = Rect {
        x: 0,
        y: 0,
        width: 32,
        height: 14,
    };
    let mut buf = Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
    let text = buffer_to_string(&buf);
    let flat: String = text
        .lines()
        .map(|l| l.trim_matches(|c: char| c == '│' || c.is_whitespace()))
        .collect::<Vec<_>>()
        .join(" ");
    assert!(flat.contains("is not supported"), "{text}");
    let error_rows = text
        .lines()
        .filter(|l| l.contains("look-") || l.contains("supported"))
        .count();
    assert!(error_rows >= 2, "{text}");
    let header = panel.last_inner.y + panel.results_start_offset;
    let last_error_row = text.lines().position(|l| l.contains("supported")).unwrap() as u16;
    assert!(
        header > last_error_row,
        "results start under the error\n{text}"
    );
}

#[test]
fn toggle_at_maps_a_click_on_each_glyph_to_the_right_kind() {
    use ratatui::buffer::Buffer;
    let tmp = TempDir::new().unwrap();
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    panel.focused = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 12,
    };
    let mut buf = Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
    let y = panel.toggle_y;
    // Each toggle is 2 cells; clicking either cell on the same row
    // should map to its kind.
    assert_eq!(
        panel.toggle_at(panel.toggle_case_x, y),
        Some(SearchToggle::CaseSensitive)
    );
    assert_eq!(
        panel.toggle_at(panel.toggle_case_x + 1, y),
        Some(SearchToggle::CaseSensitive)
    );
    assert_eq!(
        panel.toggle_at(panel.toggle_word_x, y),
        Some(SearchToggle::WholeWord)
    );
    assert_eq!(
        panel.toggle_at(panel.toggle_regex_x + 1, y),
        Some(SearchToggle::UseRegex)
    );
    // Wrong row → None.
    assert_eq!(panel.toggle_at(panel.toggle_case_x, y + 1), None);
    // Outside the toggle columns → None.
    assert_eq!(panel.toggle_at(0, y), None);
}

#[test]
fn tooltip_at_labels_each_actionable_control_and_skips_inert_cells() {
    use ratatui::buffer::Buffer;
    let tmp = TempDir::new().unwrap();
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    panel.focused = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 12,
    };
    let mut buf = Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
    let y = panel.toggle_y;
    assert_eq!(
        panel.tooltip_at(panel.toggle_case_x, y),
        Some("Match Case (Alt+C)")
    );
    assert_eq!(
        panel.tooltip_at(panel.toggle_word_x, y),
        Some("Match Whole Word (Alt+W)")
    );
    assert_eq!(
        panel.tooltip_at(panel.toggle_regex_x, y),
        Some("Use Regular Expression (Alt+R)")
    );
    assert_eq!(
        panel.tooltip_at(panel.refresh_x, panel.refresh_y),
        Some("Refresh")
    );
    assert_eq!(
        panel.tooltip_at(panel.clear_x, panel.clear_y),
        Some("Clear Search Results")
    );
    assert_eq!(
        panel.tooltip_at(panel.chevron_x, panel.chevron_y),
        Some("Toggle Replace"),
        "the chevron reads as expand while the Replace row is collapsed"
    );
    // A plain text cell on the query row carries no tooltip.
    assert_eq!(panel.tooltip_at(panel.toggle_case_x, y + 5), None);
}

#[test]
fn search_panel_navigation_clamps() {
    let tmp = TempDir::new().unwrap();
    write(&tmp.path().join("x.txt"), "a needle\nb needle\nc needle\n");
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    panel.query = "needle".into();
    panel.run_query();
    assert_eq!(panel.hits.len(), 3);
    panel.move_up();
    assert_eq!(panel.selected, 0);
    panel.move_down();
    panel.move_down();
    panel.move_down(); // would go to 3, but clamped to 2
    assert_eq!(panel.selected, 2);
}

fn hit(path: &Path, line_no: usize) -> SearchHit {
    SearchHit {
        path: path.to_path_buf(),
        line_no,
        line_text: format!("line {line_no}"),
        matches: 1,
    }
}

fn listed(panel: &SearchPanel) -> Vec<(String, usize)> {
    panel
        .hits
        .iter()
        .map(|h| {
            let root = panel
                .roots
                .iter()
                .find(|r| h.path.starts_with(r))
                .unwrap_or(&panel.root);
            let rel = h.path.strip_prefix(root).unwrap_or(&h.path);
            (rel.display().to_string(), h.line_no)
        })
        .collect()
}

/// #1340: the parallel walker finishes files in whatever order its
/// threads do, and the list showed them that way, differently each run.
#[test]
fn streamed_batches_are_listed_by_path_whatever_order_they_arrive_in() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let mut panel = SearchPanel::new(root.to_path_buf());
    panel.add_hits(vec![hit(&root.join("b.rs"), 2), hit(&root.join("b.rs"), 9)]);
    panel.add_hits(vec![hit(&root.join("src/z.rs"), 1)]);
    panel.add_hits(vec![hit(&root.join("a.rs"), 7)]);
    panel.add_hits(vec![hit(&root.join("c.rs"), 4)]);
    panel.add_hits(vec![hit(&root.join("src/m.rs"), 3)]);
    assert_eq!(
        listed(&panel),
        [
            ("a.rs".into(), 7),
            ("b.rs".into(), 2),
            ("b.rs".into(), 9),
            ("c.rs".into(), 4),
            ("src/m.rs".into(), 3),
            ("src/z.rs".into(), 1),
        ]
    );
}

#[test]
fn every_hit_of_the_first_root_is_listed_before_the_second_roots() {
    let tmp = TempDir::new().unwrap();
    // Listed in the workspace's order, which is not alphabetical.
    let first = tmp.path().join("zeta");
    let second = tmp.path().join("alpha");
    let mut panel = SearchPanel::new(first.clone());
    panel.roots = vec![first.clone(), second.clone()];
    panel.add_hits(vec![hit(&second.join("a.rs"), 1)]);
    panel.add_hits(vec![hit(&first.join("y.rs"), 1)]);
    panel.add_hits(vec![hit(&second.join("b.rs"), 1)]);
    panel.add_hits(vec![hit(&first.join("x.rs"), 1)]);
    let paths: Vec<PathBuf> = panel.hits.iter().map(|h| h.path.clone()).collect();
    assert_eq!(
        paths,
        [
            first.join("x.rs"),
            first.join("y.rs"),
            second.join("a.rs"),
            second.join("b.rs"),
        ]
    );
}

#[test]
fn the_selection_stays_on_its_hit_as_earlier_files_arrive() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let mut panel = SearchPanel::new(root.to_path_buf());
    panel.add_hits(vec![hit(&root.join("m.rs"), 1), hit(&root.join("m.rs"), 5)]);
    panel.move_down();
    assert_eq!(panel.selected_hit().map(|h| h.line_no), Some(5));
    panel.add_hits(vec![hit(&root.join("a.rs"), 3)]);
    panel.add_hits(vec![hit(&root.join("z.rs"), 3)]);
    let sel = panel.selected_hit().unwrap();
    assert_eq!((sel.path.clone(), sel.line_no), (root.join("m.rs"), 5));
}

/// #2007: hits now find their place by binary search instead of being
/// compared against every listed hit, so the list must still come out as
/// one full sort would put it, whatever the arrival order, with the
/// selection on its hit. One file's hits arriving in two drains, and two
/// roots that both hold a file, are the cases the shortcut must not skip.
#[test]
fn hits_arriving_in_any_order_and_drain_size_list_as_one_sort_would() {
    let tmp = TempDir::new().unwrap();
    let outer = tmp.path().to_path_buf();
    let inner = outer.join("pkg");
    let mut all: Vec<SearchHit> = Vec::new();
    for f in 0..60 {
        let dir = if f % 3 == 0 { &inner } else { &outer };
        let path = dir.join(format!("d{}/f{f}.rs", f % 7));
        all.extend((0..f % 5 + 1).map(|l| hit(&path, l * 4 + 1)));
    }
    let mut want = SearchPanel::new(outer.clone());
    want.roots = vec![outer.clone(), inner.clone()];
    want.add_hits(all.clone());
    let mut x = 0x9e37_79b9u64;
    let mut next = move |n: usize| {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        (x as usize) % n.max(1)
    };
    for _round in 0..20 {
        let mut shuffled = all.clone();
        for i in (1..shuffled.len()).rev() {
            shuffled.swap(i, next(i + 1));
        }
        let mut panel = SearchPanel::new(outer.clone());
        panel.roots = vec![outer.clone(), inner.clone()];
        let mut picked: Option<SearchHit> = None;
        let mut rest = shuffled.as_slice();
        while !rest.is_empty() {
            let (drain, after) = rest.split_at(next(8).min(rest.len() - 1) + 1);
            // A drain holds files in walk order, each file's hits by line.
            let mut drain = drain.to_vec();
            drain.sort_by_key(|h| (h.path.clone(), h.line_no));
            panel.add_hits(drain);
            rest = after;
            if let Some(p) = &picked {
                assert_eq!(panel.selected_hit(), Some(p), "the selection moved");
            } else if panel.hits.len() > 3 {
                panel.selected = panel.hits.len() / 2;
                picked = panel.selected_hit().cloned();
            }
        }
        assert_eq!(panel.hits, want.hits);
    }
}

fn make_panel_with_hits(tmp: &TempDir, n: usize) -> SearchPanel {
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    panel.query = "needle".into();
    panel.hits = (0..n)
        .map(|i| SearchHit {
            path: tmp.path().join("a.txt"),
            line_no: i + 1,
            line_text: format!("line {i}"),
            matches: 1,
        })
        .collect();
    panel
}

#[test]
fn hovering_an_unselected_result_row_lifts_it() {
    let tmp = TempDir::new().unwrap();
    let mut panel = make_panel_with_hits(&tmp, 20);
    panel.focus_gradient = false; // Croft Dark
    panel.theme = crate::theme::Theme::DARK_BLUE;
    panel.selected = 0;
    panel.scroll = 0;
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 30,
    };
    // Results start at inner.y + 7 (see the click-mapping in `result_at`);
    // hover the second result row, which is not the selected one.
    let row_y = area.y + 1 + 7 + 1;
    panel.hover_pointer = Some((area.x + 1, row_y));
    let mut buf = ratatui::buffer::Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
    assert_eq!(
        buf[(area.x + 1, row_y)].bg,
        Color::Rgb(0x2b, 0x31, 0x42),
        "the hovered unselected result row wears the Croft Dark hover lift"
    );
}

#[test]
fn render_preserves_user_scroll_when_selected_is_at_top() {
    // User-reported bug: scroll appears to "come in batches" because
    // every render yanks scroll back to selected=0. Wheel scrolling
    // must be decoupled from selection auto-follow so streaming hits
    // and re-renders never fight the user's scroll position.
    let tmp = TempDir::new().unwrap();
    let mut panel = make_panel_with_hits(&tmp, 100);
    panel.selected = 0;
    panel.scroll = 30;
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 30,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
    assert_eq!(
        panel.scroll, 30,
        "render must not pull scroll back to selected when the user has wheeled away"
    );
}

#[test]
fn appending_streamed_hits_does_not_reset_user_scroll() {
    // Simulates the streaming engine: user wheels down to row 30,
    // then a new batch arrives via `hits.extend(batch)`. The next
    // render must keep scroll at 30, not jump to 0.
    let tmp = TempDir::new().unwrap();
    let mut panel = make_panel_with_hits(&tmp, 80);
    panel.selected = 0;
    panel.scroll = 30;
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 30,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
    // Streaming batch: another 50 hits arrive.
    let extra: Vec<SearchHit> = (80..130)
        .map(|i| SearchHit {
            path: tmp.path().join("a.txt"),
            line_no: i + 1,
            line_text: format!("line {i}"),
            matches: 1,
        })
        .collect();
    panel.hits.extend(extra);
    ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
    assert_eq!(
        panel.scroll, 30,
        "streaming hits must not reset the user's scroll offset"
    );
}

#[test]
fn move_down_past_bottom_of_viewport_advances_scroll_to_keep_selection_visible() {
    let tmp = TempDir::new().unwrap();
    let mut panel = make_panel_with_hits(&tmp, 100);
    // Pretend a 30-row panel was rendered: viewport = 30 - 5 = 25 rows
    // (the single-row query field starts results two rows higher than the
    // old 3-row box did).
    panel.last_inner = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 30,
    };
    panel.selected = 24;
    panel.scroll = 0;
    panel.move_down();
    assert_eq!(panel.selected, 25);
    assert_eq!(
        panel.scroll, 1,
        "arrow-down past the bottom row must scroll to keep selection visible"
    );
}

#[test]
fn move_up_above_top_of_viewport_retreats_scroll_to_keep_selection_visible() {
    let tmp = TempDir::new().unwrap();
    let mut panel = make_panel_with_hits(&tmp, 100);
    panel.last_inner = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 30,
    };
    panel.selected = 50;
    panel.scroll = 50;
    panel.move_up();
    assert_eq!(panel.selected, 49);
    assert_eq!(
        panel.scroll, 49,
        "arrow-up above the first visible row must scroll back to keep selection visible"
    );
}

#[test]
fn scroll_down_via_wheel_does_not_change_selection() {
    let tmp = TempDir::new().unwrap();
    let mut panel = make_panel_with_hits(&tmp, 100);
    panel.last_inner = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 30,
    };
    panel.selected = 0;
    panel.scroll = 0;
    panel.scroll_down(20);
    assert_eq!(panel.selected, 0, "wheel-scrolling must not move selection");
    assert_eq!(panel.scroll, 20);
}

// ---- files-to-include / files-to-exclude glob filtering ----

#[test]
fn path_filter_empty_allows_everything() {
    let f = PathFilter::new("", "");
    assert!(f.is_empty());
    let root = Path::new("/proj");
    assert!(f.allows(root, Path::new("/proj/src/main.rs")));
    assert!(f.allows(root, Path::new("/proj/a/b/c.py")));
}

#[test]
fn path_filter_dot_slash_anchors_a_glob_to_the_root() {
    let root = Path::new("/ws");
    let f = PathFilter::new("./main.rs,./src/**", "");
    assert!(f.allows(root, Path::new("/ws/main.rs")));
    assert!(f.allows(root, Path::new("/ws/src/deep/x.rs")));
    assert!(!f.allows(root, Path::new("/ws/lib/main.rs")));
    assert!(!f.allows(root, Path::new("/ws/vendor/src/x.rs")));
    // Negative: a bare name still matches at any depth.
    assert!(PathFilter::new("main.rs", "").allows(root, Path::new("/ws/lib/main.rs")));
}

#[test]
fn path_filter_include_bare_glob_matches_at_any_depth() {
    // VS Code: `*.rs` with no slash means `**/*.rs`.
    let f = PathFilter::new("*.rs", "");
    assert!(!f.is_empty());
    let root = Path::new("/proj");
    assert!(f.allows(root, Path::new("/proj/main.rs")));
    assert!(f.allows(root, Path::new("/proj/src/deep/mod.rs")));
    assert!(!f.allows(root, Path::new("/proj/src/app.py")));
}

#[test]
fn path_filter_include_with_multiple_comma_separated_globs() {
    let f = PathFilter::new("*.rs, *.toml", "");
    let root = Path::new("/proj");
    assert!(f.allows(root, Path::new("/proj/main.rs")));
    assert!(f.allows(root, Path::new("/proj/Cargo.toml")));
    assert!(!f.allows(root, Path::new("/proj/notes.md")));
}

#[test]
fn path_filter_include_keeps_commas_inside_braces() {
    // #1251: `*.{ts,tsx}` is one glob, not `*.{ts` and `tsx}`.
    let f = PathFilter::new("*.{ts,tsx}", "");
    let root = Path::new("/proj");
    assert!(f.allows(root, Path::new("/proj/src/a.ts")));
    assert!(f.allows(root, Path::new("/proj/src/b.tsx")));
    assert!(!f.allows(root, Path::new("/proj/other/d.js")));
    assert!(!f.allows(root, Path::new("/proj/README.md")));
}

#[test]
fn path_filter_include_brace_glob_mixes_with_plain_ones() {
    let f = PathFilter::new("*.md, src/**/*.{js,jsx}", "");
    let root = Path::new("/proj");
    assert!(f.allows(root, Path::new("/proj/README.md")));
    assert!(f.allows(root, Path::new("/proj/src/ui/app.jsx")));
    assert!(!f.allows(root, Path::new("/proj/lib/app.jsx")));
    assert!(!f.allows(root, Path::new("/proj/src/app.ts")));
}

#[test]
fn path_filter_include_with_no_valid_glob_allows_nothing() {
    // A typed include that compiles to nothing must not widen the search
    // (and Replace All) to every file.
    let f = PathFilter::new("*.{ts", "");
    let root = Path::new("/proj");
    assert!(!f.allows(root, Path::new("/proj/src/a.ts")));
    assert!(!f.allows(root, Path::new("/proj/README.md")));
}

#[test]
fn path_filter_exclude_keeps_commas_inside_braces() {
    let f = PathFilter::new("", "*.{md,js}, target/**");
    let root = Path::new("/proj");
    assert!(!f.allows(root, Path::new("/proj/README.md")));
    assert!(!f.allows(root, Path::new("/proj/other/d.js")));
    assert!(!f.allows(root, Path::new("/proj/target/x.rs")));
    assert!(f.allows(root, Path::new("/proj/src/a.ts")));
}

/// #1345: excluding a folder excludes what is inside it, as in VS
/// Code, so `**/generated` and a bare `target` hide their files.
#[test]
fn an_excluded_folder_hides_the_files_inside_it() {
    let f = PathFilter::new("", "**/generated, target");
    let root = Path::new("/proj");
    assert!(!f.allows(root, Path::new("/proj/generated/api_pb2.py")));
    assert!(!f.allows(root, Path::new("/proj/a/generated/deep/x.py")));
    assert!(!f.allows(root, Path::new("/proj/target/debug/x.rs")));
    // Negative: a name that only starts like the folder stays.
    assert!(f.allows(root, Path::new("/proj/generated_stub.py")));
    assert!(f.allows(root, Path::new("/proj/src/targets/x.rs")));
}

/// #958: an include glob that names a folder searches what is inside
/// it: a bare `tests` at any depth, `src/widgets` and `./src` from the
/// workspace root.
#[test]
fn an_included_folder_searches_the_files_inside_it() {
    let root = Path::new("/proj");
    let f = PathFilter::new("tests", "");
    assert!(f.allows(root, Path::new("/proj/tests/cli.rs")));
    assert!(f.allows(root, Path::new("/proj/crate/tests/deep/x.rs")));
    assert!(!f.allows(root, Path::new("/proj/src/main.rs")));
    // Negative: a name that only starts like the folder is left out.
    assert!(!f.allows(root, Path::new("/proj/tests_old/x.rs")));
    let f = PathFilter::new("src/widgets", "");
    assert!(f.allows(root, Path::new("/proj/src/widgets/search.rs")));
    assert!(!f.allows(root, Path::new("/proj/src/app/mod.rs")));
    let f = PathFilter::new("./src", "");
    assert!(f.allows(root, Path::new("/proj/src/app/mod.rs")));
    assert!(!f.allows(root, Path::new("/proj/vendor/src/x.rs")));
    // A file glob names files, not a folder that happens to match it,
    // and a folder above the workspace lets nothing in.
    let f = PathFilter::new("*.rs, src", "");
    assert!(f.allows(root, Path::new("/proj/a/main.rs")));
    assert!(!f.allows(root, Path::new("/proj/generated.rs/notes.txt")));
    assert!(!f.allows(root, Path::new("/home/me/src/elsewhere.txt")));
    // The same folder names on the exclude side drop their contents.
    let f = PathFilter::new("", "src/widgets, dist");
    assert!(!f.allows(root, Path::new("/proj/src/widgets/search.rs")));
    assert!(!f.allows(root, Path::new("/proj/dist/bundle.js")));
    assert!(f.allows(root, Path::new("/proj/src/app/mod.rs")));
}

#[test]
fn path_filter_invalid_exclude_hides_nothing() {
    // Negative: a broken exclude entry drops only itself.
    let f = PathFilter::new("", "*.{md");
    let root = Path::new("/proj");
    assert!(f.allows(root, Path::new("/proj/README.md")));
    assert!(f.allows(root, Path::new("/proj/src/a.ts")));
}

#[test]
fn an_invalid_include_glob_is_named_above_the_box() {
    use ratatui::buffer::Buffer;
    let tmp = TempDir::new().unwrap();
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    panel.details_open = true;
    panel.include = String::from("*.md, *.{ts");
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 16,
    };
    let mut buf = Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
    let text: String = (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("invalid glob: *.{ts"), "{text}");
    assert!(text.contains("files to exclude\u{20}"), "{text}");
}

/// #1479: a leading `/` anchors a glob to the root (gitignore's rule):
/// `/build` is the root's `build` folder, not `scripts/build`.
#[test]
fn a_leading_slash_anchors_an_exclude_glob_to_the_root() {
    let root = Path::new("/r");
    let f = PathFilter::excluding(&[String::from("/build")]);
    assert!(f.excludes(root, Path::new("/r/build/out.js")));
    assert!(f.excludes(root, Path::new("/r/build")));
    assert!(!f.excludes(root, Path::new("/r/scripts/build/release.py")));
    assert!(!f.excludes(root, Path::new("/r/rebuild/x")));
    let f = PathFilter::excluding(&[String::from("/*.log")]);
    assert!(f.excludes(root, Path::new("/r/app.log")));
    assert!(!f.excludes(root, Path::new("/r/logs/app.log")));
}

/// #1479 negative: croft's own bare glob still matches at any depth, as
/// documented, and `**/` still does.
#[test]
fn a_bare_or_starred_exclude_glob_still_matches_at_any_depth() {
    let root = Path::new("/r");
    for glob in ["build", "**/build"] {
        let f = PathFilter::excluding(&[String::from(glob)]);
        assert!(f.excludes(root, Path::new("/r/build/out.js")), "{glob}");
        assert!(
            f.excludes(root, Path::new("/r/scripts/build/release.py")),
            "{glob}"
        );
    }
}

/// #1345: the project's exclusions show under the exclude box as a
/// checkbox, clickable, and stay out of the box's own text; with none
/// the row is not drawn.
#[test]
fn project_excludes_are_listed_under_the_exclude_box() {
    use ratatui::buffer::Buffer;
    let tmp = TempDir::new().unwrap();
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    panel.details_open = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 70,
        height: 16,
    };
    let render = |panel: &mut SearchPanel| {
        let mut buf = Buffer::empty(area);
        ratatui::widgets::Widget::render(panel, area, &mut buf);
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert!(!render(&mut panel).contains("project excludes"));
    assert_eq!(panel.project_exclude_rect, Rect::default());
    panel.project_exclude = vec![
        String::from("**/generated"),
        String::from("**/tests/fixtures"),
    ];
    let text = render(&mut panel);
    assert!(
        text.contains("[x] project excludes: **/generated, **/tests/fixtures"),
        "{text}"
    );
    let r = panel.project_exclude_rect;
    assert!(panel.project_exclude_toggle_at(r.x + 1, r.y));
    assert!(!panel.project_exclude_toggle_at(r.x + 1, r.y + 1));
    assert_eq!(panel.exclude, "", "the box keeps only what was typed");
    assert_eq!(panel.effective_exclude(), "**/generated, **/tests/fixtures");
    panel.use_exclude_settings = false;
    assert!(render(&mut panel).contains("[ ] project excludes"));
    assert_eq!(panel.effective_exclude(), "");
}

#[test]
fn path_filter_exclude_drops_matching_files() {
    let f = PathFilter::new("", "*.lock");
    let root = Path::new("/proj");
    assert!(f.allows(root, Path::new("/proj/main.rs")));
    assert!(!f.allows(root, Path::new("/proj/Cargo.lock")));
}

#[test]
fn path_filter_exclude_beats_include_for_overlapping_paths() {
    // include *.rs, but exclude anything under target/. A target/*.rs file
    // must be dropped because exclude wins.
    let f = PathFilter::new("*.rs", "target/**");
    let root = Path::new("/proj");
    assert!(f.allows(root, Path::new("/proj/src/main.rs")));
    assert!(!f.allows(root, Path::new("/proj/target/debug/build.rs")));
}

#[test]
fn path_filter_directory_glob_matches_relative_to_root() {
    let f = PathFilter::new("src/**", "");
    let root = Path::new("/proj");
    assert!(f.allows(root, Path::new("/proj/src/a/b.rs")));
    assert!(!f.allows(root, Path::new("/proj/tests/c.rs")));
}

#[test]
fn search_streaming_filtered_honours_include_glob() {
    let tmp = TempDir::new().unwrap();
    write(&tmp.path().join("keep.rs"), "needle here\n");
    write(&tmp.path().join("drop.txt"), "needle here too\n");
    let cancel = Arc::new(AtomicBool::new(false));
    let filter = PathFilter::new("*.rs", "");
    let collected = Arc::new(std::sync::Mutex::new(Vec::new()));
    let c2 = collected.clone();
    search_workspace_streaming_filtered(
        tmp.path(),
        "needle",
        SearchOpts::default(),
        &cancel,
        &HashSet::new(),
        &filter,
        move |batch| c2.lock().unwrap().extend(batch),
    );
    let hits = Arc::try_unwrap(collected).unwrap().into_inner().unwrap();
    assert_eq!(
        hits.len(),
        1,
        "only the .rs file should match the include glob"
    );
    assert!(hits[0].path.ends_with("keep.rs"));
}

#[test]
fn search_streaming_filtered_honours_exclude_glob() {
    let tmp = TempDir::new().unwrap();
    write(&tmp.path().join("a.rs"), "needle\n");
    write(&tmp.path().join("b.rs"), "needle\n");
    let cancel = Arc::new(AtomicBool::new(false));
    let filter = PathFilter::new("", "b.rs");
    let collected = Arc::new(std::sync::Mutex::new(Vec::new()));
    let c2 = collected.clone();
    search_workspace_streaming_filtered(
        tmp.path(),
        "needle",
        SearchOpts::default(),
        &cancel,
        &HashSet::new(),
        &filter,
        move |batch| c2.lock().unwrap().extend(batch),
    );
    let hits = Arc::try_unwrap(collected).unwrap().into_inner().unwrap();
    assert_eq!(hits.len(), 1);
    assert!(hits[0].path.ends_with("a.rs"), "b.rs must be excluded");
}

#[test]
fn search_worker_loop_applies_set_filter_to_subsequent_query() {
    let tmp = TempDir::new().unwrap();
    write(&tmp.path().join("keep.rs"), "needle\n");
    write(&tmp.path().join("skip.md"), "needle\n");
    let (q_tx, q_rx) = std::sync::mpsc::channel::<SearchRequest>();
    let (e_tx, e_rx) = std::sync::mpsc::channel::<SearchEvent>();
    let root = tmp.path().to_path_buf();
    let join = std::thread::spawn(move || search_worker_loop(root, q_rx, e_tx));
    q_tx.send(SearchRequest::SetFilter("*.rs".into(), String::new()))
        .unwrap();
    q_tx.send(SearchRequest::Query("needle".into(), SearchOpts::default()))
        .unwrap();
    let (_, hits, done) = collect_until_done(&e_rx, std::time::Duration::from_secs(2));
    assert!(done);
    assert_eq!(hits.len(), 1, "filter must restrict the scan to *.rs");
    assert!(hits[0].path.ends_with("keep.rs"));
    drop(q_tx);
    join.join().unwrap();
}

// ---- replace ----

#[test]
fn replace_in_text_never_matches_across_a_line_break() {
    let re = SearchOpts {
        use_regex: true,
        ..SearchOpts::default()
    };
    assert_eq!(
        replace_in_text("let a = 1\nlet b = 2;\n", "a[^;]*", "Z", re),
        Some((String::from("let Z\nlet b = 2;\n"), 1))
    );
    assert_eq!(
        replace_in_text("foo  \r\n  bar\n", "foo\\s+", "X", re).map(|r| r.0),
        Some(String::from("X\r\n  bar\n"))
    );
    // Stripping indentation leaves blank lines alone.
    assert_eq!(
        replace_in_text("  a\n\n  b\n", "^\\s+", "", re).map(|r| r.0),
        Some(String::from("a\n\nb\n"))
    );
}

#[test]
fn replace_in_file_keeps_the_mode_and_reports_a_non_utf8_file() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("a.sh");
    std::fs::write(&f, "echo foo\n").unwrap();
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        replace_in_file(&f, "foo", "bar", SearchOpts::default()),
        Some(1)
    );
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "echo bar\n");
    assert_eq!(
        std::fs::metadata(&f).unwrap().permissions().mode() & 0o777,
        0o755
    );
    let latin = dir.path().join("l.txt");
    std::fs::write(&latin, b"foo \xe9\n").unwrap();
    assert_eq!(
        replace_in_file(&latin, "foo", "bar", SearchOpts::default()),
        None
    );
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        2,
        "no temp file left"
    );
}

#[test]
fn a_symlink_planted_at_the_temp_name_is_not_written_through() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("a.txt");
    std::fs::write(&f, "foo\n").unwrap();
    let victim = dir.path().join("victim");
    std::fs::write(&victim, "keep").unwrap();
    let tmp = dir
        .path()
        .join(format!(".a.txt.croft-replace-{}", std::process::id()));
    std::os::unix::fs::symlink(&victim, &tmp).unwrap();
    assert_eq!(
        replace_in_file(&f, "foo", "bar", SearchOpts::default()),
        Some(1)
    );
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "bar\n");
}

#[test]
fn zero_width_patterns_and_longer_alternatives_replace_what_the_search_lists() {
    let re = SearchOpts {
        use_regex: true,
        ..SearchOpts::default()
    };
    assert_eq!(
        replace_in_text("a\nb\n", "^", "// ", re),
        Some((String::from("// a\n// b\n"), 2))
    );
    assert_eq!(
        replace_in_text("a\r\nb\n", "$", ";", re),
        Some((String::from("a;\r\nb;\n"), 2))
    );
    let ww = SearchOpts {
        whole_word: true,
        ..re
    };
    assert_eq!(
        replace_in_text("x foobar y", "foo|foobar", "Z", ww),
        Some((String::from("x Z y"), 1))
    );
    // Capture groups still expand against the whole match.
    assert_eq!(
        replace_in_text("let a1 = b2;", "([a-z])(\\d)", "$2$1", re).map(|r| r.0),
        Some(String::from("let 1a = 2b;"))
    );
}

#[test]
fn captures_keep_the_context_around_the_match() {
    let re = SearchOpts {
        use_regex: true,
        ..SearchOpts::default()
    };
    assert_eq!(
        replace_in_text("foo.rs", r"\b(\.rs)", "${1}x", re).map(|r| r.0),
        Some(String::from("foo.rsx"))
    );
}

#[test]
fn replace_in_file_keeps_hard_links_linked() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("a.txt");
    std::fs::write(&f, "foo\n").unwrap();
    let link = dir.path().join("b.txt");
    std::fs::hard_link(&f, &link).unwrap();
    assert_eq!(
        replace_in_file(&f, "foo", "bar", SearchOpts::default()),
        Some(1)
    );
    assert_eq!(std::fs::read_to_string(&link).unwrap(), "bar\n");
}

#[test]
fn whole_word_replace_takes_matches_edged_with_punctuation() {
    let ww = SearchOpts {
        whole_word: true,
        ..SearchOpts::default()
    };
    assert_eq!(
        replace_in_text("echo $var here $variable", "$var", "$x", ww),
        Some((String::from("echo $x here $variable"), 1))
    );
}

#[test]
fn replace_in_text_literal_replaces_all_occurrences() {
    let (out, n) =
        replace_in_text("foo bar foo baz foo", "foo", "X", SearchOpts::default()).unwrap();
    assert_eq!(n, 3);
    assert_eq!(out, "X bar X baz X");
}

#[test]
fn replace_in_text_no_match_returns_zero_and_unchanged() {
    let (out, n) = replace_in_text("hello world", "zzz", "X", SearchOpts::default()).unwrap();
    assert_eq!(n, 0);
    assert_eq!(out, "hello world");
}

#[test]
fn replace_in_text_literal_dollar_is_inserted_verbatim() {
    // A literal replacement containing `$` must not be parsed as a capture
    // reference; it should appear as-is.
    let (out, n) = replace_in_text("price=AMT", "AMT", "$5", SearchOpts::default()).unwrap();
    assert_eq!(n, 1);
    assert_eq!(out, "price=$5");
}

#[test]
fn replace_in_text_case_insensitive_by_default() {
    let (out, n) = replace_in_text("Foo foo FOO", "foo", "x", SearchOpts::default()).unwrap();
    assert_eq!(n, 3);
    assert_eq!(out, "x x x");
}

#[test]
fn replace_in_text_case_sensitive_only_exact_case() {
    let opts = SearchOpts {
        case_sensitive: true,
        ..Default::default()
    };
    let (out, n) = replace_in_text("Foo foo FOO", "foo", "x", opts).unwrap();
    assert_eq!(n, 1);
    assert_eq!(out, "Foo x FOO");
}

#[test]
fn replace_in_text_whole_word_skips_substrings() {
    let opts = SearchOpts {
        whole_word: true,
        ..Default::default()
    };
    let (out, n) = replace_in_text("cat category cat", "cat", "dog", opts).unwrap();
    assert_eq!(n, 2);
    assert_eq!(out, "dog category dog");
}

#[test]
fn replace_in_text_regex_with_capture_reference() {
    let opts = SearchOpts {
        use_regex: true,
        ..Default::default()
    };
    let (out, n) = replace_in_text("a1 b2 c3", r"([a-z])(\d)", "$2$1", opts).unwrap();
    assert_eq!(n, 3);
    assert_eq!(out, "1a 2b 3c");
}

/// #1399: a `$NAME` that is no group of the pattern is text, as in VS
/// Code: a shell, PHP or Perl variable in the replacement survives.
#[test]
fn a_dollar_name_that_is_no_group_stays_literal() {
    let opts = SearchOpts {
        use_regex: true,
        ..Default::default()
    };
    let replace =
        |text: &str, find: &str, with: &str| replace_in_text(text, find, with, opts).unwrap().0;
    assert_eq!(
        replace("cp a /opt/app\n", r"/opt/(\S+)", "$PREFIX/$1"),
        "cp a $PREFIX/app\n"
    );
    assert_eq!(replace("x", "x", "${HOME}/x"), "${HOME}/x");
    assert_eq!(replace("x", "x", "cost $"), "cost $");
    assert_eq!(replace("x", "x", "$_ and $@"), "$_ and $@");
    // VS Code's `$&` is the whole match.
    assert_eq!(replace("abc", "b", "[$&]"), "a[b]c");
    // The preview row shows the same text Replace All writes.
    assert_eq!(
        expand_replacement("/opt/app", r"/opt/(\S+)", "$PREFIX/$1", opts),
        "$PREFIX/app"
    );
}

/// #1399 negative: real references keep their meaning.
#[test]
fn real_capture_references_still_expand() {
    let opts = SearchOpts {
        use_regex: true,
        ..Default::default()
    };
    let replace =
        |text: &str, find: &str, with: &str| replace_in_text(text, find, with, opts).unwrap().0;
    assert_eq!(replace("ab", "(a)(b)", "$2$1 $0 ${1} $$"), "ba ab a $");
    assert_eq!(replace("foo", "(foo)", "$1_old"), "foo_old");
    assert_eq!(replace("cat", r"(?P<HOME>\w+)", "${HOME}/$HOME"), "cat/cat");
}

/// #1636: a numbered reference the pattern has no group for is text,
/// as in VS Code, so a price or a positional parameter survives.
#[test]
fn a_numbered_reference_with_no_group_stays_literal() {
    let opts = SearchOpts {
        use_regex: true,
        ..Default::default()
    };
    let replace =
        |text: &str, find: &str, with: &str| replace_in_text(text, find, with, opts).unwrap().0;
    assert_eq!(replace("apple", "(apple)", "$1 costs $5"), "apple costs $5");
    assert_eq!(replace("x", "x", "echo $2 $9"), "echo $2 $9");
    assert_eq!(replace("x", "x", "${3}"), "${3}");
    // The issue's repro: `$10` with one group is `$1` then `0`.
    assert_eq!(
        replace("apple\nitem 7\n", "(apple)", "v$10 costs $5"),
        "vapple0 costs $5\nitem 7\n"
    );
    // The preview row shows the same text Replace All writes.
    assert_eq!(
        expand_replacement("apple", "(apple)", "v$10 costs $5", opts),
        "vapple0 costs $5"
    );
}

/// #1636 negative: references to groups that exist keep their meaning,
/// `$10` included when the pattern has ten groups.
#[test]
fn numbered_references_to_existing_groups_still_expand() {
    let opts = SearchOpts {
        use_regex: true,
        ..Default::default()
    };
    let replace =
        |text: &str, find: &str, with: &str| replace_in_text(text, find, with, opts).unwrap().0;
    let ten = "(a)(b)(c)(d)(e)(f)(g)(h)(i)(j)";
    assert_eq!(replace("abcdefghij", ten, "$10$1"), "ja");
    assert_eq!(replace("abcdefghij", ten, "${10}"), "j");
    assert_eq!(replace("ab", "(a)(b)", "$1 ${1} $2 $0"), "a a b ab");
    assert_eq!(replace("ab", "(a)(b)", "$2_x"), "b_x");
}

#[test]
fn replace_in_text_invalid_regex_returns_none() {
    let opts = SearchOpts {
        use_regex: true,
        ..Default::default()
    };
    assert!(replace_in_text("anything", "[unclosed", "x", opts).is_none());
}

#[test]
fn replace_in_file_writes_only_when_a_match_is_replaced() {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path().join("a.txt");
    write(&p, "old value old\n");
    let n = replace_in_file(&p, "old", "new", SearchOpts::default()).unwrap();
    assert_eq!(n, 2);
    assert_eq!(fs::read_to_string(&p).unwrap(), "new value new\n");
}

/// #1175: the `srch` repro, a query with a trailing space.
fn srch_workspace() -> TempDir {
    let tmp = TempDir::new().unwrap();
    write(
        &tmp.path().join("app.py"),
        "def run(value):\n    returned = value\n    return value\n",
    );
    write(&tmp.path().join("calc.py"), "total = price  * qty\n");
    tmp
}

/// #1175: a trailing space in the query is part of what's searched, so
/// `return ` doesn't match `returned`.
#[test]
fn a_trailing_space_in_the_query_is_searched_for() {
    let tmp = srch_workspace();
    let hits = search_workspace(tmp.path(), "return ", SearchOpts::default());
    let lines: Vec<usize> = hits.iter().map(|h| h.line_no).collect();
    assert_eq!(lines, vec![3]);
}

/// #1175: Replace All rewrites only what the query matched as typed.
#[test]
fn replace_all_keeps_the_spaces_typed_in_the_query() {
    let tmp = srch_workspace();
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    panel.query = "return ".into();
    panel.replace = "yield ".into();
    panel.run_query();
    assert_eq!(panel.count_replacements(&HashSet::new()), (1, 1));
    assert_eq!(panel.replace_all_on_disk(), 1);
    assert_eq!(
        fs::read_to_string(tmp.path().join("app.py")).unwrap(),
        "def run(value):\n    returned = value\n    yield value\n"
    );
    assert_eq!(
        replace_in_text(
            "returned\nreturn x\n",
            "return ",
            "yield ",
            SearchOpts::default()
        ),
        Some((String::from("returned\nyield x\n"), 1))
    );
}

/// #1175: a query of only spaces searches for those spaces.
#[test]
fn a_query_of_spaces_finds_runs_of_spaces() {
    let tmp = srch_workspace();
    let hits = search_workspace(tmp.path(), "  ", SearchOpts::default());
    let mut files: Vec<String> = hits
        .iter()
        .map(|h| h.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    files.sort();
    files.dedup();
    assert_eq!(files, vec!["app.py", "calc.py"]);
}

/// An empty query still searches for nothing, and a query pasted with a
/// stray line break at the end still finds its text.
#[test]
fn an_empty_query_finds_nothing_and_a_pasted_newline_is_dropped() {
    let tmp = srch_workspace();
    assert!(search_workspace(tmp.path(), "", SearchOpts::default()).is_empty());
    let hits = search_workspace(tmp.path(), "returned\n", SearchOpts::default());
    assert_eq!(hits.len(), 1);
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    panel.replace = "x".into();
    assert_eq!(panel.replace_all_on_disk(), 0);
}

#[test]
fn replace_all_on_disk_replaces_across_distinct_hit_files() {
    let tmp = TempDir::new().unwrap();
    write(&tmp.path().join("a.txt"), "foo foo\n");
    write(&tmp.path().join("b.txt"), "foo\n");
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    panel.query = "foo".into();
    panel.replace = "bar".into();
    panel.run_query();
    // a.txt contributes 2 hits, b.txt 1 hit (3 hit rows, 2 files).
    let total = panel.replace_all_on_disk();
    assert_eq!(total, 3, "all three matches across both files replaced");
    assert_eq!(
        fs::read_to_string(tmp.path().join("a.txt")).unwrap(),
        "bar bar\n"
    );
    assert_eq!(
        fs::read_to_string(tmp.path().join("b.txt")).unwrap(),
        "bar\n"
    );
}

// ---- panel field focus + control hit-testing ----

#[test]
fn toggle_replace_expands_and_collapses_the_replace_row() {
    let tmp = TempDir::new().unwrap();
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    assert!(!panel.replace_open);
    panel.toggle_replace();
    assert!(panel.replace_open);
    // Focus the replace field, then collapse: focus falls back to Query.
    panel.focus_field(SearchField::Replace);
    panel.toggle_replace();
    assert!(!panel.replace_open);
    assert_eq!(panel.field, SearchField::Query);
}

#[test]
fn tab_cycles_only_through_visible_fields() {
    let tmp = TempDir::new().unwrap();
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    // Only Query visible: Tab stays on Query.
    panel.focus_next_field();
    assert_eq!(panel.field, SearchField::Query);
    // Expand replace + details: Query -> Replace -> Include -> Exclude -> Query.
    panel.replace_open = true;
    panel.details_open = true;
    panel.focus_next_field();
    assert_eq!(panel.field, SearchField::Replace);
    panel.focus_next_field();
    assert_eq!(panel.field, SearchField::Include);
    panel.focus_next_field();
    assert_eq!(panel.field, SearchField::Exclude);
    panel.focus_next_field();
    assert_eq!(panel.field, SearchField::Query);
}

#[test]
fn type_char_lands_in_the_focused_field() {
    let tmp = TempDir::new().unwrap();
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    panel.replace_open = true;
    panel.type_char('a');
    assert_eq!(panel.query, "a");
    assert_eq!(panel.replace, "");
    panel.focus_field(SearchField::Replace);
    panel.type_char('b');
    assert_eq!(panel.query, "a");
    assert_eq!(panel.replace, "b");
}

/// #1729: the hardware caret sits on the cell of the char it is before,
/// and a query longer than the row scrolls so the caret stays inside it.
#[test]
fn the_caret_follows_its_char_and_a_long_query_scrolls() {
    use ratatui::buffer::Buffer;
    let tmp = TempDir::new().unwrap();
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    panel.focused = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 30,
        height: 20,
    };
    let draw = |panel: &mut SearchPanel| {
        let mut buf = Buffer::empty(area);
        Widget::render(panel, area, &mut buf);
        buf
    };
    for c in "abcdef".chars() {
        panel.type_char(c);
    }
    draw(&mut panel);
    let (end_x, _) = panel.cursor_screen_pos().unwrap();
    panel.move_caret(|_, _| 3);
    draw(&mut panel);
    let (mid_x, y) = panel.cursor_screen_pos().unwrap();
    assert_eq!(end_x - mid_x, 3, "three chars left of the end");
    assert_eq!(y, panel.query_input_rect.y);
    // Far longer than the row: the caret stays inside it either way.
    panel.seed("0123456789".repeat(6), String::new(), String::new());
    let buf = draw(&mut panel);
    let r = panel.query_input_rect;
    let (cx, cy) = panel.cursor_screen_pos().unwrap();
    assert!(cx >= r.x && cx < r.right(), "caret {cx} inside {r:?}");
    let row: String = (r.x..r.right())
        .map(|x| buf[(x, cy)].symbol().to_string())
        .collect();
    assert!(row.contains("789"), "the tail is the part in view: {row:?}");
    panel.move_caret(|_, _| 0);
    let buf = draw(&mut panel);
    let row: String = (r.x..r.right())
        .map(|x| buf[(x, cy)].symbol().to_string())
        .collect();
    assert!(row.contains("0123"), "Home brings the head back: {row:?}");
}

/// #1729: a click in the text puts the caret under it.
#[test]
fn a_click_in_the_query_places_the_caret_under_it() {
    use ratatui::buffer::Buffer;
    let tmp = TempDir::new().unwrap();
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    panel.focused = true;
    for c in "abcdef".chars() {
        panel.type_char(c);
    }
    let area = Rect {
        x: 0,
        y: 0,
        width: 30,
        height: 20,
    };
    let mut buf = Buffer::empty(area);
    Widget::render(&mut panel, area, &mut buf);
    let (end_x, _) = panel.cursor_screen_pos().unwrap();
    panel.click_caret(end_x - 4);
    assert_eq!(panel.caret(), 2);
    panel.click_caret(end_x + 5);
    assert_eq!(panel.caret(), 6, "past the text is the end");
}

#[test]
fn focused_query_field_exposes_a_caret_and_paints_no_static_block() {
    use ratatui::buffer::Buffer;
    let tmp = TempDir::new().unwrap();
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    panel.focused = true;
    panel.type_char('a');
    panel.type_char('b');
    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 20,
    };
    let mut buf = Buffer::empty(area);
    Widget::render(&mut panel, area, &mut buf);
    // The caret sits on the query row so the app can drive the host's
    // blinking hardware caret there, matching the Source Control commit
    // box, instead of painting a static block that never blinks.
    let (cx, cy) = panel
        .cursor_screen_pos()
        .expect("a focused query field must expose a caret position");
    let r = panel.query_input_rect;
    assert_eq!(cy, r.y, "caret rides the query row");
    assert!(cx > r.x, "caret sits past the magnifier-led text start");
    let row: String = (r.x..r.right())
        .map(|x| buf[(x, r.y)].symbol().to_string())
        .collect();
    assert!(
        !row.contains('\u{2588}'),
        "the static full-block cursor must be gone now that the hardware caret blinks; row was {row:?}"
    );
}

#[test]
fn an_unfocused_search_panel_exposes_no_caret() {
    use ratatui::buffer::Buffer;
    let tmp = TempDir::new().unwrap();
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    panel.focused = false;
    panel.type_char('x');
    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 20,
    };
    let mut buf = Buffer::empty(area);
    Widget::render(&mut panel, area, &mut buf);
    assert!(
        panel.cursor_screen_pos().is_none(),
        "an unfocused panel must not claim the hardware caret"
    );
}

#[test]
fn backspace_removes_from_the_focused_field() {
    let tmp = TempDir::new().unwrap();
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    panel.details_open = true;
    panel.focus_field(SearchField::Include);
    panel.insert_str("*.rs");
    assert_eq!(panel.include, "*.rs");
    assert!(panel.backspace());
    assert_eq!(panel.include, "*.r");
    assert_eq!(panel.query, "");
}

#[test]
fn chevron_ellipsis_and_replace_all_hit_tests_after_render() {
    use ratatui::buffer::Buffer;
    let tmp = TempDir::new().unwrap();
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    panel.focused = true;
    panel.replace_open = true;
    panel.details_open = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 24,
    };
    let mut buf = Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
    assert!(panel.chevron_at(panel.chevron_x, panel.chevron_y));
    assert!(panel.ellipsis_at(panel.ellipsis_x, panel.ellipsis_y));
    assert!(panel.replace_all_at(panel.replace_all_x, panel.replace_all_y));
    // Clicking the query input row focuses the Query field.
    let q = panel.query_input_rect;
    assert_eq!(panel.field_at(q.x, q.y), Some(SearchField::Query));
    let r = panel.replace_input_rect;
    assert_eq!(panel.field_at(r.x, r.y), Some(SearchField::Replace));
}

#[test]
fn expanded_rows_push_results_start_down() {
    use ratatui::buffer::Buffer;
    let tmp = TempDir::new().unwrap();
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 30,
    };
    let mut buf = Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
    let collapsed_offset = panel.results_start_offset;
    panel.replace_open = true;
    panel.details_open = true;
    let mut buf2 = Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut panel, area, &mut buf2);
    assert!(
        panel.results_start_offset > collapsed_offset,
        "expanding replace + include/exclude must push results down (was {collapsed_offset}, now {})",
        panel.results_start_offset
    );
    // Single-row fields, each group separated by a hairline row: a divider
    // + replace field (2 rows), plus a divider + caption + field (3) each
    // for include and exclude = 8 rows.
    assert_eq!(panel.results_start_offset, collapsed_offset + 8);
}

#[test]
fn collapsed_panel_results_offset_is_five() {
    use ratatui::buffer::Buffer;
    let tmp = TempDir::new().unwrap();
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 20,
    };
    let mut buf = Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
    // header(0) + blank(1) + query(2) + separator(3) + caption(4) → results
    // begin at row 5. The single-row query field saves two rows versus the
    // old 3-row box (which started results at 7).
    assert_eq!(
        panel.results_start_offset, 5,
        "collapsed layout starts results at the 5-row offset"
    );
}
/// #1612: the issue's file, `user` → `account` with Preserve Case on,
/// keeps each occurrence's case.
#[test]
fn preserve_case_renames_user_to_account_in_each_case() {
    let file = "class UserService:\n    \"\"\"Load a user by id.\"\"\"\n\n    USER_TABLE = \"users\"\n\n    def get_user(self, user_id):\n        return self.db.find(USER_TABLE, user_id)\n";
    let opts = SearchOpts {
        preserve_case: true,
        ..SearchOpts::default()
    };
    let (out, n) = replace_in_text(file, "user", "account", opts).unwrap();
    assert_eq!(n, 8);
    assert_eq!(
        out,
        "class AccountService:\n    \"\"\"Load a account by id.\"\"\"\n\n    ACCOUNT_TABLE = \"accounts\"\n\n    def get_account(self, account_id):\n        return self.db.find(ACCOUNT_TABLE, account_id)\n"
    );
}

/// #1612 negative: with Preserve Case off the replacement still goes in
/// exactly as typed.
#[test]
fn without_preserve_case_the_replacement_goes_in_as_typed() {
    let (out, _) = replace_in_text(
        "UserService USER_TABLE",
        "user",
        "account",
        SearchOpts::default(),
    )
    .unwrap();
    assert_eq!(out, "accountService account_TABLE");
}

/// #1612: VS Code's rules, part by part across `-` and `_`.
#[test]
fn preserve_case_follows_each_dash_or_underscore_part() {
    assert_eq!(preserve_case("User", "account"), "Account");
    assert_eq!(preserve_case("USER", "account"), "ACCOUNT");
    assert_eq!(preserve_case("user", "Account"), "account");
    assert_eq!(preserve_case("User-name", "account-id"), "Account-id");
    assert_eq!(preserve_case("USER_name", "account_id"), "ACCOUNT_id");
    assert_eq!(preserve_case("Élan", "été"), "Été");
}

/// #1612 negatives: parts that do not line up are not split, a match
/// with no case gives the replacement as typed, and nothing is lost.
#[test]
fn preserve_case_leaves_what_it_cannot_read() {
    // Two parts against one: the whole match decides (mixed → capitalise).
    assert_eq!(preserve_case("User-Name", "account"), "Account");
    // Both separators: no part-by-part shaping.
    assert_eq!(preserve_case("a-b_C", "x-y_z"), "x-y_z");
    assert_eq!(preserve_case("", "Account"), "Account");
    assert_eq!(preserve_case("1-2", "a-B"), "A-B");
    assert_eq!(preserve_case("_x", "Ab"), "ab");
}

/// #1612: in regex mode the expanded captures are shaped too, and the
/// inline old→new preview shows what Replace All will write.
#[test]
fn preserve_case_shapes_regex_replacements_and_the_preview() {
    let opts = SearchOpts {
        use_regex: true,
        preserve_case: true,
        ..SearchOpts::default()
    };
    let (out, n) =
        replace_in_text("USER_TABLE user_id", r"user_(\w+)", "account_$1", opts).unwrap();
    assert_eq!((out.as_str(), n), ("ACCOUNT_TABLE account_id", 2));
    let literal = SearchOpts {
        preserve_case: true,
        ..SearchOpts::default()
    };
    assert_eq!(
        expand_replacement("User", "user", "account", literal),
        "Account"
    );
    assert_eq!(
        expand_replacement("User", "user", "account", SearchOpts::default()),
        "account"
    );
}

/// #1612: the replace row carries an `AB` chip that maps to Preserve
/// Case and names its key; with the row closed there is no chip.
#[test]
fn the_replace_row_has_a_preserve_case_toggle() {
    use ratatui::buffer::Buffer;
    let tmp = TempDir::new().unwrap();
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    panel.focused = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 12,
    };
    let mut buf = Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
    assert_eq!(panel.toggle_preserve_x, 0, "no replace row, no chip");
    panel.open_replace();
    let mut buf = Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
    let (x, y) = (panel.toggle_preserve_x, panel.toggle_preserve_y);
    assert_ne!(x, 0);
    assert_eq!(buf[(x, y)].symbol(), "A");
    assert_eq!(buf[(x + 1, y)].symbol(), "B");
    assert_eq!(panel.toggle_at(x + 1, y), Some(SearchToggle::PreserveCase));
    assert_eq!(panel.tooltip_at(x, y), Some("Preserve Case (Alt+P)"));
    assert!(panel.toggle_opt(SearchToggle::PreserveCase));
    assert!(panel.opts.preserve_case);
    // Negative: the cell left of the chip is the replace field.
    assert_eq!(panel.toggle_at(x - 1, y), None);
}

#[test]
fn expand_replacement_honours_capture_references_in_regex_mode() {
    let opts = SearchOpts {
        use_regex: true,
        ..Default::default()
    };
    assert_eq!(
        expand_replacement("beta_42", r"beta_(\d+)", "id-$1", opts),
        "id-42",
        "regex mode expands $1 against the matched slice"
    );
    let literal = SearchOpts::default();
    assert_eq!(
        expand_replacement("beta", "beta", "x$1y", literal),
        "x$1y",
        "literal mode inserts the replacement verbatim"
    );
}

#[test]
fn toggling_replace_open_focuses_the_replace_input() {
    let mut panel = SearchPanel::new(PathBuf::from("/tmp"));
    assert_eq!(panel.field, SearchField::Query);
    panel.toggle_replace();
    assert_eq!(
        panel.field,
        SearchField::Replace,
        "revealing the row must focus it — typing used to extend the query"
    );
    panel.toggle_replace();
    assert_eq!(panel.field, SearchField::Query, "collapsing returns focus");
}
#[test]
fn zero_width_regex_matches_advance_and_mark_positions() {
    let opts = SearchOpts {
        use_regex: true,
        ..Default::default()
    };
    let segs = split_for_highlight("foo", r"\b", opts);
    // Two boundary positions, the text emitted exactly once between
    // them — the old code re-emitted it and rows doubled ("foofoo").
    assert_eq!(
        segs,
        vec![
            (String::new(), true),
            (String::from("foo"), false),
            (String::new(), true),
        ]
    );
}

/// The pre-filter exists only to SKIP work, so the one thing it must
/// never do is say `false` for a line that really matches. Checked
/// against the exact matcher over the cases that tempt a shortcut:
/// case folding, whole-word narrowing, regex, and non-ASCII.
#[test]
fn the_scan_prefilter_never_hides_a_real_match() {
    let lines = [
        "ERROR disk full",
        "error disk full",
        "ErRoR",
        "no match here",
        "",
        "prefix_error_suffix",
        "\u{440}\u{443}\u{441}\u{441}\u{43a}\u{438}\u{439} error",
        "\u{130}stanbul error",
        "tab\terror",
    ];
    let needles = ["error", "ERROR", "err", "\u{440}\u{443}", "zzz", ""];
    let mut opt_sets = Vec::new();
    for case_sensitive in [true, false] {
        for whole_word in [true, false] {
            for use_regex in [true, false] {
                opt_sets.push(SearchOpts {
                    case_sensitive,
                    whole_word,
                    use_regex,
                    ..SearchOpts::default()
                });
            }
        }
    }
    for line in lines {
        for needle in needles {
            for opts in &opt_sets {
                let real =
                    !crate::widgets::editor_find::line_matches(line, *opts, needle).is_empty();
                if real {
                    assert!(
                        line_may_match(line, needle, *opts),
                        "pre-filter hid a real match: line={line:?} needle={needle:?} \
                             case_sensitive={} whole_word={} regex={}",
                        opts.case_sensitive,
                        opts.whole_word,
                        opts.use_regex
                    );
                }
            }
        }
    }
}

/// And it must actually skip something, or it is only overhead.
#[test]
fn the_scan_prefilter_rejects_lines_that_cannot_match() {
    let opts = SearchOpts::default();
    assert!(!line_may_match("nothing to see", "error", opts));
    assert!(!line_may_match(
        "short",
        "a longer needle than the line",
        opts
    ));
    let ci = SearchOpts {
        case_sensitive: false,
        ..SearchOpts::default()
    };
    assert!(line_may_match("ERROR here", "error", ci), "case folds");
    assert!(!line_may_match("ERROR here", "warning", ci));
}

/// #860: two matches on one line are two results. The header counted hit
/// LINES ("3 matches") while the Replace All confirmation counted
/// occurrences ("Replace 4 occurrence(s) across 2 file(s)").
#[test]
fn the_header_counts_occurrences_as_replace_all_does() {
    let tmp = TempDir::new().unwrap();
    write(&tmp.path().join("a.txt"), "foo foo\nfoo\n");
    write(&tmp.path().join("b.txt"), "foo\n");
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    panel.query = "foo".into();
    panel.run_query();
    assert_eq!(panel.hits.len(), 3, "one hit per matching line");
    assert_eq!(panel.result_counts(), (4, 2));
    assert_eq!(panel.count_replacements(&HashSet::new()), (4, 2));
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 14,
    };
    let mut buf = Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
    let text = buffer_to_string(&buf);
    assert!(text.contains("4 results in 2 files"), "{text}");
    // An unsaved buffer is searched in memory and counts the same way,
    // whole word included (`foobar` is not a match, as in Replace All).
    let mut out = Vec::new();
    let whole_word = SearchOpts {
        whole_word: true,
        ..SearchOpts::default()
    };
    collect_matches_in_text(
        Path::new("c.txt"),
        "foo foobar foo",
        "foo",
        whole_word,
        &mut out,
    );
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].matches, 2);
}

/// #860 regression, through the render alone: the header counts
/// occurrences ("4 results in 2 files"), where it counted hit lines
/// ("3 matches") against Replace All's 4 occurrences.
#[test]
fn the_rendered_header_counts_two_matches_on_a_line_as_two() {
    let tmp = TempDir::new().unwrap();
    write(&tmp.path().join("a.txt"), "foo foo\nfoo\n");
    write(&tmp.path().join("b.txt"), "foo\n");
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    panel.query = "foo".into();
    panel.run_query();
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 14,
    };
    let mut buf = Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
    let text = buffer_to_string(&buf);
    assert!(text.contains("4 results in 2 files"), "{text}");
}

/// #860 review guard: the header hashes a path once per run of hits,
/// but a file whose hits are not side by side still counts as one file,
/// and every hit's results still count.
#[test]
fn a_file_whose_hits_are_apart_still_counts_once() {
    let mut panel = SearchPanel::new(PathBuf::from("/w"));
    let hit = |path: &str, line_no: usize, matches: usize| SearchHit {
        path: PathBuf::from(path),
        line_no,
        line_text: String::from("foo"),
        matches,
    };
    panel.hits = vec![
        hit("/w/a.txt", 1, 2),
        hit("/w/a.txt", 2, 1),
        hit("/w/b.txt", 1, 1),
        hit("/w/a.txt", 9, 1),
        hit("/w/c.txt", 3, 3),
    ];
    assert_eq!(panel.result_counts(), (8, 3));
    panel.hits.clear();
    assert_eq!(panel.result_counts(), (0, 0), "no hits, no files");
}

/// #860 negative: one occurrence in one file is singular on both
/// counts ("1 result in 1 file"), and a line matched twice in one file
/// still names one file.
#[test]
fn the_header_is_singular_for_one_result_and_counts_files_once() {
    let tmp = TempDir::new().unwrap();
    write(&tmp.path().join("a.txt"), "foo\nbar\n");
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    panel.query = "foo".into();
    panel.run_query();
    assert_eq!(panel.result_counts(), (1, 1));
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 14,
    };
    let mut buf = Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
    let text = buffer_to_string(&buf);
    assert!(text.contains("1 result in 1 file"), "{text}");
    assert!(!text.contains("1 results"), "{text}");
    write(&tmp.path().join("a.txt"), "foo foo foo\n");
    panel.run_query();
    assert_eq!(panel.result_counts(), (3, 1), "three results, one file");
}

/// The rendered text of the row for `path_line` (e.g. "a.txt:1:"), with
/// the side bar's borders stripped.
fn result_row(buf: &Buffer, path_line: &str) -> String {
    buffer_to_string(buf)
        .lines()
        .find(|l| l.contains(path_line))
        .unwrap_or_else(|| panic!("no row for {path_line}"))
        .trim_matches(|c| c == '│' || c == ' ')
        .to_string()
}

/// #2008: the right column becomes the scrollbar once the list
/// overflows, and a hit that streams in can do that before the next
/// render draws the bar: that column then has no path hint.
#[test]
fn the_column_the_scrollbar_takes_has_no_path_hint() {
    let tmp = TempDir::new().unwrap();
    let mut panel = make_panel_with_hits(&tmp, 1);
    render_panel(&mut panel, 40);
    let fits = panel.results_viewport_height();
    panel.hits.clear();
    panel.add_hits(
        (0..fits)
            .map(|i| hit(&tmp.path().join("a.txt"), i + 1))
            .collect(),
    );
    render_panel(&mut panel, 40);
    let inner = panel.last_inner;
    let (row, right) = (
        inner.y + panel.results_start_offset,
        inner.x + inner.width - 1,
    );
    assert_eq!(
        panel.last_scrollbar,
        Rect::default(),
        "precondition: no overflow yet"
    );
    assert!(panel.result_path_at(right, row).is_some());
    panel.add_hits(vec![hit(&tmp.path().join("b.txt"), 1)]);
    assert_eq!(panel.result_path_at(right, row), None);
    assert!(panel.result_path_at(right - 1, row).is_some());
}

fn render_panel(panel: &mut SearchPanel, width: u16) -> Buffer {
    let area = Rect {
        x: 0,
        y: 0,
        width,
        height: 14,
    };
    let mut buf = Buffer::empty(area);
    ratatui::widgets::Widget::render(panel, area, &mut buf);
    buf
}

/// #860 (the issue's GIF): a match that starts past the side bar's width
/// was clipped away with everything after it, so the row showed only
/// leading context and the highlighted match (and its replace preview)
/// was invisible. The row now drops leading context instead, plainly
/// (no "…"), so the match and the replacement beside it show.
#[test]
fn a_match_past_the_panel_width_drops_leading_context_to_show() {
    let tmp = TempDir::new().unwrap();
    write(
        &tmp.path().join("a.txt"),
        "date,category,amount,description,payee,note\n",
    );
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    panel.query = "note".into();
    panel.run_query();
    let row = result_row(&render_panel(&mut panel, 40), "a.txt:1:");
    assert!(row.contains("note"), "the match is visible: {row:?}");
    assert!(row.starts_with("a.txt:1: "), "{row:?}");
    assert!(!row.contains('…'), "no ellipsis: {row:?}");
    panel.replace_open = true;
    panel.replace = "memo".into();
    let row = result_row(&render_panel(&mut panel, 40), "a.txt:1:");
    assert!(
        row.contains("notememo"),
        "the match and its replacement both show: {row:?}"
    );
}

/// #860 negative: a match that shows where it is (near the start, or
/// ending exactly on the row's last cell) is drawn exactly as its line
/// reads, leading context and all, and the context after it is cut at
/// the edge as before.
#[test]
fn a_match_that_already_shows_renders_as_before() {
    let tmp = TempDir::new().unwrap();
    // 40 wide: 38 inside the border, " a.txt:1: " takes 10, 28 remain.
    let near = "note,date,category,amount,description,payee";
    let edge = "date,category,amount,xxxnote,payee,description";
    assert_eq!(
        edge.find("note").unwrap() + 4,
        28,
        "fixture: ends on the edge"
    );
    write(&tmp.path().join("a.txt"), &format!("{near}\n"));
    write(&tmp.path().join("b.txt"), &format!("{edge}\n"));
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    panel.query = "note".into();
    panel.run_query();
    let buf = render_panel(&mut panel, 40);
    assert_eq!(
        result_row(&buf, "a.txt:1:"),
        format!("a.txt:1: {}", &near[..28])
    );
    assert_eq!(
        result_row(&buf, "b.txt:1:"),
        format!("a.txt:1: {}", &edge[..28]).replacen("a.txt", "b.txt", 1)
    );
}

/// #860 negative: the trim is by display width, so leading context of
/// double-width characters is dropped whole (never half a glyph), and
/// the match still shows.
#[test]
fn leading_context_of_wide_characters_is_dropped_whole() {
    let tmp = TempDir::new().unwrap();
    write(
        &tmp.path().join("a.txt"),
        "日本語のテキストが長く続いた後にnoteがある\n",
    );
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    panel.query = "note".into();
    panel.run_query();
    let row = result_row(&render_panel(&mut panel, 40), "a.txt:1:");
    assert!(row.contains("note"), "{row:?}");
    assert!(row.starts_with("a.txt:1: "), "{row:?}");
    assert!(!row.contains('…'), "{row:?}");
}

/// A panel with two results for `needle` in `a.rs`, the first one
/// selected, on the default theme (Croft Loch).
fn loch_results_panel(tmp: &TempDir) -> SearchPanel {
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    assert!(
        crate::widgets::surface::tones(panel.theme).is_some(),
        "the default theme is Croft Loch"
    );
    panel.query = "needle".into();
    panel.hits = (1..=2)
        .map(|line_no| SearchHit {
            path: tmp.path().join("a.rs"),
            line_no,
            line_text: String::from("let needle = 1;"),
            matches: 1,
        })
        .collect();
    panel.selected = 0;
    panel
}

/// The column and row where `label` starts on screen.
fn cell_of(buf: &Buffer, label: &str) -> (u16, u16) {
    for y in buf.area.top()..buf.area.bottom() {
        let row: String = (buf.area.left()..buf.area.right())
            .map(|x| buf[(x, y)].symbol())
            .collect();
        if let Some(i) = row.find(label) {
            return (row[..i].chars().count() as u16, y);
        }
    }
    panic!("{label} not on screen:\n{}", buffer_to_string(buf));
}

/// Croft Loch: a result row reads in the ink ramp (file name in body
/// text, line number and preview muted), the selected row wears the
/// shared selected-row anatomy across its full width (the band, and
/// the accent bar in the pane's left padding while the view has focus)
/// with its inks a step up, and the file name is never a teal chip.
#[test]
fn croft_loch_result_rows_use_the_ink_ramp_and_the_selected_row_band() {
    use crate::widgets::surface::{ROW_BAR, selected_row_fill, tones};
    let tmp = TempDir::new().unwrap();
    let mut panel = loch_results_panel(&tmp);
    let t = tones(panel.theme).unwrap();
    panel.focused = true;
    let buf = render_panel(&mut panel, 40);
    let band = crate::gradient::rgb_color(selected_row_fill(t, t.chrome, true));

    let (x, y) = cell_of(&buf, "a.rs:1:");
    let area = panel.last_area;
    assert_eq!(buf[(area.x, y)].symbol(), ROW_BAR, "the bar in the padding");
    assert_eq!(buf[(area.x, y)].fg, t.accent());
    let right = panel.last_inner.x + panel.last_inner.width - 1;
    for cx in area.x..=right {
        if (x + 12..x + 18).contains(&cx) {
            continue; // the match's own tint
        }
        assert_eq!(buf[(cx, y)].bg, band, "the band spans the row ({cx})");
    }
    assert_eq!(buf[(x, y)].fg, t.text(), "the file name is body text");
    assert!(!buf[(x, y)].modifier.contains(Modifier::BOLD));
    assert_eq!(buf[(x + 5, y)].fg, t.muted(), "the line number");
    assert_eq!(buf[(x + 8, y)].fg, t.text(), "a step-up preview");

    let (x2, y2) = cell_of(&buf, "a.rs:2:");
    assert_eq!(buf[(x2, y2)].fg, t.text());
    assert_eq!(buf[(x2, y2)].bg, t.chrome(), "other rows sit on the chrome");
    assert_eq!(buf[(x2 + 5, y2)].fg, t.muted(), "the line number is muted");
    assert_eq!(buf[(x2 + 8, y2)].fg, t.muted(), "the preview is muted");
    assert_ne!(buf[(area.x, y2)].symbol(), ROW_BAR);

    // Without focus the band greys and the bar goes.
    panel.focused = false;
    let buf = render_panel(&mut panel, 40);
    let (_, y) = cell_of(&buf, "a.rs:1:");
    assert_ne!(buf[(area.x, y)].symbol(), ROW_BAR, "no bar without focus");
    assert_eq!(
        buf[(area.x + 3, y)].bg,
        crate::gradient::rgb_color(selected_row_fill(t, t.chrome, false))
    );
}

/// Croft Loch: a result's `:N:` line number is where the hit is, so it
/// reads at 3:1 or better on every row it sits on, the plain chrome and
/// the selected row's band with and without focus, never in the faint
/// ink that sank to about 2:1.
#[test]
fn croft_loch_result_line_numbers_stay_readable_on_every_row() {
    use crate::theme::contrast_ratio;
    use crate::widgets::surface::tones;
    let rgb = |c: Color| match c {
        Color::Rgb(r, g, b) => (r, g, b),
        other => panic!("an RGB colour, not {other:?}"),
    };
    let tmp = TempDir::new().unwrap();
    let mut panel = loch_results_panel(&tmp);
    let t = tones(panel.theme).unwrap();
    for focused in [true, false] {
        panel.focused = focused;
        let buf = render_panel(&mut panel, 40);
        for label in ["a.rs:1:", "a.rs:2:"] {
            let (x, y) = cell_of(&buf, label);
            let digit = &buf[(x + 5, y)];
            assert_eq!(digit.symbol(), &label[5..6]);
            assert_ne!(digit.fg, t.faint(), "{label} focused={focused}");
            let ratio = contrast_ratio(rgb(digit.fg), rgb(digit.bg));
            assert!(ratio >= 3.0, "{label} focused={focused}: {ratio:.2}:1");
        }
    }
}

/// Croft Loch: a match in a result's preview is the amber tint over
/// the row's own fill (the selected row's first match the stronger,
/// current one), never the solid yellow block, and its ink reads on it
/// at 4.5:1 or better.
#[test]
fn croft_loch_result_matches_are_an_amber_tint_of_their_row() {
    use crate::widgets::surface::{match_style, selected_row_fill, tones};
    let tmp = TempDir::new().unwrap();
    let mut panel = loch_results_panel(&tmp);
    let t = tones(panel.theme).unwrap();
    panel.focused = true;
    let buf = render_panel(&mut panel, 40);
    let band = crate::gradient::rgb_color(selected_row_fill(t, t.chrome, true));
    let tint = |row: Color, active: bool| {
        match_style(Style::default().bg(row), t, panel.theme, active)
            .bg
            .unwrap()
    };
    for (label, row, active) in [("a.rs:1:", band, true), ("a.rs:2:", t.chrome(), false)] {
        let (x, y) = cell_of(&buf, label);
        let first = x + 12; // past "a.rs:N: let "
        for cx in first..first + 6 {
            let cell = &buf[(cx, y)];
            assert_eq!(cell.symbol(), &"needle"[usize::from(cx - first)..][..1]);
            assert_eq!(cell.bg, tint(row, active), "{label} match cell {cx}");
            assert_ne!(cell.bg, Color::Rgb(0xff, 0xd7, 0x4a));
            assert!(!cell.modifier.contains(Modifier::BOLD));
            let (Color::Rgb(fr, fg, fb), Color::Rgb(br, bg, bb)) = (cell.fg, cell.bg) else {
                panic!("RGB inks: {cell:?}");
            };
            assert!(crate::theme::contrast_ratio((fr, fg, fb), (br, bg, bb)) >= 4.5);
        }
        assert_eq!(buf[(first + 6, y)].bg, row, "the tint stops at the match");
    }
}

/// Croft Loch: the pointer over an unselected result lifts it with the
/// neutral hover fill, and its match is tinted over that lift.
#[test]
fn croft_loch_hovered_result_rows_take_the_neutral_lift() {
    use crate::widgets::surface::{match_style, tones};
    let tmp = TempDir::new().unwrap();
    let mut panel = loch_results_panel(&tmp);
    let t = tones(panel.theme).unwrap();
    let buf = render_panel(&mut panel, 40);
    let (x, y) = cell_of(&buf, "a.rs:2:");
    panel.hover_pointer = Some((x, y));
    let buf = render_panel(&mut panel, 40);
    let lift = t.hover_on(t.chrome);
    assert_eq!(buf[(x, y)].bg, lift);
    assert_eq!(buf[(x, y)].fg, t.text());
    assert_eq!(
        buf[(x + 12, y)].bg,
        match_style(Style::default().bg(lift), t, panel.theme, false)
            .bg
            .unwrap()
    );
}

/// Croft Loch: the result summary is a muted sentence whose counts are
/// body text, without the italic, and the title follows the Explorer's
/// (strong while focused, muted otherwise, never bold).
#[test]
fn croft_loch_summary_and_title_follow_the_ink_ramp() {
    let tmp = TempDir::new().unwrap();
    let mut panel = loch_results_panel(&tmp);
    let t = crate::widgets::surface::tones(panel.theme).unwrap();
    panel.focused = true;
    let buf = render_panel(&mut panel, 40);
    let (x, y) = cell_of(&buf, "2 results in 1 file");
    assert_eq!(buf[(x, y)].fg, t.text(), "the count");
    assert_eq!(buf[(x + 2, y)].fg, t.muted(), "the words");
    assert_eq!(buf[(x + 13, y)].fg, t.text(), "the file count");
    assert!(!buf[(x + 2, y)].modifier.contains(Modifier::ITALIC));
    let (tx, ty) = cell_of(&buf, "SEARCH");
    assert_eq!(buf[(tx, ty)].fg, t.strong());
    assert!(!buf[(tx, ty)].modifier.contains(Modifier::BOLD));
    // The query (the first "needle" on screen, above the results) is
    // body text in its field.
    let (qx, qy) = cell_of(&buf, "needle");
    assert!(qy < y, "the field sits above the summary");
    assert_eq!(buf[(qx, qy)].fg, t.text(), "the query is body text");
    panel.focused = false;
    let buf = render_panel(&mut panel, 40);
    assert_eq!(buf[(tx, ty)].fg, t.muted());
    // An empty field's placeholder is muted.
    panel.query.clear();
    let buf = render_panel(&mut panel, 40);
    let (px, py) = cell_of(&buf, "Search");
    assert_eq!(buf[(px, py)].fg, t.muted(), "the placeholder is muted");
}

/// The boxed themes keep their result rows exactly: Croft Black's
/// selected file name on the teal chip in bold white and its matches
/// black on the gold block, with no row bar.
#[test]
fn croft_black_result_rows_keep_the_teal_chip_and_gold_matches() {
    let tmp = TempDir::new().unwrap();
    let mut panel = loch_results_panel(&tmp);
    panel.theme = crate::theme::Theme::BLACK;
    panel.focus_gradient = true;
    panel.focused = true;
    let buf = render_panel(&mut panel, 40);
    let (x, y) = cell_of(&buf, "a.rs:1:");
    let chip = crate::gradient::rgb_color(crate::gradient::POPUP_SEL_BG);
    assert_eq!(buf[(x, y)].bg, chip);
    assert_eq!(buf[(x, y)].fg, Color::White);
    assert!(buf[(x, y)].modifier.contains(Modifier::BOLD));
    assert_ne!(
        buf[(x + 5, y)].bg,
        chip,
        "only the file name wears the chip"
    );
    assert_eq!(buf[(x + 12, y)].bg, Color::Rgb(0xff, 0xd7, 0x4a));
    assert_eq!(buf[(x + 12, y)].fg, Color::Black);
    assert_ne!(
        buf[(panel.last_area.x, y)].symbol(),
        crate::widgets::surface::ROW_BAR
    );
}
