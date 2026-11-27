//! The Explorer's file tree's own tests, moved out of file_tree.rs (#2062).

use super::*;
use std::fs;
use tempfile::TempDir;

/// The plain name ink on the default theme (Croft Loch's body text):
/// what an unchanged, unignored row's name wears.
fn plain_ink() -> Color {
    crate::widgets::surface::tones(crate::theme::Theme::default())
        .map_or(Color::White, |t| t.text())
}

fn fixture() -> (TempDir, FileTree) {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    fs::create_dir(root.join("src")).unwrap();
    fs::write(root.join("README.md"), "# hi\n").unwrap();
    fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();
    fs::write(root.join("src/lib.rs"), "pub fn x() {}\n").unwrap();
    let tree = FileTree::new(root.to_path_buf());
    (tmp, tree)
}

/// Two-root fixture: the primary from `fixture()` plus a second root
/// holding `lib/util.rs`, appended via `add_root`.
fn two_root_fixture() -> (TempDir, TempDir, FileTree) {
    let (tmp, mut tree) = fixture();
    let second = TempDir::new().unwrap();
    fs::create_dir(second.path().join("lib")).unwrap();
    fs::write(second.path().join("lib/util.rs"), "pub fn u() {}\n").unwrap();
    fs::write(second.path().join("Cargo.toml"), "[package]\n").unwrap();
    tree.add_root(second.path().to_path_buf());
    (tmp, second, tree)
}

#[test]
fn add_root_appends_a_loaded_depth_zero_section_and_keeps_the_primary() {
    let (tmp, second, tree) = two_root_fixture();
    assert_eq!(tree.root, tmp.path(), "the primary root is unchanged");
    let roots: Vec<_> = tree.root_paths().collect();
    assert_eq!(roots, vec![tmp.path(), second.path()]);
    let second_idx = tree
        .nodes
        .iter()
        .position(|n| n.path == second.path())
        .expect("the second root is a row");
    assert_eq!(tree.nodes[second_idx].depth, 0);
    assert!(tree.nodes[second_idx].expanded);
    assert!(
        tree.nodes[second_idx + 1..]
            .iter()
            .any(|n| n.path.ends_with("lib")),
        "the second root's children load beneath it"
    );
    // Idempotent: adding an existing root is a no-op, not a duplicate.
    let count = tree.nodes.len();
    let mut tree = tree;
    tree.add_root(second.path().to_path_buf());
    assert_eq!(tree.nodes.len(), count);
}

#[test]
fn remove_root_drops_the_section_and_its_subtree_but_refuses_the_primary() {
    let (tmp, second, mut tree) = two_root_fixture();
    assert!(
        !tree.remove_root(tmp.path()),
        "the primary root is never removable through remove_root"
    );
    let before = tree.nodes.len();
    assert!(tree.remove_root(second.path()));
    assert!(
        tree.nodes.len() < before,
        "the section row and its subtree leave the list"
    );
    assert!(
        !tree.nodes.iter().any(|n| n.path.starts_with(second.path())),
        "no row under the removed root survives"
    );
    assert_eq!(
        tree.root_paths().collect::<Vec<_>>(),
        vec![tmp.path()],
        "back to a single-root tree"
    );
    assert!(tree.selected < tree.nodes.len(), "selection clamps");
    assert!(
        !tree.remove_root(second.path()),
        "removing again is a no-op"
    );
}

#[test]
fn removing_an_ancestor_root_keeps_marks_in_a_surviving_nested_root_section() {
    // A nested workspace root stays its own depth-0 section when its
    // ancestor root is removed; pruning marks by path prefix wrongly
    // swept the survivor's marks too (#148 review).
    let (_tmp, second, mut tree) = two_root_fixture();
    let nested = second.path().join("lib");
    tree.add_root(nested.clone());
    let util_idx = tree
        .nodes
        .iter()
        .rposition(|n| n.path.ends_with("util.rs"))
        .expect("the nested section lists its file");
    tree.toggle_mark(util_idx);
    let marked_path = tree.nodes[util_idx].path.clone();

    assert!(tree.remove_root(second.path()));

    assert!(
        tree.root_paths().any(|r| r == nested),
        "the nested root's own section survives"
    );
    assert!(
        tree.marked.contains(&marked_path),
        "the surviving section's marks survive with it"
    );
}

#[test]
fn collapse_all_keeps_every_root_section_expanded() {
    let (_tmp, second, mut tree) = two_root_fixture();
    // Expand a subfolder in each root first.
    let src_idx = tree
        .nodes
        .iter()
        .position(|n| n.path.ends_with("src"))
        .unwrap();
    tree.selected = src_idx;
    tree.expand_selected();
    let lib_idx = tree
        .nodes
        .iter()
        .position(|n| n.path.ends_with("lib"))
        .unwrap();
    tree.selected = lib_idx;
    tree.expand_selected();

    tree.collapse_all();

    let second_root = tree
        .nodes
        .iter()
        .find(|n| n.path == second.path())
        .expect("the second root row survives Collapse All");
    assert!(
        second_root.expanded,
        "every ROOT section stays expanded, exactly like the primary"
    );
    assert!(
        tree.nodes
            .iter()
            .filter(|n| n.depth > 0)
            .all(|n| !(n.is_dir && n.expanded)),
        "all non-root folders collapse"
    );
}

#[test]
fn reveal_path_resolves_through_the_owning_root() {
    let (_tmp, second, mut tree) = two_root_fixture();
    let target = second.path().join("lib/util.rs");
    assert!(
        tree.reveal_path(&target),
        "a file under the SECOND root must be revealable"
    );
    assert_eq!(tree.nodes[tree.selected].path, target);
}

#[test]
fn delete_guard_refuses_every_root_row() {
    let (_tmp, second, tree) = two_root_fixture();
    let second_node = tree.nodes.iter().find(|n| n.path == second.path());
    assert_eq!(
        delete_target_for(second_node, &tree.root),
        None,
        "a workspace root row is never a delete target, whichever root it is"
    );
}

#[test]
fn ignored_walk_stops_at_the_owning_root() {
    let (_tmp, second, mut tree) = two_root_fixture();
    // Only the SECOND root's parent dir is in the ignored set — a state
    // that cannot legitimately mark anything INSIDE that root ignored,
    // because the ancestor walk must stop at the owning root before
    // reaching outside it.
    let mut set = std::collections::HashSet::new();
    set.insert(second.path().parent().unwrap().to_path_buf());
    tree.ignored = std::sync::Arc::new(set);
    assert!(
        !tree.is_ignored(&second.path().join("lib/util.rs")),
        "the ancestor walk must stop at the owning root, never escaping into its parents"
    );
}

#[test]
fn focused_gradient_border_draws_rounded_corner_and_keeps_title() {
    use crate::gradient::{GRAD_TL, rgb_color};
    let (_tmp, mut tree) = fixture();
    tree.theme = crate::theme::Theme::BLACK;
    tree.focused = true;
    tree.focus_gradient = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 30,
        height: 6,
    };
    let mut buf = Buffer::empty(area);
    (&mut tree).render(area, &mut buf);
    // Rounded top-left corner in the gradient's top-left colour.
    assert_eq!(buf[(0, 0)].symbol(), "\u{256d}");
    assert_eq!(buf[(0, 0)].fg, rgb_color(GRAD_TL));
    // The EXPLORER title must survive the gradient repaint of the top edge.
    let top: String = (0..area.width).map(|x| buf[(x, 0)].symbol()).collect();
    assert!(
        top.contains("EXPLORER"),
        "title clobbered by gradient: {top:?}"
    );
}

/// The dot actually reaches the screen, on the right row, and clears
/// when the file is reviewed (#345).
///
/// Separate from the predicate test above on purpose: a predicate that
/// answers correctly while nothing draws it is the same to a user as a
/// feature that was never built, and the render is where every wiring
/// mistake between the two would live.
#[test]
fn the_lane_dot_is_painted_on_the_row_and_clears_when_reviewed() {
    let (_tmp, mut tree) = fixture();
    let root = tree.root.clone();
    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 8,
    };

    let row_containing = |buf: &Buffer, needle: &str| -> Option<String> {
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .find(|line| line.contains(needle))
    };

    // Nothing touched: no dot anywhere, which is the control that makes
    // the positive assertion below mean something.
    let mut buf = Buffer::empty(area);
    (&mut tree).render(area, &mut buf);
    let clean = row_containing(&buf, "main.rs").expect("main.rs is on screen");
    assert!(
        !clean.contains(AGENT_DOT),
        "a dot with nothing unreviewed: {clean:?}"
    );

    // One unreviewed file: its row takes the dot and its neighbour
    // does not.
    tree.agent_touched =
        std::sync::Arc::new(std::collections::HashSet::from([root.join("main.rs")]));
    let mut buf = Buffer::empty(area);
    (&mut tree).render(area, &mut buf);
    let marked = row_containing(&buf, "main.rs").expect("main.rs is on screen");
    assert!(
        marked.contains(AGENT_DOT),
        "the unreviewed file took no dot: {marked:?}"
    );
    let other = row_containing(&buf, "README.md").expect("README.md is on screen");
    assert!(
        !other.contains(AGENT_DOT),
        "an untouched file took a dot: {other:?}"
    );

    // Reviewed: the dot goes, and the row is otherwise what it was.
    tree.agent_touched = std::sync::Arc::new(std::collections::HashSet::new());
    let mut buf = Buffer::empty(area);
    (&mut tree).render(area, &mut buf);
    let cleared = row_containing(&buf, "main.rs").expect("main.rs is on screen");
    assert!(
        !cleared.contains(AGENT_DOT),
        "the dot outlived the review: {cleared:?}"
    );
    assert_eq!(
        cleared, clean,
        "clearing the dot must leave the row exactly as it was"
    );
}

/// A lane's root row wears its badge (#348): a render test for the same
/// reason as the dot's above, since the wiring lives in the painter.
#[test]
fn a_root_badge_is_painted_after_the_root_name_and_only_there() {
    // #348: a worktree lane's root row shows its branch and agent. The
    // badge follows the root's name, never a subdirectory's, and goes
    // when the map no longer names the root.
    let (_tmp, _second, mut tree) = two_root_fixture();
    let lane = tree.root_paths().nth(1).unwrap().to_path_buf();
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 10,
    };
    let rows = |buf: &Buffer| -> Vec<String> {
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect()
    };
    let mut buf = Buffer::empty(area);
    (&mut tree).render(area, &mut buf);
    assert!(
        !rows(&buf).iter().any(|r| r.contains("agent/")),
        "no badge before one is set"
    );
    tree.root_badges = Arc::new(HashMap::from([(
        lane.clone(),
        String::from("agent/fix-login \u{b7} \u{25c6} claude \u{25cf}"),
    )]));
    let mut buf = Buffer::empty(area);
    (&mut tree).render(area, &mut buf);
    let lane_name = lane.file_name().unwrap().to_string_lossy().into_owned();
    let rows_now = rows(&buf);
    let row = rows_now
        .iter()
        .find(|r| r.contains(&lane_name))
        .expect("the lane root row is on screen");
    assert!(
        row.contains(&format!(
            "{lane_name}  agent/fix-login \u{b7} \u{25c6} claude \u{25cf}"
        )),
        "the badge follows the root name: {row:?}"
    );
    assert_eq!(
        rows_now.iter().filter(|r| r.contains("agent/")).count(),
        1,
        "and appears on that row alone: {rows_now:?}"
    );
    tree.root_badges = Arc::default();
    let mut buf = Buffer::empty(area);
    (&mut tree).render(area, &mut buf);
    assert!(
        !rows(&buf).iter().any(|r| r.contains("agent/")),
        "cleared with the map"
    );
}

