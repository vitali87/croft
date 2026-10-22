//! Source control: git status, staging, commits, branches, push and pull,
//! rebase, blame, the timeline and the commit graph.

use super::*;

#[test]
fn cmd_shift_s_jumps_to_source_control_from_explorer() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.handle_key(key(
        KeyCode::Char('S'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert_eq!(
        app.sidebar_view,
        SidebarView::SourceControl,
        "Cmd+Shift+S must jump to Source Control, not Search"
    );
    assert!(
        matches!(app.focus, Pane::Tree),
        "Cmd+Shift+S focuses the Source Control panel"
    );
}

/// Inactive split groups render their editors, so their git gutters paint from
/// whatever baseline they hold. `sync_git_gutters` fetched baselines for the
/// ACTIVE group only, so a tab in the other pane painted bars against nothing
/// (or, after a commit, against a baseline that HEAD had moved past) and never
/// self-corrected: focusing it makes `git_baseline_for == path`, so the fetch
/// is skipped from then on.
#[test]
fn git_gutter_baselines_are_fetched_for_inactive_split_groups() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["config", "user.email", "a@b"],
        vec!["config", "user.name", "a"],
    ] {
        let _ = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(&args)
            .status();
    }
    let f = root.join("seed.txt");
    std::fs::write(&f, "one\ntwo\n").unwrap();
    for args in [vec!["add", "."], vec!["commit", "-m", "init", "--quiet"]] {
        let _ = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(&args)
            .status();
    }

    let mut app = App::new(root.to_path_buf()).unwrap();
    app.editor.open_pinned(&f).unwrap();
    app.split_editor(); // both groups now hold seed.txt; right is focused
    app.sync_git_gutters();

    let inactive_baselines: Vec<Option<std::path::PathBuf>> = app
        .editor_layout
        .inactive_groups()
        .into_iter()
        .flat_map(|g| g.editors.iter())
        .filter(|e| e.path.as_deref() == Some(f.as_path()))
        .map(|e| e.git_baseline_for.clone())
        .collect();
    assert!(
        !inactive_baselines.is_empty(),
        "the inactive group holds a tab on seed.txt"
    );
    for baseline in inactive_baselines {
        assert_eq!(
            baseline.as_deref(),
            Some(f.as_path()),
            "a tab in an inactive split group needs its git baseline too"
        );
    }
}

#[test]
fn clicking_an_scm_entry_below_a_stale_tree_last_area_still_selects_and_opens() {
    // User report: after the Source Control list got long enough to
    // reach below where the file tree had last rendered, the rows in
    // that lower band became unclickable. Root cause: the mouse
    // handler gated all sidebar-view click branches on `rect_contains
    // (self.tree.last_area, ...)`, but `tree.last_area` is only
    // refreshed when the Explorer view renders — switching to
    // Source Control freezes that rectangle, and any click below
    // its bottom edge fell through even though it was inside the
    // SCM panel.
    use crate::git::{ChangeEntry, ChangeKind};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // Pretend the Explorer view rendered into a tall pane; then the
    // user opened the terminal which shrank the side area; then they
    // switched to Source Control. Set tree.last_area to the OLD,
    // taller rectangle so we can prove the SCM click-handler is no
    // longer dependent on it.
    app.tree.last_area = Rect {
        x: 4,
        y: 1,
        width: 30,
        height: 6,
    };
    app.set_sidebar_view(SidebarView::SourceControl);
    // Set entries AFTER set_sidebar_view because that path calls
    // refresh_source_control which overwrites entries with the live
    // git query (empty in a non-repo temp dir).
    app.source_control.status.in_repo = true;
    app.source_control.status.branch = Some("main".into());
    app.source_control.entries = (0..15)
        .map(|i| ChangeEntry {
            path: format!("file{i}.py"),
            kind: ChangeKind::Untracked,
            ..Default::default()
        })
        .collect();

    let backend = ratatui::backend::TestBackend::new(80, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();

    // Pick the y of a row that lives BELOW tree.last_area's bottom
    // but inside the SCM list. Walk the rendered list area to find
    // one.
    let stale_bottom = app.tree.last_area.y + app.tree.last_area.height;
    let list_area = app.source_control.last_list_area;
    assert!(
        list_area.y + list_area.height > stale_bottom,
        "test setup must put SCM rows below the stale tree.last_area"
    );
    // Walk from stale_bottom down looking for a row that maps to an
    // entry (skip section-header rows).
    let target_row = (stale_bottom..list_area.y + list_area.height)
        .find(|y| app.source_control.entry_at_y(*y).is_some())
        .expect("at least one entry must render below the stale tree band");

    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: list_area.x + 4,
        row: target_row,
        modifiers: KeyModifiers::NONE,
    });

    assert!(
        app.source_control.selected_change.is_some(),
        "click on an SCM entry below the stale tree.last_area must still select it"
    );
}

#[test]
fn click_on_a_deleted_source_control_entry_opens_unified_diff_with_head_lines_all_removed() {
    // A file that's tracked in HEAD but missing from the working
    // tree (or staged for deletion) used to leave the editor empty
    // when clicked, because `open_source_control_entry` fell into
    // the generic open-from-disk branch and the path doesn't exist
    // any more. New behaviour: clicking the row opens a one-pane
    // unified diff that paints every line of the HEAD blob as a
    // removed line — visually identical to `git diff` for a deletion.
    use crate::git::{ChangeEntry, ChangeKind};
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let init = std::process::Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(root)
        .status()
        .unwrap();
    assert!(init.success());
    let _ = std::process::Command::new("git")
        .args(["config", "user.email", "t@t"])
        .current_dir(root)
        .status();
    let _ = std::process::Command::new("git")
        .args(["config", "user.name", "t"])
        .current_dir(root)
        .status();
    let f = root.join("doomed.txt");
    std::fs::write(&f, b"alpha\nbeta\ngamma\n").unwrap();
    let _ = std::process::Command::new("git")
        .args(["add", "doomed.txt"])
        .current_dir(root)
        .status();
    let _ = std::process::Command::new("git")
        .args(["commit", "-q", "-m", "seed"])
        .current_dir(root)
        .status();
    // Delete in the working tree but leave it tracked in HEAD/index
    // — that's exactly the `Deleted` (unstaged) state.
    std::fs::remove_file(&f).unwrap();
    let mut app = App::new(root.to_path_buf()).unwrap();
    app.source_control.entries = vec![ChangeEntry {
        path: "doomed.txt".into(),
        kind: ChangeKind::Deleted,
        ..Default::default()
    }];
    app.set_sidebar_view(SidebarView::SourceControl);
    app.open_source_control_entry(0);
    let diff = app
        .editor
        .diff
        .as_ref()
        .expect("clicking a Deleted entry must open a diff view, not a blank tab");
    assert!(
        diff.unified,
        "the deletion view must be the single-column unified flavour, not side-by-side"
    );
    assert_eq!(diff.left_lines, vec!["alpha", "beta", "gamma"]);
    assert!(
        diff.rows
            .iter()
            .all(|r| matches!(r, crate::widgets::diff::DiffRow::Removed { .. })),
        "every visible row must be Removed so the whole pane paints red"
    );
}

#[test]
fn click_on_a_modified_source_control_entry_opens_a_diff_view_against_head() {
    // VS Code surfaces the diff between the working-tree file and
    // the HEAD version when the user clicks a Modified entry in the
    // Source Control panel. Croft used to just open the working
    // file as plain text — assert the new behaviour: the active
    // editor tab carries a `diff: Some(...)` payload.
    use crate::git::{ChangeEntry, ChangeKind};
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    // Init a real repo so git show HEAD:<path> returns content.
    let init = std::process::Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(root)
        .status()
        .unwrap();
    assert!(init.success());
    let _ = std::process::Command::new("git")
        .args(["config", "user.email", "t@t"])
        .current_dir(root)
        .status();
    let _ = std::process::Command::new("git")
        .args(["config", "user.name", "t"])
        .current_dir(root)
        .status();
    let f = root.join("a.py");
    std::fs::write(&f, b"print(1)\n").unwrap();
    let _ = std::process::Command::new("git")
        .args(["add", "a.py"])
        .current_dir(root)
        .status();
    let _ = std::process::Command::new("git")
        .args(["commit", "-q", "-m", "init"])
        .current_dir(root)
        .status();
    std::fs::write(&f, b"print(2)\n").unwrap();
    let mut app = App::new(root.to_path_buf()).unwrap();
    app.source_control.entries = vec![ChangeEntry {
        path: "a.py".into(),
        kind: ChangeKind::Modified,
        ..Default::default()
    }];
    app.set_sidebar_view(SidebarView::SourceControl);
    app.open_source_control_entry(0);
    assert!(
        app.editor.diff.is_some(),
        "clicking a Modified entry must open the editor in diff-vs-HEAD mode, not as plain text"
    );
}

/// Shared ritual for the hunk-staging tests: a real repo whose committed
/// 20-line file has been edited at line 2 and line 18, so the working
/// tree carries two well-separated hunks, opened as an SCM diff.
fn scm_two_hunk_app() -> (tempfile::TempDir, App) {
    use crate::git::{ChangeEntry, ChangeKind};
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let git = |args: &[&str]| {
        let ok = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?} failed");
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.email", "t@t"]);
    git(&["config", "user.name", "t"]);
    let original: String = (1..=20).map(|i| format!("line{i}\n")).collect();
    std::fs::write(root.join("f.txt"), original).unwrap();
    git(&["add", "f.txt"]);
    git(&["commit", "-q", "-m", "init"]);
    let mut edited: Vec<String> = (1..=20).map(|i| format!("line{i}")).collect();
    edited[1] = "LINE2".into();
    edited[17] = "LINE18".into();
    std::fs::write(root.join("f.txt"), edited.join("\n") + "\n").unwrap();
    let mut app = App::new(root.to_path_buf()).unwrap();
    app.source_control.entries = vec![ChangeEntry {
        path: "f.txt".into(),
        kind: ChangeKind::Modified,
        ..Default::default()
    }];
    app.set_sidebar_view(SidebarView::SourceControl);
    app.open_source_control_entry(0);
    assert!(app.editor.diff.is_some(), "the SCM diff must be open");
    (tmp, app)
}

#[test]
fn pressing_s_in_a_scm_diff_stages_only_the_hunk_under_the_caret() {
    let (tmp, mut app) = scm_two_hunk_app();
    // Caret on the first hunk (row 1 = the line2 edit).
    app.editor
        .diff
        .as_mut()
        .unwrap()
        .start_selection(crate::widgets::diff::DiffSide::Right, 1, 0);
    app.handle_key(key(KeyCode::Char('s'), KeyModifiers::NONE))
        .unwrap();
    let staged = git_stdout_in(tmp.path(), &["diff", "--cached"]);
    assert!(
        staged.contains("+LINE2"),
        "hunk 1 must land in the index: {staged}"
    );
    assert!(
        !staged.contains("LINE18"),
        "hunk 2 must stay unstaged: {staged}"
    );
    let unstaged = git_stdout_in(tmp.path(), &["diff"]);
    assert!(
        unstaged.contains("+LINE18"),
        "hunk 2 must remain a working-tree change: {unstaged}"
    );
}

#[test]
fn pressing_s_with_no_caret_stages_the_first_visible_hunk() {
    // Right after opening, the diff parks the first hunk in view; with no
    // click yet, S must act on that hunk, not silently no-op.
    let (tmp, mut app) = scm_two_hunk_app();
    app.handle_key(key(KeyCode::Char('s'), KeyModifiers::NONE))
        .unwrap();
    let staged = git_stdout_in(tmp.path(), &["diff", "--cached"]);
    assert!(
        staged.contains("+LINE2") && !staged.contains("LINE18"),
        "the first hunk is the default target: {staged}"
    );
}

#[test]
fn pressing_u_in_a_scm_diff_unstages_only_the_hunk_under_the_caret() {
    let (tmp, mut app) = scm_two_hunk_app();
    let ok = std::process::Command::new("git")
        .args(["add", "f.txt"])
        .current_dir(tmp.path())
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok);
    app.editor
        .diff
        .as_mut()
        .unwrap()
        .start_selection(crate::widgets::diff::DiffSide::Right, 1, 0);
    app.handle_key(key(KeyCode::Char('u'), KeyModifiers::NONE))
        .unwrap();
    let staged = git_stdout_in(tmp.path(), &["diff", "--cached"]);
    assert!(
        !staged.contains("LINE2"),
        "hunk 1 must leave the index: {staged}"
    );
    assert!(
        staged.contains("+LINE18"),
        "hunk 2 must stay staged: {staged}"
    );
}

#[test]
fn pressing_r_in_a_scm_diff_confirms_then_reverts_only_that_hunk() {
    let (tmp, mut app) = scm_two_hunk_app();
    app.editor
        .diff
        .as_mut()
        .unwrap()
        .start_selection(crate::widgets::diff::DiffSide::Right, 1, 0);
    app.handle_key(key(KeyCode::Char('r'), KeyModifiers::NONE))
        .unwrap();
    // Destructive: nothing may change until the confirm.
    let before = std::fs::read_to_string(tmp.path().join("f.txt")).unwrap();
    assert!(
        before.contains("LINE2"),
        "revert must wait for the confirm popup"
    );
    app.handle_key(key(KeyCode::Char('y'), KeyModifiers::NONE))
        .unwrap();
    let after = std::fs::read_to_string(tmp.path().join("f.txt")).unwrap();
    assert!(
        after.contains("line2") && !after.contains("LINE2"),
        "hunk 1 must be reverted: {after}"
    );
    assert!(
        after.contains("LINE18"),
        "hunk 2 must survive the revert: {after}"
    );
}

#[test]
fn revert_hunk_confirm_cancels_on_n() {
    let (tmp, mut app) = scm_two_hunk_app();
    app.handle_key(key(KeyCode::Char('r'), KeyModifiers::NONE))
        .unwrap();
    app.handle_key(key(KeyCode::Char('n'), KeyModifiers::NONE))
        .unwrap();
    let content = std::fs::read_to_string(tmp.path().join("f.txt")).unwrap();
    assert!(
        content.contains("LINE2") && content.contains("LINE18"),
        "Esc/N must leave the working tree untouched"
    );
}

#[test]
fn ctrl_shift_s_is_the_source_control_jump_where_shift_is_reported() {
    // #857: the kitty keyboard protocol reports Shift explicitly, so
    // Ctrl+Shift+S there is the Source Control jump the docs promise, like
    // Cmd+Shift+S everywhere; Save lets it go.
    let ctrl_shift = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
    for c in ['s', 'S'] {
        assert!(!is_save_key(key(KeyCode::Char(c), ctrl_shift), true));
        assert!(is_source_control_jump_key(key(
            KeyCode::Char(c),
            ctrl_shift
        )));
        let cmd_shift = KeyModifiers::SUPER | KeyModifiers::SHIFT;
        assert!(!is_save_key(key(KeyCode::Char(c), cmd_shift), false));
    }
}

/// #857 end to end: with the editor focused, Ctrl+Shift+S jumps to Source
/// Control and leaves the dirty buffer unsaved, while Ctrl+S still saves.
#[test]
fn ctrl_shift_s_in_the_editor_jumps_to_source_control_without_saving() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("notes.txt");
    std::fs::write(&path, "on disk").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.kitty_keys = true;
    app.editor.open_pinned(&path).unwrap();
    app.focus_pane(Pane::Editor);
    app.editor.insert_str("edited ");
    let ctrl_shift = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
    app.handle_key(key(KeyCode::Char('s'), ctrl_shift)).unwrap();
    assert_eq!(app.sidebar_view, SidebarView::SourceControl);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "on disk",
        "not saved"
    );
    app.focus_pane(Pane::Editor);
    app.handle_key(key(KeyCode::Char('s'), KeyModifiers::CONTROL))
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap().trim_end(),
        "edited on disk"
    );
}

#[test]
fn git_status_spans_empty_when_not_in_repo() {
    let st = crate::git::GitStatus::default();
    let spans = git_status_spans(&st);
    assert!(spans.is_empty(), "no git pill outside a git repo");
}

#[test]
fn git_status_spans_clean_branch_is_green() {
    // Agnoster convention: clean working tree → green pill.
    let st = crate::git::GitStatus {
        in_repo: true,
        branch: Some("main".into()),
        detached_hash: None,
        dirty: false,
        ahead: 0,
        behind: 0,
        head_oid: None,
        ignored: std::sync::Arc::default(),
        decorations: std::sync::Arc::default(),
        repo_root: None,
        changed_count: 0,
        prepared_message: None,
        operation: None,
        rebasing_branch: None,
    };
    let spans = git_status_spans(&st);
    let main_span = spans
        .iter()
        .find(|s| s.content.contains("main"))
        .expect("branch span");
    assert_eq!(main_span.style.fg, Some(GIT_CLEAN_COLOR));
}

#[test]
fn git_status_spans_dirty_branch_is_yellow_not_red() {
    // Agnoster convention: dirty working tree → yellow/orange pill.
    // No red bullet either — colour alone carries the state.
    let st = crate::git::GitStatus {
        in_repo: true,
        branch: Some("main".into()),
        detached_hash: None,
        dirty: true,
        ahead: 0,
        behind: 0,
        head_oid: None,
        ignored: std::sync::Arc::default(),
        decorations: std::sync::Arc::default(),
        repo_root: None,
        changed_count: 0,
        prepared_message: None,
        operation: None,
        rebasing_branch: None,
    };
    let spans = git_status_spans(&st);
    let joined: String = spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(
        !joined.contains('●'),
        "no red bullet — colour signals dirtiness"
    );
    let main_span = spans
        .iter()
        .find(|s| s.content.contains("main"))
        .expect("branch span");
    assert_eq!(main_span.style.fg, Some(GIT_DIRTY_COLOR));
}

#[test]
fn git_status_spans_renders_detached_hash_when_no_branch() {
    let st = crate::git::GitStatus {
        in_repo: true,
        branch: None,
        detached_hash: Some("abc1234".into()),
        dirty: false,
        ahead: 0,
        behind: 0,
        head_oid: None,
        ignored: std::sync::Arc::default(),
        decorations: std::sync::Arc::default(),
        repo_root: None,
        changed_count: 0,
        prepared_message: None,
        operation: None,
        rebasing_branch: None,
    };
    let spans = git_status_spans(&st);
    let joined: String = spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(joined.contains("abc1234"));
}

#[test]
fn git_status_spans_renders_ahead_behind_counts() {
    let st = crate::git::GitStatus {
        in_repo: true,
        branch: Some("main".into()),
        detached_hash: None,
        dirty: false,
        ahead: 2,
        behind: 1,
        head_oid: None,
        ignored: std::sync::Arc::default(),
        decorations: std::sync::Arc::default(),
        repo_root: None,
        changed_count: 0,
        prepared_message: None,
        operation: None,
        rebasing_branch: None,
    };
    let spans = git_status_spans(&st);
    let joined: String = spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(joined.contains("↑2"));
    assert!(joined.contains("↓1"));
}

#[test]
fn hunk_actions_refuse_a_timeline_snapshot_diff() {
    // A local-history snapshot diff is built by the same opener as the
    // Source Control diff; staging from it would apply snapshot-derived
    // patches to the git index from a view that never mentions git.
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("a.txt");
    std::fs::write(&path, "new\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor
        .open_head_diff_with_text(
            PathBuf::from("a.txt (local snapshot)"),
            "old\n",
            &path,
            false,
        )
        .unwrap();
    app.stage_hunk_at_caret();
    assert_eq!(
        app.status, "Hunk actions need a working-tree diff from Source Control",
        "a snapshot diff must refuse hunk actions"
    );
}

#[test]
fn enter_on_a_removed_line_in_a_source_control_diff_opens_the_working_file() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let foo = tmp.path().join("foo.txt");
    // Working tree dropped "bravo"; HEAD still has it.
    std::fs::write(&foo, "alpha\ncharlie\n").unwrap();
    // Mirror open_source_control_entry's Modified branch: synthetic HEAD
    // label on the left, the real working file on the right.
    let label = std::path::PathBuf::from("foo.txt (HEAD)");
    app.editor
        .open_head_diff_with_text(label, "alpha\nbravo\ncharlie\n", &foo, true)
        .unwrap();
    app.focus = Pane::Editor;
    // Rows: 0 Equal alpha, 1 Removed bravo, 2 Equal charlie. Click row 1.
    app.editor
        .diff
        .as_mut()
        .unwrap()
        .start_selection(crate::widgets::diff::DiffSide::Left, 1, 0);

    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();

    assert!(
        app.editor.diff.is_none(),
        "Enter on a removed line must leave the diff for the real working \
         file, not stay parked on the diff tab; status was {:?}",
        app.status
    );
    assert_eq!(app.editor.path.as_deref(), Some(foo.as_path()));
}

#[test]
fn enter_in_multifile_git_diff_opens_the_real_file_under_the_caret() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let src_dir = tmp.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    let foo = src_dir.join("foo.rs");
    std::fs::write(&foo, "keep1\nold\nkeep2\n").unwrap();
    let raw = concat!(
        "diff --git a/src/foo.rs b/src/foo.rs\n",
        "index 1111111..2222222 100644\n",
        "--- a/src/foo.rs\n",
        "+++ b/src/foo.rs\n",
        "@@ -1,3 +1,4 @@\n",
        " keep1\n",
        "-old\n",
        "+new1\n",
        "+new2\n",
        " keep2\n",
    );
    let label = std::path::PathBuf::from("git diff --staged");
    app.editor.open_git_diff_side_by_side(&label, raw).unwrap();
    app.focus = Pane::Editor;
    // Row 3 is "+new1" on the right side (new-file line 2).
    app.editor
        .diff
        .as_mut()
        .unwrap()
        .start_selection(crate::widgets::diff::DiffSide::Right, 3, 0);

    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();

    assert_eq!(
        app.editor.path.as_deref(),
        Some(foo.as_path()),
        "Enter on a row in the multi-file git diff must open the real file the \
         `diff --git` header names, resolved against the workspace root; status was {:?}",
        app.status
    );
    assert_eq!(app.editor.cursor_row, 1, "+new1 is new-file line 2");
}

#[test]
fn ctrl_d_with_no_anchor_stashes_selected_file() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("a.txt");
    std::fs::write(&f, "v1").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let idx = app
        .tree
        .nodes
        .iter()
        .position(|n| n.path == f)
        .expect("file must appear in tree");
    app.tree.selected = idx;
    app.handle_key(key(KeyCode::Char('d'), KeyModifiers::CONTROL))
        .unwrap();
    assert_eq!(app.compare_anchor.as_deref(), Some(f.as_path()));
}

#[test]
fn mouse_wheel_over_the_commits_graph_scrolls_it() {
    // Codeberg #41: the COMMITS section cannot be scrolled. The graph
    // renders BELOW the change list in its own strip, outside
    // source_control.last_area, so the sidebar hit-test (`in_tree`)
    // never matched and wheel events over the graph fell through.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.sidebar_view = SidebarView::SourceControl;
    app.show_tree = true;
    app.source_control.status.in_repo = true;
    let commits: Vec<crate::git::GraphCommit> = (0..100)
        .map(|i| crate::git::GraphCommit {
            hash: format!("{i:040x}"),
            short_hash: format!("{i:07x}"),
            parents: vec![format!("{:040x}", i + 1)],
            refs: Vec::new(),
            summary: format!("commit {i}"),
            author: String::from("t"),
            age_secs: 0,
        })
        .collect();
    app.commit_graph
        .set_rows(crate::widgets::commit_graph::layout_graph(commits));

    let backend = ratatui::backend::TestBackend::new(100, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|frame| app.render(frame)).unwrap();

    let area = app.commit_graph.last_area;
    assert!(area.height > 3, "the COMMITS section must render");
    let (col, row) = (area.x + 2, area.y + 2);
    assert!(
        !rect_contains(app.source_control.last_area, col, row),
        "the probe cell must sit in the graph strip, below the change list"
    );
    let before = app
        .commit_graph
        .commit_at(row)
        .expect("probe row must land on a commit")
        .hash
        .clone();
    app.handle_mouse(mouse(
        crossterm::event::MouseEventKind::ScrollDown,
        col,
        row,
    ));
    let after = app
        .commit_graph
        .commit_at(row)
        .expect("scrolled probe row must still land on a commit")
        .hash
        .clone();
    assert_ne!(
        before, after,
        "wheel-down over the COMMITS section must scroll the graph"
    );
    app.handle_mouse(mouse(crossterm::event::MouseEventKind::ScrollUp, col, row));
    assert_eq!(
        app.commit_graph.commit_at(row).map(|c| c.hash.clone()),
        Some(before),
        "wheel-up must scroll the graph back to the top"
    );
}

#[test]
fn a_graph_refresh_swallowed_by_an_inflight_fetch_refires_when_the_reply_drains() {
    // Make Root during an in-flight COMMITS fetch: set_root clears the
    // panel and triggers a refresh, but the inflight latch swallows it,
    // and the old root's reply is then dropped by the drain's root tag.
    // Without a queued re-fire nothing refetches until HEAD next moves,
    // so the panel sits on "Loading" indefinitely.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // Settle any startup fetch so the latch state is ours alone.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while app.graph_fetch_inflight {
        app.sync_explorer_panels();
        assert!(
            std::time::Instant::now() < deadline,
            "startup graph fetch must settle"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    // A fetch is mid-flight for the old root when the trigger arrives; the
    // root change has cleared the panel back to "Loading".
    app.commit_graph.clear();
    app.graph_fetch_inflight = true;
    app.refresh_commit_graph();
    // The stale reply lands: dropped by the root tag.
    app.graph_tx
        .send((
            std::path::PathBuf::from("/since-left/root"),
            None,
            Vec::new(),
        ))
        .unwrap();
    // The re-fire is the contract, but the inflight latch is transient: the
    // drain that re-fires can also drain the re-fired worker's reply in the
    // SAME call (an empty root answers in microseconds), legitimately
    // clearing the latch again. Poll the resolved outcome instead: the
    // cleared panel must leave "Loading" with rows for the CURRENT root.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !app.commit_graph.is_loaded() {
        app.sync_explorer_panels();
        assert!(
            std::time::Instant::now() < deadline,
            "the trigger swallowed during flight must re-fire once the stale reply drains"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn commits_graph_scrollbar_thumb_drag_follows_the_pointer() {
    // Codeberg #41 companion: every other sidebar section's scrollbar
    // supports a thumb drag (outline, timeline, testing, …); the COMMITS
    // graph's must too, not just a single jump-click.
    use crossterm::event::{MouseButton, MouseEventKind};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.sidebar_view = SidebarView::SourceControl;
    app.show_tree = true;
    app.source_control.status.in_repo = true;
    let commits: Vec<crate::git::GraphCommit> = (0..100)
        .map(|i| crate::git::GraphCommit {
            hash: format!("{i:040x}"),
            short_hash: format!("{i:07x}"),
            parents: vec![format!("{:040x}", i + 1)],
            refs: Vec::new(),
            summary: format!("commit {i}"),
            author: String::from("t"),
            age_secs: 0,
        })
        .collect();
    app.commit_graph
        .set_rows(crate::widgets::commit_graph::layout_graph(commits));

    let backend = ratatui::backend::TestBackend::new(100, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|frame| app.render(frame)).unwrap();

    let bar = app.commit_graph.last_scrollbar;
    assert!(
        bar.width > 0 && bar.height > 2,
        "100 commits in a capped strip must draw a scrollbar"
    );
    let body_row = app.commit_graph.last_area.y + 2;
    let top = app
        .commit_graph
        .commit_at(body_row)
        .expect("probe row must land on a commit")
        .hash
        .clone();
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), bar.x, bar.y));
    app.handle_mouse(mouse(
        MouseEventKind::Drag(MouseButton::Left),
        bar.x,
        bar.y + bar.height - 1,
    ));
    assert_ne!(
        app.commit_graph.commit_at(body_row).map(|c| c.hash.clone()),
        Some(top),
        "dragging the thumb to the lane's bottom must scroll the graph"
    );
    app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), bar.x, bar.y));
}

