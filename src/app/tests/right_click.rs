//! What each right-click menu holds (#2739), saved as text in
//! `golden/right-click-<spot>.txt`. Before the menus are opened from the
//! keyboard too, the code that finds the item under the pointer and builds
//! its menu is split in two; this ledger is what holds that split to
//! changing nothing a right-click shows or does.
//!
//! Each spot is right-clicked in an app drawn to show it, and the menu is
//! written down: where it opens, how it is drawn, the folder New File would
//! use, which pane then has the keys, and every row with what it runs.
//! Spots whose right-click also picks the item (an Explorer row, a capture,
//! a terminal pane) write what is picked after it. The workspace reads
//! `<ws>` and a commit id `<id>`, the two things that change from run to
//! run. `CROFT_BLESS=1` rewrites the files after a change meant to move a
//! menu.
//!
//! Linux only, like the screen goldens: on macOS the tab and Explorer
//! menus offer Reveal in Finder too, so its files would be others.
//!
//! The keyboard opens the same menus (Show Context Menu: Shift+F10, the
//! Menu key). Its tests below right-click an item, then press the key on
//! the same item, and want the same menu, opened just below the item.

use super::*;
use crossterm::event::{MouseButton, MouseEventKind};
use std::fmt::Write as _;

/// Every file the tests here save, for the check that no golden is orphaned.
pub(super) const RIGHT_CLICK_GOLDENS: [&str; 11] = [
    "right-click-minimap",
    "right-click-commit",
    "right-click-problem",
    "right-click-debug-row",
    "right-click-outline",
    "right-click-capture",
    "right-click-tab",
    "right-click-terminal",
    "right-click-explorer",
    "right-click-gutter",
    "right-click-editor",
];

const W: u16 = 120;
const H: u16 = 40;

fn right_click(app: &mut App, col: u16, row: u16) {
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Right), col, row));
}

fn pane_name(pane: Pane) -> &'static str {
    match pane {
        Pane::Tree => "Tree",
        Pane::Editor => "Editor",
        Pane::Terminal => "Terminal",
    }
}

fn rows(items: &[MenuEntry], depth: usize, out: &mut String) {
    let pad = "  ".repeat(depth);
    for entry in items {
        match entry {
            MenuEntry::Item { label, action } => {
                writeln!(out, "{pad}{label}  =>  {action:?}").unwrap();
            }
            MenuEntry::Submenu { label, items } => {
                writeln!(out, "{pad}{label} >").unwrap();
                rows(items, depth + 1, out);
            }
            MenuEntry::Separator => writeln!(out, "{pad}---").unwrap(),
            MenuEntry::Header(label) => writeln!(out, "{pad}[{label}]").unwrap(),
        }
    }
}

/// The open menu (or that none opened) and where the keys went, as the
/// ledger writes them under the heading `spot`.
fn written(app: &App, spot: &str) -> String {
    let mut out = format!("== {spot}\n");
    match app.context_menu.as_ref() {
        None => out.push_str("no menu\n"),
        Some(menu) => {
            writeln!(
                out,
                "opens at {:?}, {:?}{}, row {} lit",
                menu.origin,
                menu.look,
                if menu.keyed { ", keyed" } else { "" },
                menu.selected,
            )
            .unwrap();
            writeln!(out, "New File goes in {}", menu.target_dir.display()).unwrap();
            rows(&menu.items, 1, &mut out);
        }
    }
    writeln!(out, "keys: {}", pane_name(app.focus)).unwrap();
    out
}

/// `text` as the ledger saves it: the workspace as `<ws>` (both spellings
/// of a temp folder behind a symlink) and every commit id as `<id>`.
fn saved(text: &str, root: &Path) -> String {
    let mut text = text.to_string();
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    for r in [canonical.as_path(), root] {
        text = text.replace(&r.display().to_string(), "<ws>");
    }
    no_commit_ids(&text)
}

