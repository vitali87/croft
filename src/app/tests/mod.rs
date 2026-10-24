//! Whole-app tests, one file per area. A new test goes in the file for
//! the area it covers, beside the tests closest to it rather than at the
//! end of the file. Helpers shared by more than one file live here; a
//! helper only one file uses lives in that file.

use super::*;
use crate::gradient::lerp_rgb;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

mod agents;
mod codeql;
mod collab;
mod debug;
mod diff_merge;
mod editing;
mod editor_view;
mod explorer;
mod extensions;
mod files;
mod hot_exit;
mod input;
mod keybindings;
mod layout;
mod lsp;
mod menus;
mod misc;
mod navigation;
mod notebook;
mod palette;
mod problems;
mod remote;
mod review;
mod sarif;
mod scm;
mod search;
mod settings;
mod tabs;
mod tasks;
mod terminal;
mod terminal_panes;
mod testing;
mod updates;
mod viewers;
mod vim;
mod welcome;

fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
    KeyEvent::new(code, mods)
}

/// Shell command that prints `probe` on its own line WITHOUT the typed
/// command line spelling the probe out.
///
/// The terminal end-to-end tests find their probe by searching the rendered
/// grid for it, but the shell echoes what was typed, so a literal
/// `printf '<probe>\n'` puts two matching rows on screen. Under load the echo
/// arrives first, the wait loop is satisfied by it, and the row search lands
/// on the echoed command: its quoting shifts the columns, and the selection
/// comes back as `"oft-e2e-638881\\n"` instead of the probe (issue #226).
/// Splitting the probe across two printf arguments leaves the echo carrying
/// only the fragments, so nothing but the OUTPUT can ever match.
fn probe_command(probe: &str) -> String {
    let (head, tail) = probe.split_at(1);
    format!("printf '%s%s\\n' '{head}' '{tail}'\r")
}

/// Run the probe command in the app's embedded terminal and wait until the
/// probe is on screen. Panics with the actual screen contents if it never
/// arrives, so a failure says what the terminal was showing instead of
/// unwrapping `None` several lines later.
///
/// The wait is load-scaled (#397): a real shell has to start, run the
/// command, and have the reader thread drain the bytes into the grid, and
/// what stretches that is contention from the rest of the suite, not this
/// test. The fixed 15s this replaced (#226 had raised it from 3s) is what a
/// quiet machine still gets at the floor; the captured failure had the probe
/// command echoed on screen and unanswered at 15s, a starved shell rather
/// than a broken one. `spawn_budget` rather than `await_spawned` so the
/// panic can keep showing the screen.
fn await_terminal_probe(app: &mut App, probe: &str) {
    app.terminal_mut()
        .write_input(probe_command(probe).as_bytes());
    let base = crate::test_budget::tests::TERMINAL_PROBE_BASE;
    let budget = crate::test_budget::spawn_budget(base);
    let started = std::time::Instant::now();
    while started.elapsed() < budget {
        if app.terminal().visible_text().contains(probe) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    panic!(
        "the shell never printed the probe {probe:?} within {budget:?} ({base:?} base x{} total \
         for calibration and load); if this is a real failure it would also fail at 1x, so \
         re-run the full suite on the unmodified merge base under the same load before \
         suspecting your diff. Terminal showed:\n{}",
        budget.as_millis() / base.as_millis().max(1),
        app.terminal().visible_text()
    );
}

/// Display label of a menu entry ("---" for a divider) — test helper for the
/// `MenuEntry`-based context menus.
fn menu_label(e: &MenuEntry) -> &str {
    match e {
        MenuEntry::Item { label, .. }
        | MenuEntry::Submenu { label, .. }
        | MenuEntry::Header(label) => label,
        MenuEntry::Separator => "---",
    }
}

/// Top-level labels of a menu, in order.
fn menu_labels(items: &[MenuEntry]) -> Vec<&str> {
    items.iter().map(menu_label).collect()
}

/// The action carried by the first `Item` with `label`, if any.
fn menu_action_of<'a>(items: &'a [MenuEntry], label: &str) -> Option<&'a MenuAction> {
    items.iter().find_map(|e| match e {
        MenuEntry::Item { label: l, action } if l == label => Some(action),
        _ => None,
    })
}

fn dummy_activity_images() -> ActivityBarImages {
    let s = || String::from("\x1b]1337;File=inline=1:Zm9v\x07");
    ActivityBarImages {
        explorer_active: s(),
        explorer_inactive: s(),
        explorer_hovered: s(),
        search_active: s(),
        search_inactive: s(),
        search_hovered: s(),
        source_control_active: s(),
        source_control_inactive: s(),
        source_control_hovered: s(),
        remote_active: s(),
        remote_inactive: s(),
        remote_hovered: s(),
        run_debug_active: s(),
        run_debug_inactive: s(),
        run_debug_hovered: s(),
        extensions_active: s(),
        extensions_inactive: s(),
        extensions_hovered: s(),
        testing_active: s(),
        testing_inactive: s(),
        testing_hovered: s(),
        codeql_active: s(),
        codeql_inactive: s(),
        codeql_hovered: s(),
        settings_active: s(),
        settings_inactive: s(),
        settings_hovered: s(),
        layout_sidebar_on: s(),
        layout_sidebar_on_hover: s(),
        layout_sidebar_off: s(),
        layout_sidebar_off_hover: s(),
        layout_panel_on: s(),
        layout_panel_on_hover: s(),
        layout_panel_off: s(),
        layout_panel_off_hover: s(),
        layout_customize: s(),
        layout_customize_hover: s(),
    }
}

/// Stand up an app with a real file open in the editor and one frame
/// rendered (so every `last_*` rect is live), returning a cell inside the
/// editor body for tap-hover tests.
fn app_with_open_file_and_editor_cell() -> (App, tempfile::TempDir, u16, u16) {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("m.py");
    std::fs::write(&f, "value = compute()\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&f).unwrap();
    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|frame| app.render(frame)).unwrap();
    let col = app.editor.last_inner.x + app.editor.last_gutter_width as u16 + 3;
    let row = app.editor.last_inner.y + 1;
    assert!(
        rect_contains(app.editor.last_area, col, row),
        "test cell must land inside the editor body"
    );
    (app, tmp, col, row)
}

/// Every cell of `pane`'s own `last_area`, row-major.
///
/// ONE walk. The column derivation in it was wrong once, and a second copy is
/// a second place to fix it. `hyperlink_at_screen` and `line_text_at` both
/// hit-test `last_inner`, which this rect contains, and answer `None` outside
/// it, so nothing yielded here is a cell a click could not reach.
fn cells_of(pane: &crate::widgets::terminal::PtyTerminal) -> impl Iterator<Item = (u16, u16)> {
    let area = pane.last_area;
    (area.y..area.y + area.height)
        .flat_map(move |r| (area.x..area.x + area.width).map(move |c| (c, r)))
}

/// The first grid cell of `pane` that falls INSIDE the first occurrence of
/// `needle` on its row, searched row-major over the pane's own `last_area`
/// (#397).
///
/// A test that prints something into a pane and then clicks it must not assume
/// which row it landed on: the pane runs a REAL shell whose prompt races the
/// feed, so the row is a property of machine load rather than of the test.
/// Callers anchor on the exact text they printed, so unrelated output cannot
/// satisfy the search.
fn cell_carrying(pane: &crate::widgets::terminal::PtyTerminal, needle: &str) -> Option<(u16, u16)> {
    cells_of(pane).find(|&(c, r)| {
        // The cell must be ON the needle, not merely on a ROW containing
        // it. `line_text_at` hands back the whole row plus the char index
        // under the cell, and every column maps to an index (blanks
        // included), so a `text.contains` test succeeds at the row's FIRST
        // inner column whatever the needle's position. These fixtures print
        // at column 0, which hid it; a row like `bash-5.2$ some_token`
        // would have matched and clicked the `b`.
        //
        // A click lands on a cell, not a row, so the span is the claim.
        pane.line_text_at(c, r).is_some_and(|(text, idx)| {
            text.find(needle).is_some_and(|byte_start| {
                let start = text[..byte_start].chars().count();
                (start..start + needle.chars().count()).contains(&idx)
            })
        })
    })
}

/// The first grid cell of `pane` carrying an OSC 8 link to exactly `uri`,
/// searched row-major over the pane's own `last_area` (#397).
///
/// The URI is the anchor rather than "any link", so a decoy printed beside the
/// payload cannot satisfy the search. Below [`cell_carrying`] rather than
/// above it: inserting an item over a `///` block hands that block to the
/// newcomer, which is what happened here once already.
fn cell_with_link(pane: &crate::widgets::terminal::PtyTerminal, uri: &str) -> Option<(u16, u16)> {
    cells_of(pane).find(|&(c, r)| pane.hyperlink_at_screen(c, r).as_deref() == Some(uri))
}

fn mouse(
    kind: crossterm::event::MouseEventKind,
    col: u16,
    row: u16,
) -> crossterm::event::MouseEvent {
    crossterm::event::MouseEvent {
        kind,
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

/// A tree app, rendered, so `tree.last_area` is populated and a click can be
/// aimed at a real cell.
fn tree_app_with_keymap(json: &str) -> (App, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("a.rs");
    std::fs::write(&f, "let name = value;\nfn main() {}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // An open buffer, so a click in the editor region resolves to it. Without
    // one there is no editor to hit and the gesture falls through.
    app.editor.open_pinned(&f).unwrap();
    app.keymap = crate::keymap::Keymap::from_json(json);
    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|frame| app.render(frame)).unwrap();
    (app, tmp)
}

/// Tabs a.py, b.txt and c.txt, pinned in that order with c.txt active, the
/// keymap `json`, painted once so the strip has its cells.
fn three_tab_app(json: &str) -> (App, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    for (name, text) in [("a.py", "a\n"), ("b.txt", "b\n"), ("c.txt", "c\n")] {
        let f = tmp.path().join(name);
        std::fs::write(&f, text).unwrap();
        app.editor.open_pinned(&f).unwrap();
    }
    app.keymap = crate::keymap::Keymap::from_json(json);
    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|frame| app.render(frame)).unwrap();
    (app, tmp)
}

fn tab_names(app: &App) -> Vec<String> {
    (0..app.editor.tab_count())
        .map(|i| {
            app.editor
                .tab_path(i)
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                .unwrap_or_default()
        })
        .collect()
}

/// A cell on tab `idx` in the strip.
fn tab_cell(app: &App, idx: usize) -> (u16, u16) {
    let y = app.editor.tab_strip_y_for_test();
    let x = (0..app.editor.last_inner.right())
        .find(|&x| app.editor.tab_at(x, y) == Some(idx))
        .expect("the tab is drawn");
    (x, y)
}

fn press(app: &mut App, button: crossterm::event::MouseButton, (x, y): (u16, u16)) {
    use crossterm::event::MouseEventKind;
    app.handle_mouse(mouse(MouseEventKind::Down(button), x, y));
    app.handle_mouse(mouse(MouseEventKind::Up(button), x, y));
}

/// An app with `text` open in prog.py, painted once, so the gutter has its
/// geometry.
fn app_with_painted_file(text: &str) -> (App, tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("prog.py");
    std::fs::write(&f, text).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&f).unwrap();
    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|frame| app.render(frame)).unwrap();
    (app, tmp, f)
}

fn click_and_release(app: &mut App, col: u16, row: u16) {
    use crossterm::event::{MouseButton, MouseEventKind};
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), col, row));
    app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), col, row));
}

/// The paths of the open tabs, in tab order.
fn open_tab_paths(app: &App) -> Vec<PathBuf> {
    app.editor
        .editors
        .iter()
        .filter_map(|e| e.path.clone())
        .collect()
}

/// Three C files: `m.c` opened pinned, `main.c` and `m.h` beside it.
fn preview_nav_fixture() -> (tempfile::TempDir, App, PathBuf, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let mc = tmp.path().join("m.c");
    let main = tmp.path().join("main.c");
    let mh = tmp.path().join("m.h");
    std::fs::write(&mh, "#pragma once\nint add(int a, int b);\n").unwrap();
    std::fs::write(
        &mc,
        "#include \"m.h\"\nint add(int a, int b) { return a + b; }\n",
    )
    .unwrap();
    std::fs::write(
        &main,
        "#include \"m.h\"\nint main(void) { return add(1, 2); }\n",
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&mc).unwrap();
    (tmp, app, mc, main, mh)
}