#[test]
fn clicking_source_control_does_not_block_on_git_and_posts_a_changes_request() {
    // Regression for the user-reported "Source Control click feels
    // sluggish vs the other sidebar icons" bug. The cause was a
    // synchronous `git status --porcelain` shell-out from
    // `refresh_source_control` running on the UI thread, costing
    // 15-50 ms per click on a small repo and 100-300 ms on a large
    // one. The fix routes every git status query through a
    // background worker; the click path now does a single
    // non-blocking `mpsc::Sender::send` and returns within the
    // microsecond range.
    let tmp = tempfile::tempdir().unwrap();
    let _ = std::process::Command::new("git")
        .args(["-C"])
        .arg(tmp.path())
        .args(["init", "-q", "-b", "main"])
        .output();
    let _ = std::process::Command::new("git")
        .args(["-C"])
        .arg(tmp.path())
        .args(["config", "user.email", "a@b"])
        .status();
    let _ = std::process::Command::new("git")
        .args(["-C"])
        .arg(tmp.path())
        .args(["config", "user.name", "a"])
        .status();
    std::fs::write(tmp.path().join("dirty.txt"), "x").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // Pretend the worker is idle by draining its initial Status
    // response (App::new posts one to seed the badge). This isn't
    // strictly required for the assertion but keeps the test from
    // racing with that startup reply.
    std::thread::sleep(std::time::Duration::from_millis(150));
    let _ = app.drain_git_responses();
    // The synchronous shell-out used to take 15-50 ms minimum on
    // any real machine; an order of magnitude headroom catches the
    // regression even on a fast CI box. A single wall-clock sample
    // measures the scheduler as much as the code under suite load
    // (#57): one preemption between the clock reads adds milliseconds
    // with no regression present. The MINIMUM of several attempts is
    // immune — a preemption doesn't repeat on every try, while a
    // genuine synchronous shell-out fails all of them.
    let mut fastest = std::time::Duration::MAX;
    for _ in 0..5 {
        let started = std::time::Instant::now();
        app.set_sidebar_view(SidebarView::SourceControl);
        fastest = fastest.min(started.elapsed());
    }
    assert!(
        fastest < std::time::Duration::from_millis(5),
        "Source Control click must not block on git; fastest of 5 took {fastest:?}",
    );
    // The Changes request must have landed on the worker — give it
    // up to 2 s to respond, then drain. The response carries the
    // single untracked file we wrote above.
    let mut got_changes = false;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while std::time::Instant::now() < deadline {
        if app.drain_git_responses() && !app.source_control.entries.is_empty() {
            got_changes = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        got_changes,
        "the worker must reply with the dirty file's Changes entry; entries = {:?}",
        app.source_control.entries,
    );
}

#[test]
fn git_worker_status_response_feeds_the_explorer_decorations() {
    // #1513 end to end: the worker's Status reply carries the decorations
    // and drain pushes them into the file tree, so a new file and its
    // folder are marked untracked in the Explorer.
    let tmp = tempfile::tempdir().unwrap();
    let _ = std::process::Command::new("git")
        .args(["-C"])
        .arg(tmp.path())
        .args(["init", "-q", "-b", "main"])
        .output();
    std::fs::create_dir(tmp.path().join("pkg")).unwrap();
    std::fs::write(tmp.path().join("pkg/new.py"), "x = 1\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while app.tree.git_decorations.is_empty() && std::time::Instant::now() < deadline {
        let _ = app.drain_git_responses();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    use crate::git::GitDecoration as D;
    assert_eq!(
        app.tree.git_decoration(&tmp.path().join("pkg/new.py")),
        Some(D::Untracked),
        "decorations must reach the tree; map = {:?}",
        app.tree.git_decorations,
    );
    assert_eq!(
        app.tree.git_decoration(&tmp.path().join("pkg")),
        Some(D::Untracked)
    );
    assert_eq!(app.tree.git_decoration(tmp.path()), None);
}

#[test]
fn git_worker_status_response_feeds_the_explorer_ignored_set() {
    // End-to-end wiring for the greyed-out gitignored Explorer rows: the
    // worker's Status reply carries the ignored set and drain must push it
    // into the file tree, where descendants of a collapsed ignored dir
    // resolve via the ancestor walk.
    let tmp = tempfile::tempdir().unwrap();
    let _ = std::process::Command::new("git")
        .args(["-C"])
        .arg(tmp.path())
        .args(["init", "-q", "-b", "main"])
        .output();
    std::fs::write(tmp.path().join(".gitignore"), "build/\n").unwrap();
    std::fs::create_dir(tmp.path().join("build")).unwrap();
    std::fs::write(tmp.path().join("build/out.txt"), "").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // App::new seeds one Status request; wait for its reply to land.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while app.tree.ignored.is_empty() && std::time::Instant::now() < deadline {
        let _ = app.drain_git_responses();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        app.tree.is_ignored(&tmp.path().join("build")),
        "ignored dir must reach the tree; set = {:?}",
        app.tree.ignored,
    );
    assert!(
        app.tree.is_ignored(&tmp.path().join("build/out.txt")),
        "descendants of the collapsed ignored dir dim too"
    );
    assert!(!app.tree.is_ignored(&tmp.path().join(".gitignore")));
}

#[test]
fn source_control_stages_a_toplevel_entry_from_a_subdir_workspace() {
    // #139: porcelain paths are toplevel-relative, but every Source
    // Control operation ran `git -C <workspace_root>` with them as
    // cwd-relative pathspecs. A workspace rooted in a repo SUBDIR could
    // not stage anything living outside that subdir.
    let outer = tempfile::tempdir().unwrap();
    let repo = outer.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["config", "user.email", "a@b"],
        vec!["config", "user.name", "a"],
    ] {
        let ok = std::process::Command::new("git")
            .args(["-C"])
            .arg(&repo)
            .args(&args)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(ok, "git setup step failed: {args:?}");
    }
    std::fs::write(repo.join("seed.txt"), "one\ntwo\n").unwrap();
    let sub = repo.join("crates").join("foo");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join("keep.txt"), "x\n").unwrap();
    for args in [vec!["add", "."], vec!["commit", "-qm", "init"]] {
        let ok = std::process::Command::new("git")
            .args(["-C"])
            .arg(&repo)
            .args(&args)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(ok, "git setup step failed: {args:?}");
    }
    // A tracked TOPLEVEL file goes dirty while the workspace opens the
    // subdirectory two levels down.
    std::fs::write(repo.join("seed.txt"), "one\ntwo\nthree\n").unwrap();

    let mut app = App::new(sub.clone()).unwrap();
    app.refresh_source_control();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let mut entry_idx = None;
    while std::time::Instant::now() < deadline {
        let _ = app.drain_git_responses();
        if let Some(i) = app
            .source_control
            .entries
            .iter()
            .position(|e| e.path == "seed.txt")
        {
            entry_idx = Some(i);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let entry_idx = entry_idx
        .expect("the panel lists the toplevel-relative porcelain entry from a subdir workspace");
    assert_eq!(
        app.scm_root().canonicalize().unwrap(),
        repo.canonicalize().unwrap(),
        "the discovered repo root rides the status reply"
    );

    app.stage_source_control_entry(entry_idx);

    let staged = std::process::Command::new("git")
        .args(["-C"])
        .arg(&repo)
        .args(["diff", "--cached", "--name-only"])
        .output()
        .unwrap();
    assert!(staged.status.success(), "git diff --cached failed");
    assert!(
        String::from_utf8_lossy(&staged.stdout).contains("seed.txt"),
        "staging from the panel must address the file by its toplevel-relative path; \
         `git -C <subdir> add seed.txt` matches nothing"
    );
}

#[test]
fn change_workspace_root_into_a_git_subdir_flips_source_control_into_repo_state() {
    // Regression for the user's "Make Root into a .git-bearing folder
    // doesn't update Source Control" bug: the git worker thread was
    // spawned ONCE with the initial root captured by value. After
    // change_workspace_root, the worker kept shelling git status
    // against the OLD root, so the Source Control panel stayed in
    // its no-repo empty state even though the new root was a real
    // git tree.
    let outer = tempfile::tempdir().unwrap();
    let repo = outer.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    let _ = std::process::Command::new("git")
        .args(["-C"])
        .arg(&repo)
        .args(["init", "-q", "-b", "main"])
        .output();
    let _ = std::process::Command::new("git")
        .args(["-C"])
        .arg(&repo)
        .args(["config", "user.email", "a@b"])
        .status();
    let _ = std::process::Command::new("git")
        .args(["-C"])
        .arg(&repo)
        .args(["config", "user.name", "a"])
        .status();
    let mut app = App::new(outer.path().to_path_buf()).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(150));
    let _ = app.drain_git_responses();
    assert!(
        !app.source_control.status.in_repo,
        "preconditions: outer dir must NOT be a git repo"
    );

    app.change_workspace_root(repo.clone());

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let mut flipped = false;
    while std::time::Instant::now() < deadline {
        if app.drain_git_responses() && app.source_control.status.in_repo {
            flipped = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        flipped,
        "after Make Root into a real git tree the worker must re-query against the new root and the Source Control panel must report in_repo=true; saw status={:?}",
        app.source_control.status,
    );
    assert_eq!(app.source_control.status.branch.as_deref(), Some("main"));
}

#[test]
fn stage_source_control_entry_runs_git_add_and_flips_kind_to_staged() {
    let tmp = make_committed_repo();
    std::fs::write(tmp.path().join("seed.txt"), b"changed\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    assert!(
        wait_for_changes(&mut app, |a| a
            .source_control
            .entries
            .iter()
            .any(
                |e| e.path == "seed.txt" && e.kind == crate::git::ChangeKind::Modified
            )),
        "preconditions: seed.txt must appear as Modified"
    );
    let idx = app
        .source_control
        .entries
        .iter()
        .position(|e| e.path == "seed.txt")
        .unwrap();
    app.stage_source_control_entry(idx);
    assert!(
        wait_for_changes(&mut app, |a| a
            .source_control
            .entries
            .iter()
            .any(
                |e| e.path == "seed.txt" && e.kind == crate::git::ChangeKind::StagedModified
            )),
        "after stage_source_control_entry the file must read as StagedModified; saw {:?}",
        app.source_control.entries,
    );
}

#[test]
fn stage_all_source_control_then_unstage_all_round_trips_every_entry() {
    let tmp = make_committed_repo();
    std::fs::write(tmp.path().join("seed.txt"), b"changed\n").unwrap();
    std::fs::write(tmp.path().join("fresh.txt"), b"new\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    assert!(
        wait_for_changes(&mut app, |a| a.source_control.entries.len() >= 2),
        "preconditions: both files must show as changes"
    );
    app.stage_all_source_control();
    assert!(
        wait_for_changes(&mut app, |a| !a.source_control.entries.is_empty()
            && a.source_control
                .entries
                .iter()
                .all(|e| e.kind.section() == crate::git::ChangeSection::Staged)),
        "after Stage All every entry must read as staged; saw {:?}",
        app.source_control.entries,
    );
    app.unstage_all_source_control();
    assert!(
        wait_for_changes(&mut app, |a| !a.source_control.entries.is_empty()
            && a.source_control
                .entries
                .iter()
                .all(|e| e.kind.section() != crate::git::ChangeSection::Staged)),
        "after Unstage All nothing must remain staged; saw {:?}",
        app.source_control.entries,
    );
}

#[test]
fn discard_all_requires_confirmation_and_then_reverts_tracked_changes() {
    let tmp = make_committed_repo();
    std::fs::write(tmp.path().join("seed.txt"), b"dirty edit\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    wait_for_changes(&mut app, |a| {
        a.source_control
            .entries
            .iter()
            .any(|e| e.path == "seed.txt")
    });
    // Requesting discard-all only arms the confirm modal; the file is untouched.
    app.request_discard_all_source_control();
    assert!(
        app.pending_discard_all,
        "Discard All must arm a confirmation rather than act immediately"
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("seed.txt")).unwrap(),
        "dirty edit\n",
        "the working tree must be untouched until the user confirms"
    );
    app.confirm_pending_discard_all();
    assert!(!app.pending_discard_all, "confirming clears the modal");
    assert!(
        wait_for_changes(&mut app, |_| std::fs::read_to_string(
            tmp.path().join("seed.txt")
        )
        .map(|s| !s.contains("dirty edit"))
        .unwrap_or(false)),
        "after confirming, the tracked edit must be reverted to HEAD"
    );
}

/// `git -C dir <args>` for app tests, asserting success; stdout trimmed.
fn git_checked(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// #1350: Undo Last Commit (⋯ → Commit) takes the commit back: HEAD is
/// the parent, its changes are staged, and its message is in the box.
#[test]
fn undo_last_commit_via_the_menu_puts_the_message_back_in_the_box() {
    use crate::widgets::scm_menu::ScmAction;
    let tmp = make_committed_repo();
    let parent = git_checked(tmp.path(), &["rev-parse", "HEAD"]);
    std::fs::write(tmp.path().join("b.txt"), "b\n").unwrap();
    git_checked(tmp.path(), &["add", "b.txt"]);
    git_checked(tmp.path(), &["commit", "-q", "-m", "Add b too early"]);
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.dispatch_scm_action(ScmAction::UndoLastCommit);
    assert_eq!(git_checked(tmp.path(), &["rev-parse", "HEAD"]), parent);
    assert_eq!(app.source_control.message, "Add b too early");
    assert_eq!(
        git_checked(tmp.path(), &["diff", "--cached", "--name-only"]),
        "b.txt"
    );
    assert!(
        !app.source_control.commit_feedback_is_error,
        "{:?}",
        app.source_control.commit_feedback
    );
}

/// #1350: the palette has it too.
#[test]
fn undo_last_commit_is_in_the_palette() {
    let cmd = crate::widgets::command_palette::Command::from_id("git_undo_last_commit")
        .expect("there is an Undo Last Commit command");
    assert_eq!(cmd.title(), "Git: Undo Last Commit");
    let tmp = make_committed_repo();
    std::fs::write(tmp.path().join("b.txt"), "b\n").unwrap();
    git_checked(tmp.path(), &["add", "b.txt"]);
    git_checked(tmp.path(), &["commit", "-q", "-m", "second"]);
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.run_command(cmd);
    assert_eq!(app.source_control.message, "second");
}

/// #1350 negative: a commit already on the upstream is not undone on the
/// first ask; it warns that undoing it means a force push, and a second
/// ask goes ahead.
#[test]
fn undo_last_commit_warns_before_undoing_a_pushed_commit() {
    use crate::widgets::scm_menu::ScmAction;
    let tmp = make_committed_repo();
    let remote = tempfile::tempdir().unwrap();
    git_checked(remote.path(), &["init", "-q", "--bare"]);
    git_checked(
        tmp.path(),
        &["remote", "add", "origin", remote.path().to_str().unwrap()],
    );
    git_checked(tmp.path(), &["push", "-q", "-u", "origin", "main"]);
    let head = git_checked(tmp.path(), &["rev-parse", "HEAD"]);
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.dispatch_scm_action(ScmAction::UndoLastCommit);
    assert_eq!(
        git_checked(tmp.path(), &["rev-parse", "HEAD"]),
        head,
        "not yet"
    );
    let warning = app
        .source_control
        .commit_feedback
        .clone()
        .unwrap_or_default();
    assert!(warning.contains("force push"), "{warning}");
    app.dispatch_scm_action(ScmAction::UndoLastCommit);
    assert_eq!(
        app.source_control.message, "init",
        "the second ask undoes it"
    );
}

/// #1350 negative: with a typed message in the box the undo keeps it
/// rather than overwriting the user's text.
#[test]
fn undo_last_commit_keeps_a_message_the_user_typed() {
    use crate::widgets::scm_menu::ScmAction;
    let tmp = make_committed_repo();
    std::fs::write(tmp.path().join("b.txt"), "b\n").unwrap();
    git_checked(tmp.path(), &["add", "b.txt"]);
    git_checked(tmp.path(), &["commit", "-q", "-m", "second"]);
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.source_control.insert_str("my draft");
    app.dispatch_scm_action(ScmAction::UndoLastCommit);
    assert_eq!(app.source_control.message, "my draft");
    assert_eq!(
        git_checked(tmp.path(), &["log", "-1", "--format=%s"]),
        "init"
    );
}

/// #1352: Git: Revert Hunk with the caret on line 5 of an editor tab
/// restores `value_5`, in the file and the tab, and leaves line 9's
/// deletion alone.
#[test]
fn revert_hunk_in_the_editor_restores_the_head_lines() {
    let (mut app, t) = app_with_change_bars();
    app.editor.cursor_row = 4;
    app.run_command(crate::widgets::command_palette::Command::RevertHunk);
    assert!(app.pending_revert_hunk.is_some(), "{}", app.status);
    app.confirm_pending_revert_hunk();
    let disk = std::fs::read_to_string(t.path().join("v.txt")).unwrap();
    assert!(
        disk.contains("value_5\n") && !disk.contains("value_FIVE"),
        "{disk}"
    );
    assert!(!disk.contains("value_9\n"), "the other hunk stays");
    assert_eq!(
        app.editor.lines[4], "value_5",
        "the tab shows the reverted text"
    );
}

/// #1352: Git: Stage Hunk from an editor tab stages only that hunk.
#[test]
fn stage_hunk_in_the_editor_stages_only_that_hunk() {
    let (mut app, t) = app_with_change_bars();
    app.editor.cursor_row = 4;
    app.run_command(crate::widgets::command_palette::Command::StageHunk);
    let staged = git_stdout(t.path(), &["diff", "--cached"]);
    assert!(staged.contains("+value_FIVE"), "{staged}\n{}", app.status);
    assert!(!staged.contains("-value_9"), "{staged}");
    let unstaged = git_stdout(t.path(), &["diff"]);
    assert!(unstaged.contains("-value_9"), "{unstaged}");
}

/// #1352 negative: with unsaved edits the hunk actions refuse, since they
/// work on the file on disk, and say to save first.
#[test]
fn hunk_actions_in_the_editor_refuse_a_dirty_buffer() {
    let (mut app, t) = app_with_change_bars();
    app.editor.cursor_row = 4;
    app.editor.insert_str("x");
    app.run_command(crate::widgets::command_palette::Command::RevertHunk);
    assert!(app.pending_revert_hunk.is_none());
    assert!(app.status.contains("Save"), "{}", app.status);
    assert!(
        std::fs::read_to_string(t.path().join("v.txt"))
            .unwrap()
            .contains("value_FIVE")
    );
}

/// #1352 negative: on an unchanged line there is no hunk to act on, and
/// F7 in a file with no changes leaves the caret where it is.
#[test]
fn an_unchanged_line_has_no_hunk_and_a_clean_file_no_next_change() {
    let (mut app, t) = app_with_change_bars();
    app.editor.cursor_row = 0;
    app.run_command(crate::widgets::command_palette::Command::StageHunk);
    assert!(app.status.contains("No change"), "{}", app.status);
    assert_eq!(git_stdout(t.path(), &["diff", "--cached"]), "");
    app.editor.open_pinned(&t.path().join("seed.txt")).unwrap();
    app.sync_git_gutters();
    app.editor.cursor_row = 0;
    app.handle_key(key(KeyCode::F(7), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.editor.cursor_row, 0);
}

#[test]
fn create_tag_via_the_menu_opens_an_input_then_creates_the_tag() {
    use crate::widgets::input_prompt::InputPurpose;
    use crate::widgets::scm_menu::ScmAction;
    let tmp = make_committed_repo();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.dispatch_scm_action(ScmAction::CreateTag);
    let prompt = app
        .input_prompt
        .as_ref()
        .expect("Create Tag must open an input prompt");
    assert_eq!(prompt.purpose, InputPurpose::CreateTag);
    for c in "v9.9.9".chars() {
        app.input_prompt.as_mut().unwrap().push_char(c);
    }
    app.submit_input_prompt();
    // #1582: then the optional message; Enter on a blank one makes a
    // lightweight tag.
    assert_eq!(
        app.input_prompt.as_ref().map(|p| p.purpose.clone()),
        Some(InputPurpose::CreateTagMessage {
            name: String::from("v9.9.9")
        })
    );
    app.submit_input_prompt();
    assert!(app.input_prompt.is_none(), "submitting closes the prompt");
    assert!(
        crate::git::list_tags(tmp.path())
            .unwrap()
            .contains(&"v9.9.9".to_string()),
        "the tag must exist after submitting the Create Tag input"
    );
    assert_eq!(
        git_stdout(tmp.path(), &["cat-file", "-t", "v9.9.9"]),
        "commit"
    );
}

/// A committed repo with a bare `origin` it has pushed `main` to (#1582).
fn repo_with_bare_origin() -> (tempfile::TempDir, tempfile::TempDir) {
    let tmp = make_committed_repo();
    let bare = tempfile::tempdir().unwrap();
    git_stdout(bare.path(), &["init", "-q", "--bare"]);
    git_stdout(
        tmp.path(),
        &["remote", "add", "origin", bare.path().to_str().unwrap()],
    );
    git_stdout(tmp.path(), &["push", "-q", "-u", "origin", "main"]);
    (tmp, bare)
}

fn type_and_submit(app: &mut App, text: &str) {
    for c in text.chars() {
        app.input_prompt.as_mut().unwrap().push_char(c);
    }
    app.submit_input_prompt();
}

/// #1582: the Tags menu makes an annotated tag from a message, pushes it,
/// and deletes it from the remote after a confirmation, keeping the local
/// tag.
#[test]
fn the_tags_menu_annotates_pushes_and_deletes_a_remote_tag() {
    use crate::widgets::input_prompt::InputPurpose;
    use crate::widgets::scm_menu::ScmAction;
    let (tmp, bare) = repo_with_bare_origin();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.dispatch_scm_action(ScmAction::CreateTag);
    type_and_submit(&mut app, "v1.0");
    type_and_submit(&mut app, "Release 1.0");
    assert_eq!(git_stdout(tmp.path(), &["cat-file", "-t", "v1.0"]), "tag");
    assert_eq!(
        git_stdout(
            tmp.path(),
            &["tag", "-l", "--format=%(contents:subject)", "v1.0"]
        ),
        "Release 1.0"
    );

    app.run_command(crate::widgets::command_palette::Command::GitPushTags);
    wait_for_git_net(&mut app);
    assert!(app.status.starts_with("Pushed tags"), "{}", app.status);
    assert_eq!(git_stdout(bare.path(), &["tag"]), "v1.0");

    app.dispatch_scm_action(ScmAction::DeleteRemoteTag);
    wait_for_git_net(&mut app);
    let picker = app.list_picker.as_ref().expect("the remote's tags");
    assert_eq!(picker.title, "Delete Remote Tag from origin");
    app.confirm_list_picker();
    assert_eq!(
        app.input_prompt.as_ref().map(|p| p.purpose.clone()),
        Some(InputPurpose::DeleteRemoteTag {
            root: app.scm_root(),
            remote: String::from("origin"),
            name: String::from("v1.0"),
        }),
        "deleting from the remote is confirmed first"
    );
    assert_eq!(
        git_stdout(bare.path(), &["tag"]),
        "v1.0",
        "not before Enter"
    );
    app.submit_input_prompt();
    wait_for_git_net(&mut app);
    assert_eq!(
        git_stdout(bare.path(), &["tag"]),
        "",
        "gone from the remote"
    );
    assert_eq!(git_stdout(tmp.path(), &["tag"]), "v1.0", "kept locally");
}

/// #1582, negative: Esc on the confirmation keeps the remote tag, a
/// remote with no tags says so, and with no remote nothing runs.
#[test]
fn remote_tags_stay_without_a_confirmation_or_a_remote() {
    use crate::widgets::scm_menu::ScmAction;
    let (tmp, bare) = repo_with_bare_origin();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.dispatch_scm_action(ScmAction::DeleteRemoteTag);
    wait_for_git_net(&mut app);
    assert!(app.list_picker.is_none());
    assert_eq!(app.status, "No tags on origin");

    git_stdout(tmp.path(), &["tag", "v2"]);
    git_stdout(tmp.path(), &["push", "-q", "origin", "v2"]);
    app.dispatch_scm_action(ScmAction::DeleteRemoteTag);
    wait_for_git_net(&mut app);
    app.confirm_list_picker();
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert!(app.input_prompt.is_none() && app.git_net_job.is_none());
    assert_eq!(git_stdout(bare.path(), &["tag"]), "v2", "Esc keeps it");

    let lone = make_committed_repo();
    let mut app = App::new(lone.path().to_path_buf()).unwrap();
    app.dispatch_scm_action(ScmAction::PushTags);
    assert!(app.git_net_job.is_none());
    assert_eq!(app.status, "Push tags: no remote to push tags to");
    app.dispatch_scm_action(ScmAction::DeleteRemoteTag);
    assert!(app.git_net_job.is_none());
    assert_eq!(app.status, "Delete remote tag: no remote to push tags to");
}

/// #1582: the remote-tag deletion stays bound to the repository whose
/// remote was listed. When Source Control has moved to another root by
/// the time the listing lands, the picker is chosen from, or the
/// confirmation is submitted, nothing is deleted.
#[test]
fn a_remote_tag_is_not_deleted_after_source_control_moved_repos() {
    use crate::widgets::input_prompt::InputPurpose;
    use crate::widgets::scm_menu::ScmAction;
    let (tmp, bare) = repo_with_bare_origin();
    git_stdout(tmp.path(), &["tag", "v3"]);
    git_stdout(tmp.path(), &["push", "-q", "origin", "v3"]);
    let other = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let moved = "Delete remote tag cancelled: Source Control moved to another repository";

    // The listing lands after the move: no picker.
    app.show_remote_tags(other.path(), "origin", Ok(vec![String::from("v3")]));
    assert!(app.list_picker.is_none());
    assert_eq!(app.status, moved);

    // The picker was listed for another root: no confirmation.
    app.dispatch_scm_action(ScmAction::DeleteRemoteTag);
    wait_for_git_net(&mut app);
    assert!(app.list_picker.is_some());
    app.remote_tag_root = Some(other.path().to_path_buf());
    app.confirm_list_picker();
    assert!(app.input_prompt.is_none());
    assert_eq!(app.status, moved);

    // The confirmation was for another root: nothing runs.
    app.dispatch_scm_action(ScmAction::DeleteRemoteTag);
    wait_for_git_net(&mut app);
    app.confirm_list_picker();
    match &mut app.input_prompt.as_mut().expect("the confirmation").purpose {
        InputPurpose::DeleteRemoteTag { root, .. } => *root = other.path().to_path_buf(),
        other => panic!("unexpected prompt {other:?}"),
    }
    app.submit_input_prompt();
    assert!(app.input_prompt.is_none() && app.git_net_job.is_none());
    assert_eq!(app.status, moved);
    assert_eq!(
        git_stdout(bare.path(), &["tag"]),
        "v3",
        "kept on the remote"
    );
}

#[test]
fn delete_branch_via_the_menu_opens_the_branch_picker_in_delete_mode() {
    use crate::widgets::scm_menu::ScmAction;
    let tmp = make_committed_repo();
    crate::git::create_branch(tmp.path(), "doomed").unwrap();
    crate::git::checkout_branch(tmp.path(), "main").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.dispatch_scm_action(ScmAction::DeleteBranch);
    assert!(
        app.branch_picker.is_some(),
        "Delete Branch opens the branch picker"
    );
    assert_eq!(app.branch_purpose, crate::app::BranchPurpose::Delete);
}

// ---- A commit's hooks run off the UI thread (#1153) ----

/// A committed repo with `seed.txt` changed and a `pre-commit` hook that
/// runs `body`.
fn repo_with_pre_commit_hook(body: &str) -> tempfile::TempDir {
    let tmp = make_committed_repo();
    let hook = tmp.path().join(".git/hooks/pre-commit");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    std::fs::write(&hook, format!("#!/bin/sh\n{body}\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::fs::write(tmp.path().join("seed.txt"), b"changed\n").unwrap();
    tmp
}

/// A hook that runs until the test drops a `go` file next to the repo, or
/// for five seconds, whichever comes first: long enough to tell a commit
/// that waited for it from one that did not.
const HOOK_UNTIL_GO: &str =
    "i=0; while [ ! -f go ] && [ $i -lt 100 ]; do sleep 0.05; i=$((i+1)); done";

fn last_commit_subject(root: &Path) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["log", "-1", "--format=%s"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn app_with_commit_message(root: &Path, message: &str) -> App {
    let mut app = App::new(root.to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::SourceControl);
    app.source_control.message = message.to_string();
    app.source_control.message_cursor = message.chars().count();
    app
}

#[test]
fn a_commit_returns_at_once_while_its_hook_runs_and_lands_later() {
    let tmp = repo_with_pre_commit_hook(HOOK_UNTIL_GO);
    let mut app = app_with_commit_message(tmp.path(), "fix: hooked");
    let started = std::time::Instant::now();
    app.handle_source_control_key(key(KeyCode::Enter, KeyModifiers::NONE));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "Enter blocked for {:?} on the hook",
        started.elapsed()
    );
    assert!(app.git_net_job.is_some(), "the commit is in flight");
    assert_eq!(app.status, "Committing\u{2026} (running hooks)");
    assert_eq!(
        app.source_control.message, "fix: hooked",
        "kept until it lands"
    );
    app.handle_source_control_key(key(KeyCode::Enter, KeyModifiers::NONE));
    assert!(
        app.status.contains("commit is still running"),
        "{}",
        app.status
    );
    std::fs::write(tmp.path().join("go"), b"").unwrap();
    wait_for_git_net(&mut app);
    assert_eq!(last_commit_subject(tmp.path()), "fix: hooked");
    assert!(app.source_control.message.is_empty());
    assert!(app.status.starts_with("Committed: "), "{}", app.status);
}

#[test]
fn commit_and_push_runs_its_commit_off_the_ui_thread_too() {
    let tmp = repo_with_pre_commit_hook(HOOK_UNTIL_GO);
    let mut app = app_with_commit_message(tmp.path(), "feat: pushed");
    let started = std::time::Instant::now();
    app.commit_and_push_source_control();
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "blocked for {:?} on the hook",
        started.elapsed()
    );
    std::fs::write(tmp.path().join("go"), b"").unwrap();
    wait_for_git_net(&mut app);
    assert_eq!(last_commit_subject(tmp.path()), "feat: pushed");
    wait_for_git_net(&mut app);
    assert!(
        app.status.starts_with("Commit ok; push failed"),
        "with no remote the push that follows fails, after the commit: {}",
        app.status
    );
}

#[test]
fn a_commit_rejected_by_its_hook_keeps_the_message_and_says_why() {
    let tmp = repo_with_pre_commit_hook("echo 'lint: 3 problems' >&2; exit 1");
    let mut app = app_with_commit_message(tmp.path(), "wip: broken");
    app.handle_source_control_key(key(KeyCode::Enter, KeyModifiers::NONE));
    wait_for_git_net(&mut app);
    assert_eq!(last_commit_subject(tmp.path()), "init");
    assert_eq!(app.source_control.message, "wip: broken");
    assert!(app.source_control.commit_feedback_is_error);
    assert!(app.status.contains("lint: 3 problems"), "{}", app.status);
}

#[test]
fn a_message_typed_while_the_commit_runs_is_not_cleared_when_it_lands() {
    let tmp = repo_with_pre_commit_hook(HOOK_UNTIL_GO);
    let mut app = app_with_commit_message(tmp.path(), "fix: first");
    app.handle_source_control_key(key(KeyCode::Enter, KeyModifiers::NONE));
    app.source_control.message = String::from("fix: the next one");
    std::fs::write(tmp.path().join("go"), b"").unwrap();
    wait_for_git_net(&mut app);
    assert_eq!(last_commit_subject(tmp.path()), "fix: first");
    assert_eq!(app.source_control.message, "fix: the next one");
}

#[test]
fn an_empty_commit_message_is_still_refused_on_the_spot() {
    let tmp = repo_with_pre_commit_hook(HOOK_UNTIL_GO);
    let mut app = app_with_commit_message(tmp.path(), "   ");
    app.handle_source_control_key(key(KeyCode::Enter, KeyModifiers::NONE));
    assert!(app.git_net_job.is_none(), "nothing to run");
    assert_eq!(
        app.source_control.commit_feedback.as_deref(),
        Some("Empty commit message")
    );
}

/// A push runs on a worker: the call returns with the operation in flight,
/// a second one is refused while it runs, and the result lands on a drain.
#[test]
fn a_push_runs_off_the_ui_thread_and_reports_when_it_lands() {
    let tmp = make_committed_repo();
    let bare = tempfile::tempdir().unwrap();
    let git = |dir: &std::path::Path, args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap()
    };
    git(bare.path(), &["init", "-q", "--bare"]);
    git(
        tmp.path(),
        &["remote", "add", "origin", bare.path().to_str().unwrap()],
    );
    git(tmp.path(), &["push", "-q", "-u", "origin", "main"]);
    std::fs::write(tmp.path().join("seed.txt"), b"two\n").unwrap();
    git(tmp.path(), &["commit", "-qam", "two"]);
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.push_source_control();
    assert!(app.git_net_job.is_some(), "the push is in flight");
    app.pull_source_control();
    assert!(app.status.contains("still running"), "{}", app.status);
    wait_for_git_net(&mut app);
    assert!(
        !app.source_control.commit_feedback_is_error,
        "{:?}",
        app.source_control.commit_feedback
    );
    let remote_head = git(bare.path(), &["rev-parse", "main"]).stdout;
    let local_head = git(tmp.path(), &["rev-parse", "HEAD"]).stdout;
    assert_eq!(remote_head, local_head);
}

/// Run git in `dir` and return its trimmed stdout ("" on failure).
fn git_stdout(dir: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// #1356: a conflicted merge is reported as one, and Abort Merge from
/// the Source Control menu puts HEAD and the tree back as they were.
#[test]
fn abort_merge_restores_the_pre_merge_head() {
    use crate::git::RepoOp;
    let tmp = diverged_repo();
    let p = tmp.path();
    let before = git_stdout(p, &["rev-parse", "HEAD"]);
    assert!(
        !git_succeeds(p, &["merge", "feat"]),
        "the merge must conflict"
    );
    assert_eq!(crate::git::repo_operation(p), Some(RepoOp::Merge));
    let mut app = App::new(p.to_path_buf()).unwrap();
    app.dispatch_scm_action(crate::widgets::scm_menu::ScmAction::AbortOperation);
    assert_eq!(crate::git::repo_operation(p), None, "{}", app.status);
    assert_eq!(git_stdout(p, &["rev-parse", "HEAD"]), before);
    assert_eq!(git_stdout(p, &["status", "--porcelain"]), "");
}

/// #1356: Continue on a rebase whose conflict is resolved and staged
/// finishes it on the branch, with the resolution on top of `main`.
#[test]
fn continue_rebase_finishes_it_on_the_branch() {
    use crate::git::RepoOp;
    let tmp = diverged_repo();
    let p = tmp.path();
    assert!(git_succeeds(p, &["checkout", "-q", "feat"]));
    assert!(
        !git_succeeds(p, &["rebase", "main"]),
        "the rebase must conflict"
    );
    assert_eq!(
        crate::git::repo_operation(p),
        Some(RepoOp::Rebase { step: 1, total: 1 })
    );
    std::fs::write(p.join("f"), "resolved\n").unwrap();
    assert!(git_succeeds(p, &["add", "f"]));
    let mut app = App::new(p.to_path_buf()).unwrap();
    app.run_command(crate::widgets::command_palette::Command::GitContinueOperation);
    assert_eq!(crate::git::repo_operation(p), None, "{}", app.status);
    assert_eq!(git_stdout(p, &["symbolic-ref", "--short", "HEAD"]), "feat");
    assert_eq!(
        git_stdout(p, &["log", "--format=%s", "-2"]),
        "feat\nmain",
        "feat replays on top of main"
    );
}

/// #1647: mid-rebase HEAD is detached on the commit being replayed onto,
/// yet the status bar and Source Control name the branch being rebased,
/// for a merge-backend and an apply-backend rebase alike.
#[test]
fn a_rebase_names_the_branch_being_rebased() {
    let tmp = diverged_repo();
    let p = tmp.path();
    assert!(git_succeeds(p, &["checkout", "-q", "feat"]));
    for rebase in [&["rebase", "main"][..], &["rebase", "--apply", "main"][..]] {
        assert!(!git_succeeds(p, rebase), "the rebase must conflict");
        let st = crate::git::query(p);
        assert_eq!(st.head_label(), "feat", "{rebase:?}");
        let pill: String = git_status_spans(&st)
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(pill.contains("feat | REBASING 1/1"), "{rebase:?}: {pill}");
        let mut panel = crate::widgets::source_control::SourceControlPanel::new();
        panel.set_status(st, Vec::new());
        let area = ratatui::layout::Rect::new(0, 0, 60, 12);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut panel, area, &mut buf);
        let panel_text: String = (0..area.height)
            .flat_map(|y| (0..area.width).map(move |x| (x, y)))
            .map(|xy| buf[xy].symbol())
            .collect();
        assert!(panel_text.contains("feat"), "{rebase:?}: {panel_text}");
        assert!(git_succeeds(p, &["rebase", "--abort"]));
    }
}

/// #1647 negative: a plain detached checkout, and a rebase of a detached
/// HEAD (git's `head-name` is then "detached HEAD"), still show the hash.
#[test]
fn a_detached_head_still_shows_its_hash() {
    let tmp = diverged_repo();
    let p = tmp.path();
    assert!(git_succeeds(p, &["checkout", "-q", "--detach", "feat"]));
    let short = git_stdout(p, &["rev-parse", "--short", "HEAD"]);
    assert_eq!(crate::git::query(p).head_label(), short);
    assert!(
        !git_succeeds(p, &["rebase", "main"]),
        "the rebase must conflict"
    );
    let onto = git_stdout(p, &["rev-parse", "--short", "HEAD"]);
    assert_eq!(crate::git::query(p).head_label(), onto);
    assert!(git_succeeds(p, &["rebase", "--abort"]));
    assert!(git_succeeds(p, &["checkout", "-q", "main"]));
    assert_eq!(crate::git::query(p).head_label(), "main");
}

/// #1356 (the #989 path): committing from Source Control mid-rebase
/// carries on with the rebase instead of leaving HEAD detached.
#[test]
fn committing_mid_rebase_continues_the_rebase() {
    let tmp = diverged_repo();
    let p = tmp.path();
    assert!(git_succeeds(p, &["checkout", "-q", "feat"]));
    assert!(!git_succeeds(p, &["rebase", "main"]));
    std::fs::write(p.join("f"), "resolved\n").unwrap();
    assert!(git_succeeds(p, &["add", "f"]));
    let mut app = App::new(p.to_path_buf()).unwrap();
    app.source_control.message = String::from("feat");
    app.commit_source_control();
    wait_for_git_net(&mut app);
    assert_eq!(crate::git::repo_operation(p), None, "{}", app.status);
    assert_eq!(git_stdout(p, &["symbolic-ref", "--short", "HEAD"]), "feat");
}

/// #1356: Commit & Push mid-rebase continues the rebase before pushing,
/// so the push is never made from the rebase's detached HEAD.
#[test]
fn commit_and_push_mid_rebase_continues_the_rebase_first() {
    let tmp = diverged_repo();
    let p = tmp.path();
    assert!(git_succeeds(p, &["checkout", "-q", "feat"]));
    assert!(!git_succeeds(p, &["rebase", "main"]));
    std::fs::write(p.join("f"), "resolved\n").unwrap();
    assert!(git_succeeds(p, &["add", "f"]));
    let mut app = App::new(p.to_path_buf()).unwrap();
    app.source_control.message = String::from("feat");
    app.commit_and_push_source_control();
    wait_for_git_net(&mut app);
    assert_eq!(crate::git::repo_operation(p), None, "{}", app.status);
    assert_eq!(git_stdout(p, &["symbolic-ref", "--short", "HEAD"]), "feat");
}

/// #1356: Skip drops the conflicting commit and finishes the rebase.
#[test]
fn skip_drops_the_conflicting_rebase_commit() {
    let tmp = diverged_repo();
    let p = tmp.path();
    assert!(git_succeeds(p, &["checkout", "-q", "feat"]));
    assert!(!git_succeeds(p, &["rebase", "main"]));
    let mut app = App::new(p.to_path_buf()).unwrap();
    app.run_command(crate::widgets::command_palette::Command::GitSkipOperation);
    assert_eq!(crate::git::repo_operation(p), None, "{}", app.status);
    assert_eq!(std::fs::read_to_string(p.join("f")).unwrap(), "mainline\n");
}

/// #1356: a conflicted cherry-pick is reported, offered Continue and
/// Abort in the menu, and Abort ends it.
#[test]
fn a_conflicted_cherry_pick_can_be_aborted() {
    use crate::git::RepoOp;
    use crate::widgets::scm_menu::{Node, ScmAction};
    let tmp = diverged_repo();
    let p = tmp.path();
    assert!(!git_succeeds(p, &["cherry-pick", "feat"]));
    assert_eq!(crate::git::repo_operation(p), Some(RepoOp::CherryPick));
    let actions: Vec<ScmAction> = crate::widgets::scm_menu::menu_for(Some(RepoOp::CherryPick))
        .iter()
        .filter_map(|n| match n {
            Node::Item(l) => Some(l.action),
            _ => None,
        })
        .collect();
    assert!(actions.contains(&ScmAction::ContinueOperation));
    assert!(actions.contains(&ScmAction::AbortOperation));
    let mut app = App::new(p.to_path_buf()).unwrap();
    app.run_command(crate::widgets::command_palette::Command::GitAbortOperation);
    assert_eq!(crate::git::repo_operation(p), None, "{}", app.status);
    assert_eq!(git_stdout(p, &["status", "--porcelain"]), "");
}

/// #1356: bisect shows as BISECTING, and Abort is Bisect Reset.
#[test]
fn bisect_is_reported_and_reset() {
    use crate::git::RepoOp;
    let tmp = diverged_repo();
    let p = tmp.path();
    assert!(git_succeeds(p, &["bisect", "start", "main", "feat~1"]));
    assert_eq!(crate::git::repo_operation(p), Some(RepoOp::Bisect));
    let mut app = App::new(p.to_path_buf()).unwrap();
    app.run_command(crate::widgets::command_palette::Command::GitAbortOperation);
    assert_eq!(crate::git::repo_operation(p), None, "{}", app.status);
}

/// #1356 negative: with nothing in progress the commands refuse and say
/// so, HEAD stays put, and the menu offers no abort.
#[test]
fn with_nothing_in_progress_abort_and_continue_refuse() {
    use crate::widgets::scm_menu::{Node, ScmAction};
    let tmp = diverged_repo();
    let p = tmp.path();
    let before = git_stdout(p, &["rev-parse", "HEAD"]);
    assert_eq!(crate::git::repo_operation(p), None);
    let mut app = App::new(p.to_path_buf()).unwrap();
    app.run_command(crate::widgets::command_palette::Command::GitAbortOperation);
    assert!(app.status.contains("No merge"), "{}", app.status);
    app.run_command(crate::widgets::command_palette::Command::GitContinueOperation);
    assert!(app.status.contains("No "), "{}", app.status);
    assert_eq!(git_stdout(p, &["rev-parse", "HEAD"]), before);
    assert!(
        !crate::widgets::scm_menu::menu_for(None)
            .iter()
            .any(|n| matches!(
                n,
                Node::Item(l) if l.action == ScmAction::AbortOperation
            ))
    );
}

/// #1356 negative: Continue refuses a merge (a commit concludes one) and
/// leaves it in progress.
#[test]
fn continue_does_not_conclude_a_merge() {
    use crate::git::RepoOp;
    let tmp = diverged_repo();
    let p = tmp.path();
    assert!(!git_succeeds(p, &["merge", "feat"]));
    let mut app = App::new(p.to_path_buf()).unwrap();
    app.run_command(crate::widgets::command_palette::Command::GitContinueOperation);
    assert_eq!(crate::git::repo_operation(p), Some(RepoOp::Merge));
    assert!(app.status.contains("Commit"), "{}", app.status);
}

/// A committed repo whose `main` tracks `<remote>/main` in a bare repo, for
/// each named remote, then a local branch `topic` with one more commit and
/// no upstream (#1245).
fn unpublished_topic_repo(remotes: &[&str]) -> (tempfile::TempDir, Vec<tempfile::TempDir>) {
    let tmp = make_committed_repo();
    let bares: Vec<_> = remotes
        .iter()
        .map(|name| {
            let bare = tempfile::tempdir().unwrap();
            git_stdout(bare.path(), &["init", "-q", "--bare"]);
            git_stdout(
                tmp.path(),
                &["remote", "add", name, bare.path().to_str().unwrap()],
            );
            git_stdout(tmp.path(), &["push", "-q", "-u", name, "main"]);
            bare
        })
        .collect();
    git_stdout(tmp.path(), &["checkout", "-q", "-b", "topic"]);
    std::fs::write(tmp.path().join("seed.txt"), b"two\n").unwrap();
    git_stdout(tmp.path(), &["commit", "-qam", "two"]);
    (tmp, bares)
}

/// Push on a branch nobody has published runs `push -u origin topic`
/// instead of failing with "has no upstream branch".
#[test]
fn push_on_a_branch_with_no_upstream_publishes_it() {
    let (tmp, bares) = unpublished_topic_repo(&["origin"]);
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.push_source_control();
    wait_for_git_net(&mut app);
    assert!(
        !app.source_control.commit_feedback_is_error,
        "{:?}",
        app.source_control.commit_feedback
    );
    assert!(
        app.status.contains("published topic to origin"),
        "{}",
        app.status
    );
    assert_eq!(
        git_stdout(tmp.path(), &["rev-parse", "--abbrev-ref", "topic@{u}"]),
        "origin/topic"
    );
    assert_eq!(
        git_stdout(bares[0].path(), &["rev-parse", "topic"]),
        git_stdout(tmp.path(), &["rev-parse", "HEAD"])
    );
}

/// Commit & Push on an unpublished branch commits, then publishes.
#[test]
fn commit_and_push_on_a_branch_with_no_upstream_publishes_it() {
    let (tmp, bares) = unpublished_topic_repo(&["origin"]);
    std::fs::write(tmp.path().join("seed.txt"), b"three\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.source_control.message = String::from("three");
    app.commit_and_push_source_control();
    wait_for_git_net(&mut app);
    assert!(
        !app.source_control.commit_feedback_is_error,
        "{:?}",
        app.source_control.commit_feedback
    );
    assert_eq!(
        git_stdout(tmp.path(), &["log", "-1", "--format=%s"]),
        "three"
    );
    assert_eq!(
        git_stdout(bares[0].path(), &["rev-parse", "topic"]),
        git_stdout(tmp.path(), &["rev-parse", "HEAD"])
    );
}

/// Sync on an unpublished branch has nothing to pull: it publishes instead
/// of failing on the pull's "no tracking information".
#[test]
fn sync_on_a_branch_with_no_upstream_publishes_without_pulling() {
    let (tmp, bares) = unpublished_topic_repo(&["origin"]);
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.sync_source_control();
    wait_for_git_net(&mut app);
    assert!(
        !app.source_control.commit_feedback_is_error,
        "{:?}",
        app.source_control.commit_feedback
    );
    // One line naming what each half did (#858).
    assert_eq!(
        app.status,
        "Synced: pulled: nothing, branch not published yet | pushed: published topic to origin"
    );
    assert_eq!(
        git_stdout(tmp.path(), &["rev-parse", "--abbrev-ref", "topic@{u}"]),
        "origin/topic"
    );
    assert_eq!(
        git_stdout(bares[0].path(), &["rev-parse", "topic"]),
        git_stdout(tmp.path(), &["rev-parse", "HEAD"])
    );
}

/// With two remotes and no `origin`, publishing would be a guess: Push says
/// so and neither remote gets the branch.
#[test]
fn push_with_several_remotes_and_no_origin_does_not_guess() {
    let (tmp, bares) = unpublished_topic_repo(&["alpha", "beta"]);
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.push_source_control();
    wait_for_git_net(&mut app);
    assert!(app.source_control.commit_feedback_is_error);
    assert!(app.status.contains("remote.pushDefault"), "{}", app.status);
    for bare in &bares {
        assert_eq!(git_stdout(bare.path(), &["branch", "--list", "topic"]), "");
    }
    assert_eq!(
        git_stdout(tmp.path(), &["rev-parse", "--abbrev-ref", "topic@{u}"]),
        ""
    );
}

/// A repo with no remote at all has nowhere to publish to.
#[test]
fn push_with_no_remote_says_there_is_nowhere_to_publish() {
    let (tmp, _) = unpublished_topic_repo(&[]);
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.push_source_control();
    wait_for_git_net(&mut app);
    assert!(app.source_control.commit_feedback_is_error);
    assert!(app.status.contains("no remote"), "{}", app.status);
}

/// A branch that already tracks one still pulls on Sync: the new-branch
/// path must not skip the pull for it.
#[test]
fn sync_on_a_tracked_branch_still_pulls_first() {
    let (tmp, bares) = unpublished_topic_repo(&["origin"]);
    git_stdout(tmp.path(), &["checkout", "-q", "main"]);
    let other = tempfile::tempdir().unwrap();
    git_stdout(
        other.path(),
        &[
            "clone",
            "-q",
            "-b",
            "main",
            bares[0].path().to_str().unwrap(),
            ".",
        ],
    );
    git_stdout(other.path(), &["config", "user.email", "a@b"]);
    git_stdout(other.path(), &["config", "user.name", "a"]);
    std::fs::write(other.path().join("theirs.txt"), b"theirs\n").unwrap();
    git_stdout(other.path(), &["add", "theirs.txt"]);
    git_stdout(other.path(), &["commit", "-qm", "theirs"]);
    git_stdout(other.path(), &["push", "-q", "origin", "main"]);
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.sync_source_control();
    wait_for_git_net(&mut app);
    assert!(
        app.status.starts_with("Synced: pulled: Fast-forward"),
        "{}: {:?}",
        app.status,
        app.source_control.commit_feedback
    );
    assert!(tmp.path().join("theirs.txt").exists());
    assert_eq!(
        git_stdout(bares[0].path(), &["branch", "--list", "topic"]),
        ""
    );
}

/// Restoring a snapshot keeps the version it overwrites, even one never
/// snapshotted (an agent's write) or saved seconds ago (inside the merge
/// window, where the restore used to replace and delete it).
#[test]
fn restoring_a_snapshot_keeps_the_version_it_overwrites() {
    let tmp = tempfile::tempdir().unwrap();
    let hist = tempfile::tempdir().unwrap();
    let f = tmp.path().join("a.rs");
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.history_root = hist.path().to_path_buf();
    let now = super::now_millis();
    crate::history::record_in(&app.history_root, &f, b"v1\n", now - 60_000).unwrap();
    // Saved a second ago, then rewritten by someone else, never snapshotted.
    crate::history::record_in(&app.history_root, &f, b"v2\n", now - 1_000).unwrap();
    std::fs::write(&f, "v3 by an agent\n").unwrap();
    app.history_restore = Some((f.clone(), now - 60_000, b"v1\n".to_vec()));
    app.restore_history_snapshot();
    assert_eq!(std::fs::read(&f).unwrap(), b"v1\n");
    let held: Vec<Vec<u8>> = crate::history::entries_in(&app.history_root, &f)
        .iter()
        .map(|s| std::fs::read(&s.file).unwrap())
        .collect();
    assert!(held.contains(&b"v2\n".to_vec()), "{held:?}");
    assert!(held.contains(&b"v3 by an agent\n".to_vec()), "{held:?}");
}

#[test]
fn dispatching_fetch_records_a_line_in_the_git_output_log() {
    let tmp = make_committed_repo();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.dispatch_scm_action(crate::widgets::scm_menu::ScmAction::Fetch);
    wait_for_git_net(&mut app);
    assert!(
        app.git_output_log.iter().any(|l| l.contains("fetch")),
        "every dispatched git op must append to the git output log; saw {:?}",
        app.git_output_log,
    );
}

#[test]
fn unstage_source_control_entry_runs_git_reset_and_flips_kind_back_to_modified() {
    let tmp = make_committed_repo();
    std::fs::write(tmp.path().join("seed.txt"), b"changed\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let idx = {
        assert!(
            wait_for_changes(&mut app, |a| a
                .source_control
                .entries
                .iter()
                .any(
                    |e| e.path == "seed.txt" && e.kind == crate::git::ChangeKind::Modified
                )),
            "preconditions: seed.txt must appear as Modified"
        );
        app.source_control
            .entries
            .iter()
            .position(|e| e.path == "seed.txt")
            .unwrap()
    };
    app.stage_source_control_entry(idx);
    assert!(
        wait_for_changes(&mut app, |a| a
            .source_control
            .entries
            .iter()
            .any(
                |e| e.path == "seed.txt" && e.kind == crate::git::ChangeKind::StagedModified
            )),
        "preconditions: seed.txt must be staged before we can unstage it"
    );
    let staged_idx = app
        .source_control
        .entries
        .iter()
        .position(|e| e.path == "seed.txt" && e.kind == crate::git::ChangeKind::StagedModified)
        .unwrap();
    app.unstage_source_control_entry(staged_idx);
    assert!(
        wait_for_changes(&mut app, |a| a
            .source_control
            .entries
            .iter()
            .any(
                |e| e.path == "seed.txt" && e.kind == crate::git::ChangeKind::Modified
            )),
        "after unstage the file must read as Modified again; saw {:?}",
        app.source_control.entries,
    );
}

#[test]
fn request_discard_source_control_entry_only_opens_a_confirm_modal_without_touching_disk() {
    let tmp = make_committed_repo();
    std::fs::write(tmp.path().join("seed.txt"), b"changed\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    wait_for_changes(&mut app, |a| {
        a.source_control
            .entries
            .iter()
            .any(|e| e.path == "seed.txt")
    });
    let idx = app
        .source_control
        .entries
        .iter()
        .position(|e| e.path == "seed.txt")
        .unwrap();
    app.request_discard_source_control_entry(idx);
    assert!(
        app.pending_discard.is_some(),
        "Discard click must request user confirmation rather than discarding immediately"
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("seed.txt")).unwrap(),
        "changed\n",
        "request alone must not touch the working tree"
    );
}

#[test]
fn confirming_discard_for_modified_file_restores_head_content() {
    let tmp = make_committed_repo();
    std::fs::write(tmp.path().join("seed.txt"), b"changed\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    wait_for_changes(&mut app, |a| {
        a.source_control
            .entries
            .iter()
            .any(|e| e.path == "seed.txt")
    });
    let idx = app
        .source_control
        .entries
        .iter()
        .position(|e| e.path == "seed.txt")
        .unwrap();
    app.request_discard_source_control_entry(idx);
    app.confirm_pending_discard();
    assert!(
        app.pending_discard.is_none(),
        "modal must close after confirm"
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("seed.txt")).unwrap(),
        "seed\n",
        "discard must restore HEAD content of a Modified file"
    );
}