/// The depth-0 guard is load-bearing: a nested workspace root is both a
/// depth-0 section and a depth-1 child of its ancestor, with one path.
#[test]
fn a_root_badge_skips_the_same_path_where_it_appears_as_a_child() {
    let (_tmp, second, mut tree) = two_root_fixture();
    let nested = second.path().join("lib");
    // `add_root` loads the section's children, so `lib` is on screen
    // both as its own section and as the ancestor's child.
    tree.add_root(nested.clone());
    tree.root_badges = Arc::new(HashMap::from([(
        nested.clone(),
        String::from("agent/nested"),
    )]));
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 14,
    };
    let mut buf = Buffer::empty(area);
    (&mut tree).render(area, &mut buf);
    let rows: Vec<String> = (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect();
    assert!(
        rows.iter().filter(|r| r.contains("lib")).count() >= 2,
        "lib is on screen twice, as a section and as a child: {rows:?}"
    );
    assert_eq!(
        rows.iter().filter(|r| r.contains("agent/nested")).count(),
        1,
        "the badge is on the section row alone: {rows:?}"
    );
}

#[test]
fn collapse_all_closes_nested_folders_but_keeps_the_root_expanded() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    fs::create_dir_all(root.join("src/inner")).unwrap();
    fs::write(root.join("src/inner/deep.rs"), "//\n").unwrap();
    let mut tree = FileTree::new(root.to_path_buf());
    // Expand every ancestor of the deep file so three depth levels show.
    assert!(
        tree.reveal_path(&root.join("src/inner/deep.rs")),
        "precondition: reveal expands src and src/inner"
    );
    assert!(
        tree.nodes.iter().any(|n| n.path.ends_with("deep.rs")),
        "precondition: the deep file is visible before collapse"
    );

    tree.collapse_all();

    assert!(tree.nodes[0].expanded, "the workspace root stays expanded");
    assert_eq!(tree.nodes[0].depth, 0);
    assert!(
        tree.nodes.iter().skip(1).all(|n| !(n.is_dir && n.expanded)),
        "no subfolder remains expanded"
    );
    assert!(
        !tree.nodes.iter().any(|n| n.path.ends_with("deep.rs")),
        "nested descendants are dropped from the flattened list"
    );
    assert_eq!(tree.selected, 0);
    assert_eq!(tree.scroll, 0);
}

#[test]
fn views_pill_stays_clear_of_a_translated_explorer_title() {
    // From the narrowest pane whose top border holds the whole title.
    for width in 14..30u16 {
        let (_tmp, mut tree) = fixture();
        // A boxed title keeps its padding; Croft Loch's focus rule takes it.
        tree.theme = crate::theme::Theme::BLACK;
        tree.focus_gradient = false;
        let area = Rect {
            x: 0,
            y: 0,
            width,
            height: 8,
        };
        let mut buf = Buffer::empty(area);
        crate::i18n::with_test_catalog(r#"{"EXPLORER": "EXPLORADOR"}"#, || {
            (&mut tree).render(area, &mut buf);
        });
        let title_row: String = (0..area.width).map(|x| buf[(x, area.y)].symbol()).collect();
        assert!(
            title_row.contains(" EXPLORADOR "),
            "width {width}: the ⋯ pill painted over the title: {title_row:?}"
        );
        let btn = tree.header_views_btn;
        assert!(
            btn.width == 0 || btn.x >= area.x + 1 + 12,
            "width {width}: ⋯ pill {btn:?} overlaps the title"
        );
    }
    // Wide enough, the pill still renders.
    assert!(
        {
            let (_tmp, mut tree) = fixture();
            let area = Rect {
                x: 0,
                y: 0,
                width: 17,
                height: 8,
            };
            let mut buf = Buffer::empty(area);
            crate::i18n::with_test_catalog(r#"{"EXPLORER": "EXPLORADOR"}"#, || {
                (&mut tree).render(area, &mut buf);
            });
            tree.header_views_btn.width > 0
        },
        "the ⋯ pill must render once the translated title fits"
    );
}

#[test]
fn explorer_header_toolbar_paints_five_pills_and_hit_tests() {
    let (_tmp, mut tree) = fixture();
    tree.focused = true;
    tree.focus_gradient = false; // Croft Dark
    let area = Rect {
        x: 0,
        y: 0,
        width: 44,
        height: 8,
    };
    let mut buf = Buffer::empty(area);
    (&mut tree).render(area, &mut buf);

    // The toolbar lives on the root folder row (inner.y), not the title
    // border, mirroring VS Code's workspace-folder header actions.
    let root_y = tree.last_inner.y;
    let root_row: String = (0..area.width).map(|x| buf[(x, root_y)].symbol()).collect();
    for glyph in [
        crate::widgets::header_pill::NEW_FILE_GLYPH,
        crate::widgets::header_pill::NEW_FOLDER_GLYPH,
        crate::widgets::header_pill::REFRESH_GLYPH,
        crate::widgets::header_pill::COLLAPSE_ALL_GLYPH,
    ] {
        assert!(
            root_row.contains(glyph),
            "root row missing glyph {glyph:?}: {root_row:?}"
        );
    }

    for btn in [
        tree.header_new_file_btn,
        tree.header_new_folder_btn,
        tree.header_refresh_btn,
        tree.header_collapse_btn,
    ] {
        assert!(btn.width > 0, "button rect was not captured");
        assert_eq!(btn.y, root_y, "button sits on the root folder row");
    }
    // Painted left to right in VS Code's order, clear of the right border.
    assert!(tree.header_new_file_btn.x < tree.header_new_folder_btn.x);
    assert!(tree.header_new_folder_btn.x < tree.header_refresh_btn.x);
    assert!(tree.header_refresh_btn.x < tree.header_collapse_btn.x);
    assert!(tree.header_collapse_btn.x + tree.header_collapse_btn.width < area.x + area.width);

    // The ⋯ "Views and More Actions" button sits on the EXPLORER title
    // border row, not the toolbar row — that's the whole point of the
    // rework (the view-toggle affordance lives on the Explorer line).
    assert!(
        tree.header_views_btn.width > 0,
        "views (⋯) button rect was not captured"
    );
    assert_eq!(
        tree.header_views_btn.y, area.y,
        "the ⋯ button sits on the EXPLORER title line"
    );
    let title_row: String = (0..area.width).map(|x| buf[(x, area.y)].symbol()).collect();
    assert!(
        title_row.contains(crate::widgets::header_pill::MORE_LABEL.trim()),
        "title row missing the ⋯ glyph: {title_row:?}"
    );
}

#[test]
fn explorer_header_toolbar_is_withheld_when_the_panel_is_too_narrow() {
    let (_tmp, mut tree) = fixture();
    tree.focused = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 14,
        height: 6,
    };
    let mut buf = Buffer::empty(area);
    (&mut tree).render(area, &mut buf);
    assert_eq!(tree.header_new_file_btn.width, 0);
    assert_eq!(tree.header_collapse_btn.width, 0);
}

#[test]
fn explorer_header_toolbar_is_withheld_when_the_panel_is_unfocused() {
    let (_tmp, mut tree) = fixture();
    tree.focused = false;
    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 8,
    };
    let mut buf = Buffer::empty(area);
    (&mut tree).render(area, &mut buf);
    assert_eq!(tree.header_new_file_btn.width, 0);
    assert_eq!(tree.header_collapse_btn.width, 0);
}

/// Croft Loch: no box, a chrome surface, and the selection follows
/// VS Code's focus model: the band with the accent bar in the pane's
/// left padding column while the tree has focus, a grey band without the
/// bar while another pane does. The rows sit where the boxed themes put
/// them, and the focus lip starts one cell past the title.
#[test]
fn croft_loch_explorer_is_an_unboxed_surface_with_a_focus_aware_selection() {
    use crate::widgets::surface::{ROW_BAR, tones};
    let loch = crate::theme::Theme::from_id("loch");
    let s = loch.surfaces().unwrap();
    let t = tones(loch).unwrap();
    let area = Rect::new(0, 0, 30, 8);
    let render = |focused: bool| {
        let (_tmp, mut tree) = fixture();
        tree.theme = loch;
        tree.focus_gradient = loch.gradient();
        tree.focused = focused;
        tree.selected = 0;
        let mut buf = Buffer::empty(area);
        (&mut tree).render(area, &mut buf);
        (tree.last_inner, buf)
    };
    let (inner, unfocused) = render(false);
    assert_eq!(inner, Rect::new(1, 1, 28, 6), "the boxed inset is kept");
    assert_eq!(unfocused[(0, 3)].symbol(), " ", "no box on the left edge");
    assert_eq!(unfocused[(0, 3)].bg, s.chrome());
    assert_ne!(
        unfocused[(1, 1)].bg,
        s.chrome(),
        "a grey band while unfocused"
    );
    assert_ne!(unfocused[(1, 1)].bg, t.band_on(t.chrome));
    assert_eq!(unfocused[(0, 1)].symbol(), " ", "no bar while unfocused");
    assert_eq!(unfocused[(0, 0)].symbol(), " ", "no lip while unfocused");
    let (_, focused) = render(true);
    assert_eq!(
        focused[(1, 1)].bg,
        t.band_on(t.chrome),
        "the band while focused"
    );
    assert_eq!(focused[(0, 1)].symbol(), ROW_BAR, "the bar in the padding");
    assert_eq!(focused[(0, 1)].fg, t.accent());
    let top: String = (0..area.width).map(|x| focused[(x, 0)].symbol()).collect();
    assert!(
        top.starts_with("  EXPLORER \u{2594}"),
        "the lip starts a cell past the title: {top:?}"
    );
    assert_eq!(
        focused[(11, 0)].fg,
        t.accent(),
        "its accent end by the title"
    );
}