/// #1635 fixture: `a.txt` (120 lines) open with the caret on line 40, and
/// `b.txt` (60 lines, then `needle here`) beside it.
fn nav_switch_fixture() -> (tempfile::TempDir, App, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let a = root.join("a.txt");
    let b = root.join("b.txt");
    let lines = |name: &str, n: usize| -> String {
        (1..=n).map(|i| format!("{name} line {i}\n")).collect()
    };
    std::fs::write(&a, lines("alpha", 120)).unwrap();
    std::fs::write(&b, lines("beta", 60) + "needle here\n").unwrap();
    let mut app = App::new(root).unwrap();
    app.editor.open_pinned(&a).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.cursor_row = 39;
    (tmp, app, a, b)
}

fn git_stdout_in(root: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn file_node(path: &Path) -> crate::widgets::file_tree::Node {
    crate::widgets::file_tree::Node {
        path: path.to_path_buf(),
        depth: 1,
        is_dir: false,
        expanded: false,
        loaded: false,
    }
}

fn type_into_sheet(app: &mut App, text: &str) {
    for ch in text.chars() {
        app.handle_sheet_key(key(KeyCode::Char(ch), KeyModifiers::NONE));
    }
}

/// `prices.xlsx` from #1141 open in the grid, drawn once so clicks land.
fn prices_xlsx_app() -> (tempfile::TempDir, std::path::PathBuf, App) {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("prices.xlsx");
    let mut book = umya_spreadsheet::new_file();
    let ws = book.sheet_mut(0).unwrap();
    ws.cell_mut((1u32, 1u32)).set_value("item");
    ws.cell_mut((2u32, 1u32)).set_value("price");
    for (row, (item, price)) in [("bolt", 4), ("nut", 9), ("washer", 25)].iter().enumerate() {
        ws.cell_mut((1u32, row as u32 + 2)).set_value(*item);
        ws.cell_mut((2u32, row as u32 + 2)).set_value_number(*price);
    }
    umya_spreadsheet::writer::xlsx::write(&book, &p).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&p).unwrap();
    assert!(app.editor.sheet.is_some());
    draw(&mut app, 120, 40);
    (tmp, p, app)
}

/// Screen coordinates of grid cell `(row, col)` in the last drawn frame.
fn sheet_cell_on_screen(app: &App, row: usize, col: usize) -> (u16, u16) {
    let view = app.editor.sheet.as_ref().unwrap();
    (0..40)
        .flat_map(|y| (0..120).map(move |x| (x, y)))
        .find(|&(x, y)| sheet_cell_at(view, x, y) == Some((row, col)))
        .expect("the cell is on screen")
}

fn xlsx_value(p: &std::path::Path, col: u32, row: u32) -> String {
    let book = umya_spreadsheet::reader::xlsx::read(p).unwrap();
    book.sheet(0)
        .unwrap()
        .cell((col, row))
        .unwrap()
        .value()
        .to_string()
}

/// Type `text` into the focused editor, which leaves its buffer dirty.
fn type_into_editor(app: &mut App, text: &str) {
    for c in text.chars() {
        app.handle_key(key(KeyCode::Char(c), KeyModifiers::NONE))
            .unwrap();
    }
}

fn press_ctrl_q(app: &mut App) {
    app.handle_key(key(KeyCode::Char('q'), KeyModifiers::CONTROL))
        .unwrap();
}

/// An app with `a.txt` open, edited and unsaved, and the file's path (#862).
fn app_with_unsaved_file(tmp: &std::path::Path) -> (App, PathBuf) {
    let mut app = app_with_open_file(tmp, "a.txt", "alpha\n");
    type_into_editor(&mut app, "x");
    assert!(app.editor.dirty, "setup: typing leaves the buffer dirty");
    (app, tmp.join("a.txt"))
}

/// A remote connect whose install just reported `event`: the croft on the
/// host is ready to take this terminal over (no ssh is run).
fn ready_to_hand_off(app: &mut App, event: crate::install_session::InstallEvent) {
    let adopted = crate::remote::AdoptedMaster {
        host: String::from("devbox"),
        socket_dir: PathBuf::from("/nonexistent/croft-test-ctl"),
        socket_path: PathBuf::from("/nonexistent/croft-test-ctl/ctl"),
    };
    app.install_session = Some(crate::install_session::InstallSession::with_events(
        "devbox",
        None,
        Some(adopted),
        vec![event],
    ));
}

/// Hot exit (#862) on for `app`, backing up under `cache` instead of the
/// real `~/.cache/croft`.
fn with_hot_exit(mut app: App, cache: &std::path::Path) -> App {
    app.hot_exit_dir = Some(cache.to_path_buf());
    app
}

/// A hot-exit tick long after the last edit, so the buffers count as settled.
fn settle_hot_exit(app: &mut App) {
    app.tick_hot_exit(std::time::Instant::now() + std::time::Duration::from_secs(60));
}

/// The hot-exit backup files for the workspace `root` under `cache`.
fn hot_exit_files(cache: &std::path::Path, root: &std::path::Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(crate::hot_exit::workspace_dir(cache, root)) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    files
}

/// The texts hot-exit backups under `cache` hold for `file`, sorted.
fn hot_exit_texts_of(
    cache: &std::path::Path,
    root: &std::path::Path,
    file: &std::path::Path,
) -> Vec<String> {
    let mut texts: Vec<String> = hot_exit_files(cache, root)
        .iter()
        .flat_map(|f| crate::hot_exit::Backup::load(f).unwrap().buffers)
        .filter(|b| b.tab.path.as_deref() == Some(file))
        .filter_map(|b| b.tab.unsaved_text)
        .collect();
    texts.sort();
    texts
}

/// The shell script from #1563, open in the editor with the caret at 1:1.
fn app_with_run_script() -> (tempfile::TempDir, App) {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("r.sh");
    std::fs::write(
        &p,
        "echo one\necho two\nfor i in 1 2; do\n  echo loop $i\ndone\n",
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&p).unwrap();
    app.focus_pane(Pane::Editor);
    (tmp, app)
}

/// Run the Command Palette row titled `title` the way a user does: open the
/// palette (Cmd+Shift+P), type the title, pick its row, Enter. Returns false,
/// running nothing, when no row has that title.
fn run_from_palette(app: &mut App, title: &str) -> bool {
    app.handle_key(key(
        KeyCode::Char('p'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ))
    .unwrap();
    for c in title.chars() {
        app.handle_key(key(KeyCode::Char(c), KeyModifiers::NONE))
            .unwrap();
    }
    let palette = app.command_palette.as_mut().expect("the palette opened");
    let Some(row) = palette.results.iter().position(|i| i.title() == title) else {
        app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();
        return false;
    };
    palette.selected = row;
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    true
}

/// A Markdown source with one runnable `sh` fence, the caret inside it.
fn markdown_fence_app(dir: &Path) -> App {
    let mut app = app_with_open_file(dir, "R.md", "# Run\n\n```sh\necho hi\n```\n");
    app.editor.cursor_row = 3;
    app
}

fn make_committed_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path();
    let _ = std::process::Command::new("git")
        .args(["-C"])
        .arg(p)
        .args(["init", "-q", "-b", "main"])
        .output();
    let _ = std::process::Command::new("git")
        .args(["-C"])
        .arg(p)
        .args(["config", "user.email", "a@b"])
        .status();
    let _ = std::process::Command::new("git")
        .args(["-C"])
        .arg(p)
        .args(["config", "user.name", "a"])
        .status();
    std::fs::write(p.join("seed.txt"), b"seed\n").unwrap();
    let _ = std::process::Command::new("git")
        .args(["-C"])
        .arg(p)
        .args(["add", "seed.txt"])
        .status();
    let _ = std::process::Command::new("git")
        .args(["-C"])
        .arg(p)
        .args(["commit", "-q", "-m", "init"])
        .status();
    tmp
}

fn wait_for_changes(app: &mut App, predicate: impl Fn(&App) -> bool) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let mut next_kick = std::time::Instant::now();
    while std::time::Instant::now() < deadline {
        if std::time::Instant::now() >= next_kick {
            app.refresh_source_control();
            next_kick = std::time::Instant::now() + std::time::Duration::from_millis(200);
        }
        app.drain_git_responses();
        if predicate(app) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    predicate(app)
}

/// The issue's repo (#1352): `value_1`..`value_20` committed, then line 5
/// changed to `value_FIVE` and line 9 deleted, open in an editor tab with
/// its change bars computed.
fn app_with_change_bars() -> (App, tempfile::TempDir) {
    let tmp = make_committed_repo();
    let root = tmp.path();
    let f = root.join("v.txt");
    let body: String = (1..=20).map(|i| format!("value_{i}\n")).collect();
    std::fs::write(&f, &body).unwrap();
    for args in [&["add", "v.txt"][..], &["commit", "-q", "-m", "values"]] {
        let st = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .status()
            .unwrap();
        assert!(st.success());
    }
    let edited: String = (1..=20)
        .filter(|&i| i != 9)
        .map(|i| {
            if i == 5 {
                String::from("value_FIVE\n")
            } else {
                format!("value_{i}\n")
            }
        })
        .collect();
    std::fs::write(&f, edited).unwrap();
    let mut app = App::new(root.to_path_buf()).unwrap();
    app.editor.open_pinned(&f).unwrap();
    app.focus_pane(Pane::Editor);
    app.sync_git_gutters();
    (app, tmp)
}

/// Drain the network git operation in flight until it lands.
fn wait_for_git_net(app: &mut App) {
    crate::test_budget::await_spawned(
        std::time::Duration::from_secs(10),
        "the network git operation",
        || {
            app.drain_git_net();
            app.git_net_job.is_none()
        },
    );
}

/// The issue's repo (#1356): `main` and `feat` both change `f` from
/// `base`, so merging, rebasing or cherry-picking one onto the other stops
/// on a conflict.
fn diverged_repo() -> tempfile::TempDir {
    let tmp = make_committed_repo();
    let p = tmp.path();
    let f = p.join("f");
    std::fs::write(&f, "base\n").unwrap();
    assert!(git_succeeds(p, &["add", "f"]) && git_succeeds(p, &["commit", "-qm", "base"]));
    assert!(git_succeeds(p, &["checkout", "-qb", "feat"]));
    std::fs::write(&f, "feat\n").unwrap();
    assert!(git_succeeds(p, &["commit", "-qam", "feat"]));
    assert!(git_succeeds(p, &["checkout", "-q", "main"]));
    std::fs::write(&f, "mainline\n").unwrap();
    assert!(git_succeeds(p, &["commit", "-qam", "main"]));
    tmp
}

/// Run git in `dir` without an editor; true when it succeeded.
fn git_succeeds(dir: &std::path::Path, args: &[&str]) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_EDITOR", "true")
        .output()
        .unwrap()
        .status
        .success()
}

/// A repo with `seed.txt` changed and the Source Control panel open on it,
/// ready to commit (#1241).
fn scm_ready_to_commit() -> (tempfile::TempDir, App) {
    let tmp = make_committed_repo();
    std::fs::write(tmp.path().join("seed.txt"), b"changed\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    wait_for_changes(&mut app, |a| {
        a.source_control
            .entries
            .iter()
            .any(|e| e.path == "seed.txt")
    });
    app.set_sidebar_view(SidebarView::SourceControl);
    (tmp, app)
}

/// #1438: the committed repo with `seed.txt` open in a tab whose edit
/// (`edited changed`) is not saved, and the message box filled.
fn scm_with_unsaved_tab() -> (tempfile::TempDir, App) {
    let (tmp, mut app) = scm_ready_to_commit();
    app.editor
        .open_pinned(&tmp.path().join("seed.txt"))
        .unwrap();
    app.editor.insert_str("edited ");
    assert!(app.editor.dirty);
    app.source_control.message = "bump seed".to_string();
    app.source_control.message_cursor = app.source_control.message.chars().count();
    (tmp, app)
}

fn last_commit_message(root: &std::path::Path) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["log", "-1", "--format=%B"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn editor_app_with_lines(lines: &[&str]) -> App {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.lines = lines.iter().map(|s| (*s).to_string()).collect();
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 0;
    app
}

/// Wait for the provenance sidecar the save records after its snapshot
/// (#349); it lands on the same thread, strictly after the snapshot.
fn wait_for_seats(root: &std::path::Path, f: &std::path::Path, millis: u64) {
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(500),
        "the provenance sidecar",
        || crate::history::seats_for(root, f, millis).is_some(),
    );
}