#[test]
fn confirming_discard_for_untracked_file_deletes_it_from_disk() {
    let tmp = make_committed_repo();
    let new_file = tmp.path().join("scratch.txt");
    std::fs::write(&new_file, b"untracked\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    wait_for_changes(&mut app, |a| {
        a.source_control
            .entries
            .iter()
            .any(|e| e.path == "scratch.txt" && e.kind == crate::git::ChangeKind::Untracked)
    });
    let idx = app
        .source_control
        .entries
        .iter()
        .position(|e| e.path == "scratch.txt")
        .unwrap();
    app.request_discard_source_control_entry(idx);
    assert!(matches!(
        app.pending_discard.as_ref(),
        Some(pd) if pd.untracked
    ));
    app.confirm_pending_discard();
    assert!(
        !new_file.exists(),
        "discard on an Untracked entry must delete the file from disk"
    );
}

#[test]
fn cancel_pending_discard_clears_modal_without_acting() {
    let tmp = make_committed_repo();
    std::fs::write(tmp.path().join("seed.txt"), b"changed\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    wait_for_changes(&mut app, |a| {
        a.source_control
            .entries
            .iter()
            .any(|e| e.path == "seed.txt")
    });
    let idx = app
        .source_control
        .entries
        .iter()
        .position(|e| e.path == "seed.txt")
        .unwrap();
    app.request_discard_source_control_entry(idx);
    app.cancel_pending_discard();
    assert!(app.pending_discard.is_none());
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("seed.txt")).unwrap(),
        "changed\n",
        "cancel must NOT touch the working tree"
    );
}

#[test]
fn clicking_the_stage_icon_on_a_selected_unstaged_row_stages_that_entry() {
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
    let backend = ratatui::backend::TestBackend::new(80, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let idx = app
        .source_control
        .entries
        .iter()
        .position(|e| e.path == "seed.txt")
        .unwrap();
    app.source_control.selected_change = Some(idx);
    term.draw(|f| app.render(f)).unwrap();
    let actions = app
        .source_control
        .last_row_actions
        .expect("selected unstaged row must expose action rects");
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: actions.stage.x,
        row: actions.stage.y,
        modifiers: KeyModifiers::NONE,
    });
    assert!(
        wait_for_changes(&mut app, |a| a
            .source_control
            .entries
            .iter()
            .any(
                |e| e.path == "seed.txt" && e.kind == crate::git::ChangeKind::StagedModified
            )),
        "clicking the stage cell must run `git add` against that entry"
    );
}

#[test]
fn clicking_the_discard_icon_on_a_selected_unstaged_row_opens_the_confirm_modal() {
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
    let backend = ratatui::backend::TestBackend::new(80, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let idx = app
        .source_control
        .entries
        .iter()
        .position(|e| e.path == "seed.txt")
        .unwrap();
    app.source_control.selected_change = Some(idx);
    term.draw(|f| app.render(f)).unwrap();
    let actions = app
        .source_control
        .last_row_actions
        .expect("selected unstaged row must expose action rects");
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: actions.discard.x,
        row: actions.discard.y,
        modifiers: KeyModifiers::NONE,
    });
    assert!(
        app.pending_discard.is_some(),
        "discard click must arm the confirm modal (NEVER discard silently)"
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("seed.txt")).unwrap(),
        "changed\n",
        "discard click alone must NOT touch the working tree until the user confirms"
    );
}

#[test]
fn cmd_a_in_source_control_pane_selects_every_change_row() {
    let tmp = make_committed_repo();
    std::fs::write(tmp.path().join("seed.txt"), b"changed\n").unwrap();
    std::fs::write(tmp.path().join("scratch.txt"), b"new\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    wait_for_changes(&mut app, |a| a.source_control.entries.len() >= 2);
    app.set_sidebar_view(SidebarView::SourceControl);
    let n = app.source_control.entries.len();
    app.handle_source_control_key(key(KeyCode::Char('a'), KeyModifiers::SUPER));
    assert_eq!(
        app.source_control.multi_selection.len(),
        n,
        "Cmd+A in SC must populate multi_selection with every entry"
    );
}

#[test]
fn cmd_s_through_handle_key_dispatches_to_source_control_not_to_editor_save() {
    // Regression for the user's "Cmd+S didn't react" report: the
    // global `is_save_key` intercept used to fire BEFORE the SC
    // handler, so Cmd+S called `App::save` (a no-op when no editor
    // file was open) and the user's selected change never moved to
    // the index. Route through `handle_key` (not `handle_source_control_key`)
    // to exercise the full dispatcher.
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
    app.source_control.select_all_changes();
    app.handle_key(key(KeyCode::Char('s'), KeyModifiers::SUPER))
        .unwrap();
    assert!(
        wait_for_changes(&mut app, |a| a
            .source_control
            .entries
            .iter()
            .any(
                |e| e.path == "seed.txt" && e.kind.section() == crate::git::ChangeSection::Staged
            )),
        "Cmd+S routed through handle_key must reach the SC stage handler, not the global save key intercept"
    );
}

#[test]
fn staging_a_filename_with_a_space_works_after_porcelain_dequoting() {
    // Regression for "fatal: pathspec '\"linked_list copy.py\"' did
    // not match any files": porcelain output wraps filenames with
    // spaces in double quotes, and we used to feed those quotes
    // straight into `git add`. The parser now dequotes; the staging
    // path must end at the real filename.
    let tmp = make_committed_repo();
    std::fs::write(tmp.path().join("linked_list copy.py"), b"x\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    wait_for_changes(&mut app, |a| {
        a.source_control
            .entries
            .iter()
            .any(|e| e.path == "linked_list copy.py")
    });
    assert!(
        !app.source_control
            .entries
            .iter()
            .any(|e| e.path.starts_with('"') || e.path.ends_with('"')),
        "no entry's path may still carry the porcelain quoting wrapper"
    );
    let idx = app
        .source_control
        .entries
        .iter()
        .position(|e| e.path == "linked_list copy.py")
        .unwrap();
    app.stage_source_control_entry(idx);
    assert!(
        wait_for_changes(&mut app, |a| a
            .source_control
            .entries
            .iter()
            .any(|e| e.path == "linked_list copy.py"
                && e.kind.section() == crate::git::ChangeSection::Staged)),
        "Staging a file whose name contains a space must succeed; saw status={:?}",
        app.status,
    );
}

#[test]
fn cmd_s_in_source_control_pane_stages_every_multi_selected_entry() {
    let tmp = make_committed_repo();
    std::fs::write(tmp.path().join("seed.txt"), b"changed\n").unwrap();
    std::fs::write(tmp.path().join("scratch.txt"), b"new\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    wait_for_changes(&mut app, |a| a.source_control.entries.len() >= 2);
    app.set_sidebar_view(SidebarView::SourceControl);
    app.source_control.select_all_changes();
    app.handle_source_control_key(key(KeyCode::Char('s'), KeyModifiers::SUPER));
    assert!(
        wait_for_changes(&mut app, |a| {
            let kinds: Vec<_> = a
                .source_control
                .entries
                .iter()
                .map(|e| e.kind.section())
                .collect();
            !kinds.is_empty()
                && kinds
                    .iter()
                    .all(|s| *s == crate::git::ChangeSection::Staged)
        }),
        "Cmd+S with multi-selection must stage every selected entry; saw {:?}",
        app.source_control.entries,
    );
}

#[test]
fn cmd_s_with_single_selection_stages_only_that_entry() {
    let tmp = make_committed_repo();
    std::fs::write(tmp.path().join("seed.txt"), b"changed\n").unwrap();
    std::fs::write(tmp.path().join("scratch.txt"), b"new\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    wait_for_changes(&mut app, |a| a.source_control.entries.len() >= 2);
    app.set_sidebar_view(SidebarView::SourceControl);
    let scratch_idx = app
        .source_control
        .entries
        .iter()
        .position(|e| e.path == "scratch.txt")
        .unwrap();
    // Single click resets multi_selection to just this row.
    app.source_control.multi_selection.clear();
    app.source_control.multi_selection.insert(scratch_idx);
    app.source_control.selected_change = Some(scratch_idx);
    app.handle_source_control_key(key(KeyCode::Char('s'), KeyModifiers::SUPER));
    assert!(
        wait_for_changes(&mut app, |a| a
            .source_control
            .entries
            .iter()
            .any(|e| e.path == "scratch.txt"
                && e.kind.section() == crate::git::ChangeSection::Staged)),
        "scratch.txt must have been staged"
    );
    assert!(
        app.source_control
            .entries
            .iter()
            .any(|e| e.path == "seed.txt" && e.kind == crate::git::ChangeKind::Modified),
        "seed.txt must remain unstaged (single-row stage scope)"
    );
}

/// #871: `seed.txt` changed and `scratch.txt` untracked, Source Control
/// open on them.
fn scm_with_two_changes() -> (tempfile::TempDir, App) {
    let tmp = make_committed_repo();
    std::fs::write(tmp.path().join("seed.txt"), b"changed\n").unwrap();
    std::fs::write(tmp.path().join("scratch.txt"), b"new\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    wait_for_changes(&mut app, |a| a.source_control.entries.len() >= 2);
    app.set_sidebar_view(SidebarView::SourceControl);
    (tmp, app)
}

fn scm_kind_of(app: &App, path: &str) -> Option<crate::git::ChangeKind> {
    let entries = &app.source_control.entries;
    entries.iter().find(|e| e.path == path).map(|e| e.kind)
}

/// #871: Down from the message box steps into the change list, arrows move
/// the selection, and Enter opens that row's diff. Down used to scroll the
/// panel and Enter to commit ("Empty commit message").
#[test]
fn arrows_and_enter_in_source_control_open_the_selected_changes_diff() {
    let (_tmp, mut app) = scm_with_two_changes();
    for code in [KeyCode::Down, KeyCode::Down, KeyCode::Up, KeyCode::Enter] {
        app.handle_key(key(code, KeyModifiers::NONE)).unwrap();
    }
    let diff = app.editor.diff.as_ref().expect("Enter opens a diff");
    assert!(
        diff.right_path.ends_with("seed.txt"),
        "{:?}",
        diff.right_path
    );
    assert!(app.focus == Pane::Editor);
}

/// #871: in the list `s` stages the selected row, `u` unstages it and `d`
/// asks before discarding it, each staying on the file it moved.
#[test]
fn list_keys_stage_unstage_and_discard_the_selected_change() {
    use crate::git::ChangeKind;
    let (tmp, mut app) = scm_with_two_changes();
    for code in [KeyCode::Tab, KeyCode::Down, KeyCode::Char('s')] {
        app.handle_key(key(code, KeyModifiers::NONE)).unwrap();
    }
    assert!(wait_for_changes(&mut app, |a| {
        scm_kind_of(a, "scratch.txt") == Some(ChangeKind::StagedAdded)
    }));
    assert_eq!(scm_kind_of(&app, "seed.txt"), Some(ChangeKind::Modified));
    app.handle_key(key(KeyCode::Char('u'), KeyModifiers::NONE))
        .unwrap();
    assert!(wait_for_changes(&mut app, |a| {
        scm_kind_of(a, "scratch.txt") == Some(ChangeKind::Untracked)
    }));
    app.handle_key(key(KeyCode::Char('d'), KeyModifiers::NONE))
        .unwrap();
    let pending = app.pending_discard.as_ref().expect("d asks first");
    assert_eq!(pending.rel_path, "scratch.txt");
    assert!(tmp.path().join("scratch.txt").exists());
    assert!(app.source_control.message.is_empty(), "no key was typed");
}

/// #871: a partly staged file has a row in Staged and one in Changes; `s`
/// on the Staged row and `u` on the Changes row leave both halves where
/// they are, as the mouse's row actions do, instead of `git add` or `git
/// reset` on the whole file.
#[test]
fn list_keys_keep_a_partly_staged_files_halves_apart() {
    use crate::git::ChangeSection;
    let tmp = make_committed_repo();
    let git_ok = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(args)
            .status()
            .unwrap()
            .success()
    };
    std::fs::write(tmp.path().join("seed.txt"), b"staged\n").unwrap();
    assert!(git_ok(&["add", "seed.txt"]));
    std::fs::write(tmp.path().join("seed.txt"), b"staged\nunstaged\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    wait_for_changes(&mut app, |a| a.source_control.entries.len() >= 2);
    app.set_sidebar_view(SidebarView::SourceControl);
    app.handle_key(key(KeyCode::Tab, KeyModifiers::NONE))
        .unwrap();
    let row = |app: &App, section| {
        let entries = &app.source_control.entries;
        entries.iter().position(|e| e.kind.section() == section)
    };
    app.source_control.selected_change = row(&app, ChangeSection::Staged);
    app.handle_key(key(KeyCode::Char('s'), KeyModifiers::NONE))
        .unwrap();
    assert!(!git_ok(&["diff", "--quiet"]), "s staged the unstaged half");
    app.source_control.selected_change = row(&app, ChangeSection::Changes);
    app.handle_key(key(KeyCode::Char('u'), KeyModifiers::NONE))
        .unwrap();
    assert!(
        !git_ok(&["diff", "--cached", "--quiet"]),
        "u unstaged the staged half"
    );
}

/// #871: the Commit button's and menus' actions are on the palette, so
/// they need no click: Git: Checkout to… opens the branch picker, Git:
/// Stash asks for a name, and Git: Commit commits the message.
#[test]
fn git_checkout_stash_and_commit_run_from_the_palette() {
    use crate::widgets::command_palette::Command;
    let (tmp, mut app) = scm_ready_to_commit();
    let run = |app: &mut App, id: &str| app.run_command(Command::from_id(id).unwrap());
    run(&mut app, "git_checkout_to");
    assert_eq!(app.branch_purpose, crate::app::BranchPurpose::Checkout);
    assert!(
        app.branch_picker.take().is_some(),
        "the branch picker opens"
    );
    run(&mut app, "git_stash");
    assert!(
        app.input_prompt.take().is_some(),
        "the stash name prompt opens"
    );
    app.source_control.message = String::from("from the palette");
    run(&mut app, "git_commit");
    wait_for_git_net(&mut app);
    assert_eq!(last_commit_subject(tmp.path()), "from the palette");
}

#[test]
fn cmd_enter_in_source_control_falls_back_to_plain_commit_without_pushing() {
    // iTerm2 swallows Cmd+Enter as Toggle Fullscreen so the chord never
    // reaches croft. Terminals that DO deliver it should treat Cmd as
    // a no-op modifier — Enter alone commits, Ctrl+Enter pushes. The
    // SC handler's plain `KeyCode::Enter` match arm catches the
    // residual Cmd+Enter and calls `commit_source_control` (no push).
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
    app.source_control.message = "fix: plain cmd-enter".to_string();
    app.source_control.message_cursor = app.source_control.message.chars().count();
    app.handle_source_control_key(key(KeyCode::Enter, KeyModifiers::SUPER));
    wait_for_git_net(&mut app);
    let log = std::process::Command::new("git")
        .args(["-C"])
        .arg(tmp.path())
        .args(["log", "--oneline"])
        .output()
        .unwrap();
    let log_out = String::from_utf8_lossy(&log.stdout);
    assert!(
        log_out.contains("fix: plain cmd-enter"),
        "Cmd+Enter must still commit locally (push is bound to Ctrl+Enter only): log={log_out:?}"
    );
}

/// #1438: Commit with an unsaved tab in the repository asks first, and
/// commits nothing until an answer is given.
#[test]
fn commit_with_an_unsaved_tab_in_the_repo_asks_before_committing() {
    let (tmp, mut app) = scm_with_unsaved_tab();
    let head = git_out(tmp.path(), &["rev-parse", "HEAD"]);
    app.handle_source_control_key(key(KeyCode::Enter, KeyModifiers::NONE));
    wait_for_git_net(&mut app);
    let pending = app.pending_unsaved_scm.as_ref().expect("the prompt opens");
    assert_eq!(pending.op, ScmWrite::Commit);
    assert_eq!(pending.files, vec![tmp.path().join("seed.txt")]);
    assert_eq!(
        git_out(tmp.path(), &["rev-parse", "HEAD"]),
        head,
        "no commit yet"
    );
    let rendered = screen_text_863(&mut app, 120, 40);
    assert!(rendered.contains("UNSAVED CHANGES"), "{rendered}");
    assert!(rendered.contains("Save All & Commit"), "{rendered}");
    assert!(rendered.contains("seed.txt"), "{rendered}");
}

/// #1438: Commit Anyway keeps the old behaviour: the disk text is
/// committed and the tab stays unsaved.
#[test]
fn commit_anyway_commits_the_disk_text_and_leaves_the_tab_unsaved() {
    let (tmp, mut app) = scm_with_unsaved_tab();
    app.handle_source_control_key(key(KeyCode::Enter, KeyModifiers::NONE));
    app.handle_key(key(KeyCode::Char('c'), KeyModifiers::NONE))
        .unwrap();
    wait_for_git_net(&mut app);
    assert!(app.pending_unsaved_scm.is_none());
    assert_eq!(git_out(tmp.path(), &["show", "HEAD:seed.txt"]), "changed\n");
    assert!(app.editor.dirty, "Commit Anyway saves nothing");
}

/// #1438: Esc closes the prompt with no commit and the edit still unsaved.
#[test]
fn cancelling_the_unsaved_prompt_commits_nothing() {
    let (tmp, mut app) = scm_with_unsaved_tab();
    let head = git_out(tmp.path(), &["rev-parse", "HEAD"]);
    app.handle_source_control_key(key(KeyCode::Enter, KeyModifiers::NONE));
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    wait_for_git_net(&mut app);
    assert!(app.pending_unsaved_scm.is_none());
    assert_eq!(git_out(tmp.path(), &["rev-parse", "HEAD"]), head);
    assert!(app.editor.dirty);
    assert_eq!(
        app.source_control.message, "bump seed",
        "the message is kept"
    );
}

/// #1438: an unsaved tab outside the repository is not the commit's
/// business, so the commit runs straight away.
#[test]
fn an_unsaved_tab_outside_the_repo_does_not_hold_the_commit() {
    let (tmp, mut app) = scm_ready_to_commit();
    let elsewhere = tempfile::tempdir().unwrap();
    let other = elsewhere.path().join("scratch.txt");
    std::fs::write(&other, "scratch\n").unwrap();
    app.editor.open_pinned(&other).unwrap();
    app.editor.insert_str("more ");
    assert!(app.editor.dirty);
    app.source_control.message = "bump seed".to_string();
    app.source_control.message_cursor = app.source_control.message.chars().count();
    app.handle_source_control_key(key(KeyCode::Enter, KeyModifiers::NONE));
    wait_for_git_net(&mut app);
    assert!(app.pending_unsaved_scm.is_none(), "no prompt");
    assert_eq!(git_out(tmp.path(), &["show", "HEAD:seed.txt"]), "changed\n");
    assert_eq!(last_commit_message(tmp.path()), "bump seed\n\n");
}

/// #1438: the prompt's write stays bound to the repository it was asked
/// for. When Source Control has moved to another root by the time the
/// choice is made (a click on another root's tab), nothing is committed.
#[test]
fn the_unsaved_prompt_does_not_commit_after_source_control_moved_repos() {
    let (tmp, mut app) = scm_with_unsaved_tab();
    let head = git_out(tmp.path(), &["rev-parse", "HEAD"]);
    app.handle_source_control_key(key(KeyCode::Enter, KeyModifiers::NONE));
    let other = tempfile::tempdir().unwrap();
    app.pending_unsaved_scm
        .as_mut()
        .expect("the prompt opens")
        .root = other.path().to_path_buf();
    app.handle_key(key(KeyCode::Char('c'), KeyModifiers::NONE))
        .unwrap();
    wait_for_git_net(&mut app);
    assert!(app.pending_unsaved_scm.is_none());
    assert_eq!(git_out(tmp.path(), &["rev-parse", "HEAD"]), head);
    assert!(
        app.status.contains("moved to another repository"),
        "{}",
        app.status
    );
}

/// #1438: an empty message is refused before anything is asked.
#[test]
fn an_empty_message_is_refused_before_the_unsaved_prompt() {
    let (_tmp, mut app) = scm_with_unsaved_tab();
    app.source_control.message.clear();
    app.source_control.message_cursor = 0;
    app.handle_source_control_key(key(KeyCode::Enter, KeyModifiers::NONE));
    assert!(app.pending_unsaved_scm.is_none());
    assert_eq!(
        app.source_control.commit_feedback.as_deref(),
        Some("Empty commit message")
    );
}

fn stash_names(root: &std::path::Path) -> String {
    git_out(root, &["stash", "list", "--format=%gs"])
}

/// #1581: Stash asks what to call the stash, seeded with the commit box's
/// first line, and the stash takes that name; nothing is stashed until the
/// prompt is answered.
#[test]
fn stash_asks_for_a_name_seeded_from_the_commit_box() {
    let (tmp, mut app) = scm_ready_to_commit();
    app.source_control.message = "retry tweak\n\nlonger body".to_string();
    app.stash_source_control();
    let prompt = app.input_prompt.as_ref().expect("the name prompt opens");
    assert_eq!(
        prompt.purpose,
        crate::widgets::input_prompt::InputPurpose::StashMessage {
            kind: crate::git::StashKind::Tracked
        }
    );
    assert_eq!(prompt.value, "retry tweak");
    assert_eq!(stash_names(tmp.path()), "", "nothing stashed yet");
    app.submit_input_prompt();
    assert!(app.input_prompt.is_none());
    assert_eq!(stash_names(tmp.path()), "On main: retry tweak\n");
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("seed.txt")).unwrap(),
        "seed\n",
        "the work was put aside"
    );

    // Include Untracked asks too, and takes a typed name.
    std::fs::write(tmp.path().join("new.txt"), b"fresh\n").unwrap();
    app.source_control.message.clear();
    app.stash_include_untracked_source_control();
    let prompt = app.input_prompt.as_mut().expect("the -u prompt opens");
    assert_eq!(prompt.value, "", "an empty commit box seeds nothing");
    for c in "spike".chars() {
        prompt.push_char(c);
    }
    app.submit_input_prompt();
    assert_eq!(
        stash_names(tmp.path()),
        "On main: spike\nOn main: retry tweak\n"
    );
    assert!(!tmp.path().join("new.txt").exists(), "-u took the new file");
}

/// Negative (#1581): a blank name keeps git's `WIP on` name, and Esc
/// stashes nothing at all.
#[test]
fn a_blank_stash_name_is_wip_and_esc_stashes_nothing() {
    let (tmp, mut app) = scm_ready_to_commit();
    app.source_control.message.clear();
    app.stash_source_control();
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert!(app.input_prompt.is_none(), "Esc closes the prompt");
    assert_eq!(stash_names(tmp.path()), "", "Esc stashes nothing");
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("seed.txt")).unwrap(),
        "changed\n",
        "the work is untouched"
    );
    app.stash_source_control();
    app.submit_input_prompt();
    let names = stash_names(tmp.path());
    assert!(names.starts_with("WIP on main: "), "{names}");
}

