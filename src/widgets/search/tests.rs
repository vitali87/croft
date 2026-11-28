//! The search panel's own tests, moved out of search.rs (#2062).

use super::*;

use std::fs;
use tempfile::TempDir;

fn write(p: &Path, s: &str) {
    fs::write(p, s).unwrap();
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
    let mut cx = ViewCx::default();
    use ratatui::buffer::Buffer;
    let tmp = TempDir::new().unwrap();
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    cx.focused = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 12,
    };
    let mut buf = Buffer::empty(area);
    panel.draw(&cx, area, &mut buf);
    let inner_x = panel.last_inner.x;
    let inner_y = panel.last_inner.y;
    // The caret (driven as the host's blinking hardware caret, like the
    // commit box) sits at the start of the input content when empty.
    assert_eq!(
        panel.cursor_screen_pos(cx.focused),
        Some((inner_x + INPUT_TYPED_COL, inner_y + INPUT_CONTENT_ROW)),
        "caret must sit at the start of the input content area when the query is empty"
    );
}

#[test]
fn cursor_sits_after_typed_text_when_query_non_empty_and_focused() {
    let mut cx = ViewCx::default();
    use ratatui::buffer::Buffer;
    let tmp = TempDir::new().unwrap();
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    cx.focused = true;
    panel.query = String::from("foo");
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 12,
    };
    let mut buf = Buffer::empty(area);
    panel.draw(&cx, area, &mut buf);
    let inner_x = panel.last_inner.x;
    let inner_y = panel.last_inner.y;
    // INPUT_TYPED_COL + "foo" (3 cells) → caret sits 3 cells past the start
    // of typed content.
    assert_eq!(
        panel.cursor_screen_pos(cx.focused),
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
    panel.draw(&ViewCx::default(), area, &mut buf);
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
    let mut cx = ViewCx::default();
    use ratatui::buffer::Buffer;
    let tmp = TempDir::new().unwrap();
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    // The boxed input; Croft Loch draws it as an underlined field.
    cx.theme = crate::theme::Theme::BLACK;
    cx.focused = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 12,
    };
    let mut buf = Buffer::empty(area);
    panel.draw(&cx, area, &mut buf);
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
            panel.draw(&ViewCx::default(), area, &mut buf);
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
    panel.draw(&ViewCx::default(), area, &mut buf);
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
    let mut cx = ViewCx::default();
    use ratatui::buffer::Buffer;
    let tmp = TempDir::new().unwrap();
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    // The boxed input; Croft Loch draws it as an underlined field.
    cx.theme = crate::theme::Theme::BLACK;
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 10,
    };
    let mut buf = Buffer::empty(area);
    panel.draw(&cx, area, &mut buf);
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
    panel.draw(&cx, area, &mut buf2);
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
    panel.draw(&ViewCx::default(), area, &mut buf);
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
    let mut cx = ViewCx::default();
    use ratatui::buffer::Buffer;
    let tmp = TempDir::new().unwrap();
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    cx.focused = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 5,
    };
    let mut buf = Buffer::empty(area);
    panel.draw(&cx, area, &mut buf);
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
    let mut cx = ViewCx::default();
    use ratatui::buffer::Buffer;
    let tmp = TempDir::new().unwrap();
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    cx.focused = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 5,
    };
    let mut buf = Buffer::empty(area);
    panel.draw(&cx, area, &mut buf);
    assert_eq!(panel.paste_button_w, 0);
    assert!(!panel.paste_button_at(0, 1));
    assert!(!panel.paste_button_at(40, 1));
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
    panel.draw(&ViewCx::default(), area, &mut buf);
    let text = buffer_to_string(&buf);
    assert!(text.contains("Invalid regex: unclosed group"), "{text}");
    assert!(!text.contains("0 results"), "{text}");
    // Fixing the pattern brings the count back.
    panel.query = r"foo\(".into();
    panel.run_query();
    let mut buf = Buffer::empty(area);
    panel.draw(&ViewCx::default(), area, &mut buf);
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
    panel.draw(&ViewCx::default(), area, &mut buf);
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
    let mut cx = ViewCx::default();
    use ratatui::buffer::Buffer;
    let tmp = TempDir::new().unwrap();
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    cx.focused = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 12,
    };
    let mut buf = Buffer::empty(area);
    panel.draw(&cx, area, &mut buf);
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
    let mut cx = ViewCx::default();
    use ratatui::buffer::Buffer;
    let tmp = TempDir::new().unwrap();
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    cx.focused = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 12,
    };
    let mut buf = Buffer::empty(area);
    panel.draw(&cx, area, &mut buf);
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
    let mut cx = ViewCx::default();
    let tmp = TempDir::new().unwrap();
    let mut panel = make_panel_with_hits(&tmp, 20);
    cx.theme = crate::theme::Theme::DARK_BLUE;
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
    cx.pointer = Some((area.x + 1, row_y));
    let mut buf = ratatui::buffer::Buffer::empty(area);
    panel.draw(&cx, area, &mut buf);
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
    panel.draw(&ViewCx::default(), area, &mut buf);
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
    panel.draw(&ViewCx::default(), area, &mut buf);
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
    panel.draw(&ViewCx::default(), area, &mut buf);
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
    panel.draw(&ViewCx::default(), area, &mut buf);
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
        panel.draw(&ViewCx::default(), area, &mut buf);
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