/// History recording runs off-thread; poll until the store has entries (or a
/// deadline passes and the caller's assert reports the miss).
fn wait_for_snapshots(
    root: &std::path::Path,
    f: &std::path::Path,
) -> Vec<crate::history::Snapshot> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let snaps = crate::history::entries_in(root, f);
        if !snaps.is_empty() || std::time::Instant::now() > deadline {
            return snaps;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// A scratch config dir holding the given user extension manifests
/// (`<id>` → manifest text). An app pointed at it (`app.config_dir`) reads
/// its extensions and consent from there and nowhere else; the process
/// environment is never touched, so the test is safe beside every other.
fn scratch_config(manifests: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let croft = tmp.path().join("croft");
    for (id, text) in manifests {
        let dir = croft.join("extensions").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("extension.toml"), text).unwrap();
    }
    (tmp, croft)
}

/// The consent set recorded in a scratch config dir.
fn consent_on_disk(croft: &Path) -> std::collections::BTreeSet<String> {
    crate::prefs::Prefs::load(&croft.join("config.json"))
        .unwrap_or_default()
        .mcp_consented
}

fn relay_test_lock() -> &'static std::sync::Mutex<()> {
    // The relay tests share the cache-dir test override, the
    // CROFT_RELAY_KEY env var, and real relay directories, so they still
    // serialize against each other — but no test mutates `$HOME` any more
    // (#37), so the old crate-wide HOME_LOCK is gone and this lock is
    // private to the relay family.
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    &LOCK
}

/// Point `$HOME` (hence `croft_cache_dir`) at a temp dir and set a fixed
/// `CROFT_RELAY_KEY` for the duration of `body`, restoring prior values after.
/// The relay dir is `$HOME/.cache/croft/relay-<CROFT_RELAY_KEY>`, so a relay
/// test controls the rendezvous by controlling those two env vars rather than
/// injecting the old per-connection `CROFT_DROP_RELAY_*` paths. Serialized by
/// `relay_test_lock`.
const TEST_RELAY_KEY: &str = "0123456789abcdef";

fn with_relay_home<T>(home: &std::path::Path, body: impl FnOnce() -> T) -> T {
    // The cache dir is redirected through its test override instead of
    // repointing the process-global `$HOME` (#37): unrelated test threads
    // resolve `$HOME` at call time and a substituted value made them fail
    // inside the fake home. CROFT_RELAY_KEY stays an env var — it is
    // croft-specific and `relay_dir` is its only reader, so no unrelated
    // reader can race on it. The restore is RAII so a panicking `body`
    // cannot leak the redirected globals into later relay tests.
    struct RestoreRelayGlobals {
        prev_key: Option<std::ffi::OsString>,
    }
    impl Drop for RestoreRelayGlobals {
        fn drop(&mut self) {
            *crate::app::CACHE_DIR_OVERRIDE_FOR_TEST.lock().unwrap() = None;
            // SAFETY: relay tests hold relay_test_lock, serializing this
            // mutation.
            unsafe {
                match self.prev_key.take() {
                    Some(k) => std::env::set_var("CROFT_RELAY_KEY", k),
                    None => std::env::remove_var("CROFT_RELAY_KEY"),
                }
            }
        }
    }
    let _restore = RestoreRelayGlobals {
        prev_key: std::env::var_os("CROFT_RELAY_KEY"),
    };
    *crate::app::CACHE_DIR_OVERRIDE_FOR_TEST.lock().unwrap() =
        Some(home.join(".cache").join("croft"));
    // SAFETY: relay tests hold relay_test_lock, serializing this mutation.
    unsafe {
        std::env::set_var("CROFT_RELAY_KEY", TEST_RELAY_KEY);
    }
    body()
}

/// Drop a fake `bin/python` shim in `dir/.venv/bin/python` so the venv
/// detector finds an executable file. Returns the full path to the shim
/// for assertions.
fn make_venv(dir: &Path, venv_name: &str) -> PathBuf {
    let bin = dir.join(venv_name).join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let py = bin.join("python");
    std::fs::write(&py, b"#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&py).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&py, perms).unwrap();
    }
    py
}

/// #1400 fixture: `name` holds "old\n" on disk and "new\nold\n" in its
/// open, unsaved tab.
fn app_with_unsaved_pinned_file(name: &str) -> (tempfile::TempDir, App, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let file = root.join(name);
    std::fs::write(&file, "old\n").unwrap();
    let mut app = App::new(root).unwrap();
    app.editor.open_pinned(&file).unwrap();
    app.editor.insert_str("new\n");
    assert!(app.editor.dirty);
    (tmp, app, file)
}

// --- Native modal (vim) editing -------------------------------------------

fn vim_app(content: &str) -> (App, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("buf.txt");
    std::fs::write(&f, content).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&f).unwrap();
    app.focus = Pane::Editor;
    app.vim.enabled = true;
    (app, tmp)
}

fn vim_feed(app: &mut App, c: char) {
    app.handle_key(key(KeyCode::Char(c), KeyModifiers::NONE))
        .unwrap();
}

fn vim_feed_str(app: &mut App, s: &str) {
    for c in s.chars() {
        vim_feed(app, c);
    }
}

/// The issue's repo (#1345): a committed `.vscode/settings.json` hides
/// `**/generated` from files and `**/tests/fixtures` from search, and
/// `parse_order` has one real hit, six fixture hits and one generated one.
fn app_with_vscode_excludes() -> (App, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    for dir in [".vscode", "src", "tests/fixtures", "generated"] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
    }
    std::fs::write(
        root.join(".vscode/settings.json"),
        r#"{ "files.exclude": { "**/generated": true },
  "search.exclude": { "**/tests/fixtures": true } }"#,
    )
    .unwrap();
    std::fs::write(root.join("src/orders.py"), "def parse_order(x): return x\n").unwrap();
    for i in 1..=6 {
        std::fs::write(
            root.join(format!("tests/fixtures/case{i}.txt")),
            format!("parse_order fixture {i}\n"),
        )
        .unwrap();
    }
    std::fs::write(
        root.join("generated/api_pb2.py"),
        "# parse_order generated stub\n",
    )
    .unwrap();
    let app = App::new(root.to_path_buf()).unwrap();
    (app, tmp)
}

/// Run the Search panel's query to completion; the hit paths, relative
/// and sorted.
fn search_hit_paths(app: &mut App, root: &std::path::Path, query: &str) -> Vec<String> {
    app.search.query = String::from(query);
    app.submit_search_query();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !app.search.complete && std::time::Instant::now() < deadline {
        app.drain_search_results();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(app.search.complete, "the search must finish");
    let mut paths: Vec<String> = app
        .search
        .hits
        .iter()
        .map(|h| {
            h.path
                .strip_prefix(root)
                .unwrap_or(&h.path)
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    paths.sort();
    paths
}

/// Open a file into the focused editor group of a fresh App.
fn app_with_open_file(tmp: &std::path::Path, name: &str, body: &str) -> App {
    let f = tmp.join(name);
    std::fs::write(&f, body).unwrap();
    let mut app = App::new(tmp.to_path_buf()).unwrap();
    app.editor.open_pinned(&f).unwrap();
    app.focus_pane(Pane::Editor);
    app
}

// ---- A large file's outline is parsed off the UI thread (#1150) ----

/// A Python module of `defs` functions: past the inline-parse limit when
/// `defs` is large.
fn python_module(defs: usize) -> String {
    (0..defs)
        .map(|i| format!("def f{i}(x):\n    return x + {i}\n"))
        .collect()
}

/// What `sync_lsp` records after an edit, without starting a server: the
/// outline keys on it.
fn note_edit_for_outline(app: &mut App) {
    let path = app.editor.path.clone().unwrap();
    app.lsp_last_seen.insert(path, app.editor.edit_seq);
}

fn diag(
    start_char: u32,
    end_char: u32,
    severity: crate::lsp::manager::DiagnosticSeverity,
) -> crate::lsp::manager::Diagnostic {
    crate::lsp::manager::Diagnostic {
        start_line: 0,
        start_char,
        end_line: 0,
        end_char,
        severity,
        message: String::from("test"),
        tags: Default::default(),
        code: None,
    }
}

/// Run a typed block line through a real `sh` in `cwd`, as a pane would.
fn run_typed_line(line: &str, cwd: &std::path::Path) -> String {
    let out = std::process::Command::new("sh")
        .args(["-c", line.trim_end_matches('\r')])
        .current_dir(cwd)
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap() + &String::from_utf8(out.stderr).unwrap()
}

// ---------------------------------------------------------------------------
// On-screen keyboard (Termux touch typing)
// ---------------------------------------------------------------------------

fn left_click(app: &mut App, column: u16, row: u16) {
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::empty(),
    });
}

fn draw(app: &mut App, w: u16, h: u16) {
    let backend = ratatui::backend::TestBackend::new(w, h);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
}

/// Render the app at 140x50 and clone the buffer for color probing.
fn render_buf(app: &mut App) -> ratatui::buffer::Buffer {
    let backend = ratatui::backend::TestBackend::new(140, 50);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    term.backend().buffer().clone()
}

fn count_bg(buf: &ratatui::buffer::Buffer, c: Color) -> usize {
    let a = buf.area;
    (a.y..a.y + a.height)
        .flat_map(|y| (a.x..a.x + a.width).map(move |x| (x, y)))
        .filter(|&(x, y)| buf[(x, y)].bg == c)
        .count()
}

fn zen_app() -> (tempfile::TempDir, App) {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("t.py");
    std::fs::write(&p, "x = 1\ny = 2\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&p).unwrap();
    app.focus = Pane::Editor;
    (tmp, app)
}

fn sym(
    name: &str,
    kind: crate::lsp::manager::OutlineKind,
    depth: u16,
    start: u32,
    end: u32,
) -> crate::lsp::manager::OutlineSymbol {
    crate::lsp::manager::OutlineSymbol {
        name: name.to_string(),
        detail: None,
        kind,
        depth,
        line: start,
        character: 0,
        range_start_line: start,
        range_end_line: end,
        range_start_character: 0,
        range_end_character: u32::MAX,
    }
}

/// The issue's file (#1558), open in the editor.
fn occurrences_app(tmp: &Path) -> App {
    app_with_open_file(tmp, "s.py", "foo = 1\nbar = foo\nfoo += 2\nfood = 3")
}

fn select(app: &mut App, anchor: (usize, usize), head: (usize, usize)) {
    app.editor.selection = Some(crate::widgets::editor::EditorSelection { anchor, head });
    (app.editor.cursor_row, app.editor.cursor_col) = head;
}

fn cmd_shift_l(app: &mut App) {
    app.handle_key(key(
        KeyCode::Char('l'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ))
    .unwrap();
}

fn type_char(app: &mut App, c: char) {
    app.handle_key(key(KeyCode::Char(c), KeyModifiers::NONE))
        .unwrap();
}

/// `src/f1.rs`, `src/f2.rs` (open), `src/f3.rs` and an empty `src/sub/`,
/// drawn so the breadcrumb bar `src › f2.rs` is on screen (#1608).
fn app_on_src_f2(f2: &str) -> (App, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(src.join("sub")).unwrap();
    for f in ["f1.rs", "f3.rs"] {
        std::fs::write(src.join(f), format!("// {f}\n")).unwrap();
    }
    std::fs::write(src.join("f2.rs"), f2).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&src.join("f2.rs")).unwrap();
    app.focus_pane(Pane::Editor);
    draw(&mut app, 100, 30);
    (app, tmp)
}

/// Click crumb `i` of the drawn breadcrumb bar.
fn click_crumb(app: &mut App, i: usize) {
    let (x, below) = app.editor.breadcrumb_anchor(i).expect("crumb on screen");
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, below - 1));
}

/// An app on a workspace whose `.vscode/settings.json` is `vscode`, with
/// the merged settings applied, as a launch there applies them (#1440).
fn app_with_vscode_settings(vscode: &str) -> (tempfile::TempDir, App) {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".vscode")).unwrap();
    std::fs::write(tmp.path().join(".vscode/settings.json"), vscode).unwrap();
    for (name, body) in [
        ("m.c", "int x;\n"),
        ("m.py", "x=1\n"),
        ("README.md", "# r\n"),
    ] {
        std::fs::write(tmp.path().join(name), body).unwrap();
    }
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let merged = app.merged_settings_here();
    app.apply_merged_settings(&merged.prefs);
    (tmp, app)
}

// --- In-editor Find & Replace ---------------------------------------------

/// The Cmd+Opt+F chord as crossterm decodes the forwarded CSI-u sequence
/// (ALT|SUPER on macOS; Ctrl+Alt+F arrives as ALT|CONTROL on Linux).
fn replace_chord() -> KeyEvent {
    key(KeyCode::Char('f'), KeyModifiers::ALT | KeyModifiers::SUPER)
}

/// The issue's file (#1577): three functions, each using `x` twice.
const FIND_IN_SELECTION_SRC: &str =
    "def a(x):\n    return x + 1\n\ndef b(x):\n    return x * 2\n\ndef c(x):\n    return x - 3";