/// #1581: View Stash lists each stash with its age and files, and opening
/// one shows its patch in a tab without applying or dropping it.
#[test]
fn view_stash_lists_files_and_opens_the_patch() {
    let (tmp, mut app) = scm_ready_to_commit();
    std::fs::write(tmp.path().join("seed.txt"), b"retries = 5\n").unwrap();
    crate::git::stash_push(tmp.path(), Some("retries")).unwrap();
    std::fs::write(tmp.path().join("other.txt"), b"x\n").unwrap();
    crate::git::stash_push_untracked(tmp.path(), Some("other")).unwrap();
    app.run_command(crate::widgets::command_palette::Command::GitViewStash);
    let picker = app.list_picker.as_ref().expect("the picker opens");
    assert_eq!(picker.title, "View Stash");
    let labels: Vec<&str> = picker.rows.iter().map(|r| r.label.as_str()).collect();
    assert!(
        labels[0].starts_with("stash@{0}: On main: other \u{b7} ")
            && labels[0].ends_with(" \u{b7} other.txt"),
        "{labels:?}"
    );
    assert!(labels[1].ends_with(" \u{b7} seed.txt"), "{labels:?}");
    app.list_picker.as_mut().unwrap().select_next();
    app.confirm_list_picker();
    crate::test_budget::await_spawned(
        std::time::Duration::from_secs(10),
        "the stash patch",
        || {
            app.drain_git_view();
            app.git_view_job.is_none()
        },
    );
    assert_eq!(app.status, "Opened stash@{1}");
    assert_eq!(app.editor.path.as_deref(), Some(Path::new("stash@{1}")));
    let body = app.editor.lines.join("\n");
    assert!(body.contains("+retries = 5"), "{body}");
    assert_eq!(
        stash_names(tmp.path()).lines().count(),
        2,
        "nothing dropped"
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("seed.txt")).unwrap(),
        "seed\n",
        "nothing applied"
    );
}

/// Negative (#1581): with no stashes, View Stash says so and opens nothing.
#[test]
fn view_stash_without_stashes_opens_nothing() {
    let (_tmp, mut app) = scm_ready_to_commit();
    app.run_command(crate::widgets::command_palette::Command::GitViewStash);
    assert!(app.list_picker.is_none());
    assert_eq!(app.status, "No stashes to choose");
    assert!(app.git_view_job.is_none());
}

/// #1128 fixture: a repository with two stashes, `stash@{0}: quick
/// experiment` and `stash@{1}: WIP payment retries`, and the Drop Stash
/// picker open on the second.
fn drop_stash_picker_1128() -> (tempfile::TempDir, App) {
    let tmp = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(tmp.path())
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@example.com"]);
    git(&["config", "user.name", "t"]);
    std::fs::write(tmp.path().join("billing.py"), "v1\n").unwrap();
    git(&["add", "billing.py"]);
    git(&["commit", "-qm", "billing"]);
    std::fs::write(tmp.path().join("billing.py"), "half-finished refactor\n").unwrap();
    git(&["stash", "push", "-q", "-m", "WIP payment retries"]);
    std::fs::write(tmp.path().join("billing.py"), "v2\n").unwrap();
    git(&["stash", "push", "-q", "-m", "quick experiment"]);
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.open_stash_picker(crate::widgets::list_picker::ListPurpose::StashDrop);
    app.list_picker.as_mut().unwrap().select_next();
    (tmp, app)
}

fn stash_messages_1128(root: &Path) -> Vec<String> {
    crate::git::list_stashes(root)
        .unwrap()
        .into_iter()
        .map(|s| s.message)
        .collect()
}

/// #1128: picking a stash in Drop Stash asks first, naming it; Enter does
/// not answer (a second stray Enter must not drop it), and only Y drops.
#[test]
fn drop_stash_asks_before_it_deletes_the_stash() {
    let (tmp, mut app) = drop_stash_picker_1128();
    app.confirm_list_picker();
    assert_eq!(
        stash_messages_1128(tmp.path()).len(),
        2,
        "nothing dropped yet"
    );
    let screen = draw_screen(&mut app);
    assert!(screen.contains("DROP STASH?"), "{screen}");
    assert!(screen.contains("WIP payment retries"), "{screen}");
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(stash_messages_1128(tmp.path()).len(), 2, "Enter keeps it");
    app.handle_key(key(KeyCode::Char('y'), KeyModifiers::NONE))
        .unwrap();
    let left = stash_messages_1128(tmp.path());
    assert_eq!(left.len(), 1, "{left:?}");
    assert!(
        left[0].contains("quick experiment"),
        "the picked one went: {left:?}"
    );
    assert!(app.status.starts_with("Dropped stash"), "{}", app.status);
    assert!(!draw_screen(&mut app).contains("DROP STASH?"));
}

/// #1128 negative: N or Esc closes the question and keeps every stash.
#[test]
fn drop_stash_answered_no_keeps_the_stash() {
    for answer in [KeyCode::Char('n'), KeyCode::Esc] {
        let (tmp, mut app) = drop_stash_picker_1128();
        app.confirm_list_picker();
        app.handle_key(key(answer, KeyModifiers::NONE)).unwrap();
        assert_eq!(stash_messages_1128(tmp.path()).len(), 2, "{answer:?}");
        assert_eq!(app.status, "Kept the stash");
        assert!(!draw_screen(&mut app).contains("DROP STASH?"), "{answer:?}");
    }
}

/// Runs `git` in `root` for the #1128 tests, failing the test on error.
fn git_1128(root: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

/// #1128: only a plain (or shifted) Y answers. Ctrl+Shift+Y is terminal copy
/// mode, and pressing it with the question up must not drop the stash.
#[test]
fn drop_stash_ignores_a_modified_y() {
    let (tmp, mut app) = drop_stash_picker_1128();
    app.confirm_list_picker();
    app.handle_key(key(
        KeyCode::Char('Y'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert_eq!(
        stash_messages_1128(tmp.path()).len(),
        2,
        "Ctrl+Shift+Y kept it"
    );
    assert!(
        draw_screen(&mut app).contains("DROP STASH?"),
        "still asking"
    );
    app.handle_key(key(KeyCode::Char('Y'), KeyModifiers::SHIFT))
        .unwrap();
    let left = stash_messages_1128(tmp.path());
    assert_eq!(left.len(), 1, "Shift+Y answers: {left:?}");
    assert!(left[0].contains("quick experiment"), "{left:?}");
}

/// #1128: a stash pushed while the question is up shifts every
/// `stash@{N}`; Y still drops the stash the question named, not the one
/// that slid into its old index.
#[test]
fn drop_stash_drops_the_named_stash_after_the_list_moves() {
    let (tmp, mut app) = drop_stash_picker_1128();
    app.confirm_list_picker();
    std::fs::write(tmp.path().join("billing.py"), "v3\n").unwrap();
    git_1128(
        tmp.path(),
        &["stash", "push", "-q", "-m", "pushed meanwhile"],
    );
    app.handle_key(key(KeyCode::Char('y'), KeyModifiers::NONE))
        .unwrap();
    let left = stash_messages_1128(tmp.path());
    assert_eq!(left.len(), 2, "{left:?}");
    assert!(
        left.iter().all(|m| !m.contains("WIP payment retries")),
        "the named stash went: {left:?}"
    );
    assert!(
        left.iter().any(|m| m.contains("quick experiment")),
        "the one now at its old index stayed: {left:?}"
    );
    assert!(app.status.starts_with("Dropped stash"), "{}", app.status);
}

/// #1128 negative: when the named stash is dropped elsewhere while the
/// question is up, Y drops nothing and says why.
#[test]
fn drop_stash_refuses_when_the_named_stash_is_gone() {
    let (tmp, mut app) = drop_stash_picker_1128();
    app.confirm_list_picker();
    git_1128(tmp.path(), &["stash", "drop", "-q", "stash@{1}"]);
    app.handle_key(key(KeyCode::Char('y'), KeyModifiers::NONE))
        .unwrap();
    let left = stash_messages_1128(tmp.path());
    assert_eq!(left.len(), 1, "nothing else dropped: {left:?}");
    assert!(left[0].contains("quick experiment"), "{left:?}");
    assert!(
        app.status.contains("no longer in the stash list"),
        "{}",
        app.status
    );
}

/// A pasted multi-line message is committed with its subject, blank line
/// and body intact, not glued into one line.
#[test]
fn a_pasted_multi_line_message_commits_with_its_body() {
    let (tmp, mut app) = scm_ready_to_commit();
    app.handle_paste("Fix rounding\r\n\r\nTotals were floats\r\nFixes #42");
    app.handle_source_control_key(key(KeyCode::Enter, KeyModifiers::NONE));
    wait_for_git_net(&mut app);
    assert_eq!(
        last_commit_message(tmp.path()),
        "Fix rounding\n\nTotals were floats\nFixes #42\n\n"
    );
}

/// #1334: in the Source Control box Home/End work on the caret's line and
/// Ctrl+Home/Ctrl+End on the whole message.
#[test]
fn scm_home_end_follow_the_carets_line_and_ctrl_takes_the_whole_message() {
    let (_tmp, mut app) = scm_ready_to_commit();
    app.handle_paste("Add rounding\n\nRounds to cents.\nFixes 42");
    let press = |app: &mut App, code: KeyCode, mods: KeyModifiers| {
        app.handle_source_control_key(key(code, mods));
    };
    press(&mut app, KeyCode::Up, KeyModifiers::NONE);
    press(&mut app, KeyCode::Home, KeyModifiers::NONE);
    press(&mut app, KeyCode::Char('['), KeyModifiers::NONE);
    press(&mut app, KeyCode::End, KeyModifiers::NONE);
    press(&mut app, KeyCode::Char(']'), KeyModifiers::NONE);
    assert_eq!(
        app.source_control.message,
        "Add rounding\n\n[Rounds to cents.]\nFixes 42"
    );
    press(&mut app, KeyCode::Home, KeyModifiers::CONTROL);
    press(&mut app, KeyCode::Char('>'), KeyModifiers::NONE);
    press(&mut app, KeyCode::End, KeyModifiers::CONTROL);
    press(&mut app, KeyCode::Char('.'), KeyModifiers::NONE);
    assert_eq!(
        app.source_control.message,
        ">Add rounding\n\n[Rounds to cents.]\nFixes 42."
    );
}

/// #1336: finishing a conflicted `git revert` from Source Control commits
/// git's whole message, "This reverts commit <sha>." included, as
/// `git revert --continue` would.
#[test]
fn finishing_a_conflicted_revert_commits_gits_whole_message() {
    let tmp = make_committed_repo();
    let root = tmp.path();
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .output()
            .unwrap()
    };
    std::fs::write(root.join("seed.txt"), b"rate = 2\n").unwrap();
    git(&["commit", "-qam", "Raise rate", "-m", "Body explaining why."]);
    let reverted = git_stdout(root, &["rev-parse", "HEAD"]);
    std::fs::write(root.join("seed.txt"), b"rate = 3\n").unwrap();
    git(&["commit", "-qam", "Raise rate again"]);
    let out = git(&["revert", "--no-edit", "HEAD~1"]);
    assert!(
        !out.status.success(),
        "staging: the revert stops on a conflict"
    );
    std::fs::write(root.join("seed.txt"), b"rate = 1\n").unwrap();
    git(&["add", "seed.txt"]);
    let mut app = App::new(root.to_path_buf()).unwrap();
    wait_for_changes(&mut app, |a| !a.source_control.message.is_empty());
    app.set_sidebar_view(SidebarView::SourceControl);
    let expected = format!("Revert \"Raise rate\"\n\nThis reverts commit {reverted}.");
    assert_eq!(app.source_control.message, expected);
    app.handle_source_control_key(key(KeyCode::Enter, KeyModifiers::NONE));
    wait_for_git_net(&mut app);
    assert_eq!(last_commit_message(root), format!("{expected}\n\n"));
}

/// Shift+Enter and Alt+Enter start a new line; neither commits.
#[test]
fn shift_or_alt_enter_adds_a_line_instead_of_committing() {
    for modifier in [KeyModifiers::SHIFT, KeyModifiers::ALT] {
        let (tmp, mut app) = scm_ready_to_commit();
        for c in "subj".chars() {
            app.handle_source_control_key(key(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.handle_source_control_key(key(KeyCode::Enter, modifier));
        app.handle_source_control_key(key(KeyCode::Enter, modifier));
        for c in "body".chars() {
            app.handle_source_control_key(key(KeyCode::Char(c), KeyModifiers::NONE));
        }
        assert_eq!(app.source_control.message, "subj\n\nbody", "{modifier:?}");
        assert_eq!(
            last_commit_message(tmp.path()),
            "init\n\n",
            "{modifier:?} must not commit"
        );
        app.handle_source_control_key(key(KeyCode::Enter, KeyModifiers::NONE));
        wait_for_git_net(&mut app);
        assert_eq!(last_commit_message(tmp.path()), "subj\n\nbody\n\n");
    }
}

/// Plain Enter still commits a one-line message straight away.
#[test]
fn plain_enter_still_commits_a_one_line_message() {
    let (tmp, mut app) = scm_ready_to_commit();
    for c in "one line".chars() {
        app.handle_source_control_key(key(KeyCode::Char(c), KeyModifiers::NONE));
    }
    app.handle_source_control_key(key(KeyCode::Enter, KeyModifiers::NONE));
    wait_for_git_net(&mut app);
    assert_eq!(last_commit_message(tmp.path()), "one line\n\n");
    assert_eq!(app.source_control.message, "");
}

#[test]
fn a_commit_reports_only_git_s_first_line_in_the_panel_and_status_bar() {
    // #858: `git commit` prints `[branch sha] subject`, then a diffstat and
    // one `create mode` row per new file; the whole of it was the feedback.
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
    app.source_control.message = "fix: one line".to_string();
    app.source_control.message_cursor = app.source_control.message.chars().count();
    app.handle_source_control_key(key(KeyCode::Enter, KeyModifiers::NONE));
    wait_for_git_net(&mut app);
    let feedback = app
        .source_control
        .commit_feedback
        .clone()
        .unwrap_or_default();
    assert!(
        feedback.starts_with("[main ") && feedback.ends_with("] fix: one line"),
        "the feedback is git's first line: {feedback:?}"
    );
    assert_eq!(app.status, format!("Committed: {feedback}"));
    let logged = app.git_output_log.last().cloned().unwrap_or_default();
    assert!(
        logged.contains("1 file changed"),
        "the Git Output log keeps the rest: {logged:?}"
    );
}

/// #858 negative: a refused commit (nothing to commit) is still reported
/// as an error, on one line that names why, in the panel and the status
/// bar; git's full text is in the Git Output log.
#[test]
fn a_refused_commit_names_the_reason_on_one_line() {
    let tmp = make_committed_repo();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::SourceControl);
    app.source_control.message = "fix: nothing here".to_string();
    app.source_control.message_cursor = app.source_control.message.chars().count();
    app.handle_source_control_key(key(KeyCode::Enter, KeyModifiers::NONE));
    wait_for_git_net(&mut app);
    assert!(app.source_control.commit_feedback_is_error);
    let feedback = app
        .source_control
        .commit_feedback
        .clone()
        .unwrap_or_default();
    let shown = crate::git::headline(&feedback);
    assert!(
        shown.contains("nothing to commit"),
        "the panel's line names the reason: {shown:?} (feedback {feedback:?})"
    );
    assert!(
        !app.status.contains('\n'),
        "one status line: {:?}",
        app.status
    );
    assert!(app.status.contains("nothing to commit"), "{:?}", app.status);
}

#[test]
fn ctrl_enter_in_source_control_commits_and_pushes() {
    let tmp = make_committed_repo();
    // Wire a bare remote and set it as origin so `git push` has
    // somewhere to send.
    let remote = tempfile::tempdir().unwrap();
    let _ = std::process::Command::new("git")
        .args(["-C"])
        .arg(remote.path())
        .args(["init", "--bare", "-q", "-b", "main"])
        .status();
    let _ = std::process::Command::new("git")
        .args(["-C"])
        .arg(tmp.path())
        .args(["remote", "add", "origin"])
        .arg(remote.path())
        .status();
    let _ = std::process::Command::new("git")
        .args(["-C"])
        .arg(tmp.path())
        .args(["push", "-u", "origin", "main"])
        .status();
    // Make a tracked change and stage it so commit-all-tracked has
    // something to commit.
    std::fs::write(tmp.path().join("seed.txt"), b"changed\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    wait_for_changes(&mut app, |a| {
        a.source_control
            .entries
            .iter()
            .any(|e| e.path == "seed.txt")
    });
    app.set_sidebar_view(SidebarView::SourceControl);
    app.source_control.message = "fix: bump seed".to_string();
    app.source_control.message_cursor = app.source_control.message.chars().count();
    app.handle_source_control_key(key(KeyCode::Enter, KeyModifiers::CONTROL));
    // The push runs on a worker.
    wait_for_git_net(&mut app);
    // Verify the remote received the commit (its log now reports two
    // commits — the initial push + our new one).
    let log = std::process::Command::new("git")
        .args(["-C"])
        .arg(remote.path())
        .args(["log", "--oneline"])
        .output()
        .unwrap();
    let log_out = String::from_utf8_lossy(&log.stdout);
    assert!(
        log_out.lines().count() >= 2,
        "Cmd+Enter must have committed AND pushed (remote log: {log_out:?})"
    );
    assert!(
        log_out.contains("fix: bump seed"),
        "remote's log must include the new commit's subject"
    );
}

#[test]
fn focused_source_control_input_drives_a_visible_caret_at_the_message_cursor() {
    // Regression for the user's "I don't see the caret blinking in the
    // Source Control message box, so I have no clue where it is at any
    // point in time" report. After focusing the SC panel in a real
    // repo, `cursor_should_be_visible()` must report true and the
    // caret cell must point inside the rendered input box.
    let tmp = tempfile::tempdir().unwrap();
    let _ = std::process::Command::new("git")
        .args(["-C"])
        .arg(tmp.path())
        .args(["init", "-q", "-b", "main"])
        .output();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while std::time::Instant::now() < deadline {
        app.drain_git_responses();
        if app.source_control.status.in_repo {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        app.source_control.status.in_repo,
        "preconditions: tempdir must register as a git repo"
    );

    app.set_sidebar_view(SidebarView::SourceControl);
    let backend = ratatui::backend::TestBackend::new(80, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    app.handle_source_control_key(key(KeyCode::Char('f'), KeyModifiers::NONE));
    app.handle_source_control_key(key(KeyCode::Char('i'), KeyModifiers::NONE));
    app.handle_source_control_key(key(KeyCode::Char('x'), KeyModifiers::NONE));
    term.draw(|f| app.render(f)).unwrap();

    assert!(
        app.cursor_should_be_visible(),
        "caret must be visible when the Source Control panel owns focus and the input is laid out"
    );
    let (cx, cy) = app
        .source_control
        .cursor_screen_pos()
        .expect("focused SC panel must expose a caret cell");
    let r = app.source_control.last_input_area;
    assert!(
        cx >= r.x + 2 && cx < r.x + r.width - 1 && cy == r.y + 1,
        "caret cell ({cx},{cy}) must sit inside the input's text row (input rect {r:?})"
    );
    assert_eq!(
        cx,
        r.x + 2 + 3,
        "caret must advance one cell per typed char (3 chars typed → +3 cells past the first text cell)"
    );
}

#[test]
fn saving_records_a_local_history_snapshot_that_diffs_and_restores() {
    let tmp = tempfile::tempdir().unwrap();
    let hist = tempfile::tempdir().unwrap();
    let f = tmp.path().join("note.txt");
    std::fs::write(&f, "v1\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.history_root = hist.path().to_path_buf();
    app.editor.open(&f).unwrap();
    app.focus_pane(Pane::Editor);

    // Type and save: the save hook records a snapshot of the on-disk bytes
    // (off-thread, so poll for it).
    app.handle_key(key(KeyCode::Char('X'), KeyModifiers::NONE))
        .unwrap();
    app.save();
    let saved = std::fs::read_to_string(&f).unwrap();
    let snaps = wait_for_snapshots(&app.history_root, &f);
    assert_eq!(snaps.len(), 1, "a save must record one snapshot");
    assert_eq!(
        std::fs::read_to_string(&snaps[0].file).unwrap(),
        saved,
        "the snapshot holds exactly what hit the disk"
    );

    // Clicking the snapshot row opens a diff of snapshot vs working file and
    // arms restore.
    let hash = format!("local:{}", snaps[0].millis);
    app.open_timeline_diff(f.clone(), hash);
    assert!(
        app.editor.diff.is_some(),
        "a local snapshot row opens a diff view"
    );
    assert!(app.history_restore.is_some(), "restore is armed");

    // Corrupt the file, then restore the snapshot back over it.
    std::fs::write(&f, "clobbered\n").unwrap();
    app.restore_history_snapshot();
    assert_eq!(
        std::fs::read_to_string(&f).unwrap(),
        saved,
        "restore writes the snapshot contents back to disk"
    );
}

/// A snapshot click that fails (pruned .snap, unreadable file) must disarm
/// "Restore Snapshot": the palette entry otherwise writes whatever snapshot
/// was viewed BEFORE over a file that is not even on screen.
#[test]
fn a_failed_snapshot_click_disarms_a_stale_restore() {
    let tmp = tempfile::tempdir().unwrap();
    let hist = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.txt");
    let b = tmp.path().join("b.txt");
    std::fs::write(&a, "a-disk\n").unwrap();
    std::fs::write(&b, "b-disk\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.history_root = hist.path().to_path_buf();
    crate::history::record_in(&app.history_root, &a, b"a-old\n", 1000).unwrap();
    app.editor.open(&a).unwrap();
    app.open_timeline_diff(a.clone(), "local:1000".into());
    assert!(app.history_restore.is_some(), "staging: a's snapshot armed");
    // A pruned snapshot row of another file: the click fails...
    app.open_timeline_diff(b.clone(), "local:999".into());
    assert_eq!(app.status, "Snapshot no longer available");
    assert!(
        app.history_restore.is_none(),
        "the failed click left the previous file's restore armed"
    );
    // ...and restore must refuse instead of clobbering a.txt.
    app.restore_history_snapshot();
    assert_eq!(
        std::fs::read_to_string(&a).unwrap(),
        "a-disk\n",
        "restore wrote a stale snapshot over an unrelated file"
    );
}

/// A restore of a snapshot with no sidecar restores every line unknown
/// (#349 review): the buffer's previous map described the text the restore
/// replaced, and keeping it would credit lines nobody observed.
#[test]
fn restoring_a_snapshot_without_seats_leaves_every_line_unknown() {
    use crate::provenance::Seat;
    let tmp = tempfile::tempdir().unwrap();
    let hist = tempfile::tempdir().unwrap();
    let f = tmp.path().join("note.txt");
    let (v1, v2) = (b"one\n".as_slice(), b"one\ntwo\n".as_slice());
    let mut seats_v2 = crate::provenance::Provenance::new();
    seats_v2.record(1..2, Seat::Navigator);
    crate::history::record_in(hist.path(), &f, v1, 1_000).unwrap();
    let recent = now_millis() - 100;
    crate::history::record_with_seats_in(hist.path(), &f, v2, recent, &seats_v2).unwrap();
    std::fs::write(&f, v2).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.history_root = hist.path().to_path_buf();
    app.editor.open(&f).unwrap();
    app.focus_pane(Pane::Editor);
    app.sync_provenance();
    assert_eq!(app.editor.provenance, seats_v2, "staging: v2's map is live");
    app.open_timeline_diff(f.clone(), String::from("local:1000"));
    app.editor.open(&f).unwrap();
    app.restore_history_snapshot();
    assert_eq!(
        std::fs::read(&f).unwrap(),
        v1,
        "staging: v1 is back on disk"
    );
    assert_eq!(
        app.editor.provenance.attributed(),
        0,
        "a snapshot that recorded no seats restores every line unknown: {:?}",
        app.editor.provenance
    );
}

/// Restoring a snapshot carries that snapshot's seats (#349 review): onto the
/// reverted buffer, and onto the snapshot the restore records, which inside
/// the merge window supersedes the newest one and takes its sidecar with it.
/// Before this the restore recorded the just-cleared (empty) map, so a
/// persisted map was destroyed by the very command that reinstated its text.
#[test]
fn restoring_a_snapshot_carries_its_seats_onto_the_restore() {
    use crate::provenance::Seat;
    let tmp = tempfile::tempdir().unwrap();
    let hist = tempfile::tempdir().unwrap();
    let f = tmp.path().join("note.txt");
    let (v1, v2) = (b"one\ntwo\n".as_slice(), b"one\ntwo\nthree\n".as_slice());
    let mut seats_v1 = crate::provenance::Provenance::new();
    seats_v1.record(0..1, Seat::Navigator);
    let mut seats_v2 = crate::provenance::Provenance::new();
    seats_v2.record(2..3, Seat::Agent(String::from("pane 2")));
    // v2 is the newest snapshot and recent enough that the restore lands
    // inside the merge window, which must not supersede it.
    let recent = now_millis() - 100;
    crate::history::record_with_seats_in(hist.path(), &f, v1, 1_000, &seats_v1).unwrap();
    crate::history::record_with_seats_in(hist.path(), &f, v2, recent, &seats_v2).unwrap();
    std::fs::write(&f, v2).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.history_root = hist.path().to_path_buf();
    app.editor.open(&f).unwrap();
    app.focus_pane(Pane::Editor);
    app.sync_provenance();
    assert_eq!(app.editor.provenance, seats_v2, "staging: v2's map is live");

    // Arm the restore from the timeline, then make the file the active tab
    // again (the diff view closed) and restore through the real command.
    app.open_timeline_diff(f.clone(), String::from("local:1000"));
    assert!(
        app.history_restore.is_some(),
        "staging: the restore is armed"
    );
    app.editor.open(&f).unwrap();
    app.restore_history_snapshot();
    assert_eq!(
        std::fs::read(&f).unwrap(),
        v1,
        "staging: v1 is back on disk"
    );
    assert_eq!(
        app.editor.provenance, seats_v1,
        "the reverted buffer wears the restored snapshot's seats"
    );

    // The record runs off-thread: wait for the superseding snapshot.
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(500),
        "the restore's snapshot",
        || {
            crate::history::entries_in(hist.path(), &f)
                .first()
                .is_some_and(|newest| newest.millis > recent)
        },
    );
    let snaps = crate::history::entries_in(hist.path(), &f);
    // v2 is kept, not superseded: the restore overwrote it, and replacing
    // its snapshot inside the merge window lost that version for good.
    assert_eq!(
        snaps.len(),
        3,
        "v1 restored, v2 kept, v1 original: {snaps:?}"
    );
    assert_eq!(std::fs::read(&snaps[0].file).unwrap(), v1);
    assert_eq!(std::fs::read(&snaps[1].file).unwrap(), v2);
    wait_for_seats(hist.path(), &f, snaps[0].millis);
    assert_eq!(
        crate::history::seats_for(hist.path(), &f, snaps[0].millis),
        Some(seats_v1.clone()),
        "and the newest snapshot carries v1's seats, not an empty map"
    );
    // A fresh open of the restored file gets them back the ordinary way.
    let mut again = App::new(tmp.path().to_path_buf()).unwrap();
    again.history_root = hist.path().to_path_buf();
    again.editor.open(&f).unwrap();
    again.sync_provenance();
    assert_eq!(again.editor.provenance, seats_v1);
}

#[test]
fn cmd_shift_s_jumps_to_source_control_even_when_editor_is_focused() {
    let mut app = editor_app_with_lines(&["x"]);
    app.focus_pane(Pane::Editor);
    app.handle_key(key(
        KeyCode::Char('s'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert_eq!(
        app.sidebar_view,
        SidebarView::SourceControl,
        "Cmd+Shift+S must jump to Source Control even from the editor (no Save-As to collide with); Search stays on Cmd+Shift+F"
    );
    assert!(
        matches!(app.focus, Pane::Tree),
        "Cmd+Shift+S must move keyboard focus into the Source Control panel"
    );
}

#[test]
fn a_stale_commit_graph_scrollbar_rect_cannot_deaden_the_splitter() {
    // The COMMITS graph refreshes `last_scrollbar` only while it renders,
    // i.e. only under the Source Control view. After switching to Explorer
    // the stale rect used to keep punching a hole in the splitter grab
    // zone: dragging the sidebar seam inside that band silently did
    // nothing. The exemption must apply only while the graph is visible.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.sidebar_splitter_x = Some(36);
    app.last_content_width = 60;
    app.last_content_height = 20;
    app.sidebar_width = 32;
    app.commit_graph.last_scrollbar = Rect {
        x: 35,
        y: 4,
        width: 1,
        height: 10,
    };
    app.sidebar_view = SidebarView::Explorer;
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: 35,
        row: 5,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(
        app.splitter_drag,
        Some(SplitterDrag::Sidebar),
        "a stale graph scrollbar rect must not deaden the seam outside Source Control"
    );
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left),
        column: 35,
        row: 5,
        modifiers: KeyModifiers::NONE,
    });
    // While the graph IS visible, its scrollbar still wins the cell.
    app.sidebar_view = SidebarView::SourceControl;
    app.handle_mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: 35,
        row: 5,
        modifiers: KeyModifiers::NONE,
    });
    assert!(
        app.splitter_drag.is_none(),
        "the live graph scrollbar keeps its exemption from the grab zone"
    );
}

#[test]
fn cmd_shift_g_no_longer_jumps_to_source_control() {
    // Source Control is reached via Cmd+Shift+S; the old Cmd+Shift+G alias
    // was removed so the chord stays free for the editor's vim-style
    // "go to bottom of file".
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.handle_key(key(
        KeyCode::Char('G'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert_ne!(
        app.sidebar_view,
        SidebarView::SourceControl,
        "Cmd+Shift+G must no longer route to Source Control"
    );
}

/// #1444: View Changes vs main on a branch behind a moved main shows the
/// branch's own work and names the merge base it starts from.
#[test]
fn view_changes_vs_default_branch_starts_from_the_merge_base() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().canonicalize().unwrap();
    let git = |args: &[&str]| {
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(&p)
            .args(args)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.email", "a@b"]);
    git(&["config", "user.name", "a"]);
    std::fs::write(p.join("app.py"), "def a():\n    return 1\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "init"]);
    let base = git(&["rev-parse", "--short=7", "HEAD"]);
    git(&["checkout", "-qb", "feature"]);
    std::fs::write(p.join("feature.py"), "def feature():\n    return 2\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "add feature"]);
    git(&["checkout", "-q", "main"]);
    std::fs::write(p.join("teammate.py"), "def teammate():\n    return 3\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "teammate work"]);
    git(&["checkout", "-q", "feature"]);
    let mut app = App::new(p.clone()).unwrap();
    app.view_default_branch_diff_source_control();
    assert_eq!(app.status, "Showing changes since this branch left main");
    assert_eq!(
        app.editor.path.as_deref(),
        Some(std::path::Path::new(&format!(
            "git diff {base} (merge base with main)"
        )))
    );
    let diff = app.editor.diff.as_ref().expect("a diff tab opened");
    let text = format!("{:?}", diff);
    assert!(
        text.contains("feature"),
        "the branch's own file is in the view"
    );
    assert!(
        !text.contains("teammate"),
        "main's later work is not: {text}"
    );
}

#[test]
fn repositories_overview_lists_every_folder_and_a_pin_overrides_the_follow() {
    // #161: with several folders open, the panel shows a REPOSITORIES
    // overview and a clicked row pins the panel until focus crosses
    // folders again.
    let tmp = tempfile::tempdir().unwrap();
    let mk_repo = |name: &str, branch: &str| {
        let dir = tmp.path().join(name);
        std::fs::create_dir(&dir).unwrap();
        for args in [
            vec!["init", "-q", "-b", branch],
            vec!["config", "user.email", "a@b"],
            vec!["config", "user.name", "a"],
        ] {
            assert!(
                std::process::Command::new("git")
                    .args(["-C"])
                    .arg(&dir)
                    .args(&args)
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false),
                "git setup failed: {args:?}"
            );
        }
        std::fs::write(dir.join("f.txt"), "x\n").unwrap();
        dir
    };
    let a = mk_repo("repa", "main");
    let b = mk_repo("repb", "trunk");

    let mut app = App::new(a.clone()).unwrap();
    app.add_workspace_folder(b.clone());
    let b_canon = b.canonicalize().unwrap();

    // Both workers report their repos; the per-frame sync builds the rows.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let _ = app.drain_git_responses();
        app.sync_focus_flags();
        let rows = &app.source_control.repositories;
        if rows.len() == 2
            && rows.iter().any(|r| r.branch.as_deref() == Some("main"))
            && rows.iter().any(|r| r.branch.as_deref() == Some("trunk"))
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "overview rows never filled: {:?}",
            rows
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        app.source_control
            .repositories
            .iter()
            .all(|r| r.changes >= 1),
        "EVERY repository's untracked file shows in its change count: {:?}",
        app.source_control.repositories
    );

    // Anchor the focus in folder A first, then pin B: the pin wins for
    // as long as the focus stays inside A (further A files included).
    //
    // Compare against the root in the form the workspace holds it, not a
    // canonicalised one: `App::new` stores the primary root verbatim, and
    // on macOS a tempdir arrives as `/var/…` for a `/private/var/…`
    // directory. The app is self-consistent — the follow root derives from
    // the same workspace roots — so canonicalising only this side compares
    // two spellings of one path and fails everywhere `/var` is a symlink.
    let a_root = app
        .roots
        .iter()
        .find(|r| r.ends_with("repa"))
        .expect("folder A is in the workspace")
        .to_path_buf();
    app.editor.open_pinned(&a.join("f.txt")).unwrap();
    let _ = app.drain_git_responses();
    assert_eq!(app.active_scm_root, a_root);
    app.scm_pin = Some(b_canon.clone());
    let _ = app.drain_git_responses();
    assert_eq!(
        app.active_scm_root, b_canon,
        "the pin wins while focus stays put"
    );

    // CROSSING folders with the focus releases the pin: land in B (the
    // follow now agrees with the pinned repo), then back to A — the
    // panel follows again.
    app.editor.open_pinned(&b_canon.join("f.txt")).unwrap();
    let _ = app.drain_git_responses();
    assert!(
        app.scm_pin.is_none(),
        "the cross-folder focus released the pin"
    );
    app.editor.open_pinned(&a.join("f.txt")).unwrap();
    let _ = app.drain_git_responses();
    assert_eq!(
        app.active_scm_root, a_root,
        "after release the panel follows the focus again"
    );
}

#[test]
fn clicking_a_repositories_overview_row_switches_the_panel() {
    use crossterm::event::{MouseButton, MouseEventKind};
    let tmp = tempfile::tempdir().unwrap();
    let mk_repo = |name: &str, branch: &str| {
        let dir = tmp.path().join(name);
        std::fs::create_dir(&dir).unwrap();
        for args in [
            vec!["init", "-q", "-b", branch],
            vec!["config", "user.email", "a@b"],
            vec!["config", "user.name", "a"],
        ] {
            assert!(
                std::process::Command::new("git")
                    .args(["-C"])
                    .arg(&dir)
                    .args(&args)
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false),
            );
        }
        std::fs::write(dir.join("f.txt"), "x\n").unwrap();
        dir
    };
    let a = mk_repo("repa", "main");
    let b = mk_repo("repb", "trunk");
    let mut app = App::new(a.clone()).unwrap();
    app.add_workspace_folder(b.clone());
    let b_canon = b.canonicalize().unwrap();
    app.set_sidebar_view(SidebarView::SourceControl);

    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let _ = app.drain_git_responses();
        app.sync_focus_flags();
        term.draw(|frame| app.render(frame)).unwrap();
        if app.source_control.repo_row_areas.len() == 2 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "overview rows never rendered; rows: {:?}",
            app.source_control.repositories
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let (rect, root) = app.source_control.repo_row_areas[1].clone();
    assert_eq!(root, b_canon);

    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        rect.x + 2,
        rect.y,
    ));
    let _ = app.drain_git_responses();
    assert_eq!(
        app.active_scm_root, b_canon,
        "the click pins the panel to the second repository"
    );
    assert_eq!(app.scm_pin.as_deref(), Some(b_canon.as_path()));
}

#[test]
fn clicking_a_commit_graph_row_opens_the_commit_patch_tab() {
    use crossterm::event::{MouseButton, MouseEventKind};
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "one\n").unwrap();
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap()
    };
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-qm", "first commit"]);
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // The arming step of #103: render the DEFAULT Explorer sidebar once so
    // its bottom sections (OPEN EDITORS / TIMELINE / DEPENDENCIES) record
    // their hit rects — real sessions always do this before the user ever
    // reaches Source Control. Stale copies of those rects must not shadow
    // the COMMITS strip that paints in the same rows afterwards.
    {
        let backend = ratatui::backend::TestBackend::new(140, 44);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| app.render(f)).unwrap();
    }
    app.set_sidebar_view(SidebarView::SourceControl);
    // Let the git worker deliver status + graph, then render so every
    // sidebar rect (including the COMMITS strip) records its hit area.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut graph_rect = Rect::default();
    let mut row = 0u16;
    let mut ready = false;
    while std::time::Instant::now() < deadline {
        let _ = app.drain_git_responses();
        let _ = app.sync_explorer_panels();
        let backend = ratatui::backend::TestBackend::new(140, 44);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| app.render(f)).unwrap();
        graph_rect = app.commit_graph.last_area;
        // Border row, then the section header, then the first commit row.
        row = graph_rect.y + 2;
        if graph_rect.height > 1 && app.commit_graph.commit_at(row).is_some() {
            ready = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(
        ready,
        "precondition: the COMMITS graph must render with a clickable row; rect {graph_rect:?}, \
         inflight {:?}, probes {:?}",
        app.graph_fetch_inflight,
        (40u16..44)
            .map(|y| app.commit_graph.commit_at(y).map(|c| c.short_hash.clone()))
            .collect::<Vec<_>>()
    );
    let col = graph_rect.x + 6;
    let short = app
        .commit_graph
        .commit_at(row)
        .map(|c| c.short_hash.clone())
        .expect("precondition established a commit at the click row");
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), col, row));
    // The patch is read off the UI thread.
    crate::test_budget::await_spawned(
        std::time::Duration::from_secs(10),
        "the commit patch",
        || {
            app.drain_git_view();
            app.git_view_job.is_none()
        },
    );
    assert!(
        app.status.contains("Opened commit"),
        "clicking a commit row must open its patch tab; status: {:?}",
        app.status
    );
    assert_eq!(
        app.editor.path.as_deref(),
        Some(Path::new(&format!("commit {short}"))),
        "the active tab must be the clicked commit's patch"
    );
    let body = app.editor.lines.join("\n");
    assert!(
        body.contains("first commit") && body.contains("diff --git"),
        "the patch tab must carry the commit message and its diff"
    );
}

/// The issue's repo (#1627): `main` has "init app" and "update readme";
/// `feature`, off "init app", has "fix crash on empty input" (appends to
/// `app.txt`) and "add feature flag". HEAD is `main`.
fn commit_actions_repo() -> tempfile::TempDir {
    let tmp = make_committed_repo();
    let p = tmp.path();
    std::fs::write(p.join("app.txt"), "a\n").unwrap();
    assert!(git_succeeds(p, &["add", "."]) && git_succeeds(p, &["commit", "-qm", "init app"]));
    assert!(git_succeeds(p, &["checkout", "-qb", "feature"]));
    std::fs::write(p.join("app.txt"), "a\nfix\n").unwrap();
    assert!(git_succeeds(
        p,
        &["commit", "-qam", "fix crash on empty input"]
    ));
    std::fs::write(p.join("f.txt"), "f\n").unwrap();
    assert!(
        git_succeeds(p, &["add", "."]) && git_succeeds(p, &["commit", "-qm", "add feature flag"])
    );
    assert!(git_succeeds(p, &["checkout", "-q", "main"]));
    std::fs::write(p.join("b.txt"), "b\n").unwrap();
    assert!(git_succeeds(p, &["add", "."]) && git_succeeds(p, &["commit", "-qm", "update readme"]));
    tmp
}

/// An app on `p` with Source Control showing and the COMMITS graph drawn
/// from the repo's real history.
fn commit_graph_app(p: &std::path::Path) -> App {
    let mut app = App::new(p.to_path_buf()).unwrap();
    app.sidebar_view = SidebarView::SourceControl;
    app.show_tree = true;
    app.source_control.status.in_repo = true;
    app.commit_graph
        .set_rows(crate::widgets::commit_graph::layout_graph(
            crate::git::commit_graph(p, 100),
        ));
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(110, 44)).unwrap();
    term.draw(|frame| app.render(frame)).unwrap();
    app
}