fn check(name: &str, text: &str, root: &Path) {
    super::golden::check_text(name, &saved(text, root));
}

/// The first row in `rows` of `area` that `hit` names, for aiming a click.
fn row_where(area: Rect, mut hit: impl FnMut(u16) -> bool) -> u16 {
    (area.y..area.y + area.height)
        .find(|&y| hit(y))
        .unwrap_or_else(|| panic!("no row in {area:?} answers"))
}

#[test]
fn the_minimap_menu() {
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
    draw(&mut app, 160, H);
    let strip = app.minimap_img_rect;
    assert!(strip.width > 0, "the minimap is drawn");
    right_click(&mut app, strip.x + 1, strip.y + 2);
    check(
        "right-click-minimap",
        &written(&app, "the minimap strip"),
        tmp.path(),
    );
}

#[test]
fn the_commit_menu() {
    let tmp = make_committed_repo();
    let p = tmp.path();
    std::fs::write(p.join("b.txt"), "b\n").unwrap();
    assert!(git_succeeds(p, &["add", "."]) && git_succeeds(p, &["commit", "-qm", "add b"]));
    let mut app = App::new(p.to_path_buf()).unwrap();
    app.sidebar_view = SidebarView::SourceControl;
    app.show_tree = true;
    app.source_control.status.in_repo = true;
    app.commit_graph
        .set_rows(crate::widgets::commit_graph::layout_graph(
            crate::git::commit_graph(p, 100),
        ));
    draw(&mut app, W, H);
    let area = app.commit_graph.last_area;
    let y = row_where(area, |y| {
        app.commit_graph
            .commit_at(y)
            .is_some_and(|c| c.summary == "add b")
    });
    let commit = app.commit_graph.commit_at(y).cloned().unwrap();
    right_click(&mut app, area.x + 4, y);
    // The commit's own ids, by value: `core.abbrev` can make the short one
    // as few as 4 digits, which `no_commit_ids` leaves alone.
    let text = written(&app, "a COMMITS row")
        .replace(&commit.hash, "<id>")
        .replace(&commit.short_hash, "<id>");
    check("right-click-commit", &text, p);
}