/// `f.py` open with `def b` (lines 4-5) selected and the replace bar open
/// on the query `x`.
fn find_in_selection_app(tmp: &Path) -> App {
    let mut app = app_with_open_file(tmp, "f.py", FIND_IN_SELECTION_SRC);
    app.editor.selection = Some(crate::widgets::editor::EditorSelection {
        anchor: (3, 0),
        head: (5, 0),
    });
    (app.editor.cursor_row, app.editor.cursor_col) = (5, 0);
    app.handle_key(replace_chord()).unwrap();
    app.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE))
        .unwrap();
    app
}

fn alt_l(app: &mut App) {
    app.handle_key(key(KeyCode::Char('l'), KeyModifiers::ALT))
        .unwrap();
}

fn find_count(app: &App) -> usize {
    app.editor_find
        .as_ref()
        .expect("the bar is open")
        .match_count
}

// --- Merge conflict resolution ---------------------------------------------

const CONFLICTED_BODY: &str =
    "fn main() {\n<<<<<<< HEAD\n    ours();\n=======\n    theirs();\n>>>>>>> feature\n}";

// --- Auto Save (files.autoSave: afterDelay) --------------------------------

/// Rewind a buffer's last-edit stamp so the auto-save delay has elapsed.
fn age_last_edit(e: &mut crate::widgets::editor::Editor) {
    e.last_edit_at = std::time::Instant::now().checked_sub(std::time::Duration::from_secs(5));
}

/// Cmd+P, then each character of `typed` as a keystroke.
fn quick_open_typing(app: &mut App, typed: &str) {
    app.handle_key(key(KeyCode::Char('p'), KeyModifiers::SUPER))
        .unwrap();
    assert!(app.file_finder.is_some(), "Cmd+P opens Quick Open");
    for c in typed.chars() {
        app.handle_key(key(KeyCode::Char(c), KeyModifiers::NONE))
            .unwrap();
    }
}

/// A local-provider seat for tests: connects to the workspace relay but
/// spawns no child process (the endpoint is never contacted until a turn).
fn local_test_spawn(cfg: &crate::pair::PairConfig) -> anyhow::Result<crate::pair_host::PairHost> {
    crate::pair_host::PairHost::spawn(crate::pair::PairConfig {
        socket: cfg.socket.clone(),
        workspace: cfg.workspace.clone(),
        name: cfg.name.clone(),
        model: Some(String::from("test-model")),
        task: None,
        provider: crate::pair::Provider::Local {
            base_url: String::from("http://127.0.0.1:9"),
        },
    })
}

/// An owner croft with `f.txt` open on a fresh relay, and a pilot
/// stand-in seat with the file live (#1207).
fn owner_and_pilot_on(tmp: &tempfile::TempDir, text: &str) -> (App, crate::collab::CollabSession) {
    use std::time::{Duration, Instant};
    let file = tmp.path().join("f.txt");
    std::fs::write(&file, text).unwrap();
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
    let mut owner = App::new(tmp.path().to_path_buf()).unwrap();
    owner.collab_config = Some((socket.clone(), crate::collab::CollabRole::Owner));
    owner.open_file_at_launch(&file);
    let deadline = Instant::now() + Duration::from_secs(5);
    while owner.collab.is_none() {
        assert!(Instant::now() < deadline, "owner never joined the relay");
        owner.poll_collab();
        std::thread::sleep(Duration::from_millis(5));
    }
    let mut pilot = {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(ch) =
                crate::collab::CollabChannel::connect(&socket, crate::collab::CollabRole::Guest)
            {
                break crate::collab::CollabSession::new(ch, "pilot".into());
            }
            assert!(Instant::now() < deadline, "pilot never connected");
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    pilot.request_file("f.txt");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !pilot.is_live("f.txt") {
        assert!(Instant::now() < deadline, "the pilot never got the file");
        owner.poll_collab();
        pilot.poll(|_| None);
        std::thread::sleep(Duration::from_millis(5));
    }
    (owner, pilot)
}

/// Send the pilot's buffer as `text` and wait until the owner's tab shows
/// it, so each call lands on its own owner tick.
fn pilot_writes(owner: &mut App, pilot: &mut crate::collab::CollabSession, text: &str) {
    use std::time::{Duration, Instant};
    assert!(pilot.local_change("f.txt", text));
    // Let every op of this change reach the relay before the owner drains.
    std::thread::sleep(Duration::from_millis(100));
    let deadline = Instant::now() + Duration::from_secs(5);
    while owner.editor.lines.join("\n") != text {
        assert!(Instant::now() < deadline, "the owner never saw {text:?}");
        owner.poll_collab();
        pilot.poll(|_| None);
        std::thread::sleep(Duration::from_millis(5));
    }
}

// --- Preview navigation (PDF pages, spreadsheet rows, Markdown, text) -----
//
// Every tab type must stay navigable right after it is opened, from the
// keyboard and from the wheel. Opening a file from the Explorer used to leave
// focus on the tree, so arrows walked the file list and a PDF looked frozen on
// page 1; the wheel over a PDF or a spreadsheet did nothing at all.

/// A three-page PDF written inline, so the tests need no fixture file.
/// Poppler repairs the missing xref table, so exact offsets are unnecessary.
fn write_three_page_pdf(path: &std::path::Path) {
    let pdf = concat!(
        "%PDF-1.4\n",
        "1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n",
        "2 0 obj<</Type/Pages/Kids[3 0 R 4 0 R 5 0 R]/Count 3>>endobj\n",
        "3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 200]/Contents 6 0 R>>endobj\n",
        "4 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 200]/Contents 7 0 R>>endobj\n",
        "5 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 200]/Contents 8 0 R>>endobj\n",
        "6 0 obj<</Length 27>>stream\n0 0 1 rg 10 10 100 100 re f\nendstream endobj\n",
        "7 0 obj<</Length 27>>stream\n1 0 0 rg 10 10 100 100 re f\nendstream endobj\n",
        "8 0 obj<</Length 27>>stream\n0 1 0 rg 10 10 100 100 re f\nendstream endobj\n",
        "trailer<</Root 1 0 R/Size 9>>\n%%EOF\n",
    );
    std::fs::write(path, pdf).unwrap();
}

fn write_test_png(path: &std::path::Path) {
    let mut png = Vec::new();
    image::RgbaImage::from_pixel(8, 8, image::Rgba([10, 200, 30, 255]))
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .expect("encode test png");
    std::fs::write(path, &png).unwrap();
}

fn csv_body(rows: usize) -> String {
    let mut s = String::from("name,qty,price\n");
    for i in 0..rows {
        s.push_str(&format!("row{i},{i},{i}.50\n"));
    }
    s
}

/// Open `name` the way a user does: select it in the Explorer and press Enter.
fn open_from_explorer(app: &mut App, name: &str) {
    let idx = app
        .tree
        .nodes
        .iter()
        .position(|n| n.path.file_name().is_some_and(|f| f == name))
        .unwrap_or_else(|| panic!("{name} must be visible in the Explorer"));
    app.tree.selected = idx;
    app.focus_pane(Pane::Tree);
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
}

/// Give the editor pane a real rectangle so wheel events land inside it.
fn place_editor_pane(app: &mut App) -> (u16, u16) {
    let area = Rect {
        x: 20,
        y: 2,
        width: 60,
        height: 30,
    };
    app.editor.last_area = area;
    app.editor.last_full_area = area;
    app.editor.last_inner = Rect {
        x: area.x + 1,
        y: area.y + 2,
        width: area.width - 2,
        height: area.height - 3,
    };
    (area.x + 10, area.y + 10)
}

fn wheel(app: &mut App, kind: crossterm::event::MouseEventKind, col: u16, row: u16) {
    app.handle_mouse(mouse(kind, col, row));
}

fn require_pdf_backend() {
    assert!(
        crate::pdf::detect_backend().is_some(),
        "these tests need a PDF rasteriser (poppler's pdftoppm, or sips on macOS)"
    );
}

/// Click a file's row in the Explorer, the way a user opens a file with the
/// mouse. Returns nothing: the assertions live in the callers.
fn click_open_in_explorer(app: &mut App, name: &str) {
    app.tree.last_area = Rect {
        x: 4,
        y: 0,
        width: 32,
        height: 20,
    };
    app.tree.last_inner = Rect {
        x: 5,
        y: 1,
        width: 30,
        height: 18,
    };
    app.focus_pane(Pane::Tree);
    let idx = app
        .tree
        .nodes
        .iter()
        .position(|n| n.path.file_name().is_some_and(|f| f == name))
        .unwrap_or_else(|| panic!("{name} must be visible in the Explorer"));
    let row = app.tree.last_inner.y + idx as u16;
    let col = app.tree.last_inner.x + 2;
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), col, row));
    app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), col, row));
}

/// A 100x1 blue PNG: one cell tall at any pane geometry, so a scrolled
/// anchor changes only the placement row, never the baked cell size.
fn wide_short_png() -> Vec<u8> {
    let mut png = Vec::new();
    image::RgbaImage::from_pixel(100, 1, image::Rgba([0, 0, 255, 255]))
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .expect("encode test png");
    png
}

/// Stage an app with an inline image captured at the pane's bottom row and
/// the overlay freshly baked, on the given protocol.
fn app_with_baked_terminal_image(
    protocol: crate::iterm2_inline::InlineImageProtocol,
) -> (tempfile::TempDir, App) {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Terminal);
    app.inline_protocol = protocol;
    app.cell_pixel = Some((8, 16));
    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    // The pane owns a REAL shell, and its startup output arrives whenever the
    // machine gets round to it (#326). Every assertion built on this fixture
    // reads the picture's position, so a prompt landing after the bake scrolls
    // the grid out from under it and the test fails for a reason that has
    // nothing to do with what it is testing. Wait for the shell to go quiet
    // first: two consecutive idle reads, so a gap between two bursts is not
    // mistaken for the end of them.
    let mut quiet = 0;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while quiet < 2 && std::time::Instant::now() < deadline {
        let _ = app.terminals[0].take_dirty();
        std::thread::sleep(std::time::Duration::from_millis(60));
        if app.terminals[0].peek_pending_bytes() == 0 && !app.terminals[0].peek_dirty() {
            quiet += 1;
        } else {
            quiet = 0;
        }
    }
    // The deadline is a failure, not a fallback. Falling through it would hand
    // the caller exactly the unsynchronised fixture this wait exists to
    // prevent, and the resulting flake would look like the picture logic
    // rather than a shell that never settled.
    assert!(
        quiet >= 2,
        "the pane's shell never went quiet within 10s, so the fixture cannot bake against a stable grid"
    );
    let h = app.terminals[0].last_inner.height as usize;
    app.terminals[0].feed_bytes_for_test("\r\n".repeat(h * 2).as_bytes());
    app.terminals[0].push_image_for_test(wide_short_png());
    app.update_terminal_image_overlay();
    assert!(
        app.overlays.terminal_image.has_image(),
        "staging: the picture must have baked"
    );
    (tmp, app)
}

/// A repo with a real merge conflict in `shared.txt` (two blocks) produced
/// by an actual `git merge`, plus a clean second file.
fn conflicted_repo(tmp: &std::path::Path) {
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(tmp)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap()
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(
        tmp.join("shared.txt"),
        "alpha\nbase-one\nm1\nm2\nm3\nm4\nm5\nbase-two\nomega\n",
    )
    .unwrap();
    std::fs::write(tmp.join("clean.txt"), "untouched\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-qm", "base"]);
    git(&["checkout", "-q", "-b", "feature"]);
    std::fs::write(
        tmp.join("shared.txt"),
        "alpha\nfeature-one\nm1\nm2\nm3\nm4\nm5\nfeature-two\nomega\n",
    )
    .unwrap();
    git(&["commit", "-aqm", "feature edit"]);
    git(&["checkout", "-q", "main"]);
    std::fs::write(
        tmp.join("shared.txt"),
        "alpha\nmain-one\nm1\nm2\nm3\nm4\nm5\nmain-two\nomega\n",
    )
    .unwrap();
    git(&["commit", "-aqm", "main edit"]);
    let merge = git(&["merge", "feature"]);
    assert!(
        !merge.status.success(),
        "the merge must conflict for these tests"
    );
}