// ---- replace ----

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
    let mut cx = ViewCx::default();
    use ratatui::buffer::Buffer;
    let tmp = TempDir::new().unwrap();
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    cx.focused = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 30,
        height: 20,
    };
    let draw = |panel: &mut SearchPanel| {
        let mut buf = Buffer::empty(area);
        panel.draw(&cx, area, &mut buf);
        buf
    };
    for c in "abcdef".chars() {
        panel.type_char(c);
    }
    draw(&mut panel);
    let (end_x, _) = panel.cursor_screen_pos(cx.focused).unwrap();
    panel.move_caret(|_, _| 3);
    draw(&mut panel);
    let (mid_x, y) = panel.cursor_screen_pos(cx.focused).unwrap();
    assert_eq!(end_x - mid_x, 3, "three chars left of the end");
    assert_eq!(y, panel.query_input_rect.y);
    // Far longer than the row: the caret stays inside it either way.
    panel.seed("0123456789".repeat(6), String::new(), String::new());
    let buf = draw(&mut panel);
    let r = panel.query_input_rect;
    let (cx, cy) = panel.cursor_screen_pos(cx.focused).unwrap();
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
    let mut cx = ViewCx::default();
    use ratatui::buffer::Buffer;
    let tmp = TempDir::new().unwrap();
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    cx.focused = true;
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
    panel.draw(&cx, area, &mut buf);
    let (end_x, _) = panel.cursor_screen_pos(cx.focused).unwrap();
    panel.click_caret(end_x - 4);
    assert_eq!(panel.caret(), 2);
    panel.click_caret(end_x + 5);
    assert_eq!(panel.caret(), 6, "past the text is the end");
}