/// Right-click the COMMITS row whose summary is `summary`.
fn right_click_commit(app: &mut App, summary: &str) {
    use crossterm::event::{MouseButton, MouseEventKind};
    let area = app.commit_graph.last_area;
    let y = (area.y..area.y + area.height)
        .find(|&y| {
            app.commit_graph
                .commit_at(y)
                .is_some_and(|c| c.summary == summary)
        })
        .unwrap_or_else(|| panic!("{summary} is drawn in COMMITS"));
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Right),
        area.x + 4,
        y,
    ));
}

/// Pick `label` from the open context menu, or `label` inside the
/// submenu `sub`, and run it.
fn pick_commit_menu(app: &mut App, sub: Option<&str>, label: &str) {
    let menu = app.context_menu.take().expect("a context menu is open");
    let items = match sub {
        None => &menu.items,
        Some(sub) => menu
            .items
            .iter()
            .find_map(|e| match e {
                MenuEntry::Submenu { label: l, items } if l == sub => Some(items),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no {sub} submenu")),
    };
    let action = menu_action_of(items, label)
        .unwrap_or_else(|| panic!("no {label} in {:?}", menu_labels(items)))
        .clone();
    app.dispatch_menu_action(action, menu.target_dir.clone());
}

/// #1627: right-clicking a COMMITS row opens the commit's actions, where
/// it did nothing.
#[test]
fn right_clicking_a_commit_opens_its_actions() {
    let tmp = commit_actions_repo();
    let mut app = commit_graph_app(tmp.path());
    right_click_commit(&mut app, "fix crash on empty input");
    let menu = app
        .context_menu
        .as_ref()
        .expect("right-click on a commit opens a menu");
    let labels: Vec<&str> = menu_labels(&menu.items)
        .into_iter()
        .filter(|l| *l != "---")
        .collect();
    assert_eq!(
        labels,
        [
            "Cherry-Pick",
            "Revert",
            "Checkout (Detached)",
            "Create Branch Here...",
            "Create Tag Here...",
            "Reset Current Branch to Here",
            "Copy Commit Hash",
            "Copy Commit Message",
        ]
    );
}

/// #1627: Cherry-Pick puts a clean commit from another branch on HEAD.
#[test]
fn cherry_pick_from_the_graph_adds_the_commit_to_head() {
    let tmp = commit_actions_repo();
    let p = tmp.path();
    let before = git_stdout(p, &["rev-parse", "HEAD"]);
    let mut app = commit_graph_app(p);
    right_click_commit(&mut app, "fix crash on empty input");
    pick_commit_menu(&mut app, None, "Cherry-Pick");
    wait_for_git_net(&mut app);
    assert_eq!(
        git_stdout(p, &["log", "-1", "--format=%s"]),
        "fix crash on empty input"
    );
    assert_eq!(git_stdout(p, &["rev-parse", "HEAD~1"]), before);
    assert_eq!(git_stdout(p, &["branch", "--show-current"]), "main");
    assert_eq!(
        std::fs::read_to_string(p.join("app.txt")).unwrap(),
        "a\nfix\n"
    );
    assert!(app.status.starts_with("Cherry-picked"), "{}", app.status);
}

/// #1627: a cherry-pick that conflicts stops with `CHERRY_PICK_HEAD`, so
/// Git: Continue / Abort take it from there; Abort puts HEAD back.
#[test]
fn a_conflicting_cherry_pick_from_the_graph_stops_for_continue_or_abort() {
    use crate::git::RepoOp;
    let tmp = diverged_repo();
    let p = tmp.path();
    let before = git_stdout(p, &["rev-parse", "HEAD"]);
    let mut app = commit_graph_app(p);
    right_click_commit(&mut app, "feat");
    pick_commit_menu(&mut app, None, "Cherry-Pick");
    wait_for_git_net(&mut app);
    assert_eq!(crate::git::repo_operation(p), Some(RepoOp::CherryPick));
    assert!(p.join(".git/CHERRY_PICK_HEAD").exists());
    app.run_command(crate::widgets::command_palette::Command::GitAbortOperation);
    assert_eq!(crate::git::repo_operation(p), None, "{}", app.status);
    assert_eq!(git_stdout(p, &["rev-parse", "HEAD"]), before);
}

/// #1627: Revert commits the inverse with git's own message and body.
#[test]
fn revert_from_the_graph_commits_git_s_revert_message() {
    let tmp = commit_actions_repo();
    let p = tmp.path();
    let target = git_stdout(p, &["rev-parse", "HEAD"]);
    let mut app = commit_graph_app(p);
    right_click_commit(&mut app, "update readme");
    pick_commit_menu(&mut app, None, "Revert");
    wait_for_git_net(&mut app);
    assert_eq!(
        git_stdout(p, &["log", "-1", "--format=%s"]),
        "Revert \"update readme\""
    );
    assert!(
        git_stdout(p, &["log", "-1", "--format=%b"])
            .contains(&format!("This reverts commit {target}")),
    );
    assert!(!p.join("b.txt").exists());
}

/// #1627: Checkout (Detached) moves HEAD to the commit, on no branch.
#[test]
fn checkout_detached_from_the_graph_moves_head_to_the_commit() {
    let tmp = commit_actions_repo();
    let p = tmp.path();
    let target = git_stdout(p, &["rev-parse", "feature~1"]);
    let mut app = commit_graph_app(p);
    right_click_commit(&mut app, "fix crash on empty input");
    pick_commit_menu(&mut app, None, "Checkout (Detached)");
    wait_for_git_net(&mut app);
    assert_eq!(git_stdout(p, &["rev-parse", "HEAD"]), target);
    assert_eq!(git_stdout(p, &["branch", "--show-current"]), "");
}

/// #1627: Create Branch Here asks for a name and points the new branch at
/// the clicked commit, not at HEAD.
#[test]
fn create_branch_here_points_the_branch_at_the_clicked_commit() {
    let tmp = commit_actions_repo();
    let p = tmp.path();
    let target = git_stdout(p, &["rev-parse", "feature~1"]);
    let mut app = commit_graph_app(p);
    right_click_commit(&mut app, "fix crash on empty input");
    pick_commit_menu(&mut app, None, "Create Branch Here...");
    for c in "hotfix".chars() {
        app.input_prompt
            .as_mut()
            .expect("a name prompt")
            .push_char(c);
    }
    app.submit_input_prompt();
    assert_eq!(git_stdout(p, &["rev-parse", "hotfix"]), target);
    assert_eq!(git_stdout(p, &["branch", "--show-current"]), "hotfix");
}

/// #1627: Create Tag Here tags the clicked commit; HEAD is untouched.
#[test]
fn create_tag_here_tags_the_clicked_commit_not_head() {
    let tmp = commit_actions_repo();
    let p = tmp.path();
    let target = git_stdout(p, &["rev-parse", "feature"]);
    let head = git_stdout(p, &["rev-parse", "HEAD"]);
    let mut app = commit_graph_app(p);
    right_click_commit(&mut app, "add feature flag");
    pick_commit_menu(&mut app, None, "Create Tag Here...");
    for c in "v0.1".chars() {
        app.input_prompt
            .as_mut()
            .expect("a name prompt")
            .push_char(c);
    }
    app.submit_input_prompt();
    assert_eq!(git_stdout(p, &["rev-parse", "v0.1^{commit}"]), target);
    assert_eq!(git_stdout(p, &["rev-parse", "HEAD"]), head);
}

/// #1627: a soft reset moves the branch and keeps the undone change
/// staged, with no question asked.
#[test]
fn reset_soft_to_a_commit_keeps_the_changes_staged() {
    let tmp = commit_actions_repo();
    let p = tmp.path();
    let target = git_stdout(p, &["rev-parse", "main~1"]);
    let mut app = commit_graph_app(p);
    right_click_commit(&mut app, "init app");
    pick_commit_menu(
        &mut app,
        Some("Reset Current Branch to Here"),
        "Soft (keep changes staged)",
    );
    wait_for_git_net(&mut app);
    assert_eq!(git_stdout(p, &["rev-parse", "main"]), target);
    assert_eq!(git_stdout(p, &["diff", "--cached", "--name-only"]), "b.txt");
}

/// #1627 negative: a hard reset with a dirty tree asks first and changes
/// nothing; only a second ask for the same commit resets.
#[test]
fn reset_hard_asks_first_and_changes_nothing_until_asked_again() {
    let tmp = commit_actions_repo();
    let p = tmp.path();
    let head = git_stdout(p, &["rev-parse", "HEAD"]);
    let target = git_stdout(p, &["rev-parse", "main~1"]);
    std::fs::write(p.join("app.txt"), "a\nunsaved work\n").unwrap();
    let mut app = commit_graph_app(p);
    right_click_commit(&mut app, "init app");
    pick_commit_menu(
        &mut app,
        Some("Reset Current Branch to Here"),
        "Hard (discard changes)",
    );
    wait_for_git_net(&mut app);
    assert_eq!(
        git_stdout(p, &["rev-parse", "HEAD"]),
        head,
        "the first ask only warns"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("app.txt")).unwrap(),
        "a\nunsaved work\n"
    );
    assert!(app.status.contains("again"), "{}", app.status);
    right_click_commit(&mut app, "init app");
    pick_commit_menu(
        &mut app,
        Some("Reset Current Branch to Here"),
        "Hard (discard changes)",
    );
    wait_for_git_net(&mut app);
    assert_eq!(git_stdout(p, &["rev-parse", "HEAD"]), target);
    assert_eq!(std::fs::read_to_string(p.join("app.txt")).unwrap(), "a\n");
}

/// #1627 negative: work made after a hard reset's warning is warned about
/// afresh; the old warning does not confirm discarding it.
#[test]
fn a_warned_hard_reset_warns_again_after_new_work() {
    let tmp = commit_actions_repo();
    let p = tmp.path();
    let head = git_stdout(p, &["rev-parse", "HEAD"]);
    let target = git_stdout(p, &["rev-parse", "main~1"]);
    std::fs::write(p.join("app.txt"), "a\nfirst\n").unwrap();
    let mut app = commit_graph_app(p);
    let hard = |app: &mut App| {
        right_click_commit(app, "init app");
        pick_commit_menu(
            app,
            Some("Reset Current Branch to Here"),
            "Hard (discard changes)",
        );
        wait_for_git_net(app);
    };
    hard(&mut app);
    std::fs::write(p.join("app.txt"), "a\nfirst\nand much newer work\n").unwrap();
    hard(&mut app);
    assert_eq!(
        git_stdout(p, &["rev-parse", "HEAD"]),
        head,
        "the tree changed since the warning, so this ask only warns again"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("app.txt")).unwrap(),
        "a\nfirst\nand much newer work\n"
    );
    assert!(app.status.contains("again"), "{}", app.status);
    hard(&mut app);
    assert_eq!(git_stdout(p, &["rev-parse", "HEAD"]), target);
}

/// #1627 negative: switching to another branch at the same commit after
/// a hard reset's warning is warned about afresh, since the reset would
/// now move a different branch.
#[test]
fn a_warned_hard_reset_warns_again_after_a_branch_switch() {
    let tmp = commit_actions_repo();
    let p = tmp.path();
    let head = git_stdout(p, &["rev-parse", "HEAD"]);
    std::fs::write(p.join("app.txt"), "a\nwork\n").unwrap();
    assert!(git_succeeds(p, &["branch", "other"]));
    let mut app = commit_graph_app(p);
    let hard = |app: &mut App| {
        right_click_commit(app, "init app");
        pick_commit_menu(
            app,
            Some("Reset Current Branch to Here"),
            "Hard (discard changes)",
        );
        wait_for_git_net(app);
    };
    hard(&mut app);
    assert!(git_succeeds(p, &["switch", "-q", "other"]));
    hard(&mut app);
    assert_eq!(
        git_stdout(p, &["rev-parse", "other"]),
        head,
        "the branch changed since the warning, so this ask only warns again"
    );
    assert!(app.status.contains("again"), "{}", app.status);
    assert_eq!(git_stdout(p, &["rev-parse", "main"]), head);
}

/// #1627 negative: staging other content for a file whose status stays
/// `MM` after a hard reset's warning is warned about afresh.
#[test]
fn a_warned_hard_reset_warns_again_after_new_staged_content() {
    let tmp = commit_actions_repo();
    let p = tmp.path();
    let head = git_stdout(p, &["rev-parse", "HEAD"]);
    std::fs::write(p.join("app.txt"), "a\nstaged\n").unwrap();
    assert!(git_succeeds(p, &["add", "app.txt"]));
    std::fs::write(p.join("app.txt"), "a\nstaged\nunstaged\n").unwrap();
    let mut app = commit_graph_app(p);
    let hard = |app: &mut App| {
        right_click_commit(app, "init app");
        pick_commit_menu(
            app,
            Some("Reset Current Branch to Here"),
            "Hard (discard changes)",
        );
        wait_for_git_net(app);
    };
    hard(&mut app);
    // Restage without touching the working file, so status and the
    // file's size and mtime all stay as they were.
    std::fs::write(p.join(".git/restaged"), "a\nrestaged\n").unwrap();
    let blob = git_stdout(p, &["hash-object", "-w", ".git/restaged"]);
    let status = git_stdout(p, &["status", "--porcelain"]);
    assert!(git_succeeds(
        p,
        &[
            "update-index",
            "--cacheinfo",
            &format!("100644,{blob},app.txt")
        ]
    ));
    assert_eq!(git_stdout(p, &["status", "--porcelain"]), status);
    hard(&mut app);
    assert_eq!(
        git_stdout(p, &["rev-parse", "HEAD"]),
        head,
        "the index changed since the warning, so this ask only warns again"
    );
    assert!(app.status.contains("again"), "{}", app.status);
}

/// #1627 negative: a warned hard reset is not carried over to a different
/// commit or past another action; each needs its own second ask.
#[test]
fn a_warned_hard_reset_does_not_carry_to_another_commit() {
    let tmp = commit_actions_repo();
    let p = tmp.path();
    let head = git_stdout(p, &["rev-parse", "HEAD"]);
    let mut app = commit_graph_app(p);
    right_click_commit(&mut app, "init app");
    pick_commit_menu(
        &mut app,
        Some("Reset Current Branch to Here"),
        "Hard (discard changes)",
    );
    wait_for_git_net(&mut app);
    right_click_commit(&mut app, "init");
    pick_commit_menu(
        &mut app,
        Some("Reset Current Branch to Here"),
        "Hard (discard changes)",
    );
    wait_for_git_net(&mut app);
    assert_eq!(git_stdout(p, &["rev-parse", "HEAD"]), head);
    right_click_commit(&mut app, "init app");
    pick_commit_menu(
        &mut app,
        Some("Reset Current Branch to Here"),
        "Mixed (keep changes unstaged)",
    );
    wait_for_git_net(&mut app);
    assert_ne!(
        git_stdout(p, &["rev-parse", "HEAD"]),
        head,
        "mixed resets at once"
    );
}

/// #1627 negative: a right-click on the COMMITS header opens no commit
/// menu.
#[test]
fn a_right_click_off_the_commit_rows_opens_no_commit_menu() {
    use crossterm::event::{MouseButton, MouseEventKind};
    let tmp = commit_actions_repo();
    let mut app = commit_graph_app(tmp.path());
    let area = app.commit_graph.last_area;
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Right),
        area.x + 4,
        area.y,
    ));
    assert!(
        app.context_menu
            .as_ref()
            .is_none_or(|m| !menu_labels(&m.items).contains(&"Cherry-Pick")),
        "the header is not a commit"
    );
}

#[test]
fn scm_click_on_a_conflicted_entry_opens_the_merge_editor_on_the_stages() {
    let tmp = tempfile::tempdir().unwrap();
    conflicted_repo(tmp.path());
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let idx = wait_for_conflicted_entry(&mut app);
    app.open_source_control_entry(idx);
    assert_eq!(
        app.editor.path.as_deref().map(|p| p.file_name().unwrap()),
        Some(std::ffi::OsStr::new("shared.txt")),
        "the conflicted file must open"
    );
    let mv = app.editor.merge.as_ref().expect("the merge editor is up");
    assert!(!mv.from_markers, "git stages, not marker synthesis");
    assert_eq!(mv.conflicts.len(), 2, "both overlapping hunks tracked");
    // The base pane holds the stage-1 (common ancestor) content.
    assert_eq!(mv.base[1], "base-one");
    assert_eq!(mv.conflicts[0].ours, vec!["main-one".to_string()]);
    assert_eq!(mv.conflicts[0].theirs, vec!["feature-one".to_string()]);
    // The Result starts as base for conflict regions (nothing else
    // differed in this fixture, so the whole buffer equals base).
    assert_eq!(app.editor.lines[1], "base-one");
    assert_eq!(
        app.editor.cursor_row, mv.conflicts[0].result_start,
        "the cursor lands on the first conflict region"
    );
    assert!(
        app.status.contains("2 conflicts"),
        "the status counts the conflicts; was {:?}",
        app.status
    );
    // "Reopen as Text" reaches the unchanged in-buffer marker flow.
    app.run_command(crate::widgets::command_palette::Command::ReopenAsText);
    assert!(app.editor.merge.is_none(), "back to a plain text tab");
    assert_eq!(
        app.editor.conflicts().len(),
        2,
        "the marker scan sees the on-disk conflict blocks again"
    );
}

#[test]
fn accept_all_incoming_then_complete_merge_stages_the_resolved_file() {
    let tmp = tempfile::tempdir().unwrap();
    conflicted_repo(tmp.path());
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let idx = wait_for_conflicted_entry(&mut app);
    app.open_source_control_entry(idx);

    // Completing while conflicts remain must refuse.
    app.complete_merge();
    assert!(
        app.status.contains("unresolved"),
        "complete must refuse while blocks remain; was {:?}",
        app.status
    );

    // #1353's negative: a git conflict keeps its git wording.
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 40)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let screen = screen_text(&term);
    assert!(screen.contains("MERGE (git stages)"), "{screen}");
    assert!(
        screen.contains("\"Merge: Complete Merge\" stages the file"),
        "{screen}"
    );
    app.run_command(crate::widgets::command_palette::Command::MergeAcceptAllIncoming);
    assert_eq!(
        app.editor
            .merge
            .as_ref()
            .map(|mv| mv.unresolved_count())
            .unwrap_or(usize::MAX),
        0,
        "every conflict resolved"
    );
    assert_eq!(
        app.editor.lines.join("\n"),
        "alpha\nfeature-one\nm1\nm2\nm3\nm4\nm5\nfeature-two\nomega",
        "the Result keeps only the incoming side of both conflicts"
    );

    app.complete_merge();
    assert!(
        app.status.contains("Merge complete: staged shared.txt"),
        "complete must save and stage; was {:?}",
        app.status
    );
    let disk = std::fs::read_to_string(tmp.path().join("shared.txt")).unwrap();
    assert!(
        !disk.contains("<<<<<<<"),
        "the resolved file must be on disk, marker-free"
    );
    let porcelain = std::process::Command::new("git")
        .args(["-C", tmp.path().to_str().unwrap(), "status", "--porcelain"])
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&porcelain.stdout).to_string();
    let line = out
        .lines()
        .find(|l| l.contains("shared.txt"))
        .expect("shared.txt still in status");
    assert!(
        line.starts_with("M "),
        "after staging, shared.txt must be index-modified (M ), not unmerged; was {line:?}"
    );
}

/// #906: a file resolved by hand in the text flow is still unmerged in git,
/// so its MERGE CONFLICTS row stays. Opening it keeps that resolution as the
/// Result, reads each conflict's state off it, and Complete Merge stages it.
#[test]
fn a_hand_resolved_conflict_opens_as_its_own_result_and_completes() {
    use crate::merge_editor::ConflictState;
    let tmp = tempfile::tempdir().unwrap();
    conflicted_repo(tmp.path());
    let resolved = "alpha\nboth-one\nm1\nm2\nm3\nm4\nm5\nfeature-two\nomega\n";
    std::fs::write(tmp.path().join("shared.txt"), resolved).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let idx = wait_for_conflicted_entry(&mut app);
    app.open_source_control_entry(idx);
    let mv = app.editor.merge.as_ref().expect("the merge editor is up");
    let states: Vec<_> = mv.conflicts.iter().map(|c| c.state).collect();
    assert_eq!(states, [ConflictState::Manual, ConflictState::Incoming]);
    assert_eq!(app.editor.lines.join("\n") + "\n", resolved);
    assert!(!app.editor.dirty, "nothing in the file was replaced");
    app.complete_merge();
    assert!(app.status.contains("staged shared.txt"), "{}", app.status);
    assert_eq!(git_out(tmp.path(), &["show", ":shared.txt"]), resolved);
}