/// Wait for the git worker to deliver entries, then return the index of the
/// conflicted `shared.txt` entry in the Source Control panel.
/// Wait for the git worker - which spawns a real `git` per query - to report
/// the conflicted entry, and return its index.
///
/// The budget is load-scaled (#307). This helper is where
/// `accept_all_incoming_then_complete_merge_stages_the_resolved_file` was
/// actually failing under load: the untimed `git status --porcelain` at the
/// end of that test cannot time out at all, so the fixed 5s deadline here was
/// the only clock in the test, which puts it in the same family as the other
/// three rather than in a category of its own.
fn wait_for_conflicted_entry(app: &mut App) -> usize {
    app.set_sidebar_view(SidebarView::SourceControl);
    let mut found = None;
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(1000),
        "the git worker to report the conflicted entry",
        || {
            let _ = app.drain_git_responses();
            found = app
                .source_control
                .entries
                .iter()
                .position(|e| e.kind == crate::git::ChangeKind::Conflicted);
            found.is_some()
        },
    );
    found.expect("await_spawned returns only once the entry is present")
}

/// #1244: `gone.txt` deleted on one branch and modified on the other, mid
/// `git merge feature`. `deleted_on_feature` picks which side deleted it.
fn modify_delete_repo(tmp: &std::path::Path, deleted_on_feature: bool) {
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(tmp)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap()
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(tmp.join("gone.txt"), "x\ny\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-qm", "base"]);
    git(&["checkout", "-q", "-b", "feature"]);
    if deleted_on_feature {
        git(&["rm", "-q", "gone.txt"]);
        git(&["commit", "-qm", "delete"]);
    } else {
        std::fs::write(tmp.join("gone.txt"), "x\ny-feature\n").unwrap();
        git(&["commit", "-aqm", "edit"]);
    }
    git(&["checkout", "-q", "main"]);
    if deleted_on_feature {
        std::fs::write(tmp.join("gone.txt"), "x\ny-main\n").unwrap();
        git(&["commit", "-aqm", "edit"]);
    } else {
        git(&["rm", "-q", "gone.txt"]);
        git(&["commit", "-qm", "delete"]);
    }
    let merge = git(&["merge", "feature"]);
    assert!(!merge.status.success(), "modify/delete must conflict");
}

fn git_out(tmp: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(tmp)
        .args(args)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn open_modify_delete(tmp: &std::path::Path, deleted_on_feature: bool) -> App {
    modify_delete_repo(tmp, deleted_on_feature);
    let mut app = App::new(tmp.to_path_buf()).unwrap();
    let idx = wait_for_conflicted_entry_named(&mut app, "gone.txt");
    app.open_source_control_entry(idx);
    assert!(app.editor.merge.is_some(), "{}", app.status);
    app
}

fn wait_for_conflicted_entry_named(app: &mut App, name: &str) -> usize {
    app.set_sidebar_view(SidebarView::SourceControl);
    let mut found = None;
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(1000),
        "the git worker to report the conflicted entry",
        || {
            let _ = app.drain_git_responses();
            found = app.source_control.entries.iter().position(|e| {
                e.kind == crate::git::ChangeKind::Conflicted && e.path.ends_with(name)
            });
            found.is_some()
        },
    );
    found.unwrap()
}

/// A Rust buffer with the caret inside `value`, and the given keybindings.
fn lsp_nav_app(keymap: &str) -> (App, tempfile::TempDir) {
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
    app.editor.cursor_col = 9;
    app.keymap = crate::keymap::Keymap::from_json(keymap);
    (app, tmp)
}

fn linked_ts_app(dir: &std::path::Path) -> App {
    std::fs::write(
        dir.join("app.ts"),
        "import { add } from \"./math\";\nconsole.log(add(1, 2));\n",
    )
    .unwrap();
    let mut app = App::new(dir.to_path_buf()).unwrap();
    app.editor.open_pinned(&dir.join("app.ts")).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.cursor_row = 1;
    app.editor.cursor_col = 5;
    app
}

/// One UI wake: the caret has idled past the debounce and the tick runs.
/// A request it sends is answered `null`, as vtsls does outside a tag
/// pair, and drained. Returns the id of that request.
fn linked_wake(app: &mut App) -> Option<u64> {
    let sent = linked_tick_only(app);
    if let Some(id) = sent {
        answer_linked(app, id, Vec::new());
    }
    app.drain_lsp_linked_editing();
    sent
}

/// The tick alone, the answer left to the caller.
fn linked_tick_only(app: &mut App) -> Option<u64> {
    let before = app.linked_request.as_ref().map(|r| r.0);
    app.linked_observed_at = std::time::Instant::now() - std::time::Duration::from_secs(1);
    app.tick_linked_editing();
    app.linked_request
        .as_ref()
        .map(|r| r.0)
        .filter(|id| Some(*id) != before)
}

fn answer_linked(app: &App, id: u64, ranges: Vec<(u32, u32, u32, u32)>) {
    app.lsp.as_ref().unwrap().push_linked_editing_for_test(
        crate::lsp::manager::LinkedEditingResult {
            request_id: id,
            ranges,
        },
    );
}

/// A file with two functions, `fn b` the LAST thing in it (no trailing
/// newline), opened with the caret inside `fn b` and the symbol tab opened
/// from the palette.
fn app_with_symbol_tab_on_b(tmp: &tempfile::TempDir) -> (App, std::path::PathBuf) {
    let file = tmp.path().join("two.rs");
    std::fs::write(&file, "fn a() {\n    1\n}\nfn b() {\n    2\n}").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&file).unwrap();
    app.editor.cursor_row = 4;
    app.run_command(crate::widgets::command_palette::Command::OpenAsSymbolTab);
    (app, file)
}

fn painted(app: &mut App) -> String {
    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let mut all = String::new();
    for y in 0..30 {
        for x in 0..100 {
            all.push_str(term.backend().buffer()[(x, y)].symbol());
        }
        all.push('\n');
    }
    all
}

fn greet_to_welcome(
    file: &std::path::Path,
) -> Vec<(PathBuf, Vec<crate::widgets::editor::TextSpanEdit>)> {
    let edit = |row| crate::widgets::editor::TextSpanEdit {
        start: (row, 0),
        end: (row, 5),
        new_text: String::from("welcome"),
        utf16: false,
    };
    vec![(file.to_path_buf(), vec![edit(0), edit(1)])]
}

// ---- A late workspace edit never lands on text typed since (#1151) ----

/// `calc.srn` open in the editor, caret at its start, as in the issue.
fn app_with_calc_open(tmp: &tempfile::TempDir) -> (App, PathBuf) {
    let root = tmp.path().canonicalize().unwrap();
    let file = root.join("calc.srn");
    std::fs::write(&file, "total = 1\nprint(total)\ntotal += 2\n").unwrap();
    let mut app = App::new(root).unwrap();
    app.editor.open_pinned(&file).unwrap();
    app.focus = Pane::Editor;
    (app, file)
}

/// The server's rename of `total` to `sum` in calc.srn, computed against
/// the text the file held when the request went out.
fn rename_total_to_sum(file: &Path) -> Vec<(PathBuf, Vec<crate::widgets::editor::TextSpanEdit>)> {
    let span = |row, col| crate::widgets::editor::TextSpanEdit {
        start: (row, col),
        end: (row, col + 5),
        new_text: String::from("sum"),
        utf16: false,
    };
    vec![(file.to_path_buf(), vec![span(0, 0), span(1, 6), span(2, 0)])]
}

/// The issue's keystrokes while the server works: a `# header` line above.
fn type_a_header_line(app: &mut App) {
    app.handle_key(key(KeyCode::Home, KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Up, KeyModifiers::NONE))
        .unwrap();
    for c in "# header".chars() {
        app.handle_key(key(KeyCode::Char(c), KeyModifiers::NONE))
            .unwrap();
    }
}

// ---------------------------------------------------------------------------
// #313: collapsing terminal panes.
// ---------------------------------------------------------------------------

/// Split to `n` panes and render once at a size wide enough for every pane to
/// carry its full header strip.
fn app_with_terminal_panes(
    n: usize,
) -> (
    tempfile::TempDir,
    App,
    ratatui::Terminal<ratatui::backend::TestBackend>,
) {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    for _ in 1..n {
        app.split_terminal().unwrap();
    }
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 30)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    (tmp, app, term)
}

fn focused_terminal_app(dir: &std::path::Path) -> App {
    let mut app = App::new(dir.to_path_buf()).unwrap();
    app.focus_pane(Pane::Terminal);
    app.bottom_panel_tab = BottomPanelTab::Terminal;
    app.clipboard_reader = || Some(String::from("CLIP"));
    app
}

/// #471: no watcher-reported paths, for the poll-shaped refresh calls.
fn no_paths() -> std::collections::BTreeSet<std::path::PathBuf> {
    std::collections::BTreeSet::new()
}

/// #471 fixtures: a repo with one committed file, and the app rooted there.
fn repo_with_seed(text: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["config", "user.email", "a@b"],
        vec!["config", "user.name", "a"],
    ] {
        let st = std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(&args)
            .status()
            .unwrap();
        assert!(st.success(), "git {args:?}");
    }
    let f = root.join("seed.txt");
    std::fs::write(&f, text).unwrap();
    git_ok(&root, &["add", "."]);
    git_ok(&root, &["commit", "-m", "init", "--quiet"]);
    (tmp, f)
}

fn git_ok(root: &std::path::Path, args: &[&str]) {
    let st = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .status()
        .unwrap();
    assert!(st.success(), "git {args:?}");
}

/// Write `text` to `path` so that its `(mtime, len)` stamp differs from the
/// one before the write: a stamp-gated refresh must not miss a rewrite that
/// lands within the filesystem's mtime granularity of the previous one. A
/// different length differs at once; an equal length waits for the clock.
fn rewrite_later(path: &std::path::Path, text: &str) {
    let stamp = |p: &std::path::Path| {
        std::fs::metadata(p)
            .ok()
            .and_then(|m| Some((m.modified().ok()?, m.len())))
    };
    let before = stamp(path);
    std::fs::write(path, text).unwrap();
    let mut spins = 0;
    while stamp(path) == before && spins < 400 {
        std::thread::sleep(std::time::Duration::from_millis(5));
        std::fs::write(path, text).unwrap();
        spins += 1;
    }
    assert_ne!(
        stamp(path),
        before,
        "the rewrite must move the file's stamp"
    );
}

/// A committed git repo at `dir` for the worktree-lane tests (#348): a lane
/// is a sibling of the repo, so callers put the repo one level inside their
/// tempdir to keep the lane inside it too.
fn init_lane_repo(dir: &std::path::Path) {
    std::fs::create_dir_all(dir).unwrap();
    let git = |args: &[&str]| {
        let st = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .unwrap();
        assert!(st.success(), "git {args:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.email", "a@b"]);
    git(&["config", "user.name", "a"]);
    std::fs::write(dir.join("seed.txt"), "one\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "init"]);
}

/// Queue an agent's `Edit` of `file` (`old` -> `new`) through the hook
/// socket, as Claude Code's hook would; the returned stream is the hook's
/// end, kept open for the answer.
fn queue_edit_proposal(
    app: &mut App,
    dir: &Path,
    file: &Path,
    old: &str,
    new: &str,
) -> std::os::unix::net::UnixStream {
    use std::io::Write;
    let sock = dir.join("hook.sock");
    let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
    listener.set_nonblocking(true).unwrap();
    app.hook_listener = Some(listener);
    let mut hook = std::os::unix::net::UnixStream::connect(&sock).unwrap();
    let req = crate::agent_hook::EditRequest {
        agent: "claude-code".into(),
        tool: "Edit".into(),
        input: serde_json::json!({"file_path": file, "old_string": old, "new_string": new}),
        cwd: dir.into(),
    };
    writeln!(hook, "{}", serde_json::to_string(&req).unwrap()).unwrap();
    assert!(app.drain_hook_requests());
    hook
}

/// Stand up a `croft view` listener on `app` without touching process env.
///
/// The real `bind_view_socket` also exports `CROFT_VIEW_SOCK`, which is
/// process-global: setting it in one test would publish a socket into every
/// other test running in the same process. This binds the same way and
/// publishes nothing, so the server half is exercised and the neighbours are
/// left alone.
fn seat_view_listener(app: &mut App, dir: &std::path::Path) -> std::path::PathBuf {
    let sock = dir.join("view-test.sock");
    // Deliberately the production helper: seating a hand-built listener here
    // would let `prepare_view_listener` lose its non-blocking mode without a
    // single test noticing.
    let listener = crate::app::prepare_view_listener(&sock).unwrap();
    app.view_listener = Some(listener);
    sock
}

/// Accept one client from a NON-blocking listener, waiting for it (#362).
///
/// `prepare_view_listener` makes the listener non-blocking on purpose, so a
/// single `accept()` after a fixed sleep asserts that the client thread was
/// scheduled inside that window - a property of machine load rather than of
/// the code under test. When it loses, the panic names the client rather than
/// the deadline, the blocking flag or the length cap the test is about.
fn accept_when_ready(
    listener: &std::os::unix::net::UnixListener,
) -> std::os::unix::net::UnixStream {
    // Load-scaled rather than a fresh constant (CONTRIBUTING.md, "Waiting on
    // a spawned process in a test"). What blows a flat five seconds here is
    // not the accept: it is every other test in the suite spawning a shell at
    // the same moment, which is contention no constant chosen inside this
    // test can know about.
    let budget = crate::test_budget::spawn_budget(crate::test_budget::tests::VIEW_ACCEPT_BASE);
    let started = std::time::Instant::now();
    loop {
        match listener.accept() {
            Ok((stream, _)) => return stream,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    started.elapsed() < budget,
                    "no client connected within {budget:?}"
                );
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(e) => panic!("accept failed: {e}"),
        }
    }
}