#[test]
fn focused_query_field_exposes_a_caret_and_paints_no_static_block() {
    let mut cx = ViewCx::default();
    use ratatui::buffer::Buffer;
    let tmp = TempDir::new().unwrap();
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    cx.focused = true;
    panel.type_char('a');
    panel.type_char('b');
    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 20,
    };
    let mut buf = Buffer::empty(area);
    panel.draw(&cx, area, &mut buf);
    // The caret sits on the query row so the app can drive the host's
    // blinking hardware caret there, matching the Source Control commit
    // box, instead of painting a static block that never blinks.
    let (cx, cy) = panel
        .cursor_screen_pos(cx.focused)
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
    let mut cx = ViewCx::default();
    use ratatui::buffer::Buffer;
    let tmp = TempDir::new().unwrap();
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    cx.focused = false;
    panel.type_char('x');
    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 20,
    };
    let mut buf = Buffer::empty(area);
    panel.draw(&cx, area, &mut buf);
    assert!(
        panel.cursor_screen_pos(cx.focused).is_none(),
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
    let mut cx = ViewCx::default();
    use ratatui::buffer::Buffer;
    let tmp = TempDir::new().unwrap();
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    cx.focused = true;
    panel.replace_open = true;
    panel.details_open = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 24,
    };
    let mut buf = Buffer::empty(area);
    panel.draw(&cx, area, &mut buf);
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
    panel.draw(&ViewCx::default(), area, &mut buf);
    let collapsed_offset = panel.results_start_offset;
    panel.replace_open = true;
    panel.details_open = true;
    let mut buf2 = Buffer::empty(area);
    panel.draw(&ViewCx::default(), area, &mut buf2);
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
    panel.draw(&ViewCx::default(), area, &mut buf);
    // header(0) + blank(1) + query(2) + separator(3) + caption(4) → results
    // begin at row 5. The single-row query field saves two rows versus the
    // old 3-row box (which started results at 7).
    assert_eq!(
        panel.results_start_offset, 5,
        "collapsed layout starts results at the 5-row offset"
    );
}
/// #1612: the replace row carries an `AB` chip that maps to Preserve
/// Case and names its key; with the row closed there is no chip.
#[test]
fn the_replace_row_has_a_preserve_case_toggle() {
    let mut cx = ViewCx::default();
    use ratatui::buffer::Buffer;
    let tmp = TempDir::new().unwrap();
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    cx.focused = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 12,
    };
    let mut buf = Buffer::empty(area);
    panel.draw(&cx, area, &mut buf);
    assert_eq!(panel.toggle_preserve_x, 0, "no replace row, no chip");
    panel.open_replace();
    let mut buf = Buffer::empty(area);
    panel.draw(&cx, area, &mut buf);
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
    panel.draw(&ViewCx::default(), area, &mut buf);
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
    panel.draw(&ViewCx::default(), area, &mut buf);
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
    panel.draw(&ViewCx::default(), area, &mut buf);
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
    render_panel(&mut panel, &ViewCx::default(), 40);
    let fits = panel.results_viewport_height();
    panel.hits.clear();
    panel.add_hits(
        (0..fits)
            .map(|i| hit(&tmp.path().join("a.txt"), i + 1))
            .collect(),
    );
    render_panel(&mut panel, &ViewCx::default(), 40);
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

fn render_panel(panel: &mut SearchPanel, cx: &ViewCx, width: u16) -> Buffer {
    let area = Rect {
        x: 0,
        y: 0,
        width,
        height: 14,
    };
    let mut buf = Buffer::empty(area);
    panel.draw(cx, area, &mut buf);
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
    let row = result_row(
        &render_panel(&mut panel, &ViewCx::default(), 40),
        "a.txt:1:",
    );
    assert!(row.contains("note"), "the match is visible: {row:?}");
    assert!(row.starts_with("a.txt:1: "), "{row:?}");
    assert!(!row.contains('…'), "no ellipsis: {row:?}");
    panel.replace_open = true;
    panel.replace = "memo".into();
    let row = result_row(
        &render_panel(&mut panel, &ViewCx::default(), 40),
        "a.txt:1:",
    );
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
    let buf = render_panel(&mut panel, &ViewCx::default(), 40);
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
    let row = result_row(
        &render_panel(&mut panel, &ViewCx::default(), 40),
        "a.txt:1:",
    );
    assert!(row.contains("note"), "{row:?}");
    assert!(row.starts_with("a.txt:1: "), "{row:?}");
    assert!(!row.contains('…'), "{row:?}");
}