#[test]
fn the_problem_menu() {
    use crate::lsp::manager::DiagnosticSeverity;
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("hello.rs");
    std::fs::write(&file, "fn f0() {}\nfn f1() {}\nfn f2() {}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&file).unwrap();
    let mut by_server = std::collections::HashMap::new();
    by_server.insert(
        String::from("rustc"),
        vec![crate::lsp::manager::Diagnostic {
            start_line: 2,
            start_char: 3,
            end_line: 2,
            end_char: 5,
            severity: DiagnosticSeverity::Warning,
            message: String::from("function `f2` is never used"),
            tags: Default::default(),
            code: None,
            origin: None,
        }],
    );
    app.lsp_diagnostics.insert(file.clone(), by_server);
    app.rebuild_problems();
    app.bottom_panel_tab = BottomPanelTab::Problems;
    app.focus_pane(Pane::Editor);
    draw(&mut app, W, H);
    let list = app.problems.last_area;
    let y = row_where(list, |y| app.problems.diagnostic_at(y).is_some());
    right_click(&mut app, list.x + 30, y);
    check(
        "right-click-problem",
        &written(&app, "a PROBLEMS row"),
        tmp.path(),
    );
}

#[test]
fn the_debug_row_menus() {
    use crate::widgets::run_debug::{DebugRow, DebugRowKind};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.sidebar_view = SidebarView::RunDebug;
    app.show_tree = true;
    let row = |kind| DebugRow { indent: 0, kind };
    let variable = |name: &str, evaluate: Option<&str>| DebugRowKind::Variable {
        reference: 0,
        expandable: false,
        expanded: false,
        name: name.into(),
        value: "42".into(),
        type_name: "int".into(),
        evaluate_name: evaluate.map(String::from),
    };
    let rows_shown = [
        (
            "the CALL STACK header",
            DebugRowKind::Header {
                title: "CALL STACK".into(),
            },
        ),
        (
            "a frame",
            DebugRowKind::Frame {
                id: 11,
                selected: true,
                name: "main".into(),
                location: "s.py:6".into(),
            },
        ),
        (
            "a scope",
            DebugRowKind::Scope {
                name: "Locals".into(),
                index: 0,
                open: None,
            },
        ),
        (
            "a variable with an expression",
            variable("count", Some("self.count")),
        ),
        ("a variable without one", variable("tmp", None)),
        (
            "a watch",
            DebugRowKind::Watch {
                index: 0,
                expression: "len(items)".into(),
                value: "3".into(),
                available: true,
                changed: false,
                pending: false,
            },
        ),
    ];
    app.run_debug.debug_active = true;
    app.run_debug.debug_rows = rows_shown.iter().map(|(_, k)| row(k.clone())).collect();
    let mut text = String::new();
    for (i, (spot, _)) in rows_shown.iter().enumerate() {
        app.context_menu = None;
        draw(&mut app, W, H);
        let area = app.run_debug.last_area;
        let y = row_where(area, |y| app.run_debug.debug_row_at(y) == Some(i));
        right_click(&mut app, area.x + 3, y);
        text.push_str(&written(&app, &format!("Run and Debug: {spot}")));
    }
    check("right-click-debug-row", &text, tmp.path());
}

#[test]
fn the_outline_menu() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("lib.rs");
    std::fs::write(&file, "fn a() {\n    1\n}\nfn b() {\n    2\n}").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&file).unwrap();
    if !app.explorer_views.is_visible(ExplorerView::Outline) {
        app.toggle_explorer_view(ExplorerView::Outline);
    }
    app.outline.collapsed = false;
    let symbols = app.compute_syntax_outline(&file);
    app.outline.set_symbols(file.clone(), symbols);
    draw(&mut app, W, H);
    let area = app.outline.last_area;
    let y = row_where(area, |y| {
        app.outline
            .row_at(y)
            .is_some_and(|i| app.outline.symbols()[i].name == "b")
    });
    right_click(&mut app, area.x + 2, y);
    check(
        "right-click-outline",
        &written(&app, "an OUTLINE row"),
        tmp.path(),
    );
}

#[test]
fn the_capture_menu() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    for message in ["Build failed", "Test failed"] {
        app.captures.push(crate::widgets::captures::CapturedLine {
            pane: "zsh".into(),
            shell_pid: None,
            message: message.into(),
            line: format!("error: {message}"),
            anchor: None,
        });
    }
    app.bottom_panel_tab = BottomPanelTab::Captures;
    app.focus_pane(Pane::Editor);
    draw(&mut app, W, H);
    let area = app.captures.last_area;
    let (x, y) = (area.y..area.y + area.height)
        .flat_map(|y| (area.x..area.x + area.width).map(move |x| (x, y)))
        .find(|&(x, y)| app.captures.row_at(x, y) == Some(1))
        .expect("the second capture is drawn");
    right_click(&mut app, x, y);
    let mut text = written(&app, "the second CAPTURES row");
    let picked = app.captures.selected_entry().map(|c| c.message.clone());
    writeln!(text, "picked capture {picked:?}").unwrap();
    check("right-click-capture", &text, tmp.path());
}

#[test]
fn the_tab_menus() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    for name in ["a.rs", "b.rs", "c.rs"] {
        let path = tmp.path().join(name);
        std::fs::write(&path, "fn main() {}\n").unwrap();
        app.editor.open_pinned(&path).unwrap();
    }
    let mut text = String::new();
    for (i, spot) in ["the first tab", "the middle tab", "the last tab"]
        .iter()
        .enumerate()
    {
        app.context_menu = None;
        app.focus_pane(Pane::Tree);
        draw(&mut app, W, H);
        let full = app.editor.last_full_area;
        let (x, y) = (full.y..full.y + 2)
            .flat_map(|y| (full.x..full.x + full.width).map(move |x| (x, y)))
            .find(|&(x, y)| app.editor.tab_at(x, y) == Some(i))
            .unwrap_or_else(|| panic!("{spot} is drawn"));
        right_click(&mut app, x, y);
        text.push_str(&written(&app, spot));
    }
    check("right-click-tab", &text, tmp.path());
}