/// Send one request from a client thread and hand back its reply.
fn view_from_a_pane(
    sock: &std::path::Path,
    path: &std::path::Path,
) -> std::thread::JoinHandle<crate::view_ipc::ViewReply> {
    let sock = sock.to_path_buf();
    let path = path.to_path_buf();
    std::thread::spawn(move || {
        crate::view_ipc::send(&sock, &crate::view_ipc::ViewRequest::new(&path)).unwrap()
    })
}

/// Drain until the request lands: the client connects on another thread, so
/// the first non-blocking accept can legitimately find nothing yet.
///
/// The budget is the shared load-scaled one rather than a fixed two seconds
/// (CONTRIBUTING.md, "Waiting on a spawned process in a test"): `App::new`
/// has already spawned a PTY shell, a git worker and an FS watcher, and the
/// client is a thread racing all of them. Panics rather than returning false,
/// so a caller cannot turn a timeout into a bare `assert!` that says nothing
/// about which of the two happened.
fn drain_until_answered(
    app: &mut App,
    client: &std::thread::JoinHandle<crate::view_ipc::ViewReply>,
) {
    crate::test_budget::await_spawned(
        crate::test_budget::tests::VIEW_DRAIN_BASE,
        "the view request to be answered",
        || {
            app.drain_view_requests();
            client.is_finished()
        },
    );
}

/// A stand-in adapter that emits one DAP `terminated` event and then idles,
/// or (with `ends: false`) never says anything. Real processes, so the test
/// drives the same transport and `poll` the app does.
fn stub_member(ends: bool) -> crate::dap::session::DapSession {
    let script = if ends {
        "printf 'Content-Length: %d\r\n\r\n%s' 45 '{\"seq\":1,\"type\":\"event\",\"event\":\"terminated\"}'; sleep 30"
    } else {
        "sleep 30"
    };
    crate::dap::session::DapSession::launch_with(
        "sh",
        &[String::from("-c"), String::from(script)],
        std::path::Path::new("."),
        serde_json::json!({"seq": 2, "type": "request", "command": "launch", "arguments": {}}),
        std::collections::BTreeMap::new(),
    )
    .expect("sh spawns")
}

/// A workspace with two source files and breakpoints set in both (#250).
fn app_with_breakpoints() -> (tempfile::TempDir, App, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.rs");
    let b = tmp.path().join("b.rs");
    std::fs::write(&a, "fn a() {}\nfn b() {}\nfn c() {}\n").unwrap();
    std::fs::write(&b, "x\ny\nz\nw\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor
        .breakpoints
        .entry(b.clone())
        .or_default()
        .insert(4);
    app.editor
        .breakpoints
        .entry(a.clone())
        .or_default()
        .extend([3, 1]);
    app.editor
        .breakpoint_conditions
        .entry(a.clone())
        .or_default()
        .insert(3, String::from("n > 2"));
    app.editor
        .breakpoint_logs
        .entry(b.clone())
        .or_default()
        .insert(4, String::from("at {i}"));
    (tmp, app, a, b)
}

/// Poll until the set shrinks below `from` members or two seconds pass.
fn poll_until_shrinks(app: &mut App, from: usize) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while app.debug_sessions.len() >= from && std::time::Instant::now() < deadline {
        app.poll_dap();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// A workspace whose tasks.json declares a `cleanup` task (#250).
fn app_with_cleanup_task() -> (tempfile::TempDir, App) {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".vscode")).unwrap();
    std::fs::write(
        tmp.path().join(".vscode/tasks.json"),
        r#"{ "version": "2.0.0", "tasks": [
            { "label": "cleanup", "type": "shell", "command": "true" }
        ]}"#,
    )
    .unwrap();
    let app = App::new(tmp.path().to_path_buf()).unwrap();
    (tmp, app)
}

fn last_task_label(app: &App) -> Option<String> {
    app.last_task.as_ref().map(|t| t.label.clone())
}

// ---- Local history follows Explorer renames and moves (#1246) ----

fn app_with_history(root: &std::path::Path) -> (App, tempfile::TempDir) {
    let hist = tempfile::tempdir().unwrap();
    let mut app = App::new(root.to_path_buf()).unwrap();
    app.history_root = hist.path().to_path_buf();
    (app, hist)
}

// ---- Code lenses (#608) ----

fn lens_at(line: usize, text: &str, title: &str, command: &str) -> crate::code_lens::EditorLens {
    crate::code_lens::EditorLens {
        line,
        line_text: text.to_string(),
        title: title.to_string(),
        command: Some(command.to_string()),
        arguments: Vec::new(),
        server_side: false,
    }
}

/// A repo whose `a.txt` grows by one line per commit: v1, then v1+v2, then
/// v1+v2+v3.
fn scrub_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(args)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.email", "a@b"]);
    git(&["config", "user.name", "a"]);
    let mut text = String::new();
    for v in ["v1", "v2", "v3"] {
        text.push_str(v);
        text.push('\n');
        std::fs::write(tmp.path().join("a.txt"), &text).unwrap();
        git(&["add", "a.txt"]);
        git(&["commit", "-q", "-m", v]);
    }
    tmp
}

/// An app with CodeQL added, as Extensions › AVAILABLE › Add leaves it:
/// it is not built in (#1842), and the CodeQL tests drive its side bar
/// and commands.
fn codeql_app(root: PathBuf) -> App {
    let mut app = App::new(root).unwrap();
    app.installed_extensions.insert(String::from("codeql"));
    app
}

/// A workspace holding one CodeQL test, `test/Find/Find.qlref`, listed in
/// the Testing tree, and an App whose `codeql` is a stand-in: each run
/// prints what [`CodeqlStandIn::will`] last set (`@ROOT@` standing for the
/// workspace root) and exits with its code.
#[cfg(unix)]
struct CodeqlStandIn {
    app: App,
    root: String,
    out: std::path::PathBuf,
    code: std::path::PathBuf,
    _dirs: [tempfile::TempDir; 2],
}

/// The stand-in's report of `Find.qlref` passing.
#[cfg(unix)]
const STAND_IN_PASS: &str = "[1/1 comp 1s eval 9ms] PASSED @ROOT@/test/Find/Find.qlref\n";

#[cfg(unix)]
impl CodeqlStandIn {
    fn new() -> Self {
        use crate::testing::model::{TestCase, TestStatus};
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("test/Find")).unwrap();
        std::fs::write(root.join("test/qlpack.yml"), "name: acme/tests\ntests: .\n").unwrap();
        std::fs::write(root.join("test/Find/Find.qlref"), "Find.ql\n").unwrap();
        std::fs::write(root.join("test/Find/Find.expected"), "").unwrap();
        // Each run rewrites `out` and `code`, never the program, which may
        // still be starting up.
        let bin = tempfile::tempdir().unwrap();
        let (out, code) = (bin.path().join("out"), bin.path().join("code"));
        let program = bin.path().join("codeql");
        std::fs::write(
            &program,
            format!(
                "#!/bin/sh\ncat '{}'\nexit \"$(cat '{}')\"\n",
                out.display(),
                code.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut app = codeql_app(root.clone());
        app.set_codeql_program(program);
        app.testing.apply_case(TestCase {
            name: "test/Find::Find.qlref".into(),
            status: TestStatus::NotRun,
        });
        Self {
            app,
            root: root.display().to_string(),
            out,
            code,
            _dirs: [tmp, bin],
        }
    }

    /// What the next run prints, and the code it exits with.
    fn will(&self, stdout: &str, exit: i32) {
        std::fs::write(&self.out, stdout.replace("@ROOT@", &self.root)).unwrap();
        std::fs::write(&self.code, exit.to_string()).unwrap();
    }

    /// Wait for the run in flight to end, feeding its results to the app
    /// in the main loop's order. A run-all only turns the panel busy when
    /// the worker's start lands, so the end is also the run's "Running …"
    /// status being settled.
    fn finish(&mut self) {
        let app = &mut self.app;
        crate::test_budget::await_spawned(
            std::time::Duration::from_millis(500),
            "the stand-in test run to finish",
            || {
                let _ = app.test_worker.drain(&mut app.testing);
                if app.testing.take_refusal() {
                    app.status = String::from(crate::testing::NO_RUNNER_STATUS);
                }
                app.sync_coverage();
                app.tick_test_watch();
                !app.testing.is_busy() && app.test_run_status.is_none()
            },
        );
    }
}

/// A task that runs until the test ends it: its `sleep` outlasts any
/// test, so the only finish its pane reports is the one a test injects.
fn long_task(label: &str) -> crate::tasks::Task {
    crate::tasks::Task {
        label: label.to_string(),
        command: String::from("sleep 600"),
        source: String::from("test"),
        is_build: false,
        is_default: false,
        problem_matcher: None,
        is_background: false,
        vscode: None,
    }
}

/// End the command running in pane `uid` as its shell reports it (the
/// `133;D` mark: exit code and duration), and let the app drain the pane.
fn finish_pane_command(app: &mut App, uid: u64, exit: Option<i32>, millis: u64) {
    app.terminals
        .iter()
        .find(|t| t.uid() == uid)
        .expect("the pane is still open")
        .finish_command_for_test(exit, std::time::Duration::from_millis(millis));
    app.drain_terminal_bells();
}

/// A workspace with one source file and a SARIF log pointing into it.
fn sarif_fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("src")).unwrap();
    std::fs::write(
        tmp.path().join("src/a.rs"),
        "fn main() {\n    let q = format!(\"{}\", id);\n}\n",
    )
    .unwrap();
    let log = tmp.path().join("results.sarif");
    std::fs::write(
        &log,
        r#"{"version":"2.1.0","runs":[{"tool":{"driver":{"name":"lint","rules":[{"id":"R1","name":"Taint"}]}},
            "results":[{"ruleId":"R1","level":"error","message":{"text":"Query built from user input."},
            "locations":[{"physicalLocation":{"artifactLocation":{"uri":"src/a.rs"},
            "region":{"startLine":2,"startColumn":9}}}]}]}]}"#,
    )
    .unwrap();
    (tmp, log)
}

fn screen_text(term: &ratatui::Terminal<ratatui::backend::TestBackend>) -> String {
    let buf = term.backend().buffer();
    let mut all = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            all.push_str(buf[(x, y)].symbol());
        }
        all.push('\n');
    }
    all
}