/// #906: with one block resolved and a line added beside it, the merge
/// editor keeps both and asks only about the block still marked.
#[test]
fn a_partly_resolved_conflict_keeps_its_edits_in_the_merge_editor() {
    let tmp = tempfile::tempdir().unwrap();
    conflicted_repo(tmp.path());
    let file = tmp.path().join("shared.txt");
    let lines: Vec<String> = std::fs::read_to_string(&file)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    let block = crate::merge::find_conflicts(&lines)[0];
    let mut edited = lines[..block.ours_start].to_vec();
    edited.extend(["feature-one".to_string(), "# reviewed".to_string()]);
    edited.extend_from_slice(&lines[block.theirs_end + 1..]);
    std::fs::write(&file, edited.join("\n") + "\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let idx = wait_for_conflicted_entry(&mut app);
    app.open_source_control_entry(idx);
    let mv = app.editor.merge.as_ref().expect("the merge editor is up");
    assert_eq!(mv.conflicts.len(), 1, "only the marked block is left");
    assert_eq!(mv.conflicts[0].ours, ["main-two"]);
    assert_eq!(
        app.editor.lines[..3],
        ["alpha", "feature-one", "# reviewed"]
    );
}

#[test]
fn keeping_the_modified_side_of_a_modify_delete_stages_it() {
    // Negative: the side that kept the file still wins as before.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = open_modify_delete(tmp.path(), true);
    app.run_command(crate::widgets::command_palette::Command::MergeAcceptAllCurrent);
    app.complete_merge();
    assert_eq!(git_out(tmp.path(), &["show", ":gone.txt"]), "x\ny-main\n");
    assert_eq!(git_out(tmp.path(), &["ls-files", "-u", "gone.txt"]), "");
    assert!(tmp.path().join("gone.txt").exists());
}

/// #1113: a Source Control diff of a file carries the file's path but is not
/// its buffer. A rename touching that file used to be "applied" to the diff,
/// which changed nothing, and then skipped the disk because a tab "held" it.
#[test]
fn a_rename_reaches_a_file_whose_source_control_diff_is_open() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let main = tmp.path().join("main.ts");
    std::fs::write(&main, "greet(1);\ngreet(2);\n// todo\n").unwrap();
    app.editor
        .open_head_diff_with_text(
            PathBuf::from("main.ts (HEAD)"),
            "greet(1);\ngreet(2);\n",
            &main,
            true,
        )
        .unwrap();
    assert!(app.editor.diff.is_some());

    let (files, occurrences) = app.apply_rename_edits(&greet_to_welcome(&main)).unwrap();

    assert_eq!((files, occurrences), (1, 2));
    assert_eq!(
        std::fs::read_to_string(&main).unwrap(),
        "welcome(1);\nwelcome(2);\n// todo\n"
    );
}

/// Negative: a diff tab is still found as a diff. Reopening the same
/// Source Control diff reuses its tab rather than stacking another.
#[test]
fn reopening_a_source_control_diff_still_reuses_its_tab() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let main = tmp.path().join("main.ts");
    std::fs::write(&main, "b\n").unwrap();
    for _ in 0..2 {
        app.editor
            .open_head_diff_with_text(PathBuf::from("main.ts (HEAD)"), "a\n", &main, true)
            .unwrap();
    }
    let diffs = app
        .editor
        .editors
        .iter()
        .filter(|e| e.diff.is_some())
        .count();
    assert_eq!(diffs, 1);
}

/// #371: the scrubber's keys move through history, and anything else keeps
/// its normal meaning.
///
/// The consumed/not-consumed split is the point. A scrubber that swallowed
/// every key would be a mode, and the user would lose their editor to a
/// gesture they meant as a glance.
#[test]
fn the_scrubber_consumes_its_own_keys_and_no_others() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let fake = |h: &str, sum: &str| crate::git::GraphCommit {
        hash: String::from(h),
        short_hash: String::from(&h[..7]),
        parents: Vec::new(),
        refs: Vec::new(),
        summary: String::from(sum),
        author: String::from("t"),
        age_secs: 0,
    };
    app.scrubber = Some(crate::scrubber::Scrubber::new(vec![
        fake("aaaaaaa1", "newest"),
        fake("bbbbbbb2", "older"),
    ]));

    // Left steps back and lands on HEAD, not past it.
    assert!(app.handle_scrubber_key(KeyCode::Left));
    assert_eq!(
        app.scrubber.as_ref().unwrap().position(),
        crate::scrubber::Position::At(0)
    );
    assert!(app.status.contains("aaaaaaa"), "status: {}", app.status);
    assert!(
        app.status.contains("newest"),
        "the summary helps: {}",
        app.status
    );

    // Home returns to the working tree from anywhere.
    assert!(app.handle_scrubber_key(KeyCode::Left));
    assert!(app.handle_scrubber_key(KeyCode::Home));
    assert_eq!(
        app.scrubber.as_ref().unwrap().position(),
        crate::scrubber::Position::Working
    );

    // A key the scrubber does not use is NOT consumed, so it still means
    // whatever it meant before.
    assert!(
        !app.handle_scrubber_key(KeyCode::Char('x')),
        "an unrelated key must pass through"
    );
    assert!(app.scrubber.is_some(), "and must not close the scrubber");

    // Esc closes, and the live buffer is simply what is there — the
    // scrubber never replaced it, so closing cannot lose unsaved work.
    assert!(app.handle_scrubber_key(KeyCode::Esc));
    assert!(app.scrubber.is_none());
    // With no scrubber open, its keys are not consumed either.
    assert!(!app.handle_scrubber_key(KeyCode::Left));
}

#[test]
fn a_staged_view_reruns_when_a_status_refresh_lands_unchanged() {
    // Replacing an already-staged file's bytes leaves porcelain status
    // identical ("M  seed.txt" before and after), so a "status changed"
    // gate would never re-run the staged view. The completed refresh is the
    // signal, and it flows through the real worker drain here.
    let (tmp, f) = repo_with_seed("alpha\n");
    let root = tmp.path().to_path_buf();
    let mut app = App::new(root.clone()).unwrap();
    // Let the worker's first status land so later ones can compare equal.
    crate::test_budget::await_spawned(
        crate::test_budget::tests::RESTORED_SHELL_BASE,
        "the git worker's first status",
        || {
            app.try_install_pending_init();
            app.git.status().in_repo
        },
    );
    std::fs::write(&f, "alpha\nbeta\n").unwrap();
    git_ok(&root, &["add", "seed.txt"]);
    let raw = crate::git::diff_staged(&root).unwrap();
    app.editor
        .open_git_diff_side_by_side(std::path::Path::new("git diff --staged"), &raw)
        .unwrap();
    app.tag_open_diff(crate::widgets::diff::DiffSource::GitCommand {
        root: root.clone(),
        kind: crate::widgets::diff::GitDiffKind::Staged,
    });
    assert!(
        app.editor
            .diff
            .as_ref()
            .unwrap()
            .right_lines
            .iter()
            .any(|l| l == "beta")
    );

    // Same porcelain status, different index bytes.
    std::fs::write(&f, "alpha\ngamma\n").unwrap();
    git_ok(&root, &["add", "seed.txt"]);
    app.active_git_bypass_debounce();
    app.refresh_git_status_debounced();
    crate::test_budget::await_spawned(
        crate::test_budget::tests::RESTORED_SHELL_BASE,
        "the staged view to show the re-staged bytes",
        || {
            app.try_install_pending_init();
            app.editor
                .diff
                .as_ref()
                .is_some_and(|d| d.right_lines.iter().any(|l| l == "gamma"))
        },
    );
}

#[test]
fn a_rebuild_that_changes_the_rows_drops_the_hunk_anchor() {
    // `nav_anchor` is the one input to `action_row`, which S/U/R read. Over
    // rebuilt rows the remembered index names whatever now sits there, so
    // the anchor is dropped and the actions fall back to the viewport, as
    // after a manual scroll.
    let (tmp, f) = repo_with_seed("a\nb\nc\nd\ne\n");
    let root = tmp.path().to_path_buf();
    let mut app = App::new(root.clone()).unwrap();
    std::fs::write(&f, "a\nB\nc\nd\nE\n").unwrap();
    app.editor
        .open_head_diff_with_text(
            std::path::PathBuf::from("seed.txt (HEAD)"),
            "a\nb\nc\nd\ne\n",
            &f,
            true,
        )
        .unwrap();
    app.tag_open_diff(crate::widgets::diff::DiffSource::HeadVsWorking {
        root: root.clone(),
        rel: String::from("seed.txt"),
    });
    app.jump_diff_change(true);
    assert!(
        app.editor.diff.as_ref().unwrap().nav_anchor.is_some(),
        "precondition: a jump anchored a hunk"
    );
    // A new hunk ABOVE the anchored one shifts every row under the index.
    rewrite_later(&f, "A\nB\nc\nd\nE\n");
    assert!(app.refresh_open_diff_views(false, &[], &no_paths()));
    assert_eq!(
        app.editor.diff.as_ref().unwrap().nav_anchor,
        None,
        "the anchor does not survive rows it no longer describes"
    );
}

/// A staged pipe a LIVE croft holds survives the sweep at any age (#362).
///
/// Age alone was the whole rule, and age alone deletes a file that is open in
/// a tab as soon as the session outlives the window. That is not litter:
/// `sqlite_view::table_page` re-opens by path on every page turn, so the tab
/// breaks, and `vault read ... | croft view -` cannot be produced again by
/// re-running anything. The server stamps its pid in at open time and this
/// asserts the sweep honours it.
///
/// Both files here are twice the retention age, so age cannot be what
/// separates them: only the holder's liveness can.
#[test]
fn the_staging_sweep_spares_a_pipe_a_live_croft_still_holds() {
    let tmp = tempfile::tempdir().unwrap();
    let staged = tmp.path().join("view-stdin");
    std::fs::create_dir_all(&staged).unwrap();

    // A live pid that is NOT this process, so `holder == process::id()` in
    // production cannot be what spares it and the liveness probe has to run.
    let mut peer = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let live = peer.id();
    let dead = {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        pid
    };

    let held_live = staged.join(format!("held-{live}-stdin-999999-0.csv"));
    let held_dead = staged.join(format!("held-{dead}-stdin-999999-1.csv"));
    let unclaimed = staged.join("stdin-999999-2.csv");
    for p in [&held_live, &held_dead, &unclaimed] {
        std::fs::write(p, b"a,b\n1,2\n").unwrap();
    }
    let old = std::time::SystemTime::now() - (STAGED_STDIN_RETENTION * 2);
    for p in [&held_live, &held_dead, &unclaimed] {
        let f = std::fs::File::options().write(true).open(p).unwrap();
        f.set_times(std::fs::FileTimes::new().set_modified(old))
            .unwrap();
    }

    crate::app::sweep_dead_view_sockets(tmp.path());
    let live_survived = held_live.exists();
    let _ = peer.kill();
    let _ = peer.wait();

    assert!(
        live_survived,
        "a pipe held by a running croft must survive the sweep at any age: \
         the tab re-opens it by path and the bytes cannot be regenerated"
    );
    assert!(
        !held_dead.exists(),
        "a pipe whose holder has exited, and which has aged out, is litter"
    );
    assert!(
        !unclaimed.exists(),
        "and an unclaimed staged file still ages out, or the sweep stops \
         bounding the pile at all"
    );
}

#[test]
fn the_staging_sweep_leaves_a_file_it_did_not_stage() {
    // The name filter, which had no negative case: the socket sweep has one
    // and this did not. A user who drops a note in the staging directory, or
    // any other program that writes there, must not have it deleted on a
    // guess just for being old.
    let tmp = tempfile::tempdir().unwrap();
    let staged = tmp.path().join("view-stdin");
    std::fs::create_dir_all(&staged).unwrap();
    let stranger = staged.join("notes.txt");
    std::fs::write(&stranger, b"not croft's").unwrap();
    let ours = staged.join("stdin-999999-0.txt");
    std::fs::write(&ours, b"croft's").unwrap();

    // Both older than the window, so age cannot be what separates them.
    let old = std::time::SystemTime::now() - (STAGED_STDIN_RETENTION * 2);
    for p in [&stranger, &ours] {
        let f = std::fs::File::options().write(true).open(p).unwrap();
        f.set_times(std::fs::FileTimes::new().set_modified(old))
            .unwrap();
    }

    crate::app::sweep_staged_stdin(tmp.path());

    assert!(
        stranger.exists(),
        "a file croft did not stage is not croft's to delete"
    );
    assert!(
        !ours.exists(),
        "and the control: a staged file that old goes"
    );
}

// ---- Interactive rebase in croft (#620) ----

#[test]
fn rebase_todo_keys_set_the_action_and_step_down() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(
        tmp.path(),
        "git-rebase-todo",
        "pick aaa111 first\npick bbb222 second\n# comment\n",
    );
    app.editor.cursor_row = 1;
    app.handle_key(crossterm::event::KeyEvent::new(
        KeyCode::Char('f'),
        KeyModifiers::NONE,
    ))
    .unwrap();
    assert_eq!(app.editor.lines[1], "fixup bbb222 second");
    assert_eq!(
        app.editor.cursor_row, 1,
        "no commit line follows, so the caret stays on the last one"
    );
    app.editor.cursor_row = 2;
    app.handle_key(crossterm::event::KeyEvent::new(
        KeyCode::Char('d'),
        KeyModifiers::NONE,
    ))
    .unwrap();
    assert_eq!(
        app.editor.lines[2], "d# comment",
        "a comment line types normally"
    );
}

fn press_rebase_keys(app: &mut App, keys: &str) {
    for c in keys.chars() {
        app.handle_key(crossterm::event::KeyEvent::new(
            KeyCode::Char(c),
            KeyModifiers::NONE,
        ))
        .unwrap();
    }
}

const REBASE_PLAN: &str = "pick aaa111 c2\npick bbb222 c3\npick ccc333 c4\n\n# Rebase 805d070..ccc333 onto 805d070 (3 commands)\n#\n";

#[test]
fn a_key_past_the_last_commit_leaves_the_plan_valid() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "git-rebase-todo", REBASE_PLAN);
    app.editor.cursor_row = 1;
    press_rebase_keys(&mut app, "sss");
    assert_eq!(
        &app.editor.lines[..4],
        &["pick aaa111 c2", "squash bbb222 c3", "squash ccc333 c4", ""],
        "the third key lands on the last commit again, not the blank line"
    );
    assert_eq!(app.editor.cursor_row, 2);
}

#[test]
fn an_action_key_on_a_blank_plan_line_types_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "git-rebase-todo", REBASE_PLAN);
    app.editor.cursor_row = 3;
    press_rebase_keys(&mut app, "psd");
    assert_eq!(app.editor.lines[3], "", "the blank line stays blank");
    assert_eq!(app.editor.cursor_row, 3);
}

#[test]
fn the_caret_skips_comments_to_the_next_commit() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(
        tmp.path(),
        "git-rebase-todo",
        "pick aaa111 c2\n# keep this one\n\npick bbb222 c3\n",
    );
    press_rebase_keys(&mut app, "r");
    assert_eq!(app.editor.lines[0], "reword aaa111 c2");
    assert_eq!(app.editor.cursor_row, 3, "past the comment and the blank");
    press_rebase_keys(&mut app, "f");
    assert_eq!(app.editor.lines[3], "fixup bbb222 c3");
}

#[test]
fn squash_on_the_first_commit_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "git-rebase-todo", REBASE_PLAN);
    press_rebase_keys(&mut app, "s");
    assert_eq!(
        app.editor.lines[0], "pick aaa111 c2",
        "the line is unchanged"
    );
    assert_eq!(app.editor.cursor_row, 0, "the caret stays put");
    assert!(
        app.status.contains("Can't squash the first commit"),
        "{}",
        app.status
    );
    press_rebase_keys(&mut app, "f");
    assert_eq!(app.editor.lines[0], "pick aaa111 c2");
    assert!(app.status.contains("Can't fixup"), "{}", app.status);
}

#[test]
fn squash_below_only_dropped_commits_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "git-rebase-todo", REBASE_PLAN);
    press_rebase_keys(&mut app, "ds");
    assert_eq!(app.editor.lines[0], "drop aaa111 c2");
    assert_eq!(app.editor.lines[1], "pick bbb222 c3");
    assert!(app.status.contains("Can't squash"), "{}", app.status);
}

#[test]
fn other_actions_on_the_first_commit_still_apply() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "git-rebase-todo", REBASE_PLAN);
    press_rebase_keys(&mut app, "e");
    assert_eq!(app.editor.lines[0], "edit aaa111 c2");
    assert_eq!(app.editor.cursor_row, 1);
    assert_eq!(app.status, crate::rebase_todo::HINT);
}

#[test]
fn rebase_keys_type_normally_outside_a_todo_file() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "notes.txt", "pick aaa111 first\n");
    app.handle_key(crossterm::event::KeyEvent::new(
        KeyCode::Char('s'),
        KeyModifiers::NONE,
    ))
    .unwrap();
    assert_eq!(app.editor.lines[0], "spick aaa111 first");
}

#[test]
fn the_sequence_editor_command_quotes_the_binary_path() {
    assert_eq!(
        crate::widgets::terminal::sequence_editor_command(std::path::Path::new(
            "/opt/my croft/croft"
        )),
        "'/opt/my croft/croft' edit --wait --sequence-editor"
    );
}

/// Wait for the history scrubber's finished view of where it stands to
/// replace the plain stand-in (#371); the builder runs on its own thread.
fn settle_scrub_view(app: &mut App) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !matches!(app.scrub_view_key, Some((_, true))) {
        assert!(
            std::time::Instant::now() < deadline,
            "the scrubber's view was never built"
        );
        app.drain_scrub_views();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[test]
fn stepping_back_to_a_finished_scrub_view_reuses_it() {
    // #371: a step never waits on highlighting, and a commit whose view was
    // already built shows it at once rather than building it again.
    let repo = scrub_repo();
    let mut app = App::new(repo.path().to_path_buf()).unwrap();
    app.editor.open(&repo.path().join("a.txt")).unwrap();
    app.scrub_history();
    assert!(app.handle_scrubber_key(KeyCode::Left), "to HEAD");
    settle_scrub_view(&mut app);
    assert!(app.handle_scrubber_key(KeyCode::Left), "one older");
    settle_scrub_view(&mut app);
    assert!(app.handle_scrubber_key(KeyCode::Right), "back to HEAD");
    assert!(
        matches!(app.scrub_view_key, Some((_, true))),
        "HEAD's finished view was kept"
    );
    assert_eq!(
        app.scrub_view.as_ref().unwrap().lines,
        vec!["v1", "v2", "v3"]
    );
    // Home drops the view; Esc drops the kept ones with the builder.
    assert!(app.handle_scrubber_key(KeyCode::Home));
    assert!(app.scrub_view.is_none() && app.scrub_view_key.is_none());
    assert!(app.handle_scrubber_key(KeyCode::Esc));
    assert!(app.scrub_views.is_empty() && app.scrub_builder.is_none());
}

#[test]
fn scrubbing_shows_the_file_at_each_commit_and_its_changes() {
    // #371: at each position the editor shows the file at that commit, and
    // the git gutter shows that commit's own change.
    let repo = scrub_repo();
    let mut app = App::new(repo.path().to_path_buf()).unwrap();
    app.editor.open(&repo.path().join("a.txt")).unwrap();
    app.scrub_history();
    assert!(app.handle_scrubber_key(KeyCode::Left), "to HEAD");
    settle_scrub_view(&mut app);
    let view = app.scrub_view.as_mut().expect("a historical view at HEAD");
    assert_eq!(view.lines, vec!["v1", "v2", "v3"]);
    assert_eq!(
        view.git_mark_at(2),
        Some(crate::widgets::editor::GitMark::Added)
    );
    assert_eq!(view.git_mark_at(1), None, "unchanged in that commit");
    assert!(app.handle_scrubber_key(KeyCode::Left), "one older");
    settle_scrub_view(&mut app);
    let view = app.scrub_view.as_mut().unwrap();
    assert_eq!(view.lines, vec!["v1", "v2"]);
    assert_eq!(
        view.git_mark_at(1),
        Some(crate::widgets::editor::GitMark::Added)
    );
    assert!(app.handle_scrubber_key(KeyCode::Left), "the root commit");
    settle_scrub_view(&mut app);
    let view = app.scrub_view.as_mut().unwrap();
    assert_eq!(view.lines, vec!["v1"]);
    assert_eq!(
        view.git_mark_at(0),
        Some(crate::widgets::editor::GitMark::Added),
        "a root commit adds every line"
    );
    // Painted in place of the live buffer.
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let buf = term.backend().buffer();
    let mut screen = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            screen.push_str(buf[(x, y)].symbol());
        }
        screen.push('\n');
    }
    // Look at editor lines only (`N ┃text`): the explorer shows the temp
    // dir's random name, which can itself contain "v2".
    assert!(
        screen.contains("┃v1") && !screen.contains("┃v2"),
        "only the root commit's text shows:\n{screen}"
    );
}

/// The repo from #1304: `old.py` gets v1 and v2, is renamed to `new.py`, an
/// unrelated commit lands, then `new.py` gets v3. Returns the repo and the
/// text after each version.
fn renamed_file_repo() -> (tempfile::TempDir, [&'static str; 3]) {
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
    let texts = [
        "def v1():\n    pass\n",
        "def v1():\n    pass\n\ndef v2():\n    pass\n",
        "def v1():\n    pass\n\ndef v2():\n    pass\n\ndef v3():\n    pass\n",
    ];
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.email", "a@b"]);
    git(&["config", "user.name", "a"]);
    std::fs::write(tmp.path().join("old.py"), texts[0]).unwrap();
    git(&["add", "old.py"]);
    git(&["commit", "-q", "-m", "v1 in old.py"]);
    std::fs::write(tmp.path().join("old.py"), texts[1]).unwrap();
    git(&["commit", "-q", "-am", "v2"]);
    git(&["mv", "old.py", "new.py"]);
    git(&["commit", "-q", "-m", "rename"]);
    std::fs::write(tmp.path().join("notes.txt"), "unrelated\n").unwrap();
    git(&["add", "notes.txt"]);
    git(&["commit", "-q", "-m", "unrelated"]);
    std::fs::write(tmp.path().join("new.py"), texts[2]).unwrap();
    git(&["commit", "-q", "-am", "v3"]);
    (tmp, texts)
}

fn lines_of(text: &str) -> Vec<String> {
    text.lines().map(str::to_string).collect()
}

#[test]
fn scrubbing_follows_a_renamed_file_back_past_the_rename() {
    let (repo, texts) = renamed_file_repo();
    let mut app = App::new(repo.path().to_path_buf()).unwrap();
    app.editor.open(&repo.path().join("new.py")).unwrap();
    app.scrub_history();
    fn step(app: &mut App) -> &mut crate::widgets::editor::Editor {
        assert!(app.handle_scrubber_key(KeyCode::Left));
        settle_scrub_view(app);
        app.scrub_view.as_mut().unwrap()
    }
    assert_eq!(step(&mut app).lines, lines_of(texts[2]), "v3");
    assert_eq!(step(&mut app).lines, lines_of(texts[1]), "unrelated");
    let view = step(&mut app);
    assert_eq!(view.lines, lines_of(texts[1]), "rename");
    for row in 0..view.lines.len() {
        assert_eq!(
            view.git_mark_at(row),
            None,
            "the rename changed no line, so row {row} has no mark"
        );
    }
    let view = step(&mut app);
    assert_eq!(view.lines, lines_of(texts[1]), "v2 shows old.py's text");
    assert_eq!(
        view.git_mark_at(4),
        Some(crate::widgets::editor::GitMark::Added),
        "v2's own change is marked against v1 in old.py"
    );
    assert_eq!(view.git_mark_at(0), None);
    assert_eq!(step(&mut app).lines, lines_of(texts[0]), "v1 in old.py");
}

#[test]
fn the_scrub_bar_names_the_file_as_it_was_called_before_a_rename() {
    let (repo, _) = renamed_file_repo();
    let mut app = App::new(repo.path().to_path_buf()).unwrap();
    app.editor.open(&repo.path().join("new.py")).unwrap();
    app.scrub_history();
    let bar = |app: &mut App| {
        settle_scrub_view(app);
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30)).unwrap();
        term.draw(|f| app.render(f)).unwrap();
        let buf = term.backend().buffer();
        let y = app.scrub_slider.y;
        (0..buf.area.width)
            .map(|x| buf[(x, y)].symbol().to_string())
            .collect::<String>()
    };
    for _ in 0..3 {
        assert!(app.handle_scrubber_key(KeyCode::Left));
    }
    let at_rename = bar(&mut app);
    assert!(!at_rename.contains("old.py"), "{at_rename}");
    assert!(app.handle_scrubber_key(KeyCode::Left));
    let at_v2 = bar(&mut app);
    assert!(at_v2.contains("2/5 \u{b7} old.py"), "{at_v2}");
}

#[test]
fn a_file_added_later_still_did_not_exist_before_it() {
    // Negative: following renames must not invent a history for a file
    // that was created, not moved.
    let (repo, _) = renamed_file_repo();
    let mut app = App::new(repo.path().to_path_buf()).unwrap();
    app.editor.open(&repo.path().join("notes.txt")).unwrap();
    app.scrub_history();
    for _ in 0..3 {
        assert!(app.handle_scrubber_key(KeyCode::Left));
    }
    settle_scrub_view(&mut app);
    let view = app.scrub_view.as_ref().unwrap();
    assert_eq!(view.lines.len(), 1, "{:?}", view.lines);
    assert!(
        view.lines[0].starts_with("(notes.txt did not exist at "),
        "{:?}",
        view.lines
    );
}

#[test]
fn leaving_the_scrubber_restores_the_live_buffer_with_its_unsaved_edits() {
    let repo = scrub_repo();
    let mut app = App::new(repo.path().to_path_buf()).unwrap();
    app.editor.open(&repo.path().join("a.txt")).unwrap();
    app.editor.lines.push(String::from("UNSAVED LIVE EDIT"));
    app.editor.dirty = true;
    let live = app.editor.lines.clone();
    app.scrub_history();
    app.handle_scrubber_key(KeyCode::Left);
    app.handle_scrubber_key(KeyCode::Left);
    app.focus_pane(Pane::Editor);
    // Typing while looking at history must not reach the hidden buffer.
    app.handle_key(key(KeyCode::Char('x'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.editor.lines, live, "the live buffer took no keystroke");
    assert!(app.status.contains("history"), "{}", app.status);
    app.handle_scrubber_key(KeyCode::Esc);
    assert!(app.scrub_view.is_none(), "the historical view is gone");
    assert_eq!(app.editor.lines, live);
    assert!(app.editor.dirty, "dirty state untouched");
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let buf = term.backend().buffer();
    let mut screen = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            screen.push_str(buf[(x, y)].symbol());
        }
    }
    assert!(
        screen.contains("UNSAVED LIVE EDIT"),
        "the live buffer is painted again"
    );
}

#[test]
fn a_missing_file_from_a_known_repository_downloads_once_its_host_is_trusted() {
    // #577: the run's versionControlProvenance names the repository and
    // commit; nothing is fetched until the user trusts the host.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("remote.sarif");
        std::fs::write(
            &log,
            r#"{"version":"2.1.0","runs":[{"tool":{"driver":{"name":"ci"}},
              "versionControlProvenance":[{"repositoryUri":"https://github.com/o/r","revisionId":"c0ffee"}],
              "results":[{"message":{"text":"m"},"locations":[{"physicalLocation":{"artifactLocation":{"uri":"src/gone.rs"},"region":{"startLine":2,"startColumn":3}}}]}]}]}"#,
        )
        .unwrap();
        fn fake(url: &str) -> Result<Vec<u8>, String> {
            assert_eq!(
                url,
                "https://raw.githubusercontent.com/o/r/c0ffee/src/gone.rs"
            );
            Ok(b"one\n  two\nthree\n".to_vec())
        }
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.sarif_fetch = fake;
        app.editor.open_preview(&log).unwrap();
        app.editor.sarif.as_mut().unwrap().selected = 1;
        app.open_selected_sarif_result();
        match app.input_prompt.as_ref().map(|p| &p.purpose) {
            Some(crate::widgets::input_prompt::InputPurpose::SarifTrustHost { host, .. }) => {
                assert_eq!(host, "github.com")
            }
            other => panic!("a trust prompt, not {other:?}: {}", app.status),
        }
        assert!(app.sarif_download.is_none(), "nothing fetched before trust");
        // Enter on the empty field trusts the host and downloads.
        app.submit_input_prompt();
        assert!(crate::sarif::resolve::trusted_hosts().contains(&"github.com".to_string()));
        let rx_wait = std::time::Instant::now();
        while app.sarif_download.is_some() && rx_wait.elapsed().as_secs() < 10 {
            app.poll_sarif_download();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let opened = app.editor.path.clone().expect("the download opened");
        assert!(opened.starts_with(croft_cache_dir()), "{opened:?}");
        assert_eq!(opened.file_name().unwrap(), "gone.rs");
        assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (1, 2));

        // Now trusted: the next open downloads without asking.
        let mut again = App::new(tmp.path().to_path_buf()).unwrap();
        again.sarif_fetch = fake;
        again.editor.open_preview(&log).unwrap();
        again.editor.sarif.as_mut().unwrap().selected = 1;
        again.open_selected_sarif_result();
        assert!(again.input_prompt.is_none(), "{}", again.status);
        assert!(again.sarif_download.is_some());
    });
}

/// #371: at a scrubbed commit, Enter opens that version of the file as a
/// tab of its own, and "Diff Scrubbed File to Working Tree" diffs it
/// against the file on disk. Both leave the scrubber and the live buffer
/// (dirty or not) untouched. At the working tree, Enter is ordinary typing.
#[test]
fn the_scrubber_opens_a_commits_version_here_or_diffs_it_to_the_working_tree() {
    let repo = scrub_repo();
    let file = repo.path().join("a.txt");
    // Uncommitted work on disk, and more unsaved in the buffer.
    std::fs::write(&file, "v1\nv2\nv3\nv4\n").unwrap();
    let mut app = App::new(repo.path().to_path_buf()).unwrap();
    app.editor.open(&file).unwrap();
    app.editor.lines.push(String::from("unsaved"));
    app.editor.dirty = true;
    app.scrub_history();
    assert!(
        !app.handle_scrubber_key(KeyCode::Enter),
        "at the working tree there is nothing to open"
    );
    assert!(app.handle_scrubber_key(KeyCode::Left), "to HEAD");
    assert!(app.handle_scrubber_key(KeyCode::Left), "to v2");
    let short = app
        .scrubber
        .as_ref()
        .unwrap()
        .commit()
        .unwrap()
        .short_hash
        .clone();

    assert!(app.handle_scrubber_key(KeyCode::Enter));
    assert!(app.scrubber.is_none() && app.scrub_view.is_none());
    assert_eq!(
        app.editor.path.as_deref(),
        Some(Path::new(&format!("a.txt @ {short}")))
    );
    assert_eq!(app.editor.lines, vec!["v1", "v2"]);
    let live = app.editor.find_tab_with_path(&file).expect("the live tab");
    app.editor.select(live);
    assert!(app.editor.dirty, "the live buffer keeps its unsaved edit");
    assert_eq!(app.editor.lines.last().map(String::as_str), Some("unsaved"));

    app.scrub_history();
    assert!(app.handle_scrubber_key(KeyCode::Left));
    assert!(app.handle_scrubber_key(KeyCode::Left));
    app.run_command(crate::widgets::command_palette::Command::ScrubDiffToWorkingTree);
    assert!(app.scrubber.is_none());
    let diff = app.editor.diff.as_ref().expect("a diff tab");
    assert_eq!(diff.left_lines, vec!["v1", "v2"]);
    assert_eq!(
        diff.right_lines,
        vec!["v1", "v2", "v3", "v4", "unsaved"],
        "the working copy as the user has it, unsaved edit included (#1572)"
    );

    // With no scrubbed commit, the commands say so rather than guess.
    app.run_command(crate::widgets::command_palette::Command::ScrubOpenHere);
    assert!(app.status.contains("Step the scrubber"), "{}", app.status);
}

/// #371: switching to another file while scrubbing shows that file at the
/// scrubbed commit, not the previous file's history over it.
#[test]
fn switching_files_while_scrubbing_shows_the_new_files_history() {
    let repo = scrub_repo();
    std::fs::write(repo.path().join("new.txt"), "fresh\n").unwrap();
    let mut app = App::new(repo.path().to_path_buf()).unwrap();
    app.editor.open(&repo.path().join("a.txt")).unwrap();
    app.scrub_history();
    assert!(app.handle_scrubber_key(KeyCode::Left), "to HEAD");
    assert_eq!(
        app.scrub_view.as_ref().unwrap().lines,
        vec!["v1", "v2", "v3"]
    );
    app.editor.open(&repo.path().join("new.txt")).unwrap();
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let view = app.scrub_view.as_ref().expect("still scrubbing");
    assert!(
        view.lines[0].contains("new.txt did not exist"),
        "{:?}",
        view.lines
    );
}

/// #371: the scrubber's slider sits on the editor's bottom row; a click at
/// its left end shows the oldest commit, and dragging to its right end
/// returns to the working tree.
#[test]
fn the_scrubber_slider_seeks_on_click_and_drag() {
    let repo = scrub_repo();
    let mut app = App::new(repo.path().to_path_buf()).unwrap();
    app.editor.open(&repo.path().join("a.txt")).unwrap();
    app.scrub_history();
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let track = app.scrub_slider;
    assert!(track.width > 10, "the slider lays out: {track:?}");
    let row: String = (0..120)
        .map(|x| term.backend().buffer()[(x, track.y)].symbol().to_string())
        .collect();
    assert!(row.contains("working tree"), "{row}");
    let mouse = |app: &mut App, kind, column| {
        app.handle_mouse(crossterm::event::MouseEvent {
            kind,
            column,
            row: track.y,
            modifiers: KeyModifiers::NONE,
        });
    };
    mouse(
        &mut app,
        crossterm::event::MouseEventKind::Down(MouseButton::Left),
        track.x,
    );
    assert_eq!(
        app.scrub_view.as_ref().unwrap().lines,
        vec!["v1"],
        "the oldest commit"
    );
    term.draw(|f| app.render(f)).unwrap();
    let row: String = (0..120)
        .map(|x| term.backend().buffer()[(x, track.y)].symbol().to_string())
        .collect();
    assert!(row.contains(" 1/3 "), "{row}");
    // The drag keeps seeking off the row too.
    mouse(
        &mut app,
        crossterm::event::MouseEventKind::Drag(MouseButton::Left),
        track.x + track.width + 5,
    );
    assert!(app.scrub_view.is_none(), "back at the working tree");
    mouse(
        &mut app,
        crossterm::event::MouseEventKind::Up(MouseButton::Left),
        0,
    );
    assert!(!app.scrub_dragging);
    assert!(app.scrubber.is_some(), "the scrubber stays open");
}