/// A panel with two results for `needle` in `a.rs`, the first one
/// selected, on the default theme (Croft Loch).
fn loch_results_panel(tmp: &TempDir) -> SearchPanel {
    let mut panel = SearchPanel::new(tmp.path().to_path_buf());
    assert!(
        crate::theme::tones(crate::theme::Theme::default()).is_some(),
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
    let mut cx = ViewCx::default();
    use crate::theme::tones;
    use crate::widgets::surface::{ROW_BAR, selected_row_fill};
    let tmp = TempDir::new().unwrap();
    let mut panel = loch_results_panel(&tmp);
    let t = tones(cx.theme).unwrap();
    cx.focused = true;
    let buf = render_panel(&mut panel, &cx, 40);
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
    cx.focused = false;
    let buf = render_panel(&mut panel, &cx, 40);
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
    let mut cx = ViewCx::default();
    use crate::theme::contrast_ratio;
    use crate::theme::tones;
    let rgb = |c: Color| match c {
        Color::Rgb(r, g, b) => (r, g, b),
        other => panic!("an RGB colour, not {other:?}"),
    };
    let tmp = TempDir::new().unwrap();
    let mut panel = loch_results_panel(&tmp);
    let t = tones(cx.theme).unwrap();
    for focused in [true, false] {
        cx.focused = focused;
        let buf = render_panel(&mut panel, &cx, 40);
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
    let mut cx = ViewCx::default();
    use crate::theme::tones;
    use crate::widgets::surface::{match_style, selected_row_fill};
    let tmp = TempDir::new().unwrap();
    let mut panel = loch_results_panel(&tmp);
    let t = tones(cx.theme).unwrap();
    cx.focused = true;
    let buf = render_panel(&mut panel, &cx, 40);
    let band = crate::gradient::rgb_color(selected_row_fill(t, t.chrome, true));
    let tint = |row: Color, active: bool| {
        match_style(Style::default().bg(row), t, cx.theme, active)
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
    let mut cx = ViewCx::default();
    use crate::theme::tones;
    use crate::widgets::surface::match_style;
    let tmp = TempDir::new().unwrap();
    let mut panel = loch_results_panel(&tmp);
    let t = tones(cx.theme).unwrap();
    let buf = render_panel(&mut panel, &cx, 40);
    let (x, y) = cell_of(&buf, "a.rs:2:");
    cx.pointer = Some((x, y));
    let buf = render_panel(&mut panel, &cx, 40);
    let lift = t.hover_on(t.chrome);
    assert_eq!(buf[(x, y)].bg, lift);
    assert_eq!(buf[(x, y)].fg, t.text());
    assert_eq!(
        buf[(x + 12, y)].bg,
        match_style(Style::default().bg(lift), t, cx.theme, false)
            .bg
            .unwrap()
    );
}

/// Croft Loch: the result summary is a muted sentence whose counts are
/// body text, without the italic, and the title follows the Explorer's
/// (strong while focused, muted otherwise, never bold).
#[test]
fn croft_loch_summary_and_title_follow_the_ink_ramp() {
    let mut cx = ViewCx::default();
    let tmp = TempDir::new().unwrap();
    let mut panel = loch_results_panel(&tmp);
    let t = crate::theme::tones(cx.theme).unwrap();
    cx.focused = true;
    let buf = render_panel(&mut panel, &cx, 40);
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
    cx.focused = false;
    let buf = render_panel(&mut panel, &cx, 40);
    assert_eq!(buf[(tx, ty)].fg, t.muted());
    // An empty field's placeholder is muted.
    panel.query.clear();
    let buf = render_panel(&mut panel, &cx, 40);
    let (px, py) = cell_of(&buf, "Search");
    assert_eq!(buf[(px, py)].fg, t.muted(), "the placeholder is muted");
}

/// The boxed themes keep their result rows exactly: Croft Black's
/// selected file name on the teal chip in bold white and its matches
/// black on the gold block, with no row bar.
#[test]
fn croft_black_result_rows_keep_the_teal_chip_and_gold_matches() {
    let mut cx = ViewCx::default();
    let tmp = TempDir::new().unwrap();
    let mut panel = loch_results_panel(&tmp);
    cx.theme = crate::theme::Theme::BLACK;
    cx.focused = true;
    let buf = render_panel(&mut panel, &cx, 40);
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

/// #2751: a wheel step and a click read in the same burst: the click meets
/// the rows the last frame drew, not the ones the wheel has moved there
/// before the next frame.
#[test]
fn a_click_after_a_wheel_step_meets_the_drawn_rows() {
    let tmp = TempDir::new().unwrap();
    let mut panel = make_panel_with_hits(&tmp, 100);
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 30,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    panel.draw(&ViewCx::default(), area, &mut buf);
    let y = panel.last_inner.y + panel.results_start_offset + 1;
    let drawn = panel.hit_at_y(y);
    assert!(drawn.is_some(), "a row is drawn there");
    let before = panel.scroll;
    panel.scroll_down(3);
    assert!(panel.scroll > before, "the wheel scrolled the rows");
    assert_eq!(panel.hit_at_y(y), drawn, "the row drawn there");
}