/// Croft Loch's type ramp: the title is the strong ink only while the
/// tree has focus, folder names are body text and not bold (only the
/// workspace root keeps its weight), and the chevrons are the muted
/// codicon pair the panel uses.
#[test]
fn croft_loch_tree_type_ramp_keeps_weight_for_the_root_alone() {
    use crate::widgets::surface::{CHEVRON_UNFOLDED, tones};
    let loch = crate::theme::Theme::LOCH;
    let t = tones(loch).unwrap();
    let area = Rect::new(0, 0, 30, 8);
    let (_tmp, mut tree) = fixture();
    tree.theme = loch;
    tree.focused = true;
    tree.selected = 2;
    let mut buf = Buffer::empty(area);
    (&mut tree).render(area, &mut buf);
    let at = |buf: &Buffer, needle: &str| -> (u16, u16) {
        for y in 0..area.height {
            let row: String = (0..area.width).map(|x| buf[(x, y)].symbol()).collect();
            if let Some(i) = row.find(needle) {
                return (row[..i].chars().count() as u16, y);
            }
        }
        panic!("{needle} not on screen");
    };
    let title = at(&buf, "EXPLORER");
    assert_eq!(buf[title].fg, t.strong(), "focused title");
    assert!(!buf[title].modifier.contains(Modifier::BOLD));
    let src = at(&buf, "src");
    assert_eq!(buf[src].fg, t.text());
    assert!(
        !buf[src].modifier.contains(Modifier::BOLD),
        "folders are not bold"
    );
    assert_eq!(buf[(src.0 - 4, src.1)].fg, t.muted(), "muted chevron");
    let root = tree.last_inner;
    assert!(
        buf[(root.x + 4, root.y)].modifier.contains(Modifier::BOLD),
        "the workspace root keeps its weight"
    );
    assert_eq!(buf[(root.x, root.y)].symbol(), CHEVRON_UNFOLDED.to_string());
    tree.focused = false;
    let mut buf = Buffer::empty(area);
    (&mut tree).render(area, &mut buf);
    assert_eq!(buf[at(&buf, "EXPLORER")].fg, t.muted(), "unfocused title");
}

/// Croft Loch draws a thin guide under each ancestor folder (never a
/// box glyph, which the side bar's seam test counts), brighter down the
/// selected row's parent; the root's level has none.
#[test]
fn croft_loch_draws_thin_indent_guides_and_brightens_the_active_one() {
    use crate::widgets::surface::tones;
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    fs::create_dir_all(root.join("a/b")).unwrap();
    fs::write(root.join("a/b/deep.rs"), "//\n").unwrap();
    fs::write(root.join("a/side.rs"), "//\n").unwrap();
    let mut tree = FileTree::new(root.to_path_buf());
    tree.theme = crate::theme::Theme::LOCH;
    let t = tones(tree.theme).unwrap();
    assert!(tree.reveal_path(&root.join("a/b/deep.rs")));
    let deep = tree
        .nodes
        .iter()
        .position(|n| n.path.ends_with("deep.rs"))
        .unwrap();
    tree.selected = deep;
    let area = Rect::new(0, 0, 40, 10);
    let mut buf = Buffer::empty(area);
    (&mut tree).render(area, &mut buf);
    let inner = tree.last_inner;
    let row_of = |needle: &str| -> u16 {
        (0..area.height)
            .find(|&y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
                    .contains(needle)
            })
            .unwrap()
    };
    let (deep_y, side_y, b_y) = (row_of("deep.rs"), row_of("side.rs"), row_of(" b"));
    // `deep.rs` sits at depth 3: guides for `a` (column 2) and `b`
    // (column 4); `b` is its parent, so that guide is the active one.
    assert_eq!(buf[(inner.x + 2, deep_y)].symbol(), "\u{258f}");
    assert_eq!(buf[(inner.x + 2, deep_y)].fg, t.hairline());
    assert_eq!(buf[(inner.x + 4, deep_y)].fg, t.faint(), "the active guide");
    assert_eq!(buf[(inner.x + 2, side_y)].fg, t.hairline());
    assert_eq!(buf[(inner.x + 2, b_y)].symbol(), "\u{258f}");
    assert_eq!(
        buf[(inner.x, b_y)].symbol(),
        " ",
        "no guide at the root level"
    );
    for y in 0..area.height {
        for x in 0..area.width {
            assert!(
                !"\u{2502}\u{250c}\u{2510}\u{2500}".contains(buf[(x, y)].symbol()),
                "a box glyph at ({x},{y})"
            );
        }
    }
}

/// Croft Loch: a hovered row lifts by the chrome's neutral step, a
/// marked row wears a soft tint of the band, the sticky ancestor band
/// sits on the raised tone so it reads against the rows, and the
/// scrollbar lane keeps the chrome.
#[test]
fn croft_loch_hover_marks_and_sticky_band_use_the_tones() {
    use crate::widgets::surface::tones;
    let loch = crate::theme::Theme::LOCH;
    let t = tones(loch).unwrap();
    let (_tmp, mut tree) = fixture();
    tree.theme = loch;
    tree.focused = true;
    tree.toggle_mark(2);
    tree.selected = 0;
    let area = Rect::new(0, 0, 30, 8);
    tree.hover_pointer = Some((2, 2));
    let mut buf = Buffer::empty(area);
    (&mut tree).render(area, &mut buf);
    assert_eq!(buf[(2, 2)].bg, t.hover_on(t.chrome), "hover lift");
    let marked = buf[(2, 3)].bg;
    assert_ne!(marked, t.chrome());
    assert_ne!(marked, t.band_on(t.chrome), "softer than the band");

    // A tall tree scrolled into a folder pins that folder on top.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    fs::create_dir(root.join("dir")).unwrap();
    for i in 0..20 {
        fs::write(root.join(format!("dir/f{i:02}.rs")), "//\n").unwrap();
    }
    let mut tree = FileTree::new(root.to_path_buf());
    tree.theme = loch;
    assert!(tree.reveal_path(&root.join("dir/f19.rs")));
    tree.selected = tree.nodes.len() - 1;
    let mut buf = Buffer::empty(area);
    (&mut tree).render(area, &mut buf);
    let &(y, _) = tree.sticky_rows_for_test().first().expect("a sticky row");
    assert_eq!(buf[(tree.last_inner.x + 3, y)].bg, t.raised());
    let bar = tree.last_scrollbar;
    assert!(bar.width > 0);
    let lane: Vec<_> = (bar.y..bar.bottom()).map(|y| buf[(bar.x, y)].bg).collect();
    assert!(lane.contains(&t.chrome()), "no canvas stripe: {lane:?}");
    assert!(!lane.contains(&loch.editor_bg()));
}

#[test]
fn hovering_an_unselected_row_lifts_it_without_touching_the_selection() {
    let (_tmp, mut tree) = fixture();
    tree.focused = true;
    tree.focus_gradient = false; // Croft Dark
    tree.theme = crate::theme::Theme::DARK_BLUE;
    tree.selected = 0; // the root row stays selected
    let area = Rect {
        x: 0,
        y: 0,
        width: 30,
        height: 8,
    };
    // Rest the pointer on the second visible row (a child of the root).
    let inner_y = area.y + 1;
    tree.hover_pointer = Some((area.x + 1, inner_y + 1));
    let mut buf = Buffer::empty(area);
    (&mut tree).render(area, &mut buf);
    assert_eq!(
        buf[(area.x + 1, inner_y + 1)].bg,
        Color::Rgb(0x2b, 0x31, 0x42),
        "the hovered child row wears the Croft Dark hover lift"
    );
    assert_eq!(
        buf[(area.x + 1, inner_y)].bg,
        Color::Rgb(0x09, 0x4d, 0x77),
        "the selected row keeps its selection bg, not the hover lift"
    );
}

#[test]
fn a_selected_row_is_not_overridden_by_hovering_it() {
    let (_tmp, mut tree) = fixture();
    // Croft Dark's selection blue; Croft Loch's band is checked below.
    tree.theme = crate::theme::Theme::DARK_BLUE;
    tree.focused = true;
    tree.focus_gradient = false;
    tree.selected = 0;
    let area = Rect {
        x: 0,
        y: 0,
        width: 30,
        height: 8,
    };
    let inner_y = area.y + 1;
    // Hover the selected row itself.
    tree.hover_pointer = Some((area.x + 1, inner_y));
    let mut buf = Buffer::empty(area);
    (&mut tree).render(area, &mut buf);
    assert_eq!(
        buf[(area.x + 1, inner_y)].bg,
        tree.theme.ui(Color::Rgb(0x09, 0x4d, 0x77)),
        "selection outranks hover so the row never dims to the hover lift"
    );
    let loch = crate::theme::Theme::LOCH;
    let t = crate::widgets::surface::tones(loch).unwrap();
    tree.theme = loch;
    let mut buf = Buffer::empty(area);
    (&mut tree).render(area, &mut buf);
    assert_eq!(buf[(area.x + 1, inner_y)].bg, t.band_on(t.chrome));
}

#[test]
fn croft_dark_keeps_solid_blue_focus_border() {
    let (_tmp, mut tree) = fixture();
    tree.theme = crate::theme::Theme::DARK_BLUE;
    tree.focused = true;
    tree.focus_gradient = false;
    let area = Rect {
        x: 0,
        y: 0,
        width: 30,
        height: 6,
    };
    let mut buf = Buffer::empty(area);
    (&mut tree).render(area, &mut buf);
    // Square corner, solid blue: the historical Croft Dark highlight.
    assert_eq!(buf[(0, 0)].symbol(), "\u{250c}");
    assert_eq!(buf[(0, 0)].fg, Color::Rgb(0x4e, 0x9a, 0xff));
}

#[test]
fn new_lists_root_and_children() {
    let (_tmp, tree) = fixture();
    // Root + 3 children (src/, main.rs, README.md).
    assert_eq!(tree.nodes.len(), 4);
    assert!(tree.nodes[0].is_dir);
    assert!(tree.nodes[0].expanded);
}

#[test]
fn explorer_lists_gitignored_files() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir(tmp.path().join(".git")).unwrap();
    fs::write(tmp.path().join(".gitignore"), "*.txt\n").unwrap();
    fs::write(tmp.path().join("dummy2.txt"), "").unwrap();
    let tree = FileTree::new(tmp.path().to_path_buf());
    assert!(
        tree.nodes.iter().any(|n| n.path.ends_with("dummy2.txt")),
        "Explorer must show disk reality, including gitignored files"
    );
}

/// An unreviewed file carries the agent-lane dot, and only that file
/// does (#345).
///
/// The dot is a decoration on the FILE the agent wrote, not on the
/// directories above it. A directory is not something an agent changed
/// and not something a review can clear, so marking the tree all the way
/// to the root would put a mark on rows that no gesture can ever remove —
/// which is why this is a set membership test rather than a prefix walk
/// like `is_ignored`.
#[test]
fn only_the_unreviewed_files_themselves_take_the_lane_dot() {
    let (_tmp, mut tree) = fixture();
    let root = tree.root.clone();
    tree.agent_touched =
        std::sync::Arc::new(std::collections::HashSet::from([root.join("src/main.rs")]));
    assert!(tree.is_agent_touched(&root.join("src/main.rs")));
    // Not the directories on the way to it: a directory cannot be
    // reviewed, so a mark there would never clear.
    assert!(
        !tree.is_agent_touched(&root.join("src")),
        "a parent directory must not inherit the dot"
    );
    assert!(!tree.is_agent_touched(&root));
    // Nor a sibling the agent did not touch.
    assert!(!tree.is_agent_touched(&root.join("src/other.rs")));
    // An empty set is the common case and must be cheap and quiet.
    tree.agent_touched = std::sync::Arc::new(std::collections::HashSet::new());
    assert!(!tree.is_agent_touched(&root.join("src/main.rs")));
}