/// #371's criterion, measured: after a warm-up pass, holding an arrow key
/// back across up to 200 commits of this repository's own
/// `src/app/mod.rs` stays under a frame (16 ms) per step, the step and the
/// frame it paints. Highlighting that file takes over half a second, so a
/// step shows the plain text the builder already split and swaps the
/// finished view in behind it. Needs this checkout's history, so it is
/// ignored by default.
#[test]
#[ignore]
fn scrubbing_the_biggest_file_stays_under_a_frame_per_step() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut app = App::new(root.clone()).unwrap();
    app.editor.open(&root.join("src/app/mod.rs")).unwrap();
    app.scrub_history();
    let steps = app.scrubber.as_ref().map_or(0, |s| s.len()).min(200);
    assert!(steps > 20, "only {steps} commits to scrub");
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 50)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    // A held arrow key repeats about 30 times a second; between repeats the
    // main loop takes in what the builder produced.
    let repeat = std::time::Duration::from_millis(33);
    // Warm-up, as the criterion allows: one pass reads every version.
    for _ in 0..steps {
        std::thread::sleep(repeat);
        app.drain_scrub_views();
        app.handle_scrubber_key(KeyCode::Left);
        app.sync_outline();
    }
    app.handle_scrubber_key(KeyCode::Home);
    let mut times = Vec::with_capacity(steps);
    for _ in 0..steps {
        std::thread::sleep(repeat);
        app.drain_scrub_views();
        let t = std::time::Instant::now();
        app.handle_scrubber_key(KeyCode::Left);
        // What the main loop runs between the key and the frame: the
        // Outline follows the commit on screen.
        app.sync_outline();
        term.draw(|f| app.render(f)).unwrap();
        times.push(t.elapsed());
    }
    times.sort();
    let p50 = times[times.len() / 2];
    let p95 = times[times.len() * 95 / 100];
    let max = *times.last().unwrap();
    eprintln!("{steps} steps: p50 {p50:?} p95 {p95:?} max {max:?}");
    assert!(p95 < std::time::Duration::from_millis(16), "p95 {p95:?}");
}

/// What `git push` writes to stderr for a push to GitHub: `remote:`
/// chatter, the destination, then the ref update.
const PUSH_OUTPUT_858: &str = "remote: \nremote: Create a pull request for 'main' on GitHub by visiting:\nremote:      https://github.com/o/r/pull/new/main\nremote: \nTo github.com:o/r.git\n   5a6cd5c..9f1e2d3  main -> main";

/// #858: after Commit & Push the status bar said "Committed & pushed:"
/// followed by git push's whole multi-line stderr, squashed into the one
/// status row, which is what the issue asked to stop. Status and panel now
/// carry one line (the commit's headline and the ref update); the push's
/// full text is in the Git Output log.
#[test]
fn commit_and_push_reports_one_line_and_logs_the_push() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.finish_commit_and_push(
        String::from("[main 9f1e2d3] fix: one line"),
        Ok(String::from(PUSH_OUTPUT_858)),
    );
    let feedback = app.source_control.commit_feedback.clone().unwrap();
    for text in [&app.status, &feedback] {
        assert!(!text.contains('\n'), "one line: {text:?}");
        assert!(!text.contains("remote:"), "no remote chatter: {text:?}");
        assert!(text.contains("[main 9f1e2d3] fix: one line"), "{text:?}");
        assert!(text.contains("main -> main"), "{text:?}");
    }
    assert!(!app.source_control.commit_feedback_is_error);
    let logged = app.git_output_log.last().cloned().unwrap_or_default();
    assert!(
        logged.contains("Create a pull request"),
        "the Git Output log keeps the whole push: {logged:?}"
    );
}

/// #858: Push (the commit menu's) reported `Pushed: ` and the same
/// multi-line stderr; it now reports the ref update alone.
#[test]
fn push_reports_its_ref_update_on_one_line_and_logs_the_rest() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.finish_push(Ok(String::from(PUSH_OUTPUT_858)));
    assert_eq!(app.status, "Pushed: 5a6cd5c..9f1e2d3 main -> main");
    assert_eq!(
        app.source_control.commit_feedback.as_deref(),
        Some("pushed: 5a6cd5c..9f1e2d3 main -> main")
    );
    let logged = app.git_output_log.last().cloned().unwrap_or_default();
    assert!(
        logged.contains("github.com/o/r/pull/new/main"),
        "{logged:?}"
    );
}

/// #858 negative: a push whose output is only `remote:` chatter reports
/// that it pushed and nothing of the chatter, never a stray `remote:`.
#[test]
fn a_push_with_only_remote_chatter_reports_just_pushed() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let chatter = "remote: Resolving deltas: 100% (2/2)\nremote: \nremote: Done";
    app.finish_push(Ok(String::from(chatter)));
    assert_eq!(app.status, "Pushed");
    assert_eq!(
        app.source_control.commit_feedback.as_deref(),
        Some("pushed")
    );
    app.finish_commit_and_push(
        String::from("[main 9f1e2d3] fix: one line"),
        Ok(String::from(chatter)),
    );
    assert_eq!(
        app.status,
        "Committed & pushed: [main 9f1e2d3] fix: one line"
    );
    // A one-line push result is kept as git wrote it.
    app.finish_push(Ok(String::from("Everything up-to-date")));
    assert_eq!(app.status, "Pushed: Everything up-to-date");
}

/// #858 negative: a failed push still names the error, on one line: the
/// rejected ref update with git's reason, or git's `fatal:` line, never the
/// `To` destination or a `hint:`. It stays an error.
#[test]
fn a_failed_push_still_names_the_error_on_one_line() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let rejected = "To github.com:o/r.git\n ! [rejected]        main -> main (fetch first)\nerror: failed to push some refs to 'github.com:o/r.git'\nhint: Updates were rejected because the remote contains work that you do\nhint: not have locally.";
    app.finish_commit_and_push(
        String::from("[main 9f1e2d3] fix: one line"),
        Err(String::from(rejected)),
    );
    assert!(app.source_control.commit_feedback_is_error);
    assert_eq!(
        app.status,
        "Commit ok; push failed: [rejected] main -> main (fetch first)"
    );
    assert_eq!(
        app.source_control.commit_feedback.as_deref(),
        Some("commit ok; push failed: [rejected] main -> main (fetch first)")
    );
    let fatal = "fatal: 'origin' does not appear to be a git repository\nfatal: Could not read from remote repository.\n\nPlease make sure you have the correct access rights\nand the repository exists.";
    app.finish_push(Err(String::from(fatal)));
    assert!(app.source_control.commit_feedback_is_error);
    assert_eq!(
        app.status,
        "Push failed: fatal: 'origin' does not appear to be a git repository"
    );
    let logged = app.git_output_log.last().cloned().unwrap_or_default();
    assert!(logged.contains("correct access rights"), "{logged:?}");
}

/// pricing.py as `main` and `feature` both start from it (#858).
const PRICING_858: &str = "TAX_RATE = 0.21\nFREE_SHIPPING_OVER = 50\n\n\ndef shipping(total):\n    if total >= FREE_SHIPPING_OVER:\n        return 0\n    return 3.99\n";

/// A repo on `main` whose `feature` branch writes `feature_pricing` to
/// pricing.py (and adds feature.txt) while `main` raises FREE_SHIPPING_OVER
/// to 75 (#858). With `feature` lowering it to 40, merging or rebasing
/// stops on a conflict in pricing.py; with [`PRICING_858`] unchanged it is
/// clean.
fn pricing_repo_858(feature_pricing: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().to_path_buf();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&p)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git setup step failed: {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q", "-b", "main"]);
    for (key, value) in [
        ("user.email", "a@b"),
        ("user.name", "a"),
        ("commit.gpgsign", "false"),
        ("merge.autoStash", "false"),
        ("rebase.autoStash", "false"),
    ] {
        git(&["config", key, value]);
    }
    std::fs::write(p.join("pricing.py"), PRICING_858).unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-qm", "base"]);
    git(&["checkout", "-q", "-b", "feature"]);
    std::fs::write(p.join("pricing.py"), feature_pricing).unwrap();
    std::fs::write(p.join("feature.txt"), "feature\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-qm", "feature: lower free shipping"]);
    git(&["checkout", "-q", "main"]);
    std::fs::write(
        p.join("pricing.py"),
        PRICING_858.replace("OVER = 50", "OVER = 75"),
    )
    .unwrap();
    git(&["commit", "-qam", "main: raise free shipping"]);
    tmp
}

/// [`pricing_repo_858`] whose merge or rebase stops on a conflict.
fn conflicting_pricing_repo_858() -> tempfile::TempDir {
    pricing_repo_858(&PRICING_858.replace("OVER = 50", "OVER = 40"))
}

/// A Branch submenu verb (Merge, Rebase, Delete Branch) on `branch`, the
/// way a user runs it: the SCM menu opens the branch picker, the name is
/// typed, Enter.
fn pick_branch_858(app: &mut App, action: crate::widgets::scm_menu::ScmAction, branch: &str) {
    app.dispatch_scm_action(action);
    assert!(app.branch_picker.is_some(), "{action:?} opens the picker");
    for c in branch.chars() {
        app.handle_key(key(KeyCode::Char(c), KeyModifiers::NONE))
            .unwrap();
    }
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
}

/// #858: SCM ⋯ → Branch → Merge onto a conflicting branch. Git's text
/// starts with `Auto-merging pricing.py`, and the panel and the status bar
/// said "Merged failed: Auto-merging pricing.py", which reads like a step
/// that went fine and leaves the conflict out. They now name the conflict,
/// on one line; the Git Output log keeps git's whole text.
#[test]
fn a_merge_stopped_on_a_conflict_names_the_conflict_not_auto_merging() {
    use crate::widgets::scm_menu::ScmAction;
    let tmp = conflicting_pricing_repo_858();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::SourceControl);
    pick_branch_858(&mut app, ScmAction::Merge, "feature");
    assert!(app.source_control.commit_feedback_is_error);
    assert_eq!(
        app.source_control.commit_feedback.as_deref(),
        Some("Merge: 1 conflict (pricing.py)")
    );
    assert_eq!(app.status, "Merge: 1 conflict (pricing.py)");
    let logged = app.git_output_log.last().cloned().unwrap_or_default();
    assert!(logged.starts_with("$ git merge"), "{logged:?}");
    assert!(
        logged.contains("Auto-merging pricing.py")
            && logged.contains("CONFLICT (content): Merge conflict in pricing.py"),
        "the Git Output log keeps git's whole text: {logged:?}"
    );
}

/// #858: SCM ⋯ → Branch → Rebase that stops on a conflict. Git names the
/// conflict on stdout and `error: could not apply …` on stderr, after a
/// `Rebasing (1/1)` progress line it redraws with a carriage return; only
/// stderr was kept, so the conflict never reached the panel, which said
/// "Rebased failed: Rebasing (1/1)…". It now names the conflict.
#[test]
fn a_rebase_stopped_on_a_conflict_names_the_conflict() {
    use crate::widgets::scm_menu::ScmAction;
    let tmp = conflicting_pricing_repo_858();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::SourceControl);
    pick_branch_858(&mut app, ScmAction::Rebase, "feature");
    assert!(app.source_control.commit_feedback_is_error);
    assert_eq!(
        app.source_control.commit_feedback.as_deref(),
        Some("Rebase: 1 conflict (pricing.py)")
    );
    assert_eq!(app.status, "Rebase: 1 conflict (pricing.py)");
    let logged = app.git_output_log.last().cloned().unwrap_or_default();
    assert!(
        logged.contains("CONFLICT (content): Merge conflict in pricing.py")
            && logged.contains("could not apply"),
        "the Git Output log has both of git's streams: {logged:?}"
    );
}

/// #858: a failed op names the operation, "Merge failed" or "Rebase
/// failed", not the past tense its success uses ("Merged failed",
/// "Rebased failed"), and a failure with no conflict names git's `error:`
/// or `fatal:` line. Covers the branch picker, an input prompt and a
/// worker op.
#[test]
fn a_failed_scm_op_names_the_operation_and_git_s_error_line() {
    use crate::widgets::scm_menu::ScmAction;
    let tmp = conflicting_pricing_repo_858();
    // A local edit the merge would overwrite, and that rebase refuses.
    std::fs::write(tmp.path().join("pricing.py"), "TAX_RATE = 0.2\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::SourceControl);
    let one_line = |app: &App| {
        assert!(app.source_control.commit_feedback_is_error);
        assert_eq!(
            app.source_control.commit_feedback.as_deref(),
            Some(app.status.as_str())
        );
        assert!(!app.status.contains('\n'), "{:?}", app.status);
    };

    pick_branch_858(&mut app, ScmAction::Merge, "feature");
    assert_eq!(
        app.status,
        "Merge failed: error: Your local changes to the following files would be overwritten by merge:"
    );
    one_line(&app);

    pick_branch_858(&mut app, ScmAction::Rebase, "feature");
    assert_eq!(
        app.status,
        "Rebase failed: error: cannot rebase: You have unstaged changes."
    );
    one_line(&app);

    pick_branch_858(&mut app, ScmAction::DeleteBranch, "main");
    assert!(
        app.status
            .starts_with("Delete branch failed: error: cannot delete branch 'main'"),
        "{:?}",
        app.status
    );
    one_line(&app);

    crate::git::create_tag(tmp.path(), "v1", None).unwrap();
    app.dispatch_scm_action(ScmAction::CreateTag);
    for c in "v1".chars() {
        app.input_prompt.as_mut().unwrap().push_char(c);
    }
    app.submit_input_prompt();
    // The optional message (#1582), left blank.
    app.submit_input_prompt();
    assert_eq!(
        app.status,
        "Create tag failed: fatal: tag 'v1' already exists"
    );
    one_line(&app);

    // No upstream: a worker op, reported when it lands.
    std::process::Command::new("git")
        .arg("-C")
        .arg(tmp.path())
        .args(["checkout", "-q", "--", "."])
        .status()
        .unwrap();
    app.dispatch_scm_action(ScmAction::PullRebase);
    wait_for_git_net(&mut app);
    assert_eq!(
        app.status,
        "Pull (rebase) failed: There is no tracking information for the current branch."
    );
    one_line(&app);
}

/// #858: Pull, Sync and Pop Stash stop on conflicts the same way a merge
/// does, `Auto-merging …` first; they name the conflict too, and the Git
/// Output log keeps git's text.
#[test]
fn a_pull_sync_or_stash_pop_stopped_on_a_conflict_names_the_conflict() {
    let tmp = make_committed_repo();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    };
    std::fs::write(tmp.path().join("seed.txt"), "stashed\n").unwrap();
    git(&["stash", "-q"]);
    std::fs::write(tmp.path().join("seed.txt"), "committed\n").unwrap();
    git(&["commit", "-qam", "conflicting"]);
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.stash_pop_source_control();
    assert!(app.source_control.commit_feedback_is_error);
    assert_eq!(app.status, "Stash pop: 1 conflict (seed.txt)");
    assert_eq!(
        app.source_control.commit_feedback.as_deref(),
        Some("stash pop: 1 conflict (seed.txt)")
    );
    let logged = app.git_output_log.last().cloned().unwrap_or_default();
    assert!(logged.contains("Auto-merging seed.txt"), "{logged:?}");

    // Git's text for a pull that fetched, then stopped merging.
    let pull = "Auto-merging pricing.py\nCONFLICT (content): Merge conflict in pricing.py\nAutomatic merge failed; fix conflicts and then commit the result.\nFrom github.com:o/r\n   5a6cd5c..9f1e2d3  main       -> origin/main";
    app.finish_pull(Err(String::from(pull)));
    assert!(app.source_control.commit_feedback_is_error);
    assert_eq!(app.status, "Pull: 1 conflict (pricing.py)");
    assert_eq!(
        app.source_control.commit_feedback.as_deref(),
        Some("pull: 1 conflict (pricing.py)")
    );
    let logged = app.git_output_log.last().cloned().unwrap_or_default();
    assert!(logged.contains("5a6cd5c..9f1e2d3"), "{logged:?}");

    app.finish_sync(Err(String::from(pull)), None);
    assert_eq!(app.status, "Sync failed on pull: 1 conflict (pricing.py)");
    assert_eq!(
        app.source_control.commit_feedback.as_deref(),
        Some("sync: pull failed: 1 conflict (pricing.py)")
    );
}

/// #858: a clean merge that auto-merged a file, and a clean rebase, report
/// git's result line, not the `Auto-merging` note before it or the
/// `Rebasing (1/1)` progress git redrew over.
#[test]
fn a_clean_merge_or_rebase_reports_git_s_result_line_not_its_progress() {
    use crate::widgets::scm_menu::ScmAction;
    // `feature` changes another line of pricing.py: git auto-merges it.
    let tmp = pricing_repo_858(&PRICING_858.replace("3.99", "4.99"));
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::SourceControl);
    pick_branch_858(&mut app, ScmAction::Merge, "feature");
    assert!(!app.source_control.commit_feedback_is_error);
    assert_eq!(app.status, "Merged: Merge made by the 'ort' strategy.");
    assert_eq!(
        app.source_control.commit_feedback.as_deref(),
        Some("Merged: Merge made by the 'ort' strategy.")
    );

    let tmp = pricing_repo_858(&PRICING_858.replace("3.99", "4.99"));
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::SourceControl);
    pick_branch_858(&mut app, ScmAction::Rebase, "feature");
    assert!(!app.source_control.commit_feedback_is_error);
    assert_eq!(
        app.status,
        "Rebased: Successfully rebased and updated refs/heads/main."
    );
}

/// #858 negative: a clean merge of another file still reports its normal
/// one-line success, as a success.
#[test]
fn a_clean_merge_still_reports_its_one_line_success() {
    use crate::widgets::scm_menu::ScmAction;
    let tmp = pricing_repo_858(PRICING_858);
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::SourceControl);
    pick_branch_858(&mut app, ScmAction::Merge, "feature");
    assert!(!app.source_control.commit_feedback_is_error);
    assert_eq!(app.status, "Merged: Merge made by the 'ort' strategy.");
    assert_eq!(
        app.source_control.commit_feedback.as_deref(),
        Some("Merged: Merge made by the 'ort' strategy.")
    );
    let logged = app.git_output_log.last().cloned().unwrap_or_default();
    assert!(logged.contains("feature.txt"), "{logged:?}");
}

/// Draw the whole app and return the frame (#858).
fn draw_858(
    term: &mut ratatui::Terminal<ratatui::backend::TestBackend>,
    app: &mut App,
) -> ratatui::buffer::Buffer {
    term.draw(|f| app.render(f)).unwrap();
    term.backend().buffer().clone()
}

/// Assert that `feedback`, shown in the Source Control panel, changes no
/// cell of its row from the panel's right border to the screen's edge,
/// over whatever the editor shows there, and return that row (#858). Only
/// that row is compared: the feedback is one line, and the editor's other
/// rows may change between frames as the git worker reports.
fn assert_feedback_stays_in_the_panel_858(
    term: &mut ratatui::Terminal<ratatui::backend::TestBackend>,
    app: &mut App,
    feedback: &str,
) -> u16 {
    app.source_control.commit_feedback = None;
    draw_858(term, app);
    let bare = draw_858(term, app);
    app.source_control.commit_feedback = Some(feedback.to_string());
    let shown = draw_858(term, app);
    let panel = app.source_control.last_area;
    let width = shown.area.width;
    assert!(
        panel.width > 0 && panel.right() < width,
        "the panel sits left of the editor: {panel:?}"
    );
    let head: String = feedback.chars().take(12).collect();
    let row = (panel.top()..panel.bottom())
        .find(|&y| {
            (panel.left()..panel.right())
                .map(|x| shown[(x, y)].symbol())
                .collect::<String>()
                .contains(&head)
        })
        .unwrap_or_else(|| panic!("the panel shows {head:?}"));
    for x in panel.right() - 1..width {
        assert_eq!(
            shown[(x, row)].symbol(),
            bare[(x, row)].symbol(),
            "cell ({x}, {row}) from the Source Control panel's right border on changed with feedback {feedback:?}"
        );
    }
    row
}

/// Assert that `row` crosses the merge editor's source panes below their
/// titles, where the panes paint only their text and the cells between
/// show whatever was drawn there first, as in the issue's frames (#858).
fn assert_row_crosses_the_merge_panes_858(app: &App, row: u16) {
    let panes = app
        .editor
        .merge
        .as_ref()
        .expect("the merge editor is up")
        .last_panes_area;
    assert!(
        row > panes.y + 1 && row + 1 < panes.bottom(),
        "the feedback row {row} crosses the source panes {panes:?}"
    );
}

/// #858, the issue's frames: with the merge editor open, the Source
/// Control feedback, first the merge's conflict and then, once it is
/// resolved, a merge commit's long subject, paints nothing past the panel.
/// The merge editor paints only its text cells, so feedback drawn past the
/// panel showed through between them.
#[test]
fn scm_feedback_paints_nothing_over_the_merge_editor() {
    use crate::widgets::scm_menu::ScmAction;
    let tmp = conflicting_pricing_repo_858();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::SourceControl);
    pick_branch_858(&mut app, ScmAction::Merge, "feature");
    let conflict = app
        .source_control
        .commit_feedback
        .clone()
        .expect("the merge reports");
    let idx = wait_for_conflicted_entry(&mut app);
    app.open_source_control_entry(idx);
    assert!(app.editor.merge.is_some(), "the merge editor is up");
    // Tall enough that the feedback row crosses the source panes rather
    // than the RESULT bar, which the merge editor paints edge to edge.
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(150, 60)).unwrap();
    let row = assert_feedback_stays_in_the_panel_858(&mut term, &mut app, &conflict);
    assert_row_crosses_the_merge_panes_858(&app, row);

    // Resolve, stage, and commit the merge with a subject longer than the
    // panel.
    app.run_command(crate::widgets::command_palette::Command::MergeAcceptAllIncoming);
    app.complete_merge();
    assert!(app.status.contains("Merge complete"), "{:?}", app.status);
    app.set_sidebar_view(SidebarView::SourceControl);
    app.source_control.message =
        "Merge branch 'feature': keep the lower free-shipping threshold for EU customers"
            .to_string();
    app.source_control.message_cursor = app.source_control.message.chars().count();
    app.handle_source_control_key(key(KeyCode::Enter, KeyModifiers::NONE));
    wait_for_git_net(&mut app);
    let subject = app
        .source_control
        .commit_feedback
        .clone()
        .unwrap_or_default();
    assert!(
        subject.starts_with("[main ") && subject.ends_with("for EU customers"),
        "the commit's first line: {subject:?}"
    );
    assert!(app.editor.merge.is_some(), "the merge editor is still up");
    let row = assert_feedback_stays_in_the_panel_858(&mut term, &mut app, &subject);
    assert_row_crosses_the_merge_panes_858(&app, row);
}

/// Run git in `dir` for a #858 fixture, asserting it succeeded.
fn git_858(dir: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Local settings for a #858 fixture repo: an identity, no signing and
/// no push negotiation from the user's global config, and how `git pull`
/// reconciles (`pull.rebase`).
fn configure_858(dir: &std::path::Path, pull_rebase: &str) {
    for (key, value) in [
        ("user.email", "a@b"),
        ("user.name", "a"),
        ("commit.gpgsign", "false"),
        ("push.negotiate", "false"),
        ("pull.rebase", pull_rebase),
    ] {
        git_858(dir, &["config", key, value]);
    }
}

/// A bare remote on `main`; `theirs`, a clone that pushes to it; and
/// `mine`, the clone the app opens (#858).
struct Remote858 {
    _dir: tempfile::TempDir,
    bare: std::path::PathBuf,
    theirs: std::path::PathBuf,
    mine: std::path::PathBuf,
}

/// [`Remote858`] with f.txt ("one") on `main` in all three; `mine` pulls
/// the way `pull_rebase` says.
fn remote_858(pull_rebase: &str) -> Remote858 {
    let dir = tempfile::tempdir().unwrap();
    let bare = dir.path().join("remote.git");
    let theirs = dir.path().join("theirs");
    let mine = dir.path().join("mine");
    std::fs::create_dir(&theirs).unwrap();
    git_858(
        dir.path(),
        &["init", "-q", "--bare", "-b", "main", "remote.git"],
    );
    git_858(&theirs, &["init", "-q", "-b", "main"]);
    configure_858(&theirs, "false");
    std::fs::write(theirs.join("f.txt"), "one\n").unwrap();
    git_858(&theirs, &["add", "-A"]);
    git_858(&theirs, &["commit", "-qm", "one"]);
    git_858(
        &theirs,
        &["remote", "add", "origin", bare.to_str().unwrap()],
    );
    git_858(&theirs, &["push", "-q", "-u", "origin", "main"]);
    git_858(dir.path(), &["clone", "-q", bare.to_str().unwrap(), "mine"]);
    configure_858(&mine, pull_rebase);
    Remote858 {
        _dir: dir,
        bare,
        theirs,
        mine,
    }
}

/// Commit `text` to `file` in `repo`.
fn commit_858(repo: &std::path::Path, file: &str, text: &str) {
    std::fs::write(repo.join(file), text).unwrap();
    git_858(repo, &["add", "-A"]);
    git_858(repo, &["commit", "-qm", file]);
}

/// #858: Pull reported git's whole text, "Pulled: Updating a..b\n
/// Fast-forward\n f.txt | 1 +\n 1 file changed…", squashed into the status
/// row. It now reports git's result line; the Git Output log keeps the
/// rest.
#[test]
fn a_fast_forward_pull_reports_fast_forward_on_one_line() {
    let r = remote_858("false");
    commit_858(&r.theirs, "f.txt", "one\ntwo\n");
    git_858(&r.theirs, &["push", "-q"]);
    let mut app = App::new(r.mine.clone()).unwrap();
    app.pull_source_control();
    wait_for_git_net(&mut app);
    assert_eq!(app.status, "Pulled: Fast-forward");
    assert_eq!(
        app.source_control.commit_feedback.as_deref(),
        Some("pulled: Fast-forward")
    );
    assert!(!app.source_control.commit_feedback_is_error);
    let logged = app.git_output_log.last().cloned().unwrap_or_default();
    assert!(
        logged.starts_with("$ git pull")
            && logged.contains("Updating ")
            && logged.contains("f.txt"),
        "the Git Output log keeps git's whole text: {logged:?}"
    );
}

/// #858: a pull that rebases prints only to stderr: the fetch's `From
/// <url>` and ref update, then `Rebasing (1/1)` redrawn into "Successfully
/// rebased …". It reported "Pulled: From /…" and the rest; now the result.
#[test]
fn a_rebasing_pull_reports_the_rebase_not_the_fetch() {
    let r = remote_858("true");
    commit_858(&r.theirs, "f.txt", "one\ntwo\n");
    git_858(&r.theirs, &["push", "-q"]);
    commit_858(&r.mine, "g.txt", "mine\n");
    let mut app = App::new(r.mine.clone()).unwrap();
    app.pull_source_control();
    wait_for_git_net(&mut app);
    assert_eq!(
        app.status,
        "Pulled: Successfully rebased and updated refs/heads/main."
    );
    assert_one_line_858(&app);
}

/// #858 negative: a pull with nothing to bring in still says so, rather
/// than an empty "Pulled".
#[test]
fn a_pull_that_is_already_up_to_date_still_says_so() {
    let r = remote_858("false");
    let mut app = App::new(r.mine.clone()).unwrap();
    app.pull_source_control();
    wait_for_git_net(&mut app);
    assert_eq!(app.status, "Pulled: Already up to date.");
    assert_eq!(
        app.source_control.commit_feedback.as_deref(),
        Some("pulled: Already up to date.")
    );
}

/// #858 negative: a pull that fails still says it failed, with git's
/// `fatal:` line, and one stopped on conflicts still names them.
#[test]
fn a_failed_or_conflicted_pull_still_says_why() {
    let r = remote_858("false");
    git_858(&r.mine, &["config", "pull.ff", "only"]);
    commit_858(&r.theirs, "f.txt", "one\ntheirs\n");
    git_858(&r.theirs, &["push", "-q"]);
    commit_858(&r.mine, "f.txt", "one\nmine\n");
    let mut app = App::new(r.mine.clone()).unwrap();
    app.pull_source_control();
    wait_for_git_net(&mut app);
    assert!(app.source_control.commit_feedback_is_error);
    assert_eq!(
        app.status,
        "Pull failed: fatal: Not possible to fast-forward, aborting."
    );
    assert_one_line_858(&app);

    git_858(&r.mine, &["config", "pull.ff", "false"]);
    app.pull_source_control();
    wait_for_git_net(&mut app);
    assert!(app.source_control.commit_feedback_is_error);
    assert_eq!(app.status, "Pull: 1 conflict (f.txt)");
    assert_eq!(
        app.source_control.commit_feedback.as_deref(),
        Some("pull: 1 conflict (f.txt)")
    );
}

/// #858: Sync reported "Synced" in the status bar and, in the panel,
/// "synced (pulled: <git pull's text> | pushed: <git push's text>)" with
/// every line of both. It now names both halves on one line, the way
/// Commit & Push does; the Git Output log keeps git's text.
#[test]
fn a_sync_reports_both_halves_on_one_line() {
    let r = remote_858("false");
    commit_858(&r.theirs, "f.txt", "one\ntwo\n");
    git_858(&r.theirs, &["push", "-q"]);
    commit_858(&r.mine, "g.txt", "mine\n");
    let mut app = App::new(r.mine.clone()).unwrap();
    app.sync_source_control();
    wait_for_git_net(&mut app);
    assert!(!app.source_control.commit_feedback_is_error);
    assert!(
        app.status
            .starts_with("Synced: pulled: Merge made by the 'ort' strategy. | pushed: ")
            && app.status.ends_with(" main -> main"),
        "{:?}",
        app.status
    );
    assert_eq!(
        app.source_control.commit_feedback.as_deref(),
        Some(app.status.replacen("Synced", "synced", 1).as_str())
    );
    assert_one_line_858(&app);
    let log = app.git_output_log.join("\n");
    assert!(
        log.contains("$ git pull") && log.contains("$ git push") && log.contains("To "),
        "{log:?}"
    );
}

/// #858 negative: a sync with nothing to move still reports both halves,
/// never an empty "Synced".
#[test]
fn a_sync_with_nothing_to_move_still_reports_both_halves() {
    let r = remote_858("false");
    let mut app = App::new(r.mine.clone()).unwrap();
    app.sync_source_control();
    wait_for_git_net(&mut app);
    assert_eq!(
        app.status,
        "Synced: pulled: Already up to date. | pushed: Everything up-to-date"
    );
}

/// #858: a sync whose push the remote refuses reported "pull ok; push
/// failed: " and git push's whole stderr (`remote:` lines, `To <url>`,
/// the rejected ref, `error:`). It now names the rejected ref.
#[test]
fn a_sync_whose_push_is_refused_names_the_rejected_ref() {
    use std::os::unix::fs::PermissionsExt;
    let r = remote_858("false");
    let hook = r.bare.join("hooks").join("pre-receive");
    std::fs::write(&hook, "#!/bin/sh\necho 'no pushes today' >&2\nexit 1\n").unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    commit_858(&r.mine, "g.txt", "mine\n");
    let mut app = App::new(r.mine.clone()).unwrap();
    app.sync_source_control();
    wait_for_git_net(&mut app);
    assert!(app.source_control.commit_feedback_is_error);
    assert_eq!(
        app.status,
        "Sync: pull ok; push failed: [remote rejected] main -> main (pre-receive hook declined)"
    );
    assert_eq!(
        app.source_control.commit_feedback.as_deref(),
        Some("pull ok; push failed: [remote rejected] main -> main (pre-receive hook declined)")
    );
    let log = app.git_output_log.join("\n");
    assert!(log.contains("no pushes today"), "{log:?}");
}

/// #858: Fetch and Push (Force) reported git's first line, the fetch's
/// `From <url>` or the push's `To <url>`; they now report the ref update,
/// as Push does.
#[test]
fn fetch_and_force_push_report_the_ref_update_not_the_url() {
    use crate::widgets::scm_menu::ScmAction;
    let r = remote_858("false");
    commit_858(&r.theirs, "f.txt", "one\ntwo\n");
    git_858(&r.theirs, &["push", "-q"]);
    let mut app = App::new(r.mine.clone()).unwrap();
    app.dispatch_scm_action(ScmAction::Fetch);
    wait_for_git_net(&mut app);
    assert!(
        app.status.starts_with("Fetched: ") && app.status.ends_with(" main -> origin/main"),
        "{:?}",
        app.status
    );
    assert_one_line_858(&app);

    git_858(&r.mine, &["merge", "-q", "--ff-only", "origin/main"]);
    commit_858(&r.mine, "g.txt", "mine\n");
    git_858(&r.mine, &["push", "-q"]);
    git_858(
        &r.mine,
        &["commit", "-q", "--amend", "-m", "g.txt, reworded"],
    );
    app.dispatch_scm_action(ScmAction::PushForce);
    wait_for_git_net(&mut app);
    assert!(
        app.status.starts_with("Force-pushed: ")
            && app.status.ends_with(" main -> main (forced update)"),
        "{:?}",
        app.status
    );
    assert_one_line_858(&app);
}

/// #858: switching branch with a local edit reported git's stdout, the
/// `M\tseed.txt` list of carried-over changes, as "Switched to: M…"; a
/// refused switch reported git's whole error. Both are one line now.
#[test]
fn switching_branch_reports_one_line() {
    use crate::widgets::scm_menu::ScmAction;
    let tmp = make_committed_repo();
    configure_858(tmp.path(), "false");
    git_858(tmp.path(), &["branch", "feature"]);
    std::fs::write(tmp.path().join("seed.txt"), "edited\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::SourceControl);
    pick_branch_858(&mut app, ScmAction::CheckoutTo, "feature");
    assert!(!app.source_control.commit_feedback_is_error);
    assert_eq!(app.status, "Switched to feature");
    assert_eq!(
        app.source_control.commit_feedback.as_deref(),
        Some("Switched to feature")
    );

    // `main` moves on under the edit: switching back would overwrite it.
    git_858(tmp.path(), &["stash", "-q"]);
    git_858(tmp.path(), &["checkout", "-q", "main"]);
    commit_858(tmp.path(), "seed.txt", "main moved\n");
    git_858(tmp.path(), &["checkout", "-q", "feature"]);
    git_858(tmp.path(), &["stash", "pop", "-q"]);
    pick_branch_858(&mut app, ScmAction::CheckoutTo, "main");
    assert!(app.source_control.commit_feedback_is_error);
    assert_eq!(
        app.status,
        "Branch: error: Your local changes to the following files would be overwritten by checkout:"
    );
    assert_one_line_858(&app);
}

/// #858: popping the latest stash reported git's `git status` dump
/// ("Stash popped: On branch main\nChanges not staged…"); it now reports
/// the stash it dropped.
#[test]
fn popping_a_stash_reports_the_dropped_entry_on_one_line() {
    let tmp = make_committed_repo();
    std::fs::write(tmp.path().join("seed.txt"), "stashed\n").unwrap();
    git_858(tmp.path(), &["stash", "-q"]);
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.stash_pop_source_control();
    assert!(!app.source_control.commit_feedback_is_error);
    assert!(
        app.status
            .starts_with("Stash popped: Dropped refs/stash@{0} ("),
        "{:?}",
        app.status
    );
    assert!(
        app.source_control
            .commit_feedback
            .as_deref()
            .is_some_and(|f| f.starts_with("popped: Dropped refs/stash@{0} (")),
        "{:?}",
        app.source_control.commit_feedback
    );
    assert_one_line_858(&app);
}

/// #858: a failed clone reported "Clone failed: Cloning into 'notrepo'...\n
/// fatal: …\nfatal: …\n\nPlease make sure…"; it now reports git's first
/// `fatal:` line.
#[test]
fn a_failed_clone_names_git_s_fatal_line() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    let not_a_repo = tmp.path().join("elsewhere").join("notrepo");
    std::fs::create_dir(&ws).unwrap();
    std::fs::create_dir_all(&not_a_repo).unwrap();
    let url = format!("file://{}", not_a_repo.display());
    let mut app = App::new(ws).unwrap();
    let err = crate::git::clone_into(tmp.path(), &url)
        .map(|_| ())
        .expect_err("nothing to clone");
    assert!(err.contains('\n'), "git's text runs to lines: {err:?}");
    app.finish_clone(Err(err), tmp.path());
    assert!(app.source_control.commit_feedback_is_error);
    assert_eq!(
        app.status,
        format!(
            "Clone failed: fatal: '{}' does not appear to be a git repository",
            not_a_repo.display()
        )
    );
    assert_one_line_858(&app);
}

/// A workspace with `report.py` open and an unsaved comment typed on line 1.
fn app_with_an_edited_report(dir: &Path) -> App {
    std::fs::write(
        dir.join("report.py"),
        "def total(xs):\n    return sum(xs)\n",
    )
    .unwrap();
    let mut app = App::new(dir.to_path_buf()).unwrap();
    app.editor.open_pinned(&dir.join("report.py")).unwrap();
    app.focus = Pane::Editor;
    app.editor.insert_str("# draft ");
    assert!(app.editor.dirty, "precondition: the tab has unsaved edits");
    app
}

/// #1285: Revert File drops every unsaved edit in one step and says so.
#[test]
fn revert_file_restores_the_text_on_disk_and_clears_dirty() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_an_edited_report(tmp.path());
    revert_file(&mut app);
    assert_eq!(
        app.editor.lines,
        vec!["def total(xs):", "    return sum(xs)"]
    );
    assert!(!app.editor.dirty);
    assert_eq!(app.status, "Reverted report.py");
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("report.py")).unwrap(),
        "def total(xs):\n    return sum(xs)\n",
        "the file on disk is not touched"
    );
}