#[test]
fn the_terminal_menus() {
    let (tmp, mut app, _term) = app_with_terminal_panes(1);
    let mut text = String::new();
    let aim = |app: &mut App, idx: usize, spot: &str, text: &mut String| {
        app.context_menu = None;
        app.focus_pane(Pane::Editor);
        draw(app, 160, 30);
        let area = app.terminals[idx].last_area;
        right_click(app, area.x + area.width / 2, area.y + area.height / 2);
        text.push_str(&written(app, spot));
        writeln!(text, "active pane {}", app.active_terminal).unwrap();
    };
    aim(&mut app, 0, "a lone terminal", &mut text);
    app.split_terminal().unwrap();
    app.split_terminal().unwrap();
    aim(&mut app, 1, "the middle of three terminals", &mut text);
    app.broadcast_input = true;
    aim(
        &mut app,
        1,
        "the middle of three terminals, broadcasting",
        &mut text,
    );
    app.broadcast_input = false;
    app.toggle_terminal_collapse(0);
    app.context_menu = None;
    draw(&mut app, 160, 30);
    let strip = app.terminal_strip_rects[0];
    right_click(&mut app, strip.x, strip.y + 1);
    text.push_str(&written(&app, "a folded terminal's strip"));
    writeln!(text, "active pane {}", app.active_terminal).unwrap();
    check("right-click-terminal", &text, tmp.path());
}

#[test]
fn the_explorer_menus() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(root.join("README.md"), "# hi\n").unwrap();
    std::fs::write(root.join("notes.txt"), "n\n").unwrap();
    let mut app = App::new(root.to_path_buf()).unwrap();
    app.sidebar_view = SidebarView::Explorer;
    app.show_tree = true;
    let mut text = String::new();
    let picked = |app: &App, text: &mut String| {
        let name = |p: &Path| p.strip_prefix(root).unwrap_or(p).display().to_string();
        let mut marked: Vec<String> = app.tree.marked.iter().map(|p| name(p)).collect();
        marked.sort();
        let selected = app
            .tree
            .nodes
            .get(app.tree.selected)
            .map(|n| name(&n.path))
            .unwrap_or_default();
        writeln!(text, "selected {selected:?}, marked {marked:?}").unwrap();
    };
    let row_of = |app: &App, name: &str| {
        let i = app
            .tree
            .nodes
            .iter()
            .position(|n| n.path == root.join(name))
            .unwrap_or_else(|| panic!("{name} is in the tree"));
        row_where(app.tree.last_area, |y| app.tree.node_at_y(y) == Some(i))
    };
    for (spot, name) in [("a folder row", "src"), ("a file row", "README.md")] {
        app.context_menu = None;
        app.focus_pane(Pane::Editor);
        draw(&mut app, W, H);
        let (x, y) = (app.tree.last_area.x + 3, row_of(&app, name));
        right_click(&mut app, x, y);
        text.push_str(&written(&app, spot));
        picked(&app, &mut text);
    }
    // Two marked rows: a right-click on one of them keeps both.
    app.context_menu = None;
    app.tree.marked.clear();
    app.tree.marked.insert(root.join("README.md"));
    app.tree.marked.insert(root.join("notes.txt"));
    draw(&mut app, W, H);
    let (x, y) = (app.tree.last_area.x + 3, row_of(&app, "notes.txt"));
    right_click(&mut app, x, y);
    text.push_str(&written(&app, "one of two marked rows"));
    picked(&app, &mut text);
    // Below the last row: the workspace's own menu.
    app.context_menu = None;
    draw(&mut app, W, H);
    let area = app.tree.last_area;
    let y = row_where(area, |y| {
        y > area.y && app.tree.node_at_y(y).is_none() && app.tree.sticky_row_at(y).is_none()
    });
    right_click(&mut app, area.x + 3, y);
    text.push_str(&written(&app, "the empty space below the rows"));
    picked(&app, &mut text);
    check("right-click-explorer", &text, root);
}