/// #345: a collapsed folder with an unreviewed agent write anywhere
/// under it takes the dimmer rollup dot; expanded it does not (the
/// file's own dot shows), and it clears once the write is reviewed.
#[test]
fn a_collapsed_folder_rolls_up_an_unreviewed_agent_write() {
    let (_tmp, mut tree) = fixture();
    let root = tree.root.clone();
    let src = root.join("src");
    let row = |tree: &mut FileTree, name: &str| -> String {
        let area = Rect::new(0, 0, 40, 12);
        let mut buf = Buffer::empty(area);
        tree.render(area, &mut buf);
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .find(|l| l.contains(name))
            .unwrap_or_default()
    };
    tree.agent_touched = std::sync::Arc::new(std::collections::HashSet::from([src.join("lib.rs")]));
    assert!(tree.hides_agent_touched(&src));
    assert!(tree.hides_agent_touched(&root), "any depth rolls up");
    assert!(
        !tree.hides_agent_touched(&src.join("lib.rs")),
        "not the file itself"
    );
    let dir_idx = tree.nodes.iter().position(|n| n.path == src).unwrap();
    assert!(!tree.nodes[dir_idx].expanded);
    assert!(
        row(&mut tree, "src").contains(AGENT_DOT),
        "{:?}",
        row(&mut tree, "src")
    );
    // Expanded: the folder drops its dot; the file shows its own.
    tree.selected = dir_idx;
    tree.expand_selected();
    let expanded = row(&mut tree, "src");
    assert!(!expanded.contains(AGENT_DOT), "{expanded:?}");
    assert!(row(&mut tree, "lib.rs").contains(AGENT_DOT));
    // Reviewed: nothing left to roll up.
    tree.agent_touched = std::sync::Arc::new(std::collections::HashSet::new());
    assert!(!tree.hides_agent_touched(&src));
}

#[test]
fn is_ignored_covers_set_members_and_their_descendants() {
    let (_tmp, mut tree) = fixture();
    let root = tree.root.clone();
    tree.ignored = std::sync::Arc::new(std::collections::HashSet::from([
        root.join("target"),
        root.join("debug.log"),
    ]));
    assert!(tree.is_ignored(&root.join("debug.log")));
    assert!(tree.is_ignored(&root.join("target")));
    assert!(
        tree.is_ignored(&root.join("target/debug/deps/foo.rlib")),
        "descendants of an ignored dir are ignored too"
    );
    assert!(!tree.is_ignored(&root.join("main.rs")));
    assert!(!tree.is_ignored(&root));
}

/// Render `tree` into a 40x10 buffer and find `needle`'s row: its y, the
/// colour of its first cell, and the cell at the badge column (the
/// border takes the last column and one blank column sits before it).
fn row_with_badge(tree: &mut FileTree, needle: &str) -> (Color, String, Color) {
    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 10,
    };
    let mut buf = Buffer::empty(area);
    tree.render(area, &mut buf);
    let badge_x = area.width - 3;
    for y in 0..area.height {
        let cells: Vec<String> = (0..area.width)
            .map(|x| buf[(x, y)].symbol().to_string())
            .collect();
        let joined = cells.concat();
        let Some(target) = joined.find(needle) else {
            continue;
        };
        let mut acc = 0usize;
        for (x, s) in cells.iter().enumerate() {
            if acc == target {
                let badge = &buf[(badge_x, y)];
                return (buf[(x as u16, y)].fg, badge.symbol().to_string(), badge.fg);
            }
            acc += s.len();
        }
    }
    panic!("{needle} not on screen");
}

/// #1513: a changed file's name takes its status colour and its letter
/// sits at the row's right edge; a folder with changes inside takes the
/// colour and a dot.
#[test]
fn changed_rows_wear_their_git_status_colour_and_letter() {
    use crate::git::GitDecoration as D;
    let (_tmp, mut tree) = fixture();
    let root = tree.root.clone();
    tree.git_decorations = Arc::new(HashMap::from([
        (root.join("main.rs"), D::Modified),
        (root.join("README.md"), D::Added),
        (root.join("src"), D::Untracked),
        (root.join("src/lib.rs"), D::Untracked),
    ]));
    let theme = tree.theme;
    let (fg, badge, badge_fg) = row_with_badge(&mut tree, "main.rs");
    assert_eq!(fg, theme.git_decoration_fg(D::Modified));
    assert_eq!(badge, "M", "a modified file shows M at the right edge");
    assert_eq!(badge_fg, theme.git_decoration_fg(D::Modified));
    let (fg, badge, _) = row_with_badge(&mut tree, "README.md");
    assert_eq!(fg, theme.git_decoration_fg(D::Added));
    assert_eq!(badge, "A", "a staged new file shows A");
    let (fg, badge, badge_fg) = row_with_badge(&mut tree, "src");
    assert_eq!(fg, theme.git_decoration_fg(D::Untracked));
    assert_eq!(
        badge, "\u{25cf}",
        "a folder with changes inside shows a dot"
    );
    assert_eq!(badge_fg, theme.git_decoration_fg(D::Untracked));
}

/// #1513 negatives: an unchanged file, a git-ignored file, the workspace
/// root row and every row while the history scrubber shows a commit
/// carry no status colour or badge, whatever the map says.
#[test]
fn unchanged_ignored_root_and_scrubbed_rows_wear_no_git_status() {
    use crate::git::GitDecoration as D;
    let (_tmp, mut tree) = fixture();
    tree.focused = false;
    let root = tree.root.clone();
    let root_name = root.file_name().unwrap().to_string_lossy().into_owned();
    tree.git_decorations = Arc::new(HashMap::from([
        (root.clone(), D::Modified),
        (root.join("README.md"), D::Untracked),
        (root.join("src/lib.rs"), D::Modified),
    ]));
    tree.ignored = Arc::new(HashSet::from([root.join("README.md")]));
    let theme = tree.theme;
    let (fg, badge, _) = row_with_badge(&mut tree, "main.rs");
    assert_eq!((fg, badge.as_str()), (plain_ink(), " "), "unchanged");
    let (fg, badge, _) = row_with_badge(&mut tree, "README.md");
    assert_eq!(
        (fg, badge.as_str()),
        (theme.ignored_fg(), " "),
        "an ignored row stays dimmed and unbadged"
    );
    let (_, badge, _) = row_with_badge(&mut tree, &root_name);
    assert_eq!(badge, " ", "the workspace root row wears no badge");
    assert_eq!(tree.git_decoration(&root), None);
    tree.ignored = Arc::default();
    tree.scrub_tree = Some((root.clone(), Arc::default()));
    assert_eq!(
        tree.git_decoration(&root.join("README.md")),
        None,
        "the working tree's changes are not the scrubbed commit's"
    );
}

#[test]
fn ignored_rows_render_with_the_dimmed_foreground() {
    let (_tmp, mut tree) = fixture();
    let root = tree.root.clone();
    tree.ignored = std::sync::Arc::new(std::collections::HashSet::from([root.join("README.md")]));
    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 10,
    };
    let mut buf = Buffer::empty(area);
    (&mut tree).render(area, &mut buf);
    let dim = tree.theme.ignored_fg();
    // Multi-byte icon glyphs mean a byte offset into the joined row text
    // is not a column; walk the cells to map the match back to its column.
    let row_fg = |needle: &str| {
        for y in 0..area.height {
            let cells: Vec<String> = (0..area.width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect();
            let joined = cells.concat();
            let Some(target) = joined.find(needle) else {
                continue;
            };
            let mut acc = 0usize;
            for (x, s) in cells.iter().enumerate() {
                if acc == target {
                    return buf[(x as u16, y)].fg;
                }
                acc += s.len();
            }
        }
        panic!("{needle} not on screen");
    };
    assert_eq!(
        row_fg("README.md"),
        dim,
        "a gitignored file's name wears the dimmed foreground"
    );
    assert_eq!(
        row_fg("main.rs"),
        plain_ink(),
        "non-ignored names keep the normal foreground"
    );
}

/// #371: while scrubbing, a row absent from the commit's tree dims its
/// name; present files and folders, the root row, and another root the
/// listing never covered keep the normal foreground.
#[test]
fn rows_missing_from_the_scrubbed_commit_render_dimmed() {
    let (_tmp, second, mut tree) = two_root_fixture();
    let root = tree.root.clone();
    tree.scrub_tree = Some((
        root.clone(),
        std::sync::Arc::new(crate::git::CommitTree {
            paths: std::collections::HashSet::from([
                root.join("src"),
                root.join("src/lib.rs"),
                root.join("README.md"),
            ]),
            submodules: Vec::new(),
        }),
    ));
    assert!(tree.is_absent_at_scrub(&root.join("main.rs")));
    assert!(!tree.is_absent_at_scrub(&root.join("README.md")));
    assert!(!tree.is_absent_at_scrub(&root), "the root row stays");
    assert!(!tree.is_absent_at_scrub(&second.path().join("Cargo.toml")));
    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 14,
    };
    let mut buf = Buffer::empty(area);
    (&mut tree).render(area, &mut buf);
    let dim = tree.theme.ignored_fg();
    let row_fg = |needle: &str| {
        for y in 0..area.height {
            let cells: Vec<String> = (0..area.width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect();
            let joined = cells.concat();
            let Some(target) = joined.find(needle) else {
                continue;
            };
            let mut acc = 0usize;
            for (x, s) in cells.iter().enumerate() {
                if acc == target {
                    return buf[(x as u16, y)].fg;
                }
                acc += s.len();
            }
        }
        panic!("{needle} not on screen");
    };
    assert_eq!(row_fg("main.rs"), dim, "absent at the commit: dimmed");
    assert_eq!(row_fg("README.md"), plain_ink(), "present: normal");
    assert_eq!(row_fg("src"), plain_ink(), "a present folder: normal");
    assert_eq!(
        row_fg("Cargo.toml"),
        plain_ink(),
        "another root is outside the listing"
    );
    // The working tree: nothing dims.
    tree.scrub_tree = None;
    assert!(!tree.is_absent_at_scrub(&root.join("main.rs")));
}

#[test]
fn directories_sort_before_files() {
    let (_tmp, tree) = fixture();
    // Skip root (idx 0). Next should be the directory.
    assert!(tree.nodes[1].is_dir);
    assert!(!tree.nodes[2].is_dir);
    assert!(!tree.nodes[3].is_dir);
}

fn sorted(names: &[&str]) -> Vec<String> {
    let mut v: Vec<String> = names.iter().map(|s| s.to_string()).collect();
    v.sort_by(|a, b| natural_cmp(a, b));
    v
}