/// A stand-in `codeql`: logs its arguments, writes `body` to the path its
/// `--output=` names, and exits with `code` (printing `err` on stderr).
#[cfg(unix)]
fn fake_codeql(dir: &std::path::Path, body: &str, code: i32, err: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let reply = dir.join("codeql-reply");
    std::fs::write(&reply, body).unwrap();
    let script = format!(
        "#!/bin/sh\necho \"$*\" >> '{log}'\nfor a in \"$@\"; do case \"$a\" in --output=*) cp '{reply}' \"${{a#--output=}}\" ;; esac; done\n[ -n '{err}' ] && echo '{err}' >&2\nexit {code}\n",
        log = dir.join("codeql-calls.log").display(),
        reply = reply.display(),
    );
    let bin = dir.join("codeql");
    std::fs::write(&bin, script).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

/// Drain the query run until it lands, or fail after a few seconds.
fn wait_for_codeql(app: &mut App) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while app.codeql_run.is_some() {
        app.drain_codeql_run();
        assert!(
            std::time::Instant::now() < deadline,
            "the query run never finished"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// A workspace with `q.ql` open and database "app" current.
fn codeql_query_fixture(tmp: &std::path::Path, source: &str) -> App {
    let db = tmp.join("dbs/app");
    std::fs::create_dir_all(&db).unwrap();
    let store = crate::codeql_db::DatabaseStore {
        databases: vec![crate::codeql_db::DbEntry {
            name: String::from("app"),
            path: db,
            language: Some(String::from("rust")),
            added: 0,
            former_names: Vec::new(),
        }],
        current: Some(0),
        sort_by: None,
    };
    store.save(&App::codeql_db_store_path()).unwrap();
    let q = tmp.join("q.ql");
    std::fs::write(&q, source).unwrap();
    let mut app = codeql_app(tmp.to_path_buf());
    app.editor.open(&q).unwrap();
    app
}

/// A stand-in `codeql` whose `execute query-server2` speaks the query
/// server's framed JSON-RPC: it answers `registerDatabases`, and answers
/// `runQuery` with `result` (a JSON object), writing `bqrs` to the output
/// path first. Every request is logged to `requests.jsonl`; `bqrs decode`
/// copies `csv` to its output.
#[cfg(unix)]
fn fake_query_server(dir: &std::path::Path, result: &str, csv: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(dir.join("result.json"), result).unwrap();
    std::fs::write(dir.join("reply.csv"), csv).unwrap();
    let script = format!(
        r#"#!/usr/bin/env python3
import json, sys, shutil
d = {dir:?}
args = sys.argv[1:]
if args[:2] == ["execute", "query-server2"]:
    inp, out = sys.stdin.buffer, sys.stdout.buffer
    while True:
        n = None
        while True:
            line = inp.readline()
            if not line:
                sys.exit(0)
            line = line.strip()
            if not line and n is not None:
                break
            if line.lower().startswith(b"content-length:"):
                n = int(line.split(b":")[1])
        msg = json.loads(inp.read(n))
        with open(d + "/requests.jsonl", "a") as f:
            f.write(json.dumps(msg) + "\n")
        if msg["method"] == "evaluation/runQuery":
            open(msg["params"]["body"]["outputPath"], "w").write("bqrs")
            res = json.load(open(d + "/result.json"))
        else:
            res = {{}}
        body = json.dumps({{"jsonrpc": "2.0", "id": msg["id"], "result": res}}).encode()
        out.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
        out.flush()
for a in args:
    if a.startswith("--output="):
        shutil.copy(d + "/reply.csv", a[len("--output="):])
"#,
        dir = dir.display().to_string()
    );
    let program = dir.join("codeql");
    std::fs::write(&program, script).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    program
}

fn draw_screen(app: &mut App) -> String {
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 40)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    screen_text(&term)
}

/// Two results in one file and one in another, for navigation.
fn sarif_nav_fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.rs"), "l1\nl2\nl3\nl4\nl5\nl6\n").unwrap();
    std::fs::write(tmp.path().join("b.rs"), "m1\nm2\n").unwrap();
    let log = tmp.path().join("nav.sarif");
    let r = |uri: &str, line: u32, msg: &str| {
        format!(
            r#"{{"message":{{"text":"{msg}"}},"locations":[{{"physicalLocation":{{"artifactLocation":{{"uri":"{uri}"}},"region":{{"startLine":{line},"startColumn":1}}}}}}]}}"#
        )
    };
    std::fs::write(
        &log,
        format!(
            r#"{{"version":"2.1.0","runs":[{{"tool":{{"driver":{{"name":"lint"}}}},"results":[{},{},{}]}}]}}"#,
            r("a.rs", 2, "first"),
            r("a.rs", 5, "second"),
            r("b.rs", 1, "third")
        ),
    )
    .unwrap();
    (tmp, log)
}

/// A CodeQL run's SARIF (#1369): `web.py` relative to `%SRCROOT%`, which
/// only the database defines. Returns the log, after recording it in the
/// query history as the output of a run on `db`.
fn codeql_run_on(tmp: &std::path::Path, db: &std::path::Path) -> PathBuf {
    let log = tmp.join("results/run-1/results.sarif");
    std::fs::create_dir_all(log.parent().unwrap()).unwrap();
    std::fs::write(
        &log,
        r#"{"version":"2.1.0","runs":[{"tool":{"driver":{"name":"CodeQL"}},"results":[
            {"message":{"text":"execute call"},"locations":[{"physicalLocation":{"artifactLocation":{"uri":"web.py","uriBaseId":"%SRCROOT%"},"region":{"startLine":4,"startColumn":5}}}]}
        ]}]}"#,
    )
    .unwrap();
    let mut history = crate::codeql_query::History::default();
    history.push(crate::codeql_query::HistoryEntry {
        query: tmp.join("find_calls.ql"),
        database: String::from("db"),
        database_path: Some(db.to_path_buf()),
        started: 1,
        seconds: 3,
        status: crate::codeql_query::RunStatus::Succeeded,
        output: log.clone(),
        name: None,
        results: Some(1),
    });
    history.save(&App::codeql_history_path()).unwrap();
    log
}

const WEB_PY_1369: &str = "import sqlite3\n\ndef q(cur, name):\n    cur.execute(name)\n";

fn status_row(term: &ratatui::Terminal<ratatui::backend::TestBackend>) -> String {
    let buf = term.backend().buffer();
    let y = buf.area.height - 1;
    (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect()
}

/// A stub `gh` for the viewed-sync tests (#365): it logs its argv to
/// `gh-args`, answers `pr view` with PR x/y#42 (a.rs, b.rs, c.rs), and
/// runs `graphql` for everything else.
fn viewed_sync_gh(
    dir: &std::path::Path,
    graphql: &str,
) -> (std::path::PathBuf, std::path::PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let log = dir.join("gh-args");
    let gh = dir.join("gh");
    std::fs::write(
        &gh,
        format!(
            r#"#!/bin/sh
echo "$@" >> '{}'
case "$*" in
  "pr view"*) echo '{{"number": 42, "title": "t", "url": "https://github.com/x/y/pull/42", "author": {{"login": "a"}}, "files": [{{"path": "a.rs", "additions": 1, "deletions": 0, "changeType": "ADDED"}}, {{"path": "b.rs", "additions": 1, "deletions": 0, "changeType": "ADDED"}}, {{"path": "c.rs", "additions": 1, "deletions": 0, "changeType": "ADDED"}}], "statusCheckRollup": []}}' ;;
{graphql}
esac
"#,
            log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    (gh, log)
}

/// Wait until every queued viewed mutation has run.
fn settle_viewed_writes(app: &mut App) {
    crate::test_budget::await_spawned(
        std::time::Duration::from_secs(5),
        "the viewed mutations",
        || {
            app.poll_pr_gh();
            app.pr_viewed_queue.is_empty() && app.pr_viewed_write.is_none()
        },
    );
}

/// Assert the status bar and the Source Control panel each carry one line.
fn assert_one_line_858(app: &App) {
    let feedback = app
        .source_control
        .commit_feedback
        .clone()
        .unwrap_or_default();
    for text in [&app.status, &feedback] {
        assert!(!text.contains('\n'), "one line: {text:?}");
        assert!(!text.contains("From "), "no fetch source: {text:?}");
        assert!(!text.contains("To "), "no push destination: {text:?}");
    }
}

/// Every cell of a drawn frame, row after row.
fn screen_text_863(app: &mut App, w: u16, h: u16) -> String {
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let buf = term.backend().buffer();
    let mut screen = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            screen.push_str(buf[(x, y)].symbol());
        }
        screen.push('\n');
    }
    screen
}

/// #1203: open `notes/plan.md`, type a line, then take its folder away the
/// way `git checkout` of a branch without it does.
fn app_with_plan_whose_folder_was_removed(tmp: &std::path::Path) -> App {
    std::fs::create_dir(tmp.join("notes")).unwrap();
    let mut app = app_with_open_file(tmp, "notes/plan.md", "# Plan\n\n- step one\n");
    app.editor.cursor_row = 2;
    app.editor.cursor_col = "- step one".len();
    app.editor.insert_str("\n- step two");
    std::fs::remove_dir_all(tmp.join("notes")).unwrap();
    app
}

/// Run File: Save As… the way the palette does and answer its prompt.
fn save_as(app: &mut App, answer: &str) {
    let cmd = crate::widgets::command_palette::Command::from_id("save_as")
        .expect("there is a File: Save As… command");
    app.run_command(cmd);
    let prompt = app.prompt.as_mut().expect("Save As asks for a path");
    prompt.buffer = answer.to_string();
    app.commit_prompt();
}

/// The modifier for File: New Untitled Text File: `Cmd` everywhere, and
/// `Ctrl` off macOS.
fn new_untitled_chords() -> Vec<KeyModifiers> {
    if cfg!(target_os = "macos") {
        vec![KeyModifiers::SUPER]
    } else {
        vec![KeyModifiers::CONTROL, KeyModifiers::SUPER]
    }
}

/// Run File: Revert File the way the palette does.
fn revert_file(app: &mut App) {
    let cmd = crate::widgets::command_palette::Command::from_id("revert_file")
        .expect("there is a File: Revert File command");
    app.run_command(cmd);
}

/// Types `text` into whatever overlay has the keyboard, one key at a time.
fn type_keys_852(app: &mut App, text: &str) {
    for c in text.chars() {
        app.handle_key(key(KeyCode::Char(c), KeyModifiers::NONE))
            .unwrap();
    }
}