#[test]
fn the_gutter_menus() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("prog.py");
    std::fs::write(&f, "l1\nl2\nl3\nl4\nl5\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&f).unwrap();
    app.focus_pane(Pane::Tree);
    draw(&mut app, W, H);
    let col = app.editor.last_inner.x;
    let row = app.editor.last_inner.y + 2;
    right_click(&mut app, col, row);
    let mut text = written(&app, "the gutter of line 3");
    app.context_menu = None;
    app.editor
        .breakpoints
        .entry(f.clone())
        .or_default()
        .insert(3);
    app.editor
        .breakpoint_conditions
        .entry(f.clone())
        .or_default()
        .insert(3, "x > 1".into());
    app.editor
        .breakpoint_logs
        .entry(f.clone())
        .or_default()
        .insert(3, "x is {x}".into());
    right_click(&mut app, col, row);
    text.push_str(&written(
        &app,
        "the gutter of line 3, with a conditional logpoint",
    ));
    check("right-click-gutter", &text, tmp.path());
}

#[test]
fn the_editor_menus() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("main.rs");
    std::fs::write(&f, "fn main() {\n    let total = 1;\n}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&f).unwrap();
    app.focus_pane(Pane::Tree);
    draw(&mut app, W, H);
    let x = app.editor.last_inner.x + app.editor.last_gutter_width + 6;
    let y = app.editor.last_inner.y + 1;
    right_click(&mut app, x, y);
    let mut text = written(&app, "the text, nothing selected");
    app.context_menu = None;
    app.editor.selection = Some(crate::widgets::editor::EditorSelection {
        anchor: (1, 4),
        head: (1, 9),
    });
    right_click(&mut app, x, y);
    text.push_str(&written(&app, "the text, a word selected"));
    check("right-click-editor", &text, tmp.path());
}