/// #1365: numbers by value, text without case, as VS Code lists them.
#[test]
fn names_sort_naturally() {
    assert_eq!(
        sorted(&["chapter10", "chapter2", "chapter1"]),
        ["chapter1", "chapter2", "chapter10"]
    );
    assert_eq!(
        sorted(&["step10.sql", "step2.sql", "step11.sql", "step1.sql"]),
        ["step1.sql", "step2.sql", "step10.sql", "step11.sql"]
    );
    assert_eq!(
        sorted(&["Zeta.py", "apple.py", "README.md", "Makefile", "banana.py"]),
        ["apple.py", "banana.py", "Makefile", "README.md", "Zeta.py"]
    );
    assert_eq!(
        sorted(&["img12.png", "img1.png", "img3.png"])[2],
        "img12.png"
    );
}

#[test]
fn natural_order_is_total_for_ties_zeros_and_huge_numbers() {
    use std::cmp::Ordering;
    // Equal ignoring case: still a fixed order, the bytes'.
    assert_eq!(natural_cmp("A.txt", "a.txt"), Ordering::Less);
    assert_eq!(natural_cmp("a.txt", "A.txt"), Ordering::Greater);
    // Numerically equal: the shorter first, as VS Code does.
    assert_eq!(natural_cmp("v1", "v01"), Ordering::Less);
    assert_eq!(natural_cmp("v01", "v1"), Ordering::Greater);
    assert_eq!(natural_cmp("v01", "v001"), Ordering::Less);
    assert_eq!(natural_cmp("v1", "v002"), Ordering::Less);
    let huge = "x123456789012345678901234567890";
    let huger = "x1234567890123456789012345678901";
    assert_eq!(natural_cmp(huge, huger), Ordering::Less);
    assert_eq!(natural_cmp(huge, huge), Ordering::Equal);
    assert_eq!(natural_cmp("", "a"), Ordering::Less);
    assert_eq!(natural_cmp("a1", "a"), Ordering::Greater);
    assert_eq!(natural_cmp("_init.py", "apple.py"), Ordering::Less);
}

#[cfg(unix)]
#[test]
fn non_utf8_names_with_the_same_lossy_rendering_keep_a_fixed_order() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let fe = Path::new("/d").join(OsStr::from_bytes(&[0xFE]));
    let ff = Path::new("/d").join(OsStr::from_bytes(&[0xFF]));
    assert_eq!(explorer_name_cmp(&fe, &ff), std::cmp::Ordering::Less);
    assert_eq!(explorer_name_cmp(&ff, &fe), std::cmp::Ordering::Greater);
}

#[test]
fn the_explorer_lists_numbered_and_capitalised_names_in_order() {
    let tmp = tempfile::tempdir().unwrap();
    for d in ["chapter1", "chapter10", "chapter2", "assets"] {
        std::fs::create_dir(tmp.path().join(d)).unwrap();
    }
    for f in [
        "README.md",
        "Makefile",
        "apple.py",
        "step10.sql",
        "step2.sql",
    ] {
        std::fs::write(tmp.path().join(f), "").unwrap();
    }
    let tree = FileTree::new(tmp.path().to_path_buf());
    let names: Vec<String> = tree.nodes[1..]
        .iter()
        .map(|n| n.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        [
            "assets",
            "chapter1",
            "chapter2",
            "chapter10",
            "apple.py",
            "Makefile",
            "README.md",
            "step2.sql",
            "step10.sql"
        ],
        "folders first, each group in natural order"
    );
}

#[test]
fn move_up_clamps_at_zero() {
    let (_tmp, mut tree) = fixture();
    tree.selected = 0;
    tree.move_up();
    assert_eq!(tree.selected, 0);
}

#[test]
fn move_down_clamps_at_last() {
    let (_tmp, mut tree) = fixture();
    let last = tree.nodes.len() - 1;
    tree.selected = last;
    tree.move_down();
    assert_eq!(tree.selected, last);
}

#[test]
fn move_down_then_up() {
    let (_tmp, mut tree) = fixture();
    tree.move_down();
    assert_eq!(tree.selected, 1);
    tree.move_down();
    assert_eq!(tree.selected, 2);
    tree.move_up();
    assert_eq!(tree.selected, 1);
}

#[test]
fn end_jumps_to_last() {
    let (_tmp, mut tree) = fixture();
    tree.end();
    assert_eq!(tree.selected, tree.nodes.len() - 1);
}

#[test]
fn home_jumps_to_first() {
    let (_tmp, mut tree) = fixture();
    tree.selected = 3;
    tree.home();
    assert_eq!(tree.selected, 0);
}

#[test]
fn activate_file_returns_path() {
    let (_tmp, mut tree) = fixture();
    // Find a file node.
    let file_idx = tree.nodes.iter().position(|n| !n.is_dir).unwrap();
    tree.selected = file_idx;
    let path = tree.activate();
    assert!(path.is_some());
    assert!(path.unwrap().is_file());
}

#[test]
fn activate_directory_expands_and_collapses() {
    let (_tmp, mut tree) = fixture();
    // src/ at index 1 (after directories-first sort).
    tree.selected = 1;
    let total_before = tree.nodes.len();
    // Expand src/.
    let opened = tree.activate();
    assert!(opened.is_none()); // no file opened
    assert!(tree.nodes[1].expanded);
    assert!(tree.nodes.len() > total_before);
    // Collapse it again.
    let collapsed = tree.activate();
    assert!(collapsed.is_none());
    assert!(!tree.nodes[1].expanded);
    assert_eq!(tree.nodes.len(), total_before);
}

#[test]
fn expand_selected_does_not_collapse_open_directory() {
    let (_tmp, mut tree) = fixture();
    tree.selected = 1;
    tree.expand_selected();
    let expanded_len = tree.nodes.len();

    tree.expand_selected();

    assert!(tree.nodes[1].expanded);
    assert_eq!(tree.nodes.len(), expanded_len);
}

#[test]
fn collapse_selected_does_not_expand_closed_directory() {
    let (_tmp, mut tree) = fixture();
    tree.selected = 1;

    tree.collapse_selected();

    assert!(!tree.nodes[1].expanded);
    assert_eq!(tree.nodes.len(), 4);
}

#[test]
fn select_clamps_to_valid_index() {
    let (_tmp, mut tree) = fixture();
    let last = tree.nodes.len() - 1;
    tree.select(last);
    assert_eq!(tree.selected, last);
    // Out of range: select() should be a no-op.
    let prev = tree.selected;
    tree.select(9999);
    assert_eq!(tree.selected, prev);
}

#[test]
fn collapse_resets_selection_if_above_new_len() {
    let (_tmp, mut tree) = fixture();
    // Expand src/, point selection at a child, then collapse.
    tree.selected = 1;
    tree.activate();
    // Move selection inside the expanded subtree.
    let inside = tree.nodes.len() - 1;
    tree.selected = inside;
    // Collapse src/.
    tree.selected = 1;
    tree.activate();
    assert!(tree.selected < tree.nodes.len());
}

#[test]
fn validate_new_name_accepts_normal() {
    assert!(validate_new_name("hello.rs").is_ok());
    assert!(validate_new_name("My Folder").is_ok());
    assert!(validate_new_name("a").is_ok());
}

#[test]
fn validate_new_name_rejects_empty_or_whitespace() {
    assert!(validate_new_name("").is_err());
    assert!(validate_new_name("   ").is_err());
}

#[test]
fn validate_new_name_rejects_dots() {
    assert!(validate_new_name(".").is_err());
    assert!(validate_new_name("..").is_err());
}

#[test]
fn validate_new_name_rejects_path_separators() {
    assert!(validate_new_name("a/b").is_err());
    assert!(validate_new_name("a\\b").is_err());
    assert!(validate_new_name("../escape").is_err());
}

#[test]
fn validate_new_name_rejects_nul() {
    assert!(validate_new_name("evil\0name").is_err());
}

#[test]
fn create_target_dir_for_directory_returns_self() {
    let dir = Node {
        path: PathBuf::from("/r/a"),
        depth: 1,
        is_dir: true,
        expanded: false,
        loaded: false,
        folded: 0,
    };
    assert_eq!(
        create_target_dir_for(Some(&dir), Path::new("/r")),
        PathBuf::from("/r/a")
    );
}

#[test]
fn create_target_dir_for_file_returns_parent() {
    let file = Node {
        path: PathBuf::from("/r/a/b.txt"),
        depth: 2,
        is_dir: false,
        expanded: false,
        loaded: false,
        folded: 0,
    };
    assert_eq!(
        create_target_dir_for(Some(&file), Path::new("/r")),
        PathBuf::from("/r/a")
    );
}

#[test]
fn create_target_dir_for_none_returns_root() {
    assert_eq!(
        create_target_dir_for(None, Path::new("/r")),
        PathBuf::from("/r")
    );
}

#[test]
fn create_file_in_creates_empty_file() {
    let tmp = TempDir::new().unwrap();
    let path = create_file_in(tmp.path(), "new.txt").unwrap();
    assert!(path.is_file());
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
}

#[test]
fn create_file_in_errors_when_already_exists() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("x.txt"), "stuff").unwrap();
    let err = create_file_in(tmp.path(), "x.txt").unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
}

#[test]
fn create_folder_in_creates_directory() {
    let tmp = TempDir::new().unwrap();
    let path = create_folder_in(tmp.path(), "newdir").unwrap();
    assert!(path.is_dir());
}

#[test]
fn create_folder_in_errors_when_already_exists() {
    let tmp = TempDir::new().unwrap();
    std::fs::create_dir(tmp.path().join("d")).unwrap();
    let err = create_folder_in(tmp.path(), "d").unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
}

#[test]
fn delete_target_for_root_returns_none() {
    let tmp = TempDir::new().unwrap();
    let root_node = Node {
        path: tmp.path().to_path_buf(),
        depth: 0,
        is_dir: true,
        expanded: true,
        loaded: true,
        folded: 0,
    };
    assert!(delete_target_for(Some(&root_node), tmp.path()).is_none());
}

#[test]
fn delete_target_for_empty_space_returns_none() {
    let tmp = TempDir::new().unwrap();
    assert!(delete_target_for(None, tmp.path()).is_none());
}

#[test]
fn delete_target_for_file_returns_path() {
    let tmp = TempDir::new().unwrap();
    let f = tmp.path().join("doomed.txt");
    std::fs::write(&f, "").unwrap();
    let node = Node {
        path: f.clone(),
        depth: 1,
        is_dir: false,
        expanded: false,
        loaded: false,
        folded: 0,
    };
    assert_eq!(delete_target_for(Some(&node), tmp.path()), Some(f));
}

#[test]
fn delete_target_for_subfolder_returns_path() {
    let tmp = TempDir::new().unwrap();
    let sub = tmp.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    let node = Node {
        path: sub.clone(),
        depth: 1,
        is_dir: true,
        expanded: false,
        loaded: false,
        folded: 0,
    };
    assert_eq!(delete_target_for(Some(&node), tmp.path()), Some(sub));
}