/// Opens the Command Palette with `Cmd+Shift+P`, as a user would.
fn open_palette_852(app: &mut App) {
    app.handle_key(key(
        KeyCode::Char('p'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert!(
        app.command_palette.is_some(),
        "Cmd+Shift+P opens the palette"
    );
}

/// Renders `app` at `w`x`h` and returns the frame's buffer.
fn render_buffer_852(app: &mut App, w: u16, h: u16) -> ratatui::buffer::Buffer {
    let backend = ratatui::backend::TestBackend::new(w, h);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    term.backend().buffer().clone()
}

// #852 (comment 3): the editor-tab actions (the tab menu's Close Others,
// Close to the Right, Close Saved, Close All, Pin / Unpin and Keep Open) are
// palette commands under VS Code's names, so they can be searched, bound in
// keybindings.json, and run on Linux without Super.

/// An app with `names` open as kept (non-preview) tabs in one group, the
/// last one active and focused. Auto-save is off so an edited tab stays
/// dirty.
fn tabs_app_852(names: &[&str]) -> (App, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.auto_save = false;
    app.auto_save_on_focus_change = false;
    for name in names {
        let p = tmp.path().join(name);
        std::fs::write(&p, "x\n").unwrap();
        app.editor.open_pinned(&p).unwrap();
    }
    app.focus_pane(Pane::Editor);
    (app, tmp)
}

/// A group's tabs by file name, in strip order.
fn tab_names_852(tabs: &crate::widgets::editor::EditorTabs) -> Vec<String> {
    tabs.editors
        .iter()
        .map(|e| {
            e.path
                .as_deref()
                .and_then(Path::file_name)
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
        .collect()
}

/// Runs `query` from the palette as a user does: `Cmd+Shift+P`, type it,
/// Enter. Returns the title of the row Enter ran, `None` when none matched.
fn run_in_palette_852(app: &mut App, query: &str) -> Option<String> {
    open_palette_852(app);
    type_keys_852(app, query);
    let ran = app
        .command_palette
        .as_ref()
        .and_then(|p| p.selected_item())
        .map(|item| item.title().to_string());
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    ran
}

/// Makes the active tab dirty the way a user does, by typing into it.
fn type_into_active_tab_852(app: &mut App) {
    app.focus_pane(Pane::Editor);
    app.handle_key(key(KeyCode::Char('Z'), KeyModifiers::NONE))
        .unwrap();
    assert!(
        app.editor.dirty,
        "fixture: the active tab has unsaved edits"
    );
}

/// A split whose right (active) group is `[b.rs (pinned), c.rs, p.rs
/// (preview), d.rs (unsaved), e.rs]` with `active` selected, beside a left
/// group `[a.rs, b.rs]`.
fn split_tabs_app_852(active: &str) -> (App, tempfile::TempDir) {
    let (mut app, tmp) = tabs_app_852(&["a.rs", "b.rs"]);
    app.split_editor();
    assert!(app.editor_layout.is_split(), "fixture: two groups");
    for name in ["c.rs", "d.rs", "e.rs"] {
        let p = tmp.path().join(name);
        std::fs::write(&p, "x\n").unwrap();
        app.editor.open_pinned(&p).unwrap();
    }
    app.editor.toggle_pin(0);
    app.editor.select(1);
    let p = tmp.path().join("p.rs");
    std::fs::write(&p, "x\n").unwrap();
    app.editor.open_preview(&p).unwrap();
    app.editor.select(3);
    type_into_active_tab_852(&mut app);
    assert_eq!(
        tab_names_852(&app.editor),
        ["b.rs", "c.rs", "p.rs", "d.rs", "e.rs"],
        "fixture"
    );
    let idx = tab_names_852(&app.editor)
        .iter()
        .position(|n| n == active)
        .unwrap();
    app.editor.select(idx);
    app.status.clear();
    (app, tmp)
}

/// Every group's tabs as (name, pinned, preview, dirty), the active group
/// first, then the active index, whether the layout is split, the status
/// line and the reopen stack (by file name: each fixture has its own
/// directory).
type TabsSnapshot852 = (
    Vec<Vec<(String, bool, bool, bool)>>,
    usize,
    bool,
    String,
    Vec<String>,
);

fn tabs_snapshot_852(app: &App) -> TabsSnapshot852 {
    let group = |tabs: &crate::widgets::editor::EditorTabs| {
        tab_names_852(tabs)
            .into_iter()
            .zip(&tabs.editors)
            .map(|(n, e)| (n, e.pinned, e.preview, e.dirty))
            .collect::<Vec<_>>()
    };
    let mut groups = vec![group(&app.editor)];
    groups.extend(app.editor_layout.inactive_groups().into_iter().map(group));
    (
        groups,
        app.editor.active_index(),
        app.editor_layout.is_split(),
        app.status.clone(),
        app.closed_tabs
            .iter()
            .map(|t| t.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect(),
    )
}

// ---- Multi-line paste into a terminal without bracketed paste (#1274) ----

/// An app whose active pane runs a program that never turns on bracketed
/// paste (`sh` reading a line), focused, with nothing written yet.
fn plain_paste_app() -> (App, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.terminals[0] = crate::widgets::terminal::PtyTerminal::new_running(
        "/bin/sh",
        &[String::from("-c"), String::from("sleep 30")],
        tmp.path(),
    )
    .unwrap();
    app.active_terminal = 0;
    app.focus_pane(Pane::Terminal);
    (app, tmp)
}

fn pasted(app: &App) -> String {
    String::from_utf8_lossy(&app.terminals[0].written_bytes_for_test()).into_owned()
}

const TWO_LINES: &str = "echo first-line\ntouch pasted-ran\n";

/// Two panes, the first active and maximized so each has a rail row,
/// drawn once so the rows are laid out.
fn two_pane_paste_app() -> (App, tempfile::TempDir) {
    let (mut app, tmp) = plain_paste_app();
    app.split_terminal().unwrap();
    app.active_terminal = 0;
    app.focus_pane(Pane::Terminal);
    app.terminal_pane_maximized = true;
    draw(&mut app, 180, 40);
    (app, tmp)
}

// --- #1215: keys that move the caret sideways scroll the view to it -----------

fn wide_line_app(tmp: &std::path::Path) -> (App, ratatui::Terminal<ratatui::backend::TestBackend>) {
    let line = format!(
        "start {} END",
        (0..120)
            .map(|i| format!("w{i:03}"))
            .collect::<Vec<_>>()
            .join(" ")
    );
    let mut app = app_with_open_file(tmp, "h300.txt", &format!("{line}\nshort\n"));
    app.focus_pane(Pane::Editor);
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
    term.draw(|frame| app.render(frame)).unwrap();
    (app, term)
}

// ---- Git diffs of files that aren't UTF-8 (#1242) ----

/// A repo with `name` committed as `head` bytes, then `head + added` on disk.
fn encoded_repo(name: &str, head: &[u8], added: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = make_committed_repo();
    let f = tmp.path().join(name);
    std::fs::write(&f, head).unwrap();
    git_stdout_in(tmp.path(), &["add", name]);
    git_stdout_in(tmp.path(), &["commit", "-qm", "encoded"]);
    std::fs::write(&f, [head, added].concat()).unwrap();
    (tmp, f)
}

const LATIN1_HEAD: &[u8] = b"name;city\nM\xfcller;K\xf6ln\nJos\xe9;M\xe1laga\n";
const LATIN1_ADDED: &[u8] = b"Zo\xeb;Z\xfcrich\n";

/// #910: `tax.py` open with an unsaved function, rewritten on disk behind
/// it (a `git checkout`, a restore, an agent), and the sweep that notices.
fn app_with_a_disk_conflict(tmp: &std::path::Path) -> (App, std::path::PathBuf) {
    let f = tmp.join("tax.py");
    std::fs::write(&f, "RATE = 0.1\n").unwrap();
    let mut app = App::new(tmp.to_path_buf()).unwrap();
    app.history_root = tmp.join(".history");
    app.editor.open_pinned(&f).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.cursor_row = 0;
    app.editor.cursor_col = "RATE = 0.1".len();
    app.editor
        .insert_str("\ndef tax(amount):\n    return amount * RATE");
    assert!(app.editor.dirty);
    std::fs::write(&f, "RATE = 0.2  # from the other branch\n").unwrap();
    app.reload_open_file_after_external_change();
    assert!(
        matches!(
            app.input_prompt.as_ref().map(|p| &p.purpose),
            Some(crate::widgets::input_prompt::InputPurpose::ReloadConflict { .. })
        ),
        "the conflict prompt opens"
    );
    (app, f)
}

fn type_keys(app: &mut App, text: &str) {
    for c in text.chars() {
        app.handle_key(key(KeyCode::Char(c), KeyModifiers::NONE))
            .unwrap();
    }
}

// ---- Find bar seeding selects the word under the caret (#1278) ----

/// orders.py from the issue, open and focused, caret at `(row, col)`.
fn find_seed_app(row: usize, col: usize) -> (App, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("orders.py");
    std::fs::write(
        &f,
        "def loadOrders(path):\n    return readRows(path)\n\n\ndef saveOrders(path, rows):\n    writeRows(path, rows)\n",
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&f).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.cursor_row = row;
    app.editor.cursor_col = col;
    (app, tmp)
}

fn ctrl(c: char) -> KeyEvent {
    key(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn type_str(app: &mut App, s: &str) {
    for c in s.chars() {
        app.handle_key(key(KeyCode::Char(c), KeyModifiers::NONE))
            .unwrap();
    }
}

fn find_query(app: &App) -> String {
    app.editor_find.as_ref().map(|s| s.query.clone()).unwrap()
}

/// An app with `n` symbols (`sym_000`, `sym_001`, …, symbol `i` on line
/// `2 * i`) in an open file, the Explorer's OUTLINE expanded and the
/// secondary side bar showing the same Outline (#1326).
fn secondary_outline_app(tmp: &tempfile::TempDir, n: u32) -> (App, std::path::PathBuf) {
    use crate::lsp::manager::{OutlineKind, OutlineSymbol};
    let f = tmp.path().join("m.py");
    let text: String = (0..n)
        .map(|i| format!("def sym_{i:03}(x):\n    return x\n"))
        .collect();
    std::fs::write(&f, text).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.explorer_views = Default::default();
    app.editor.open_pinned(&f).unwrap();
    let syms = (0..n)
        .map(|i| OutlineSymbol {
            name: format!("sym_{i:03}"),
            detail: None,
            kind: OutlineKind::Function,
            depth: 0,
            line: 2 * i,
            character: 4,
            range_start_line: 2 * i,
            range_start_character: 0,
            range_end_line: 2 * i + 1,
            range_end_character: 12,
        })
        .collect();
    app.outline.set_symbols(f.clone(), syms);
    app.outline.collapsed = false;
    app.secondary_side_bar_visible = true;
    (app, f)
}

/// The columns left of the editor (the Explorer) and right of it (the
/// secondary side bar), so a symbol name in the editor text never counts.
fn outline_hosts(app: &App) -> (std::ops::Range<u16>, std::ops::Range<u16>) {
    let e = app.editor.last_full_area;
    (0..e.x, e.x + e.width..e.x + e.width + 60)
}

/// Where `text` is painted, searching only columns `xs`.
fn find_painted(
    term: &ratatui::Terminal<ratatui::backend::TestBackend>,
    text: &str,
    xs: std::ops::Range<u16>,
) -> Option<(u16, u16)> {
    let buf = term.backend().buffer();
    (0..buf.area.height).find_map(|y| {
        let row: String = xs
            .clone()
            .filter_map(|x| buf.cell((x, y)).map(|c| c.symbol().to_string()))
            .collect();
        let at = row.find(text)?;
        Some((xs.start + row[..at].chars().count() as u16, y))
    })
}

/// A workspace whose `node_modules/.bin/tsc` logs its arguments and, run on
/// `tsconfig.app.json`, reports one error, the way tsc does.
#[cfg(unix)]
fn fake_tsc_workspace(tsconfig: &str) -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("tsconfig.json"), tsconfig).unwrap();
    std::fs::create_dir_all(tmp.path().join("src")).unwrap();
    std::fs::write(tmp.path().join("src/main.ts"), "const n: string = 1;\n").unwrap();
    let bin = tmp.path().join("node_modules/.bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(
        bin.join("tsc"),
        "#!/bin/sh\necho \"$*\" >> calls.log\ncase \"$*\" in *tsconfig.app.json*) \
         echo \"src/main.ts(1,7): error TS2322: Type 'number' is not assignable to type 'string'.\"; \
         exit 2;; esac\nexit 0\n",
    )
    .unwrap();
    std::fs::set_permissions(bin.join("tsc"), std::fs::Permissions::from_mode(0o755)).unwrap();
    tmp
}

#[cfg(unix)]
fn run_project_check(tmp: &tempfile::TempDir) -> (String, String) {
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.run_command(crate::widgets::command_palette::Command::ProblemsCheckProject);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while app.project_check_running && std::time::Instant::now() < deadline {
        if !app.drain_project_check() {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
    let calls = std::fs::read_to_string(tmp.path().join("calls.log")).unwrap_or_default();
    (app.status.clone(), calls)
}

/// `a.py` reading `value = 1`, open in the editor, then `00` typed at the
/// end of the line and saved: the issue's (#1568) setup.
fn app_with_a_saved_edit() -> (tempfile::TempDir, std::path::PathBuf, App) {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.py");
    std::fs::write(&a, "value = 1\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&a).unwrap();
    app.focus_pane(Pane::Editor);
    app.handle_editor_key(key(KeyCode::End, KeyModifiers::NONE));
    type_str(&mut app, "00");
    app.handle_key(ctrl('s')).unwrap();
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "value = 100\n");
    assert!(!app.editor.dirty);
    (tmp, a, app)
}

/// A repository whose `src/main.rs` is committed and on `origin`
/// (`git@github.com:acme/widgets.git`), opened in the editor (#1610).
fn app_on_a_pushed_github_file() -> (App, tempfile::TempDir, String) {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(p)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::create_dir(p.join("src")).unwrap();
    std::fs::write(
        p.join("src/main.rs"),
        "fn main() {\n    println!(\"hi\");\n    println!(\"there\");\n}\n",
    )
    .unwrap();
    git(&["add", "."]);
    git(&[
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "commit",
        "-qm",
        "init",
    ]);
    git(&["remote", "add", "origin", "git@github.com:acme/widgets.git"]);
    git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
    let sha = git(&["rev-parse", "HEAD"]);
    let mut app = App::new(p.to_path_buf()).unwrap();
    app.editor.open_pinned(&p.join("src/main.rs")).unwrap();
    app.focus_pane(Pane::Editor);
    (app, tmp, sha)
}

// --- #1565: rearranging editor tabs -------------------------------------------

/// An app with `a.py`, `b.py` and `c.py` open as kept tabs, `c.py` active.
fn three_tab_app_1565() -> (App, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.py", "a = 1\n");
    app.editor.pin_active();
    for name in ["b.py", "c.py"] {
        let p = tmp.path().join(name);
        std::fs::write(&p, "x = 1\n").unwrap();
        app.editor.open_pinned(&p).unwrap();
    }
    app.focus_pane(Pane::Editor);
    assert_eq!(tab_names_852(&app.editor), ["a.py", "b.py", "c.py"]);
    (app, tmp)
}

/// A file with one conflict block open in the merge editor, whose Result
/// (`top / mine / bottom` once Accept Current runs) differs from the disk.
fn app_with_merge_editor_open(dir: &std::path::Path) -> (App, std::path::PathBuf) {
    let file = dir.join("f.txt");
    std::fs::write(
        &file,
        "top\n<<<<<<< HEAD\nmine\n=======\nyours\n>>>>>>> other\nbottom\n",
    )
    .unwrap();
    let mut app = App::new(dir.to_path_buf()).unwrap();
    app.editor.open_pinned(&file).unwrap();
    app.focus_pane(Pane::Editor);
    app.run_command(crate::widgets::command_palette::Command::MergeOpenEditor);
    assert!(app.editor.merge.is_some(), "setup: the merge editor is up");
    app.run_command(crate::widgets::command_palette::Command::MergeAcceptCurrent);
    assert!(app.editor.dirty, "setup: the Result is unsaved");
    (app, file)
}