/// `written` without where the menu opens, which a key and a click give
/// differently.
fn wherever(text: &str) -> String {
    text.lines()
        .map(|line| match line.strip_prefix("opens at ") {
            Some(rest) => format!("opens {}", rest.split_once("), ").map_or(rest, |(_, r)| r)),
            None => line.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn shift_f10(app: &mut App) {
    app.handle_key(key(KeyCode::F(10), KeyModifiers::SHIFT))
        .unwrap();
}

/// Right-click at `(x, y)`, then close the menu and press Shift+F10: the
/// two menus, as `written` gives them under `spot` without their spots.
fn clicked_then_keyed(app: &mut App, x: u16, y: u16, spot: &str) -> (String, String) {
    let focus = app.focus;
    right_click(app, x, y);
    let clicked = wherever(&written(app, spot));
    app.context_menu = None;
    app.focus_pane(focus);
    shift_f10(app);
    (clicked, wherever(&written(app, spot)))
}

fn opened_at(app: &App) -> (u16, u16) {
    app.context_menu.as_ref().expect("a menu is open").origin
}

fn text_app() -> (tempfile::TempDir, App) {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("main.rs");
    std::fs::write(&f, "fn main() {\n    let total = 1;\n}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&f).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.cursor_row = 1;
    app.editor.cursor_col = 5;
    draw(&mut app, W, H);
    (tmp, app)
}

/// #2739: Shift+F10 in the editor opens the text's menu for the caret,
/// just below it: the menu a right-click on the caret's cell opens.
#[test]
fn shift_f10_in_the_editor_opens_the_text_menu_below_the_caret() {
    let (_tmp, mut app) = text_app();
    let (x, y) = app.editor.cursor_screen_pos().expect("the caret is drawn");
    let (clicked, keyed) = clicked_then_keyed(&mut app, x, y, "the caret");
    assert!(
        keyed.contains("GoToDefinitionAt { row: 1, col: 5 }"),
        "{keyed}"
    );
    assert_eq!(keyed, clicked);
    assert_eq!(opened_at(&app), (x, y + 1), "just below the caret");
}

/// #2739: the Menu key, as a terminal on the modern keyboard protocol
/// reports it, opens the same menu.
#[test]
fn the_menu_key_opens_the_same_menu() {
    let (_tmp, mut app) = text_app();
    shift_f10(&mut app);
    let by_shift_f10 = written(&app, "the caret");
    app.context_menu = None;
    app.handle_key(key(KeyCode::Menu, KeyModifiers::NONE))
        .unwrap();
    assert!(app.context_menu.is_some(), "the Menu key opened a menu");
    assert_eq!(written(&app, "the caret"), by_shift_f10);
}

/// #2739: Shift+F10 was Step Over. It opens the menu now and goes no
/// further; bare F10 still steps (`is_run_to_cursor_key` and the step-over
/// arm come after it).
#[test]
fn shift_f10_no_longer_steps_over() {
    let (_tmp, mut app) = text_app();
    assert!(!is_show_context_menu_key(key(
        KeyCode::F(10),
        KeyModifiers::NONE
    )));
    assert!(!is_show_context_menu_key(key(
        KeyCode::F(10),
        KeyModifiers::CONTROL
    )));
    shift_f10(&mut app);
    assert!(app.context_menu.is_some(), "Shift+F10 opened the menu");
}

/// #2739: in the Explorer, the selected row's menu.
#[test]
fn shift_f10_in_the_explorer_opens_the_selected_rows_menu() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::write(root.join("README.md"), "# hi\n").unwrap();
    let mut app = App::new(root.to_path_buf()).unwrap();
    app.sidebar_view = SidebarView::Explorer;
    app.show_tree = true;
    app.focus_pane(Pane::Tree);
    let idx = app
        .tree
        .nodes
        .iter()
        .position(|n| n.path == root.join("README.md"))
        .unwrap();
    app.tree.select_replace(idx);
    draw(&mut app, W, H);
    let area = app.tree.last_area;
    let y = row_where(area, |y| app.tree.node_at_y(y) == Some(idx));
    let (clicked, keyed) = clicked_then_keyed(&mut app, area.x + 3, y, "README.md");
    assert!(
        keyed.contains("Rename(\"") && keyed.contains("README.md"),
        "{keyed}"
    );
    assert_eq!(keyed, clicked);
    assert_eq!(opened_at(&app).1, y + 1, "just below the row");
}

/// #2739: with COMMITS holding the keys, the picked commit's menu.
#[test]
fn shift_f10_in_commits_opens_the_picked_commits_menu() {
    let tmp = make_committed_repo();
    let p = tmp.path();
    std::fs::write(p.join("b.txt"), "b\n").unwrap();
    assert!(git_succeeds(p, &["add", "."]) && git_succeeds(p, &["commit", "-qm", "add b"]));
    let mut app = App::new(p.to_path_buf()).unwrap();
    app.sidebar_view = SidebarView::SourceControl;
    app.show_tree = true;
    app.source_control.status.in_repo = true;
    app.commit_graph
        .set_rows(crate::widgets::commit_graph::layout_graph(
            crate::git::commit_graph(p, 100),
        ));
    app.focus_pane(Pane::Tree);
    app.source_control.keys = crate::widgets::source_control::KeyStop::Commits;
    app.commit_graph.select(1);
    draw(&mut app, W, H);
    let area = app.commit_graph.last_area;
    let y = row_where(area, |y| app.commit_graph.row_index_at(y) == Some(1));
    let (clicked, keyed) = clicked_then_keyed(&mut app, area.x + 4, y, "the picked commit");
    assert!(keyed.contains("Cherry-Pick"), "{keyed}");
    assert_eq!(keyed, clicked);
    assert_eq!(opened_at(&app).1, y + 1, "just below the row");
}

/// #2739: in a terminal, the active pane's menu, at its cursor.
#[test]
fn shift_f10_in_a_terminal_opens_the_panes_menu() {
    let (_tmp, mut app, _term) = app_with_terminal_panes(2);
    app.bottom_panel_tab = BottomPanelTab::Terminal;
    app.active_terminal = 1;
    app.focus_pane(Pane::Terminal);
    draw(&mut app, 160, 30);
    let area = app.terminals[1].last_area;
    let (clicked, keyed) = clicked_then_keyed(
        &mut app,
        area.x + area.width / 2,
        area.y + area.height / 2,
        "the active pane",
    );
    assert!(keyed.contains("RenameTerminal(1)"), "{keyed}");
    assert_eq!(keyed, clicked);
}

/// #2739: in CAPTURES, the selected capture's menu.
#[test]
fn shift_f10_in_captures_opens_the_selected_captures_menu() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    for message in ["Build failed", "Test failed"] {
        app.captures.push(crate::widgets::captures::CapturedLine {
            pane: "zsh".into(),
            shell_pid: None,
            message: message.into(),
            line: format!("error: {message}"),
            anchor: None,
        });
    }
    app.bottom_panel_tab = BottomPanelTab::Captures;
    app.focus_pane(Pane::Terminal);
    app.captures.select_index(0);
    draw(&mut app, W, H);
    let r = app
        .captures
        .row_rect(0)
        .expect("the first capture is drawn");
    let (clicked, keyed) = clicked_then_keyed(&mut app, r.x + 2, r.y, "the selected capture");
    assert!(keyed.contains("AskNavigatorAboutCapture"), "{keyed}");
    assert_eq!(keyed, clicked);
    assert_eq!(
        app.captures.selected_entry().map(|c| c.message.as_str()),
        Some("Build failed")
    );
    assert_eq!(opened_at(&app).1, r.y + 1, "just below the row");
}

/// #2739: in PROBLEMS, which has no keyboard row yet, the first problem
/// shown.
#[test]
fn shift_f10_in_problems_opens_the_first_problems_menu() {
    use crate::lsp::manager::{Diagnostic, DiagnosticSeverity};
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("hello.rs");
    std::fs::write(&file, "fn f0() {}\nfn f1() {}\nfn f2() {}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let warning = |line: u32, message: &str| Diagnostic {
        start_line: line,
        start_char: 3,
        end_line: line,
        end_char: 5,
        severity: DiagnosticSeverity::Warning,
        message: message.into(),
        tags: Default::default(),
        code: None,
        origin: None,
    };
    let mut by_server = std::collections::HashMap::new();
    by_server.insert(
        String::from("rustc"),
        vec![
            warning(1, "`f1` is never used"),
            warning(2, "`f2` is never used"),
        ],
    );
    app.lsp_diagnostics.insert(file.clone(), by_server);
    app.rebuild_problems();
    app.bottom_panel_tab = BottomPanelTab::Problems;
    app.focus_pane(Pane::Terminal);
    draw(&mut app, W, H);
    let list = app.problems.last_area;
    let y = row_where(list, |y| app.problems.diagnostic_at(y).is_some());
    let (clicked, keyed) = clicked_then_keyed(&mut app, list.x + 30, y, "the first problem");
    assert!(keyed.contains("f1"), "{keyed}");
    assert_eq!(keyed, clicked);
}

/// #2739: where what has the keys has no right-click menu (the Search
/// view), Shift+F10 opens nothing.
#[test]
fn shift_f10_opens_nothing_where_nothing_has_a_menu() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.sidebar_view = SidebarView::Search;
    app.show_tree = true;
    app.focus_pane(Pane::Tree);
    draw(&mut app, W, H);
    shift_f10(&mut app);
    assert!(app.context_menu.is_none());
}