#[test]
fn move_to_trash_removes_file_from_workspace() {
    let tmp = TempDir::new().unwrap();
    let f = tmp.path().join("bye.txt");
    std::fs::write(&f, "see ya").unwrap();
    assert!(f.exists());
    move_to_trash(&f).unwrap();
    assert!(
        !f.exists(),
        "file should be gone from the workspace after trash"
    );
}

#[test]
fn move_to_trash_removes_folder_from_workspace() {
    let tmp = TempDir::new().unwrap();
    let d = tmp.path().join("byedir");
    std::fs::create_dir(&d).unwrap();
    std::fs::write(d.join("inner.txt"), "x").unwrap();
    assert!(d.exists());
    move_to_trash(&d).unwrap();
    assert!(
        !d.exists(),
        "folder should be gone from the workspace after trash"
    );
}

#[test]
fn affected_dir_for_event_returns_parent() {
    let tmp = TempDir::new().unwrap();
    let sub = tmp.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    let f = sub.join("a.txt");
    std::fs::write(&f, "").unwrap();
    let canon = tmp.path().canonicalize().unwrap();
    let res = affected_dir_for_event(&f, tmp.path(), &canon).unwrap();
    assert_eq!(res, sub.canonicalize().unwrap());
}

#[test]
fn affected_dir_for_event_returns_none_for_root_itself() {
    let tmp = TempDir::new().unwrap();
    let canon = tmp.path().canonicalize().unwrap();
    assert!(affected_dir_for_event(tmp.path(), tmp.path(), &canon).is_none());
}

#[test]
fn affected_dir_for_event_returns_none_when_outside_root() {
    let tmp = TempDir::new().unwrap();
    let other = TempDir::new().unwrap();
    let f = other.path().join("x.txt");
    std::fs::write(&f, "").unwrap();
    let canon = tmp.path().canonicalize().unwrap();
    assert!(affected_dir_for_event(&f, tmp.path(), &canon).is_none());
}

#[test]
fn affected_dir_for_top_level_file_returns_root() {
    let tmp = TempDir::new().unwrap();
    let f = tmp.path().join("top.txt");
    std::fs::write(&f, "").unwrap();
    let canon = tmp.path().canonicalize().unwrap();
    let res = affected_dir_for_event(&f, tmp.path(), &canon).unwrap();
    assert_eq!(res, tmp.path().canonicalize().unwrap());
}

#[test]
fn affected_dir_for_event_resolves_correctly_for_external_creation() {
    // Simulating: an external process creates `<root>/sub/x.txt`. The
    // watcher fires for that path; the helper should report `<root>/sub`
    // as the directory to refresh.
    let tmp = TempDir::new().unwrap();
    let sub = tmp.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    let f = sub.join("x.txt");
    std::fs::write(&f, "external write").unwrap();
    let canon = tmp.path().canonicalize().unwrap();
    let dir = affected_dir_for_event(&f, tmp.path(), &canon).expect("inside root");
    // Reload the tree's children of `sub` and verify x.txt is found.
    let mut tree = FileTree::new(tmp.path().to_path_buf());
    // Expand `sub` so its children are loaded.
    let sub_idx = tree
        .nodes
        .iter()
        .position(|n| n.is_dir && n.path.ends_with("sub"))
        .unwrap();
    tree.selected = sub_idx;
    tree.activate();
    // Now create another file externally and confirm refresh_children
    // (driven by the watcher path the helper produces) picks it up.
    let f2 = sub.join("y.txt");
    std::fs::write(&f2, "").unwrap();
    let canon_dir = dir.clone();
    let target_idx = tree.index_of_dir(&canon_dir).unwrap();
    tree.refresh_children(target_idx);
    assert!(
        tree.nodes.iter().any(|n| n.path.ends_with("y.txt")),
        "y.txt should appear after refresh_children driven by the watcher event"
    );
}

#[test]
fn refresh_children_preserves_collapsed_state_on_external_change() {
    // FS events fire asynchronously inside any expanded ancestor. If the
    // user just collapsed a folder (e.g. .git), an event arriving inside
    // it must NOT pop it back open — refresh_children must preserve the
    // user's collapse choice.
    let tmp = TempDir::new().unwrap();
    let sub = tmp.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(sub.join("a.txt"), "").unwrap();
    let mut tree = FileTree::new(tmp.path().to_path_buf());
    // Expand `sub`, then collapse it again to simulate the user's choice.
    let sub_idx = tree
        .nodes
        .iter()
        .position(|n| n.is_dir && n.path.ends_with("sub"))
        .unwrap();
    tree.selected = sub_idx;
    tree.activate();
    let sub_idx = tree
        .nodes
        .iter()
        .position(|n| n.is_dir && n.path.ends_with("sub"))
        .unwrap();
    tree.selected = sub_idx;
    tree.activate();
    assert!(
        !tree.nodes[sub_idx].expanded,
        "precondition: sub is collapsed"
    );
    // External write fires a watcher event for `sub`.
    std::fs::write(sub.join("b.txt"), "").unwrap();
    tree.refresh_children(sub_idx);
    assert!(
        !tree.nodes[sub_idx].expanded,
        "refresh_children must not re-expand a folder the user collapsed"
    );
}

#[test]
fn refresh_children_preserves_expanded_descendants_when_parent_refreshes() {
    let (_tmp, mut tree) = fixture();
    tree.selected = 1;
    tree.activate();
    let src_path = tree.nodes[1].path.clone();
    assert!(tree.nodes[1].expanded);
    assert!(
        tree.nodes.iter().any(|n| n.path.ends_with("src/lib.rs")),
        "precondition: expanded child should be visible"
    );

    tree.refresh_children(0);

    let src_idx = tree
        .nodes
        .iter()
        .position(|n| n.path == src_path)
        .expect("src should still be present");
    assert!(
        tree.nodes[src_idx].expanded,
        "expanded child directory should survive a root refresh"
    );
    assert!(
        tree.nodes.iter().any(|n| n.path.ends_with("src/lib.rs")),
        "expanded descendant children should be reloaded after parent refresh"
    );
}

#[test]
fn refresh_children_picks_up_newly_created_file() {
    let (tmp, mut tree) = fixture();
    let root_dir_idx = tree.index_of_dir(tmp.path()).unwrap();
    std::fs::write(tmp.path().join("brand-new.txt"), "").unwrap();
    let before = tree.nodes.len();
    tree.refresh_children(root_dir_idx);
    let after = tree.nodes.len();
    assert!(after > before, "expected new node to appear after refresh");
    assert!(
        tree.nodes.iter().any(|n| n.path.ends_with("brand-new.txt")),
        "the newly created file should be in the tree"
    );
}

/// The Maven layout of #1486: `src/main/java/com/acme/App.java` beside a
/// README, so the root itself lists two entries.
fn maven_fixture() -> (TempDir, FileTree) {
    let tmp = TempDir::new().unwrap();
    let acme = tmp.path().join("src/main/java/com/acme");
    fs::create_dir_all(&acme).unwrap();
    fs::write(acme.join("App.java"), "class App {}\n").unwrap();
    fs::write(tmp.path().join("README.md"), "# hi\n").unwrap();
    let tree = FileTree::new(tmp.path().to_path_buf());
    (tmp, tree)
}

/// Each row as `depth label`, the root's left out.
fn rows(tree: &FileTree) -> String {
    let label = |n: &Node| format!("{} {}", n.depth, n.label());
    tree.nodes[1..]
        .iter()
        .map(label)
        .collect::<Vec<_>>()
        .join(", ")
}

fn expand(tree: &mut FileTree, path: &Path) {
    tree.select(tree.row_of(path).unwrap());
    tree.expand_selected();
}

const FOLDED: &str = "1 src/main/java/com/acme, 2 App.java, 1 README.md";

#[test]
fn expanding_a_chain_of_only_child_folders_opens_one_row() {
    let (tmp, mut tree) = maven_fixture();
    assert_eq!(rows(&tree), "1 src, 1 README.md", "unlisted, src is src");
    expand(&mut tree, &tmp.path().join("src"));
    assert_eq!(rows(&tree), FOLDED, "one expansion, App.java one level in");
    let row = &tree.nodes[1];
    assert_eq!(row.path, tmp.path().join("src/main/java/com/acme"));
    assert!(row.expanded && row.folded == 4);
    let area = Rect::new(0, 0, 40, 6);
    let mut buf = Buffer::empty(area);
    (&mut tree).render(area, &mut buf);
    let painted: String = (0..40).map(|x| buf[(x, 2)].symbol()).collect();
    assert!(painted.contains("src/main/java/com/acme"), "{painted:?}");
}

#[test]
fn a_folder_with_two_entries_or_one_file_does_not_fold() {
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join("src/main")).unwrap();
    fs::create_dir_all(tmp.path().join("src/test")).unwrap();
    fs::create_dir_all(tmp.path().join("docs")).unwrap();
    fs::write(tmp.path().join("docs/a.md"), "").unwrap();
    let mut tree = FileTree::new(tmp.path().to_path_buf());
    expand(&mut tree, &tmp.path().join("src"));
    expand(&mut tree, &tmp.path().join("docs"));
    assert_eq!(rows(&tree), "1 docs, 2 a.md, 1 src, 2 main, 2 test");
}

/// A folder whose only entry is a symlink back to itself does not fold
/// into a loop: a symlink is listed as itself, never as a folder, so
/// the chain stops there.
#[cfg(unix)]
#[test]
fn a_symlink_back_to_its_folder_does_not_fold() {
    let tmp = TempDir::new().unwrap();
    let a = tmp.path().join("a");
    fs::create_dir(&a).unwrap();
    std::os::unix::fs::symlink(&a, a.join("again")).unwrap();
    fs::write(tmp.path().join("README.md"), "").unwrap();
    let mut tree = FileTree::new(tmp.path().to_path_buf());
    expand(&mut tree, &a);
    assert_eq!(rows(&tree), "1 a, 2 again, 1 README.md");
}

#[test]
fn compact_folders_off_lists_every_level_and_toggles_open_rows() {
    let (tmp, mut tree) = maven_fixture();
    tree.set_compact_folders(false);
    expand(&mut tree, &tmp.path().join("src"));
    assert_eq!(rows(&tree), "1 src, 2 main, 1 README.md");
    tree.set_compact_folders(true);
    assert_eq!(rows(&tree), FOLDED, "turned on, the open src folds");
    tree.set_compact_folders(false);
    assert_eq!(
        rows(&tree),
        "1 src, 2 main, 3 java, 4 com, 5 acme, 6 App.java, 1 README.md",
        "every folder the row showed stays open"
    );
}