/// #1285: the revert is one undo step, so Ctrl+Z brings the edits back,
/// marked unsaved again, and redo reverts again.
#[test]
fn undo_after_revert_file_brings_the_edits_back() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_an_edited_report(tmp.path());
    revert_file(&mut app);
    assert!(app.editor.undo());
    assert_eq!(app.editor.lines[0], "# draft def total(xs):");
    assert!(app.editor.dirty, "the restored edits are unsaved again");
    assert!(app.editor.redo());
    assert_eq!(app.editor.lines[0], "def total(xs):");
}

/// #1285 negative: on a tab with nothing unsaved Revert File changes
/// nothing, adds no undo step, and says there is nothing to revert.
#[test]
fn revert_file_on_a_clean_tab_does_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("report.py"), "x = 1\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor
        .open_pinned(&tmp.path().join("report.py"))
        .unwrap();
    revert_file(&mut app);
    assert_eq!(app.editor.lines, vec!["x = 1"]);
    assert!(!app.editor.undo(), "no undo step was added");
    assert!(app.status.contains("no unsaved changes"), "{}", app.status);
}

/// #1285 negative: a buffer with no file behind it has nothing to revert
/// to, so its text stays.
#[test]
fn revert_file_keeps_a_buffer_with_no_file() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.insert_str("scratch");
    revert_file(&mut app);
    assert_eq!(app.editor.lines, vec!["scratch"]);
    assert!(app.status.contains("no file"), "{}", app.status);
}

/// Clicking the file in Source Control opens its HEAD diff, decoded with the
/// encoding its tab uses, instead of failing with "not UTF-8".
#[test]
fn a_windows_1252_file_opens_its_source_control_diff() {
    let (tmp, f) = encoded_repo("latin1.txt", LATIN1_HEAD, LATIN1_ADDED);
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&f).unwrap();
    app.editor
        .reopen_with_encoding(encoding_rs::WINDOWS_1252)
        .unwrap();
    wait_for_changes(&mut app, |a| {
        a.source_control
            .entries
            .iter()
            .any(|e| e.path == "latin1.txt")
    });
    let idx = app
        .source_control
        .entries
        .iter()
        .position(|e| e.path == "latin1.txt")
        .unwrap();
    app.open_source_control_entry(idx);
    let diff = app
        .editor
        .diff
        .as_ref()
        .unwrap_or_else(|| panic!("no diff opened: {}", app.status));
    assert_eq!(diff.left_lines[1], "Müller;Köln");
    assert_eq!(
        diff.right_lines.last().map(String::as_str),
        Some("Zoë;Zürich")
    );
    let changed = diff
        .rows
        .iter()
        .filter(|r| !matches!(r, crate::widgets::diff::DiffRow::Equal { .. }))
        .count();
    assert_eq!(changed, 1, "only the added line differs");
}

/// With no tab open, the file is decoded the way opening it would: a BOM
/// names its encoding, so a closed UTF-16 file still opens its diff.
#[test]
fn a_closed_utf16_file_opens_its_source_control_diff() {
    let utf16 = |s: &str| -> Vec<u8> { s.encode_utf16().flat_map(u16::to_le_bytes).collect() };
    let head = [&[0xff, 0xfe][..], &utf16("one\ntwo\n")].concat();
    let (tmp, _) = encoded_repo("app.rc", &head, &utf16("three\n"));
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    wait_for_changes(&mut app, |a| {
        a.source_control.entries.iter().any(|e| e.path == "app.rc")
    });
    let idx = app
        .source_control
        .entries
        .iter()
        .position(|e| e.path == "app.rc")
        .unwrap();
    app.open_source_control_entry(idx);
    let diff = app
        .editor
        .diff
        .as_ref()
        .unwrap_or_else(|| panic!("no diff opened: {}", app.status));
    assert_eq!(diff.left_lines, ["one", "two"]);
    assert_eq!(diff.right_lines.last().map(String::as_str), Some("three"));
}

/// Staging a hunk writes the diff's decoded text back as a patch, which for
/// a non-UTF-8 file would put UTF-8 bytes into a windows-1252 blob: refused,
/// and the index is left alone.
#[test]
fn staging_a_hunk_in_a_non_utf8_diff_is_refused() {
    let (tmp, f) = encoded_repo("latin1.txt", LATIN1_HEAD, LATIN1_ADDED);
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&f).unwrap();
    app.editor
        .reopen_with_encoding(encoding_rs::WINDOWS_1252)
        .unwrap();
    wait_for_changes(&mut app, |a| {
        a.source_control
            .entries
            .iter()
            .any(|e| e.path == "latin1.txt")
    });
    let idx = app
        .source_control
        .entries
        .iter()
        .position(|e| e.path == "latin1.txt")
        .unwrap();
    app.open_source_control_entry(idx);
    assert!(app.editor.diff.is_some(), "{}", app.status);
    app.stage_hunk_at_caret();
    assert!(app.status.contains("windows-1252"), "{}", app.status);
    assert_eq!(
        git_stdout_in(tmp.path(), &["diff", "--cached", "--name-only"]),
        ""
    );
}

/// #989: an App on the issue's repro, a merge stopped on a conflict.
fn app_in_a_conflicted_merge() -> (App, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let script = r#"set -e
git init -q -b main && git config user.email a@b && git config user.name a
printf '# Orders\n\nRun `python pricing.py`.\n' > README.md && git add . && git commit -qm init
git checkout -qb hotfix && sed 's/python pricing.py/python -m pricing/' README.md > README.tmp && mv README.tmp README.md && git commit -qam fix
git checkout -q main && sed 's/python pricing.py/uv run pricing.py/' README.md > README.tmp && mv README.tmp README.md && git commit -qam uv
! git merge -q hotfix >/dev/null 2>&1"#;
    let out = std::process::Command::new("sh")
        .args(["-c", script])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.source_control.message = String::from("Merge hotfix");
    (app, tmp)
}

fn committed_readme(root: &std::path::Path) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["show", "HEAD:README.md"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn commit_refuses_while_merge_conflicts_are_unresolved() {
    let (mut app, tmp) = app_in_a_conflicted_merge();
    app.commit_source_control();
    wait_for_git_net(&mut app);
    assert!(
        !committed_readme(tmp.path()).contains("<<<<<<<"),
        "the conflict markers were committed"
    );
    let feedback = app
        .source_control
        .commit_feedback
        .clone()
        .unwrap_or_default();
    assert!(feedback.contains("Resolve 1 merge conflict"), "{feedback}");
    assert!(app.source_control.commit_feedback_is_error);
    assert_eq!(
        app.source_control.message, "Merge hotfix",
        "the message is kept"
    );
}

#[test]
fn commit_all_refuses_while_merge_conflicts_are_unresolved() {
    // Commit All stages everything first, and `git add -A` marks a conflict
    // resolved with its markers in it.
    let (mut app, tmp) = app_in_a_conflicted_merge();
    app.commit_all_source_control();
    assert!(!committed_readme(tmp.path()).contains("<<<<<<<"));
    assert_eq!(
        crate::git::unmerged_paths(tmp.path()),
        vec![String::from("README.md")],
        "nothing was staged"
    );
    assert!(
        app.status.contains("Resolve 1 merge conflict"),
        "{}",
        app.status
    );
}

/// Workspace `tl/` holding the repo `tl/sub/` (#1227): the workspace root
/// is in no repo, so its worker has no `repo_root`.
fn nested_repo_1227() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let sub = tmp.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["config", "user.email", "a@b"],
        vec!["config", "user.name", "a"],
    ] {
        let _ = std::process::Command::new("git")
            .arg("-C")
            .arg(&sub)
            .args(&args)
            .status();
    }
    let f = sub.join("plain.txt");
    std::fs::write(&f, "one\ntwo\n").unwrap();
    for args in [vec!["add", "."], vec!["commit", "-m", "plain", "--quiet"]] {
        let _ = std::process::Command::new("git")
            .arg("-C")
            .arg(&sub)
            .args(&args)
            .status();
    }
    (tmp, f)
}

#[test]
fn git_gutter_baseline_comes_from_a_repo_below_the_workspace_root() {
    let (tmp, f) = nested_repo_1227();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&f).unwrap();
    app.sync_git_gutters();
    assert_eq!(
        app.editor.git_head_lines,
        Some(crate::packed_lines::PackedLines::from(&["one", "two"][..])),
        "the committed text of sub/plain.txt is the gutter baseline"
    );
}

#[test]
fn git_gutter_baseline_stays_empty_for_a_file_outside_any_repo() {
    let (tmp, _) = nested_repo_1227();
    let loose = tmp.path().join("loose.txt");
    std::fs::write(&loose, "x\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&loose).unwrap();
    app.sync_git_gutters();
    assert_eq!(app.editor.git_head_lines, None);
}

#[test]
fn source_control_lists_the_repo_found_one_folder_down() {
    let (tmp, _) = nested_repo_1227();
    std::fs::create_dir(tmp.path().join("plain_dir")).unwrap();
    std::fs::create_dir_all(tmp.path().join(".hidden/.git")).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.sidebar_view = SidebarView::SourceControl;
    let _ = render_buf(&mut app);
    let sub = tmp.path().join("sub").canonicalize().unwrap();
    assert_eq!(
        app.source_control.nested_repos,
        vec![(String::from("sub/"), sub.clone())],
        "only sub/ holds a repo; hidden folders are skipped"
    );
    assert_eq!(
        app.source_control.last_init_repo_button_area,
        Rect::default()
    );
    let (btn, _) = app.source_control.nested_repo_button_areas[0].clone();
    left_click(&mut app, btn.x + btn.width / 2, btn.y + 1);
    assert_eq!(app.workspace_root(), sub.as_path());
}

#[test]
fn source_control_still_offers_initialize_with_no_repo_below() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join("plain_dir")).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.sidebar_view = SidebarView::SourceControl;
    let _ = render_buf(&mut app);
    assert!(app.source_control.nested_repos.is_empty());
    assert!(app.source_control.last_init_repo_button_area.width > 0);
}

/// `app/` with a submodule `lib` whose checked-out commit moved past the
/// recorded one and whose working tree is dirty, plus a tracked `a.txt`.
fn repo_with_moved_dirty_submodule() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("libsrc");
    let app = tmp.path().join("app");
    for dir in [&lib, &app] {
        std::fs::create_dir_all(dir).unwrap();
        git_ok(dir, &["init", "-q", "-b", "main"]);
        git_ok(dir, &["config", "user.email", "a@b"]);
        git_ok(dir, &["config", "user.name", "a"]);
    }
    std::fs::write(lib.join("lib.txt"), "v1\n").unwrap();
    git_ok(&lib, &["add", "."]);
    git_ok(&lib, &["commit", "-qm", "l1"]);
    std::fs::write(app.join("a.txt"), "one\n").unwrap();
    git_ok(
        &app,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            "../libsrc",
            "lib",
        ],
    );
    git_ok(&app, &["add", "."]);
    git_ok(&app, &["commit", "-qm", "app1"]);
    let inner = app.join("lib");
    git_ok(&inner, &["config", "user.email", "a@b"]);
    git_ok(&inner, &["config", "user.name", "a"]);
    std::fs::write(inner.join("lib.txt"), "v2\n").unwrap();
    git_ok(&inner, &["commit", "-qam", "l2"]);
    std::fs::write(inner.join("lib.txt"), "v2\ndirty\n").unwrap();
    (tmp, app)
}

/// Clicking a changed submodule shows what moved in it, the commit range
/// and its own changes, instead of "git show HEAD failed: bad object"
/// (#1235).
#[test]
fn clicking_a_changed_submodule_shows_its_diff() {
    let (_tmp, root) = repo_with_moved_dirty_submodule();
    let mut app = App::new(root.clone()).unwrap();
    app.source_control.entries = vec![crate::git::ChangeEntry {
        path: "lib".into(),
        kind: crate::git::ChangeKind::Modified,
        ..Default::default()
    }];
    app.set_sidebar_view(SidebarView::SourceControl);
    app.open_source_control_entry(0);
    assert!(!app.status.contains("failed"), "status: {}", app.status);
    assert_eq!(
        app.editor.path.as_deref(),
        Some(std::path::Path::new("lib (submodule)"))
    );
    let text = app.editor.lines.join("\n");
    assert!(text.starts_with("Submodule lib "), "{text}");
    assert!(text.contains("> l2"), "the commit it moved to: {text}");
    assert!(
        text.contains("+dirty"),
        "and its uncommitted change: {text}"
    );
}

/// Discard on a submodule changes nothing, so it must not report
/// "Discarded": it refuses and says why, leaving the submodule as it was.
#[test]
fn discarding_a_submodule_refuses_instead_of_claiming_success() {
    let (_tmp, root) = repo_with_moved_dirty_submodule();
    let inner_head = git_out(&root.join("lib"), &["rev-parse", "HEAD"]);
    let mut app = App::new(root.clone()).unwrap();
    app.pending_discard = Some(PendingDiscard {
        rel_path: "lib".into(),
        untracked: false,
        staged: false,
    });
    app.confirm_pending_discard();
    assert!(
        !app.status.starts_with("Discarded"),
        "status: {}",
        app.status
    );
    assert!(app.status.contains("submodule"), "status: {}", app.status);
    assert_eq!(
        git_out(&root.join("lib"), &["rev-parse", "HEAD"]),
        inner_head
    );
    assert_eq!(
        std::fs::read_to_string(root.join("lib/lib.txt")).unwrap(),
        "v2\ndirty\n"
    );
}

/// Negative: in the same repository a plain file still discards, and a
/// staged submodule (`git add lib`, which records the new commit) is a
/// submodule all the same.
#[test]
fn a_plain_file_beside_a_submodule_still_discards() {
    let (_tmp, root) = repo_with_moved_dirty_submodule();
    std::fs::write(root.join("a.txt"), "changed\n").unwrap();
    let mut app = App::new(root.clone()).unwrap();
    app.pending_discard = Some(PendingDiscard {
        rel_path: "a.txt".into(),
        untracked: false,
        staged: false,
    });
    app.confirm_pending_discard();
    assert_eq!(app.status, "Discarded a.txt");
    assert_eq!(
        std::fs::read_to_string(root.join("a.txt")).unwrap(),
        "one\n"
    );

    git_ok(&root, &["add", "lib"]);
    let err = crate::git::discard_path(&root, "lib", false, true).unwrap_err();
    assert!(err.contains("submodule"), "{err}");
}

#[test]
fn the_selected_source_control_change_is_announced() {
    use crate::git::{ChangeEntry, ChangeKind};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus = Pane::Tree;
    app.sidebar_view = SidebarView::SourceControl;
    let change = |path: &str, kind| ChangeEntry {
        path: path.into(),
        kind,
        additions: 1,
        deletions: 0,
    };
    app.source_control.entries = vec![
        change("app.py", ChangeKind::Modified),
        change("new.py", ChangeKind::StagedAdded),
    ];
    app.source_control.selected_change = None;
    let snap = app.a11y_snapshot();
    assert_eq!(snap.focus, "Source Control, 2 changes");
    assert_eq!(snap.item, None, "nothing selected, nothing read");
    app.source_control.selected_change = Some(1);
    assert_eq!(
        app.a11y_snapshot().item.as_deref(),
        Some("new.py, added, staged")
    );
    app.source_control.selected_change = Some(0);
    assert_eq!(
        app.a11y_snapshot().item.as_deref(),
        Some("app.py, modified")
    );
}

/// The issue's fork setup (#1445): `up` with `main` and `feat`, cloned to
/// `clone` (so `origin`, with its `origin/HEAD`), plus `upstream` pointing
/// at `up` too, and `my/fork`, a remote whose name holds a `/`, carrying a
/// `solo` branch no other remote has.
fn clone_with_two_remotes() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let up = tmp.path().join("up");
    std::fs::create_dir(&up).unwrap();
    git_checked(&up, &["init", "-q", "-b", "main"]);
    git_checked(
        &up,
        &[
            "-c",
            "user.name=a",
            "-c",
            "user.email=a@b",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "i",
        ],
    );
    git_checked(&up, &["branch", "feat"]);
    let fork = tmp.path().join("fork");
    git_checked(tmp.path(), &["clone", "-q", "up", "fork"]);
    git_checked(&fork, &["branch", "solo"]);
    git_checked(tmp.path(), &["clone", "-q", "up", "clone"]);
    let clone = tmp.path().join("clone");
    git_checked(&clone, &["remote", "add", "upstream", "../up"]);
    git_checked(&clone, &["remote", "add", "my/fork", "../fork"]);
    git_checked(&clone, &["fetch", "-q", "--all"]);
    (tmp, clone)
}

/// Open the Checkout picker on `clone`, type `query` and press Enter.
fn pick_branch(clone: &std::path::Path, query: &str) -> App {
    let mut app = App::new(clone.to_path_buf()).unwrap();
    app.open_branch_picker();
    for c in query.chars() {
        app.handle_branch_picker_key(key(KeyCode::Char(c), KeyModifiers::NONE));
    }
    app.handle_branch_picker_key(key(KeyCode::Enter, KeyModifiers::NONE));
    app
}

/// #1445: a clone's `origin/HEAD`, which git shortens to plain `origin`,
/// is a pointer, not a branch: the picker lists no `origin` row.
#[test]
fn the_branch_picker_lists_no_remote_head_in_a_clone() {
    let (_tmp, clone) = clone_with_two_remotes();
    let mut app = App::new(clone.clone()).unwrap();
    app.open_branch_picker();
    let shown: Vec<String> = app
        .branch_picker
        .as_ref()
        .expect("the picker opens")
        .branches
        .iter()
        .map(|b| b.display.clone())
        .collect();
    assert!(
        !shown.iter().any(|d| d == "origin" || d.ends_with("/HEAD")),
        "{shown:?}"
    );
    // Both remotes' copies of `feat` are listed: the pick decides which.
    assert!(shown.contains(&"origin/feat".to_string()), "{shown:?}");
    assert!(shown.contains(&"upstream/feat".to_string()), "{shown:?}");
}

/// #1445: picking a branch that two remotes carry checks it out tracking
/// the remote picked, where `git switch feat` refused the ambiguous name.
#[test]
fn picking_a_branch_on_two_remotes_tracks_the_remote_picked() {
    let (_tmp, clone) = clone_with_two_remotes();
    let app = pick_branch(&clone, "upstream/feat");
    assert!(
        app.branch_picker.is_none(),
        "the switch worked: {}",
        app.status
    );
    assert_eq!(git_checked(&clone, &["branch", "--show-current"]), "feat");
    assert_eq!(
        git_checked(&clone, &["rev-parse", "--abbrev-ref", "feat@{upstream}"]),
        "upstream/feat"
    );
}

/// #1445: a remote whose name holds a `/` is split by the remote list, not
/// at the first `/`, so its branch checks out as itself.
#[test]
fn a_remote_named_with_a_slash_checks_out_its_branch() {
    let (_tmp, clone) = clone_with_two_remotes();
    let app = pick_branch(&clone, "my/fork/solo");
    assert!(
        app.branch_picker.is_none(),
        "the switch worked: {}",
        app.status
    );
    assert_eq!(git_checked(&clone, &["branch", "--show-current"]), "solo");
    assert_eq!(
        git_checked(&clone, &["rev-parse", "--abbrev-ref", "solo@{upstream}"]),
        "my/fork/solo"
    );
}

/// #1445 negative: a local branch still hides the remote ones of the same
/// name, and picking a local branch is a plain switch.
#[test]
fn a_local_branch_hides_its_remotes_and_switches_plainly() {
    let (_tmp, clone) = clone_with_two_remotes();
    git_checked(&clone, &["switch", "-q", "-c", "side"]);
    let mut app = App::new(clone.clone()).unwrap();
    app.open_branch_picker();
    let shown: Vec<String> = app
        .branch_picker
        .as_ref()
        .unwrap()
        .branches
        .iter()
        .map(|b| b.display.clone())
        .collect();
    assert!(!shown.iter().any(|d| d.ends_with("/main")), "{shown:?}");
    drop(app);
    let app = pick_branch(&clone, "main");
    assert!(app.branch_picker.is_none(), "{}", app.status);
    assert_eq!(git_checked(&clone, &["branch", "--show-current"]), "main");
}

/// A modified file whose tab has unsaved edits, opened from Source Control
/// (#1572): `seed.txt` is `a = 2` on disk against `a = 1` at HEAD, and its
/// open tab reads `# a = 2`.
fn scm_app_with_an_unsaved_modified_file() -> (tempfile::TempDir, std::path::PathBuf, App, usize) {
    let (tmp, f) = repo_with_seed("a = 1\n");
    std::fs::write(&f, "a = 2\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&f).unwrap();
    app.focus_pane(Pane::Editor);
    type_str(&mut app, "# ");
    assert!(
        app.editor.dirty,
        "staging: the file's tab has unsaved edits"
    );
    assert!(wait_for_changes(&mut app, |a| {
        a.source_control
            .entries
            .iter()
            .any(|e| e.path == "seed.txt")
    }));
    let idx = app
        .source_control
        .entries
        .iter()
        .position(|e| e.path == "seed.txt")
        .unwrap();
    (tmp, f, app, idx)
}

fn diff_tab_count(app: &App) -> usize {
    app.editor.iter_tabs().filter(|e| e.diff.is_some()).count()
}

#[test]
fn reopening_the_source_control_diff_of_an_unsaved_file_reuses_its_diff_tab() {
    let (_tmp, _f, mut app, idx) = scm_app_with_an_unsaved_modified_file();
    app.open_source_control_entry(idx);
    app.open_source_control_entry(idx);
    app.open_source_control_entry(idx);
    assert_eq!(
        diff_tab_count(&app),
        1,
        "every click on the row must land on the one diff tab"
    );
    assert_eq!(app.editor.tab_count(), 2, "the file's tab and its diff");
}

#[test]
fn the_source_control_diff_of_an_unsaved_file_shows_the_unsaved_text() {
    let (_tmp, _f, mut app, idx) = scm_app_with_an_unsaved_modified_file();
    app.open_source_control_entry(idx);
    let diff = app.editor.diff.as_ref().expect("a diff view is open");
    assert_eq!(
        diff.left_lines,
        vec!["a = 1".to_string()],
        "HEAD on the left"
    );
    assert_eq!(
        diff.right_lines,
        vec!["# a = 2".to_string()],
        "the right side is the buffer the user is editing, not the disk"
    );
}

#[test]
fn the_source_control_diff_leaves_the_unsaved_tab_alone() {
    let (_tmp, f, mut app, idx) = scm_app_with_an_unsaved_modified_file();
    app.open_source_control_entry(idx);
    app.open_source_control_entry(idx);
    let tab = app
        .editor
        .iter_tabs()
        .find(|e| e.diff.is_none() && e.path.as_deref() == Some(f.as_path()))
        .expect("the file's own tab is still open");
    assert!(tab.dirty, "its unsaved edits are kept");
    assert_eq!(tab.lines[0], "# a = 2");
    assert_eq!(
        std::fs::read_to_string(&f).unwrap(),
        "a = 2\n",
        "nothing saved"
    );
}

#[test]
fn the_source_control_diff_of_a_clean_file_reuses_its_tab_and_reads_the_disk() {
    let (tmp, f) = repo_with_seed("a = 1\n");
    std::fs::write(&f, "a = 2\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&f).unwrap();
    assert!(wait_for_changes(&mut app, |a| {
        a.source_control
            .entries
            .iter()
            .any(|e| e.path == "seed.txt")
    }));
    let idx = app
        .source_control
        .entries
        .iter()
        .position(|e| e.path == "seed.txt")
        .unwrap();
    app.open_source_control_entry(idx);
    app.open_source_control_entry(idx);
    assert_eq!(app.editor.tab_count(), 1, "the clean tab becomes the diff");
    let diff = app.editor.diff.as_ref().expect("a diff view is open");
    assert_eq!(diff.right_lines, vec!["a = 2".to_string()]);
}

#[test]
fn hunk_actions_refuse_a_source_control_diff_of_unsaved_text() {
    let (tmp, _f, mut app, idx) = scm_app_with_an_unsaved_modified_file();
    app.open_source_control_entry(idx);
    app.stage_hunk_at_caret();
    assert_eq!(
        app.status,
        "Save the file first: hunk actions apply to the file on disk"
    );
    let staged = std::process::Command::new("git")
        .arg("-C")
        .arg(tmp.path())
        .args(["diff", "--cached", "--name-only"])
        .output()
        .unwrap();
    assert!(
        staged.stdout.is_empty(),
        "unsaved text must never reach the index: {}",
        String::from_utf8_lossy(&staged.stdout)
    );
}

#[test]
fn the_source_control_diff_finds_the_unsaved_copy_in_an_inactive_split_group() {
    let (_tmp, _f, mut app, idx) = scm_app_with_an_unsaved_modified_file();
    // The new active group opens seed.txt clean from disk; the dirty copy
    // now sits in the inactive group.
    app.split_editor();
    assert!(
        !app.editor.dirty,
        "staging: the active group's copy is clean"
    );
    app.open_source_control_entry(idx);
    let diff = app.editor.diff.as_ref().expect("a diff view is open");
    assert_eq!(
        diff.right_lines,
        vec!["# a = 2".to_string()],
        "the unsaved text from the other split, not the disk"
    );
    assert!(diff.right_is_unsaved, "hunk actions must refuse this view");
}

#[test]
fn saving_the_file_lets_hunk_actions_run_on_its_unsaved_source_control_diff() {
    let (_tmp, f, mut app, idx) = scm_app_with_an_unsaved_modified_file();
    app.open_source_control_entry(idx);
    assert!(app.editor.diff.as_ref().unwrap().right_is_unsaved);
    // The save writes exactly the text the view already shows, so the
    // rebuilt rows are identical to the rendered ones.
    std::fs::write(&f, "# a = 2\n").unwrap();
    let touched = std::collections::BTreeSet::from([f.clone()]);
    app.refresh_open_diff_views(false, &[], &touched);
    assert!(
        !app.editor.diff.as_ref().unwrap().right_is_unsaved,
        "the saved transition clears the marker even with identical rows"
    );
    app.stage_hunk_at_caret();
    assert_eq!(app.status, "Staged hunk in seed.txt");
}

#[test]
fn hunk_actions_still_stage_from_a_source_control_diff_of_a_saved_file() {
    let (tmp, f) = repo_with_seed("a = 1\n");
    std::fs::write(&f, "a = 2\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    assert!(wait_for_changes(&mut app, |a| {
        a.source_control
            .entries
            .iter()
            .any(|e| e.path == "seed.txt")
    }));
    let idx = app
        .source_control
        .entries
        .iter()
        .position(|e| e.path == "seed.txt")
        .unwrap();
    app.open_source_control_entry(idx);
    app.stage_hunk_at_caret();
    assert_eq!(app.status, "Staged hunk in seed.txt");
}

/// #1610: Git: Copy Permalink copies the selected lines' link at HEAD's
/// commit; whole lines selected with Shift+Down end on the last full line.
/// Copy Head Link names the branch, and the caret alone links its line.
#[test]
fn copy_permalink_puts_the_selected_lines_link_on_the_clipboard() {
    use crate::widgets::command_palette::Command;
    let (mut app, _tmp, sha) = app_on_a_pushed_github_file();
    app.editor.selection = Some(crate::widgets::editor::EditorSelection {
        anchor: (1, 0),
        head: (3, 0),
    });
    app.run_command(Command::GitCopyPermalink);
    let want = format!("https://github.com/acme/widgets/blob/{sha}/src/main.rs#L2-L3");
    assert_eq!(app.status, format!("Copied {want}"));
    assert_eq!(
        crate::clipboard::test_clip::mock_read().as_deref(),
        Some(want.as_str())
    );
    app.editor.selection = None;
    app.editor.cursor_row = 3;
    app.run_command(Command::GitCopyHeadLink);
    assert_eq!(
        app.status,
        "Copied https://github.com/acme/widgets/blob/main/src/main.rs#L4"
    );
    app.run_command(Command::GitOpenOnRemote);
    assert_eq!(
        app.status,
        format!("Open https://github.com/acme/widgets/blob/{sha}/src/main.rs#L4")
    );
}

/// #1610 negatives: a commit not on the remote yet copies nothing and says
/// to push; with no file open there is nothing to link.
#[test]
fn copy_permalink_refuses_an_unpushed_commit() {
    use crate::widgets::command_palette::Command;
    let (mut app, tmp, sha) = app_on_a_pushed_github_file();
    let _ = std::process::Command::new("git")
        .arg("-C")
        .arg(tmp.path())
        .args(["update-ref", "-d", "refs/remotes/origin/main"])
        .status();
    crate::clipboard::test_clip::mock_write("before");
    app.run_command(Command::GitCopyPermalink);
    assert_eq!(
        app.status,
        format!(
            "No link: commit {} is not on origin yet, so the link would not open: push it first",
            &sha[..7]
        )
    );
    assert_eq!(
        crate::clipboard::test_clip::mock_read().as_deref(),
        Some("before")
    );
    let mut empty = App::new(tmp.path().to_path_buf()).unwrap();
    empty.run_command(Command::GitCopyPermalink);
    assert_eq!(empty.status, "Open a file to link to its lines");
}