#[test]
fn an_entry_added_inside_a_chain_splits_it_on_refresh() {
    let (tmp, mut tree) = maven_fixture();
    let main = tmp.path().join("src/main");
    expand(&mut tree, &tmp.path().join("src"));
    let app = main.join("java/com/acme/App.java");
    tree.select(tree.row_of(&app).unwrap());
    fs::write(main.join("Other.java"), "").unwrap();
    // The watcher reports `main`, which only the compact row shows.
    tree.refresh_children(tree.index_of_dir(&main).unwrap());
    assert_eq!(
        rows(&tree),
        "1 src/main, 2 java/com/acme, 3 App.java, 2 Other.java, 1 README.md"
    );
    assert_eq!(tree.selected_path(), Some(app.as_path()));
    fs::remove_file(main.join("Other.java")).unwrap();
    tree.refresh_children(tree.index_of_dir(&main).unwrap());
    assert_eq!(rows(&tree), FOLDED, "the chain folds back");
}

#[test]
fn a_collapsed_chain_survives_a_parent_refresh_and_reveal_opens_it() {
    let (tmp, mut tree) = maven_fixture();
    expand(&mut tree, &tmp.path().join("src"));
    tree.collapse_selected();
    tree.refresh_children(0);
    assert_eq!(rows(&tree), "1 src/main/java/com/acme, 1 README.md");
    let app = tmp.path().join("src/main/java/com/acme/App.java");
    assert!(tree.reveal_path(&app));
    assert_eq!(tree.selected_path(), Some(app.as_path()));
    assert_eq!(tree.nodes[tree.selected].depth, 2);
}

#[test]
fn page_up_and_page_down() {
    let (_tmp, mut tree) = fixture();
    let last = tree.nodes.len().saturating_sub(1);
    tree.page_down(100);
    assert_eq!(tree.selected, last);
    tree.page_up(100);
    assert_eq!(tree.selected, 0);
}

#[test]
fn rename_in_renames_a_file_within_its_parent() {
    let tmp = TempDir::new().unwrap();
    let old = tmp.path().join("old.txt");
    std::fs::write(&old, "hello").unwrap();
    let new_path = rename_in(tmp.path(), &old, "new.txt").unwrap();
    assert_eq!(new_path, tmp.path().join("new.txt"));
    assert!(!old.exists(), "old name must be gone");
    assert!(new_path.exists(), "new name must exist");
    assert_eq!(std::fs::read_to_string(&new_path).unwrap(), "hello");
}

#[test]
fn rename_in_renames_a_folder_within_its_parent() {
    let tmp = TempDir::new().unwrap();
    let old = tmp.path().join("olddir");
    std::fs::create_dir(&old).unwrap();
    std::fs::write(old.join("inner.txt"), "x").unwrap();
    let new_path = rename_in(tmp.path(), &old, "newdir").unwrap();
    assert_eq!(new_path, tmp.path().join("newdir"));
    assert!(new_path.is_dir());
    assert_eq!(
        std::fs::read_to_string(new_path.join("inner.txt")).unwrap(),
        "x"
    );
}

#[test]
fn rename_in_errors_when_target_already_exists() {
    let tmp = TempDir::new().unwrap();
    let a = tmp.path().join("a.txt");
    let b = tmp.path().join("b.txt");
    std::fs::write(&a, "a").unwrap();
    std::fs::write(&b, "b").unwrap();
    let err = rename_in(tmp.path(), &a, "b.txt").unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    // Both originals must still exist intact.
    assert!(a.exists());
    assert!(b.exists());
}

#[test]
fn a_hard_link_under_the_new_name_is_still_a_clash() {
    // A second directory entry for the same file is a separate name:
    // renaming onto it would succeed without moving anything.
    let tmp = TempDir::new().unwrap();
    let a = tmp.path().join("readme.md");
    std::fs::write(&a, "x").unwrap();
    std::fs::hard_link(&a, tmp.path().join("README.md")).unwrap();
    for err in [
        rename_target(tmp.path(), &a, "README.md").unwrap_err(),
        rename_in(tmp.path(), &a, "README.md").unwrap_err(),
    ] {
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    }
    assert!(a.exists());
    // On a case-sensitive volume a name nothing answers to is no
    // respelling of anything.
    assert!(!is_case_only_respelling(tmp.path(), &a, "Readme.md"));
}

#[test]
fn rename_in_rejects_invalid_names() {
    let tmp = TempDir::new().unwrap();
    let a = tmp.path().join("a.txt");
    std::fs::write(&a, "a").unwrap();
    // Slashes and reserved names must be rejected before the syscall.
    assert!(rename_in(tmp.path(), &a, "sub/x.txt").is_err());
    assert!(rename_in(tmp.path(), &a, "").is_err());
    assert!(rename_in(tmp.path(), &a, ".").is_err());
    assert!(rename_in(tmp.path(), &a, "..").is_err());
    // Original is untouched.
    assert!(a.exists());
}

#[test]
fn select_replace_clears_marks_and_sets_anchor() {
    let (_tmp, mut tree) = fixture();
    tree.marked.insert(tree.nodes[1].path.clone());
    tree.select_replace(2);
    assert_eq!(tree.selected, 2);
    assert_eq!(tree.anchor, 2);
    assert!(tree.marked.is_empty());
}

#[test]
fn extend_to_marks_inclusive_range() {
    let (_tmp, mut tree) = fixture();
    tree.select_replace(1);
    tree.extend_to(3);
    assert_eq!(tree.selected, 3);
    assert!(tree.is_marked(1));
    assert!(tree.is_marked(2));
    assert!(tree.is_marked(3));
}

#[test]
fn extend_to_works_in_either_direction() {
    let (_tmp, mut tree) = fixture();
    tree.select_replace(3);
    tree.extend_to(1);
    assert_eq!(tree.selected, 1);
    assert!(tree.is_marked(1));
    assert!(tree.is_marked(2));
    assert!(tree.is_marked(3));
}

#[test]
fn extend_to_replaces_previous_range() {
    let (_tmp, mut tree) = fixture();
    tree.select_replace(0);
    tree.extend_to(2);
    tree.extend_to(1);
    assert!(tree.is_marked(0));
    assert!(tree.is_marked(1));
    assert!(!tree.is_marked(2));
}

#[test]
fn toggle_mark_adds_then_removes() {
    let (_tmp, mut tree) = fixture();
    tree.toggle_mark(1);
    assert!(tree.is_marked(1));
    tree.toggle_mark(1);
    assert!(!tree.is_marked(1));
}

#[test]
fn action_paths_returns_marks_in_tree_order() {
    let (_tmp, mut tree) = fixture();
    tree.select_replace(1);
    tree.toggle_mark(2);
    let paths = tree.action_paths();
    assert_eq!(paths.len(), 2);
    assert_eq!(paths[0], tree.nodes[1].path);
    assert_eq!(paths[1], tree.nodes[2].path);
}

#[test]
fn action_paths_falls_back_to_selected_when_no_marks() {
    let (_tmp, mut tree) = fixture();
    tree.selected = 2;
    let paths = tree.action_paths();
    assert_eq!(paths, vec![tree.nodes[2].path.clone()]);
}

#[test]
fn move_up_and_down_clear_marks() {
    let (_tmp, mut tree) = fixture();
    tree.select_replace(1);
    tree.extend_to(2);
    assert!(!tree.marked.is_empty());
    tree.move_down();
    assert!(tree.marked.is_empty());
    assert_eq!(tree.anchor, tree.selected);
}

#[test]
fn move_down_extend_grows_selection_from_anchor() {
    let (_tmp, mut tree) = fixture();
    tree.select_replace(1);
    tree.move_down_extend();
    assert!(tree.is_marked(1));
    assert!(tree.is_marked(2));
}

#[test]
fn unique_destination_in_avoids_collision() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("a.txt");
    std::fs::write(&src, "x").unwrap();
    let dst = tmp.path().join("dest");
    std::fs::create_dir(&dst).unwrap();
    let p = unique_destination_in(&dst, &src);
    assert_eq!(p, dst.join("a.txt"));
    std::fs::write(&p, "x").unwrap();
    let p2 = unique_destination_in(&dst, &src);
    assert_eq!(p2, dst.join("a copy.txt"));
}

#[test]
fn move_into_renames_when_no_collision() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("a.txt");
    std::fs::write(&src, "x").unwrap();
    let dst_dir = tmp.path().join("dest");
    std::fs::create_dir(&dst_dir).unwrap();
    let placed = move_into(&dst_dir, &src).unwrap();
    assert_eq!(placed, dst_dir.join("a.txt"));
    assert!(!src.exists());
    assert!(placed.exists());
}

#[test]
fn move_into_dedupes_collision() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("a.txt");
    std::fs::write(&src, "src").unwrap();
    let dst_dir = tmp.path().join("dest");
    std::fs::create_dir(&dst_dir).unwrap();
    std::fs::write(dst_dir.join("a.txt"), "preexisting").unwrap();
    let placed = move_into(&dst_dir, &src).unwrap();
    assert_eq!(placed.file_name().unwrap(), "a copy.txt");
    assert_eq!(std::fs::read_to_string(&placed).unwrap(), "src");
    assert_eq!(
        std::fs::read_to_string(dst_dir.join("a.txt")).unwrap(),
        "preexisting"
    );
}

/// #1174: moving an item into the folder it is already in moves
/// nothing, where it used to rename it to "<name> copy".
#[test]
fn move_into_its_own_folder_leaves_it_where_it_is() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("a.txt");
    std::fs::write(&src, "x").unwrap();
    let dir = tmp.path().join("src");
    std::fs::create_dir(&dir).unwrap();
    assert_eq!(move_into(tmp.path(), &src).unwrap(), src);
    assert_eq!(move_into(tmp.path(), &dir).unwrap(), dir);
    let mut names: Vec<_> = std::fs::read_dir(tmp.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    names.sort();
    assert_eq!(names, ["a.txt", "src"]);
    assert_eq!(std::fs::read_to_string(&src).unwrap(), "x");
}

/// #1174 negative: a cut file deleted before the paste is not "already
/// in this folder"; the move fails as missing.
#[test]
fn move_into_its_own_folder_of_a_missing_file_fails() {
    let tmp = TempDir::new().unwrap();
    let gone = tmp.path().join("gone.txt");
    assert!(!is_directly_in(tmp.path(), &gone));
    assert!(move_into(tmp.path(), &gone).is_err());
    #[cfg(unix)]
    {
        let link = tmp.path().join("dangling");
        std::os::unix::fs::symlink(tmp.path().join("nowhere"), &link).unwrap();
        assert!(is_directly_in(tmp.path(), &link), "the link itself exists");
    }
}

#[test]
fn move_into_refuses_to_move_directory_into_itself() {
    let tmp = TempDir::new().unwrap();
    let d = tmp.path().join("d");
    std::fs::create_dir(&d).unwrap();
    let inner = d.join("inner");
    std::fs::create_dir(&inner).unwrap();
    assert!(move_into(&inner, &d).is_err());
}

#[test]
fn copy_into_recurses_into_subfolders() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("d");
    std::fs::create_dir(&src).unwrap();
    std::fs::write(src.join("a.txt"), "hi").unwrap();
    let inner = src.join("inner");
    std::fs::create_dir(&inner).unwrap();
    std::fs::write(inner.join("b.txt"), "deep").unwrap();
    let dst_dir = tmp.path().join("dest");
    std::fs::create_dir(&dst_dir).unwrap();
    let placed = copy_into(&dst_dir, &src).unwrap();
    assert_eq!(placed, dst_dir.join("d"));
    assert_eq!(std::fs::read_to_string(placed.join("a.txt")).unwrap(), "hi");
    assert_eq!(
        std::fs::read_to_string(placed.join("inner/b.txt")).unwrap(),
        "deep"
    );
    assert!(src.exists(), "copy must leave source intact");
}

#[test]
fn find_prefix_matches_visible_nodes_from_start() {
    let (_tmp, tree) = fixture();
    let idx_src = tree
        .nodes
        .iter()
        .position(|n| n.path.file_name().is_some_and(|s| s == "src"))
        .unwrap();
    let idx_readme = tree
        .nodes
        .iter()
        .position(|n| n.path.file_name().is_some_and(|s| s == "README.md"))
        .unwrap();
    let idx_main = tree
        .nodes
        .iter()
        .position(|n| n.path.file_name().is_some_and(|s| s == "main.rs"))
        .unwrap();
    assert_eq!(tree.find_prefix("s", 0), Some(idx_src));
    assert_eq!(tree.find_prefix("r", 0), Some(idx_readme));
    assert_eq!(tree.find_prefix("m", 0), Some(idx_main));
}

#[test]
fn find_prefix_is_case_insensitive() {
    let (_tmp, tree) = fixture();
    let want = tree
        .nodes
        .iter()
        .position(|n| n.path.file_name().is_some_and(|s| s == "README.md"))
        .unwrap();
    assert_eq!(tree.find_prefix("README", 0), Some(want));
    assert_eq!(tree.find_prefix("readme", 0), Some(want));
    assert_eq!(tree.find_prefix("ReAdMe", 0), Some(want));
}

#[test]
fn find_prefix_wraps_around_to_find_earlier_match() {
    let (_tmp, tree) = fixture();
    let want = tree
        .nodes
        .iter()
        .position(|n| n.path.file_name().is_some_and(|s| s == "README.md"))
        .unwrap();
    let after = tree.nodes.len() - 1;
    assert_eq!(
        tree.find_prefix("r", after),
        Some(want),
        "wrap-around must find the only 'r' node when start is past it"
    );
}

#[test]
fn find_prefix_returns_none_when_no_match() {
    let (_tmp, tree) = fixture();
    assert_eq!(tree.find_prefix("zzzz", 0), None);
}

#[test]
fn find_prefix_returns_none_for_empty_prefix() {
    let (_tmp, tree) = fixture();
    assert_eq!(tree.find_prefix("", 0), None);
}

#[test]
fn rename_in_no_op_returns_ok_with_same_path() {
    // Renaming to the same name is a useful early-return: it covers the
    // case where the user opens the prompt, doesn't change anything,
    // and hits Enter. Treat it as success rather than ErrorKind::AlreadyExists.
    let tmp = TempDir::new().unwrap();
    let a = tmp.path().join("keep.txt");
    std::fs::write(&a, "k").unwrap();
    let new_path = rename_in(tmp.path(), &a, "keep.txt").unwrap();
    assert_eq!(new_path, a);
    assert!(a.exists());
}

// --- Explorer filter (issue #27) ---------------------------------------

#[test]
fn subsequence_match_is_case_insensitive_and_fuzzy() {
    // Exact substring.
    assert!(subsequence_match("README.md", "read").is_some());
    // Case-insensitive.
    assert!(subsequence_match("README.md", "READ").is_some());
    assert!(subsequence_match("README.md", "rEaD").is_some());
    // Fuzzy: characters in order but not contiguous ("rdme" ⊂ "README.md").
    assert!(subsequence_match("README.md", "rdme").is_some());
    // Order matters: "daer" is not a subsequence of "README".
    assert!(subsequence_match("README.md", "dRr").is_none());
    // No match.
    assert!(subsequence_match("main.rs", "xyz").is_none());
}

#[test]
fn subsequence_match_reports_matched_byte_offsets() {
    // "lib" matches the first three chars of "lib.rs".
    assert_eq!(subsequence_match("lib.rs", "lib"), Some(vec![0, 1, 2]));
    // Empty query matches with no highlighted offsets.
    assert_eq!(subsequence_match("anything", ""), Some(Vec::new()));
    // Fuzzy offsets: "mn" in "main" -> m@0, n@3.
    assert_eq!(subsequence_match("main", "mn"), Some(vec![0, 3]));
}

#[test]
fn visible_indices_returns_everything_when_no_query() {
    let (_tmp, tree) = fixture();
    let all: Vec<usize> = (0..tree.nodes.len()).collect();
    // No filter open at all.
    assert_eq!(tree.visible_indices(), all);
}
fn deep_fixture() -> (TempDir, FileTree) {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    fs::create_dir_all(root.join("outer/inner")).unwrap();
    for i in 0..12 {
        fs::write(root.join(format!("outer/inner/f{i:02}.rs")), "x\n").unwrap();
    }
    let mut tree = FileTree::new(root.to_path_buf());
    // Three levels of rows, not the compact row `outer/inner` (#1486).
    tree.set_compact_folders(false);
    // Expand outer, then inner, so the deep files are real rows.
    let outer = tree
        .nodes
        .iter()
        .position(|n| n.path.ends_with("outer"))
        .unwrap();
    tree.selected = outer;
    tree.expand_selected();
    let inner = tree
        .nodes
        .iter()
        .position(|n| n.path.ends_with("inner"))
        .unwrap();
    tree.selected = inner;
    tree.expand_selected();
    (tmp, tree)
}

#[test]
fn sticky_ancestors_walk_outermost_first_and_cap_keeps_the_deepest() {
    let (_tmp, tree) = deep_fixture();
    let deep = tree
        .nodes
        .iter()
        .position(|n| n.path.ends_with("f08.rs"))
        .unwrap();
    let chain = sticky_ancestors(&tree.nodes, deep, 3);
    assert_eq!(chain.len(), 3, "root, outer, inner");
    assert!(tree.nodes[chain[0]].depth < tree.nodes[chain[1]].depth);
    assert!(tree.nodes[chain[2]].path.ends_with("inner"));
    let capped = sticky_ancestors(&tree.nodes, deep, 2);
    assert_eq!(capped.len(), 2);
    assert!(
        tree.nodes[capped[1]].path.ends_with("inner")
            && tree.nodes[capped[0]].path.ends_with("outer"),
        "the cap drops the OUTERMOST level, keeping the closest context"
    );
    assert!(
        sticky_ancestors(&tree.nodes, 0, 3).is_empty(),
        "the root row has no ancestors"
    );
}

#[test]
fn sticky_band_pins_ancestors_and_maps_clicks_but_never_covers_the_selection() {
    let (_tmp, mut tree) = deep_fixture();
    let deep = tree
        .nodes
        .iter()
        .position(|n| n.path.ends_with("f09.rs"))
        .unwrap();
    tree.selected = deep;
    tree.scroll = deep.saturating_sub(3);
    let area = Rect {
        x: 0,
        y: 0,
        width: 34,
        height: 8,
    };
    let mut buf = Buffer::empty(area);
    (&mut tree).render(area, &mut buf);
    assert!(
        !tree.sticky_rows.is_empty(),
        "scrolled deep inside inner/, the band must pin ancestors"
    );
    let (y, idx) = tree.sticky_rows[0];
    assert!(tree.nodes[idx].is_dir, "only directories pin");
    assert_eq!(tree.sticky_row_at(y), Some(idx));
    let row: String = (area.x..area.x + area.width)
        .map(|x| buf[(x, y)].symbol().to_string())
        .collect();
    let name = tree.nodes[idx]
        .path
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert!(
        row.contains(&name),
        "the pinned row shows the directory name; row {row:?}"
    );

    // Selection on the top visible row: the band must yield entirely.
    tree.selected = tree.scroll;
    let mut buf = Buffer::empty(area);
    (&mut tree).render(area, &mut buf);
    assert!(
        tree.sticky_rows.is_empty(),
        "the band never covers the selected row"
    );
}

#[test]
fn affected_dir_for_event_fast_path_agrees_with_the_resolving_path() {
    // The zero-syscall fast path must answer exactly what resolving both
    // sides answers, for the ordinary case: a canonical root and an event
    // path spelled under it.
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let sub = root.join("a/b");
    std::fs::create_dir_all(&sub).unwrap();
    let f = sub.join("f.rs");
    std::fs::write(&f, "x").unwrap();
    assert_eq!(
        affected_dir_for_event(&f, &root, &root),
        Some(sub.canonicalize().unwrap())
    );
}

#[test]
fn affected_dir_for_event_ignores_the_root_and_outsiders() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    assert!(affected_dir_for_event(&root, &root, &root).is_none());
    let outside = TempDir::new().unwrap();
    let other = outside.path().canonicalize().unwrap().join("x.rs");
    std::fs::write(&other, "x").unwrap();
    assert!(affected_dir_for_event(&other, &root, &root).is_none());
}

#[cfg(unix)]
#[test]
fn affected_dir_for_event_resolves_a_root_reached_through_a_symlink() {
    // A non-canonical root must NOT take the fast path: the answer has to
    // be the resolved parent, which is what the caller's tree is keyed on.
    // This is the case a macOS TempDir hits for real (/var -> /private/var).
    let tmp = TempDir::new().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let real = base.join("real");
    std::fs::create_dir_all(real.join("sub")).unwrap();
    let f = real.join("sub/f.rs");
    std::fs::write(&f, "x").unwrap();
    let link = base.join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let canon_link = link.canonicalize().unwrap();
    assert_ne!(link, canon_link, "the symlink must not be its own canon");
    assert_eq!(
        affected_dir_for_event(&f, &link, &canon_link),
        Some(real.join("sub"))
    );
}

/// #2751: a wheel step and a click read in the same burst: the click meets
/// the rows the last frame drew, not the ones the wheel has moved there
/// before the next frame.
#[test]
fn a_click_after_a_wheel_step_meets_the_drawn_rows() {
    let tmp = TempDir::new().unwrap();
    for i in 0..40 {
        fs::write(tmp.path().join(format!("f{i:02}.txt")), "").unwrap();
    }
    let mut tree = FileTree::new(tmp.path().to_path_buf());
    let area = Rect {
        x: 0,
        y: 0,
        width: 30,
        height: 10,
    };
    let mut buf = Buffer::empty(area);
    (&mut tree).render(area, &mut buf);
    let y = tree.last_inner.y + 1;
    let drawn = tree.node_at_y(y).expect("a row is drawn there");
    let before = tree.scroll;
    tree.scroll_down(3);
    assert!(tree.scroll > before, "the wheel scrolled the rows");
    assert_eq!(
        tree.node_at_y(y),
        Some(drawn),
        "the row drawn there, not the one scrolled there"
    );
}
