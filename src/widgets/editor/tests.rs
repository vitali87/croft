//! The editor's own tests, moved out of editor.rs (#2062).

/// #369: a symbol tab turned into its file's tab keeps the step it
/// last mirrored, so should a sibling's undo step span the change, the
/// rest of it folds in rather than taking another undo step.
#[test]
fn dropping_the_symbol_clip_keeps_folding_the_siblings_burst() {
    let lines = |s: &[&str]| s.iter().map(|l| l.to_string()).collect::<Vec<_>>();
    let mut e = Editor::new();
    e.lines = lines(&["fn m() {}"]);
    e.mirror_lines_from(&lines(&["fn m() {}x"]), 7, (0, 10));
    e.drop_symbol_clip();
    e.mirror_lines_from(&lines(&["fn m() {}xy"]), 7, (0, 11));
    assert_eq!(e.undo_stack.len(), 1);
    assert!(e.undo());
    assert_eq!(e.lines, lines(&["fn m() {}"]));
}

/// An over-cap document reaches the hex viewer WITH a reason (#506).
///
/// End to end rather than by inspection: this drives the real `open`
/// and reads the status line the user sees, so it stays true if either
/// side is reworded. The docx branch has no second route to explain
/// itself -- `archive::kind_from_ext` does not claim `.docx` -- so
/// without the note this open is a silent hex dump.
#[test]
fn an_over_cap_document_says_why_it_landed_in_hex() {
    use std::io::Write as _;
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("huge.docx");
    {
        let f = std::fs::File::create(&p).unwrap();
        let mut z = zip::ZipWriter::new(f);
        let o = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        z.start_file("word/document.xml", o).unwrap();
        z.write_all(b"<?xml version=\"1.0\"?><w:document/>")
            .unwrap();
        z.finish().unwrap();
    }
    std::fs::OpenOptions::new()
        .append(true)
        .open(&p)
        .unwrap()
        .set_len(crate::docx::MAX_DOC_LISTING_BYTES + 1)
        .unwrap();

    let mut ed = Editor::new();
    ed.open(&p)
        .expect("an over-cap document still opens, as hex");
    assert!(
        ed.status.contains("hex viewer:"),
        "the open must carry a reason, not land silently: {:?}",
        ed.status
    );
    assert!(
        crate::archive::is_list_cap_refusal(&ed.status),
        "and the reason must be recognisable as a SIZE refusal by the \
             predicate that owns that vocabulary, so a reword on either \
             side cannot silently decouple them: {:?}",
        ed.status
    );
}

/// #1979 review: a docx saved under a `.doc` name takes the zip route and
/// its listing cap, so past that cap it says why too. The cap follows the
/// bytes, as the route does, not the name.
#[test]
fn an_over_cap_docx_named_doc_says_why_it_landed_in_hex() {
    use std::io::Write as _;
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("huge.doc");
    {
        let f = std::fs::File::create(&p).unwrap();
        let mut z = zip::ZipWriter::new(f);
        let o = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        z.start_file("word/document.xml", o).unwrap();
        z.write_all(b"<?xml version=\"1.0\"?><w:document/>")
            .unwrap();
        z.finish().unwrap();
    }
    std::fs::OpenOptions::new()
        .append(true)
        .open(&p)
        .unwrap()
        .set_len(crate::docx::MAX_DOC_LISTING_BYTES + 1)
        .unwrap();
    assert!(crate::docx::is_past_listing_cap(&p));

    let mut ed = Editor::new();
    ed.open(&p).expect("it still opens, as hex");
    assert!(
        crate::archive::is_list_cap_refusal(&ed.status),
        "the open says it was refused for its size: {:?}",
        ed.status
    );
}

use super::*;
use std::io::Write;
use tempfile::NamedTempFile;

fn blame_line(
    author: &str,
    summary: &str,
    age: i64,
    uncommitted: bool,
) -> std::sync::Arc<crate::git::BlameCommit> {
    std::sync::Arc::new(crate::git::BlameCommit {
        hash: "abc12345".into(),
        summary: summary.into(),
        author: author.into(),
        age_secs: age,
        uncommitted,
    })
}

/// Blame for lines whose text as blamed is not known.
fn blame_of(commits: Vec<std::sync::Arc<crate::git::BlameCommit>>) -> crate::git::Blame {
    crate::git::Blame::new(commits, None)
}

#[test]
fn snippet_expansion_places_caret_and_tab_advances_with_drift() {
    let mut e = editor_with("");
    e.insert_str("for");
    e.expand_snippet("for ${1:i} in ${2:xs}:", 3);
    // Prefix removed, body inserted, caret on the first placeholder with it
    // selected so the next keystroke replaces it.
    assert_eq!(e.lines[0], "for i in xs:");
    assert_eq!((e.cursor_row, e.cursor_col), (0, 5));
    assert!(e.snippet_active());
    assert_eq!(e.selection_text(), "i");
    // Replacing the 1-char placeholder with a 3-char name shifts $2 right.
    e.insert_str("idx");
    assert_eq!(e.lines[0], "for idx in xs:");
    assert!(e.snippet_next());
    assert_eq!(
        e.selection_text(),
        "xs",
        "$2 must track the drift from the longer $1"
    );
    // The last stop ends the session.
    assert!(!e.snippet_active());
}

#[test]
fn replacing_an_outer_placeholder_drops_the_stops_nested_in_it() {
    let mut e = editor_with("");
    e.expand_snippet("${1:foo(${2:x})} ${3:z}", 0);
    assert_eq!(e.selection_text(), "foo(x)");
    e.insert_str("bar");
    assert!(e.snippet_next());
    assert_eq!(
        e.selection_text(),
        "z",
        "the nested $2 went with its placeholder"
    );
    // A same-length replacement is still a replacement.
    let mut e = editor_with("");
    e.expand_snippet("${1:foo(${2:x})} ${3:z}", 0);
    e.insert_str("barbaz");
    assert!(e.snippet_next());
    assert_eq!(e.selection_text(), "z");
    // A plain Tab through an untouched placeholder keeps the nested stop.
    let mut e = editor_with("");
    e.expand_snippet("${1:foo(${2:x})} ${3:z}", 0);
    assert!(e.snippet_next());
    assert_eq!(e.selection_text(), "x");
}

/// On-type formatting (#254) keys off real keystrokes only: the typed
/// paths (`insert_char`, auto-pairs included, and `insert_newline`)
/// record the trigger, while paste/snippet insertion through
/// `insert_str` must not — VS Code never fires formatOnType on paste.
#[test]
fn typing_records_last_typed_but_paste_does_not() {
    let mut e = editor_with("");
    e.insert_char(';');
    assert_eq!(e.last_typed, Some((';', e.edit_seq)));
    e.insert_newline();
    assert_eq!(e.last_typed, Some(('\n', e.edit_seq)));
    e.auto_close_pairs = true;
    e.insert_char('(');
    assert_eq!(
        e.last_typed,
        Some(('(', e.edit_seq)),
        "an auto-paired opener is still a keystroke"
    );
    // Typing the closer over the auto-inserted one moves the caret but
    // changes no content, so the seq deliberately stays put: `edit_seq`
    // means "buffer content unchanged", and an in-flight formatting
    // reply computed against byte-identical text is still safe to apply.
    let seq_before_typeover = e.edit_seq;
    e.insert_char(')');
    assert_eq!(
        e.last_typed,
        Some((')', seq_before_typeover)),
        "a type-over records the keystroke against the unchanged seq"
    );
    assert_eq!(
        e.edit_seq, seq_before_typeover,
        "caret-only motion must not masquerade as a buffer edit"
    );
    let before = e.last_typed;
    e.insert_str("let x = 1;");
    assert_eq!(e.last_typed, before, "paste must not count as typing");
    e.multi_insert_char(';');
    assert_eq!(
        e.last_typed, before,
        "multi-cursor typing must not arm the trigger: one reply cannot \
             be right at every caret"
    );
    assert_ne!(
        e.last_typed.unwrap().1,
        e.edit_seq,
        "and the stale record can no longer match the buffer seq"
    );
}

/// UTF-16 mapping for LSP positions: characters count by UTF-16 code
/// units, and an empty buffer (a fresh scratch tab before any edit)
/// answers the origin instead of panicking.
#[test]
fn pos_to_utf16_counts_utf16_units_and_survives_an_empty_buffer() {
    let e = editor_with("日本🙂x");
    assert_eq!(e.pos_to_utf16(0, 0), (0, 0));
    assert_eq!(e.pos_to_utf16(0, 2), (0, 2));
    assert_eq!(e.pos_to_utf16(0, 3), (0, 4), "🙂 is a surrogate pair");
    assert_eq!(e.pos_to_utf16(0, 4), (0, 5));
    assert_eq!(e.pos_to_utf16(9, 99), (0, 5), "past-the-end clamps");
    let mut empty = Editor::new();
    empty.lines.clear();
    assert_eq!(empty.pos_to_utf16(0, 0), (0, 0));
}

/// #288: every position-carrying LSP request sends UTF-16 columns and every
/// server answer comes back in them, so the two conversions must be exact
/// inverses. A char column used raw as UTF-16 (or the reverse) lands the
/// caret to the LEFT of the symbol on any line with a surrogate pair, which
/// is what made hover/definition/completion miss on emoji lines.
#[test]
fn utf16_and_char_columns_round_trip_across_surrogate_pairs() {
    let e = editor_with("let 🙂 = 🙂🙂;");
    let chars = e.lines[0].chars().count();
    for col in 0..=chars {
        let (_, u16col) = e.pos_to_utf16(0, col);
        assert_eq!(
            e.utf16_col_to_char_pub(0, u16col),
            col,
            "char col {col} must survive the round trip"
        );
    }
    // The divergence the bug rode on: past three surrogate pairs the UTF-16
    // column runs three units ahead of the character column.
    let (_, after_three) = e.pos_to_utf16(0, 10);
    assert_eq!(after_three, 13, "each pair costs one extra unit");
    assert_eq!(
        e.utf16_col_to_char_pub(0, 10),
        9,
        "reading a UTF-16 column as a char column would slip two to the left"
    );
}

/// A failed revert must not launder the buffer clean: clearing `dirty`
/// before the fallible reload meant a file deleted between the conflict
/// popup and the Enter left unsaved edits flagged clean — the tab lost
/// its unsaved marker, auto-save ignored it, and the next FS sweep
/// silently auto-reverted the edits when the file reappeared.
#[test]
fn a_failed_revert_keeps_the_buffer_dirty() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("gone.txt");
    std::fs::write(&path, "disk\n").unwrap();
    let mut e = Editor::new();
    e.open(&path).unwrap();
    e.insert_char('x');
    assert!(e.dirty, "staging: unsaved edits");
    std::fs::remove_file(&path).unwrap();
    assert!(e.revert_to_disk().is_err(), "staging: the reload fails");
    assert!(
        e.dirty,
        "a failed reload must keep the unsaved edits marked dirty"
    );
}

/// A snippet stop can outlive its row: rows deleted mid-session (the
/// backspace join) leave later stops pointing past the end of the
/// buffer, and Tab then planted a selection on the vanished row — the
/// next edit indexed `lines[row]` out of bounds and panicked the TUI.
#[test]
fn a_snippet_stop_on_a_deleted_row_cannot_plant_a_selection_out_of_bounds() {
    let mut e = editor_with("");
    e.insert_str("try");
    e.expand_snippet(
        "try:\n    ${1:pass}\nexcept ${2:Exception}:\n    ${3:raise}",
        3,
    );
    assert!(e.snippet_active());
    assert_eq!(e.selection_text(), "pass");
    // Delete the placeholder, its indent, and the row join — the buffer
    // shrinks below stop $3's recorded row, and backspace keeps the
    // session alive by design.
    for _ in 0..6 {
        e.backspace();
    }
    assert!(e.lines.len() < 4, "staging: a row was joined away");
    e.snippet_next();
    e.snippet_next();
    // The next edit must not panic on a selection past the buffer end.
    e.insert_char('x');
}

/// A reused tab (the preview tab navigating file to file) must drop its
/// collab attachment when it loads a DIFFERENT path: the old generation
/// belongs to another file's doc, and keeping it lets the next collab
/// tick broadcast this file's disk text as edits to that doc. A
/// same-path reload keeps the attachment on purpose — that is how an
/// owner's reload-diff (external change, Replace All) converges as ops.
#[test]
fn open_resets_collab_attachment_only_on_a_path_change() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.txt");
    let b = tmp.path().join("b.txt");
    std::fs::write(&a, "aaa").unwrap();
    std::fs::write(&b, "bbb").unwrap();
    let mut e = Editor::new();
    e.open(&a).unwrap();
    e.collab_doc_gen = 7;
    e.collab_synced_seq = 3;
    e.open(&b).unwrap();
    assert_eq!(
        e.collab_doc_gen, 0,
        "opening another path must detach the buffer from the old doc"
    );
    assert_eq!(
        e.collab_synced_seq, e.edit_seq,
        "a fresh open has nothing pending to extract"
    );
    e.collab_doc_gen = 7;
    e.open(&b).unwrap();
    assert_eq!(
        e.collab_doc_gen, 7,
        "a same-path reload must keep its attachment so the reload broadcasts as edits"
    );
}

/// #928: a snippet expanded in an undo group removes its typed prefix
/// in that group too, so one Undo brings the prefix back with the body
/// gone, not an empty line.
#[test]
fn a_grouped_snippet_undoes_its_prefix_removal_with_its_body() {
    let mut e = editor_with("");
    e.insert_str("for");
    e.expand_snippet_with_in("for ${1:i} in ${2:xs}:", 3, &|_| None, Some(9));
    assert_eq!(e.lines[0], "for i in xs:");
    assert!(e.undo());
    assert_eq!(e.lines[0], "for", "one undo restores the typed prefix");
}

#[test]
fn snippet_indents_continuation_lines_to_the_caret() {
    let mut e = editor_with("    ");
    e.cursor_row = 0;
    e.cursor_col = 4;
    e.expand_snippet("if x:\n    $0", 0);
    assert_eq!(e.lines[0], "    if x:");
    assert_eq!(
        e.lines[1], "        ",
        "continuation line keeps the 4-space block indent plus its own"
    );
}

/// #1554: a snippet body's `\t` is one indent level (VS Code's
/// convention) and was inserted as a literal tab, leaving a tab after
/// spaces in a space-indented file (ruff W191 / E101). It becomes the
/// buffer's indent unit, and the tab stop on that line moves with it.
#[test]
fn a_snippet_tab_becomes_the_buffers_indent_unit() {
    let mut e = editor_with("def run():\n    ");
    e.lang = Some(LangKind::Python);
    e.cursor_row = 1;
    e.cursor_col = 4;
    e.expand_snippet("if __name__ == \"__main__\":\n\t${1:main()}", 0);
    assert_eq!(e.lines[1], "    if __name__ == \"__main__\":");
    assert_eq!(
        e.lines[2], "        main()",
        "four spaces of block indent, four for the tab"
    );
    assert_eq!((e.cursor_row, e.cursor_col), (2, 14));
    assert_eq!(
        e.selection_text(),
        "main()",
        "the stop still selects its placeholder"
    );
}

/// #1554: nested levels (`\t\t`) each become a unit, and a stop past
/// the indentation on such a line lands on its text.
#[test]
fn nested_snippet_tabs_each_become_an_indent_unit() {
    let mut e = editor_with("");
    e.lang = Some(LangKind::Python);
    e.expand_snippet(
        "try:\n\tfor x in xs:\n\t\t${1:pass}\nexcept ${2:E}:\n\t${0}",
        0,
    );
    assert_eq!(
        e.lines,
        vec![
            "try:",
            "    for x in xs:",
            "        pass",
            "except E:",
            "    "
        ]
    );
    assert_eq!(e.selection_text(), "pass");
}

/// Negative: a tab-indented buffer keeps the body's tabs, and a tab
/// inside a line (not leading) is left alone in any buffer.
#[test]
fn snippet_tabs_stay_tabs_in_a_tab_indented_buffer_and_mid_line() {
    let mut e = editor_with("func f() {\n\t\n}");
    e.set_indent_style(IndentStyle {
        width: 4,
        use_spaces: false,
    });
    e.cursor_row = 1;
    e.cursor_col = 1;
    e.expand_snippet("if x {\n\t${1:y}\n}", 0);
    assert_eq!(e.lines[1], "\tif x {");
    assert_eq!(e.lines[2], "\t\ty");
    assert_eq!(e.selection_text(), "y");

    let mut e = editor_with("");
    e.lang = Some(LangKind::Python);
    e.expand_snippet("a\tb\n\tc\td", 0);
    assert_eq!(e.lines, vec!["a\tb", "    c\td"]);
}

#[test]
fn current_line_blame_annotation_reads_author_age_and_summary() {
    let mut e = editor_with("one\ntwo\n");
    e.path = Some(PathBuf::from("f.rs"));
    e.set_blame(
        PathBuf::from("f.rs"),
        Some(blame_of(vec![
            blame_line("Vitali", "fix: the thing", 90, false),
            blame_line("Alice", "feat: two", 7200, false),
        ])),
    );
    e.cursor_row = 0;
    assert_eq!(
        e.current_line_blame_annotation().as_deref(),
        Some("Vitali, 1 minute ago • fix: the thing")
    );
    e.cursor_row = 1;
    assert_eq!(
        e.current_line_blame_annotation().as_deref(),
        Some("Alice, 2 hours ago • feat: two")
    );
}

#[test]
fn blame_is_not_shown_on_a_row_that_holds_another_lines_text() {
    let mut e = editor_with("one\ntwo\nthree\n");
    e.path = Some(PathBuf::from("f.rs"));
    let commits = ["A", "B", "C"].map(|author| blame_line(author, "s", 90, false));
    e.set_blame(
        PathBuf::from("f.rs"),
        Some(crate::git::Blame::new(
            commits.to_vec(),
            Some(PackedLines::from(&["one", "two", "three"][..])),
        )),
    );
    // A cut and paste that keeps the count: `one` moved to the end.
    e.lines = vec!["two".into(), "three".into(), "one".into()];
    e.cursor_row = 0;
    assert_eq!(e.current_line_blame_annotation(), None);
}

#[test]
fn current_line_blame_annotation_marks_uncommitted_lines() {
    let mut e = editor_with("edited\n");
    e.path = Some(PathBuf::from("f.rs"));
    e.set_blame(
        PathBuf::from("f.rs"),
        Some(blame_of(vec![blame_line(
            "Not Committed Yet",
            "x",
            0,
            true,
        )])),
    );
    e.cursor_row = 0;
    assert_eq!(
        e.current_line_blame_annotation().as_deref(),
        Some("Uncommitted changes")
    );
}

#[test]
fn current_line_blame_annotation_is_none_when_disabled_or_unavailable() {
    let mut e = editor_with("one\n");
    e.path = Some(PathBuf::from("f.rs"));
    // No blame fetched yet.
    assert!(e.current_line_blame_annotation().is_none());
    e.set_blame(
        PathBuf::from("f.rs"),
        Some(blame_of(vec![blame_line("V", "s", 5, false)])),
    );
    e.blame_enabled = false;
    assert!(
        e.current_line_blame_annotation().is_none(),
        "the toggle suppresses the annotation without dropping the data"
    );
    // A cursor past the blamed range (line added after the fetch) is safe.
    e.blame_enabled = true;
    e.cursor_row = 9;
    assert!(e.current_line_blame_annotation().is_none());
}

/// A blame reply fetched for one file must never paint on another: the
/// preview tab is reused across files, and the async refetch takes frames
/// to land, so a same-line-count neighbour would wear the old blame.
#[test]
fn blame_for_another_file_never_paints_on_this_one() {
    let mut e = editor_with("one\ntwo\n");
    e.path = Some(PathBuf::from("a.rs"));
    e.set_blame(
        PathBuf::from("a.rs"),
        Some(blame_of(vec![
            blame_line("Alice", "s", 5, false),
            blame_line("Alice", "s", 5, false),
        ])),
    );
    e.path = Some(PathBuf::from("b.rs"));
    assert!(
        e.current_line_blame_annotation().is_none(),
        "a.rs blame painted on b.rs while its own fetch was still in flight"
    );
}

#[test]
fn opening_a_fifo_is_refused_instead_of_blocking() {
    let dir = tempfile::tempdir().unwrap();
    let fifo = dir.path().join("pipe");
    let made = std::process::Command::new("mkfifo").arg(&fifo).status();
    if !made.is_ok_and(|s| s.success()) {
        eprintln!("skipping: mkfifo unavailable");
        return;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut e = Editor::new();
        let _ = tx.send(e.open(&fifo).map_err(|e| e.to_string()));
    });
    let result = rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("open returned instead of blocking on the pipe");
    assert!(result.unwrap_err().contains("not a regular file"));
}

#[test]
fn deleting_closes_clean_tabs_under_the_path_and_keeps_dirty_ones() {
    let mut tabs = EditorTabs::new();
    tabs.add_tab_with_path(PathBuf::from("/w/dir/a.rs"));
    tabs.add_tab_with_path(PathBuf::from("/w/dir/sub/b.rs"));
    tabs.editors[2].dirty = true;
    tabs.add_tab_with_path(PathBuf::from("/w/other.rs"));
    tabs.add_tab_with_path(PathBuf::from("/w/dirt.rs"));
    assert_eq!(tabs.close_clean_tabs_under(Path::new("/w/dir")), 1);
    let left: Vec<_> = tabs.editors.iter().filter_map(|e| e.path.clone()).collect();
    assert_eq!(
        left,
        vec![
            PathBuf::from("/w/dir/sub/b.rs"),
            PathBuf::from("/w/other.rs"),
            PathBuf::from("/w/dirt.rs"),
        ]
    );
}

/// #258: every diff opener must apply the configured default, not just
/// the HEAD-vs-working one. `open_diff` backs the Compare actions, and
/// missing the setter there meant Compare always started in `Off`
/// regardless of config (caught in review on #292).
#[test]
fn every_diff_opener_applies_the_configured_whitespace_default() {
    use crate::widgets::diff::DiffWhitespace;
    let tmp = tempfile::TempDir::new().unwrap();
    let a = tmp.path().join("a.txt");
    let b = tmp.path().join("b.txt");
    std::fs::write(&a, "x = 1;\n").unwrap();
    std::fs::write(&b, "    x = 1;\n").unwrap();

    // The openers live on EditorTabs, which derefs to the active tab, so
    // the default is read from the tab the app syncs it onto.
    let mut tabs = EditorTabs::new();
    tabs.editors[tabs.active].diff_ws_default = DiffWhitespace::Leading;

    // Compare two explorer files.
    tabs.open_diff(&a, &b).unwrap();
    assert_eq!(
        tabs.diff.as_ref().unwrap().ws_mode,
        DiffWhitespace::Leading,
        "open_diff (the Compare actions) must honour the default"
    );

    // HEAD-vs-working, the other user-facing entry point.
    let mut t2 = EditorTabs::new();
    t2.editors[t2.active].diff_ws_default = DiffWhitespace::All;
    t2.open_head_diff_with_text(a.clone(), "x = 1;\n", &b, true)
        .unwrap();
    assert_eq!(
        t2.diff.as_ref().unwrap().ws_mode,
        DiffWhitespace::All,
        "the HEAD diff must honour it too"
    );
}

/// Opening a real file into an editor showing a diff must drop the diff
/// view, or a restore-then-reload keeps rendering the stale side-by-side.
#[test]
fn opening_a_real_file_clears_a_stale_diff_view() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("a.txt");
    std::fs::write(&path, "disk\n").unwrap();
    let mut e = Editor::new();
    e.diff = Some(crate::widgets::diff::DiffData::build_unified_deletion(
        path.clone(),
        "old\n",
    ));
    e.open(&path).unwrap();
    assert!(
        e.diff.is_none(),
        "the stale diff kept rendering over the freshly opened file"
    );
}

/// Deduping a snapshot/HEAD diff onto an existing tab must not destroy a
/// dirty buffer: the TIMELINE always targets the file being edited, so
/// replacing the tab wholesale silently discards unsaved edits.
#[test]
fn a_head_diff_never_replaces_a_dirty_tab_in_place() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("a.txt");
    std::fs::write(&path, "disk\n").unwrap();
    let mut tabs = EditorTabs::new();
    tabs.open(&path).unwrap();
    tabs.lines[0].push('x');
    tabs.dirty = true;
    tabs.open_head_diff_with_text(
        PathBuf::from("a.txt (local snapshot)"),
        "old\n",
        &path,
        false,
    )
    .unwrap();
    assert!(
        tabs.editors
            .iter()
            .any(|e| e.dirty && e.diff.is_none() && e.lines[0] == "diskx"),
        "the dirty buffer was destroyed by the snapshot diff"
    );
    assert!(
        tabs.editors.iter().any(|e| e.diff.is_some()),
        "staging: the diff itself still opened in some tab"
    );
}

#[test]
fn disabling_the_csv_viewer_opens_the_file_as_plain_text() {
    // A .csv opens in the inline sheet viewer by default, but when the CSV
    // extension is disabled in the Extensions panel the same file must fall
    // through to a normal text buffer (no sheet, raw rows as lines).
    let f = tempfile::Builder::new()
        .suffix(".csv")
        .tempfile()
        .expect("temp csv");
    std::fs::write(f.path(), "a,b\n1,2\n").expect("write csv");

    let mut on = Editor::new();
    on.open(f.path()).expect("open with viewer enabled");
    assert!(on.sheet.is_some(), "enabled CSV viewer renders the sheet");

    let mut off = Editor::new();
    off.csv_viewer_enabled = false;
    off.open(f.path()).expect("open with viewer disabled");
    assert!(
        off.sheet.is_none(),
        "disabled CSV viewer must not build a sheet"
    );
    assert_eq!(
        off.lines,
        vec!["a,b".to_string(), "1,2".to_string()],
        "disabled CSV viewer opens the raw rows as text lines"
    );
}

/// A whole-buffer swap leaves no stale attribution behind (#349).
///
/// A reload replaces `lines` outright, and the map described the text
/// that was replaced; keeping it would credit lines to whoever wrote the
/// buffer's PREVIOUS contents, the invariant's worst form, because
/// nothing about the resulting overlay looks wrong. An undo swaps the
/// buffer too, but brings back the map of the text it brings back:
/// here that text had no attribution, so none is left behind.
#[test]
fn a_whole_buffer_swap_forgets_the_old_attributions() {
    let mut ed = Editor::new();
    ed.lines = vec![String::from("a"), String::from("b")];
    ed.cursor_row = 0;
    ed.cursor_col = 0;
    ed.insert_str_as("x\ny", crate::provenance::Seat::Navigator);
    assert!(
        ed.provenance.attributed() > 0,
        "staging: something is attributed before the swap"
    );

    ed.undo();
    assert_eq!(
        ed.provenance.attributed(),
        0,
        "undo left a map describing text that is gone: {:?}",
        ed.provenance
    );

    // A reload is the same shape: `load_text` replaces the buffer, so
    // whatever the map said described the file's previous contents.
    let mut re = Editor::new();
    re.lines = vec![String::from("old")];
    re.cursor_row = 0;
    re.cursor_col = 0;
    re.insert_str_as("mine", crate::provenance::Seat::Me);
    assert!(re.provenance.attributed() > 0, "staging: attributed");
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("other.txt");
    std::fs::write(&f, "entirely different\ntext\n").unwrap();
    re.open(&f).unwrap();
    assert_eq!(
        re.provenance.attributed(),
        0,
        "a reload kept attributions for text it replaced: {:?}",
        re.provenance
    );
}

/// An undo/redo cycle restores the provenance the buffer had at each
/// step (#349, criterion 2): the snapshot carries the map, so undoing an
/// edit brings back the attributions of the text it brings back, and
/// redoing brings back the edit's own. Distinguishable seats per line,
/// or a seat landing on the wrong line would be invisible.
#[test]
fn an_undo_cycle_restores_the_provenance_of_each_step() {
    use crate::provenance::Seat;
    let mut ed = Editor::new();
    ed.lines = vec![String::from("a"), String::from("b")];
    ed.provenance.record(0..1, Seat::Peer(String::from("ada")));
    ed.provenance.record(1..2, Seat::Generated);
    ed.cursor_row = 1;
    ed.cursor_col = 0;
    ed.insert_str_as("x\n", Seat::Navigator);
    assert_eq!(ed.lines, vec!["a", "x", "b"], "staging: the edit landed");
    // Inserting at column 0 splits `b`, and a split credits both halves
    // to the splitting seat (the shipped rule), so `b` reads Navigator
    // now; the undo below must bring back the Generated it had.
    assert_eq!(
        ed.provenance.seat(1),
        Some(&Seat::Navigator),
        "staging: attributed"
    );
    assert_eq!(
        ed.provenance.seat(2),
        Some(&Seat::Navigator),
        "staging: the split half too"
    );

    assert!(ed.undo(), "the edit undoes");
    assert_eq!(ed.lines, vec!["a", "b"]);
    assert_eq!(
        ed.provenance.seat(0),
        Some(&Seat::Peer(String::from("ada"))),
        "undo restores the first line's seat: {:?}",
        ed.provenance
    );
    assert_eq!(
        ed.provenance.seat(1),
        Some(&Seat::Generated),
        "and the second line's, which the edit had moved: {:?}",
        ed.provenance
    );

    assert!(ed.redo(), "the edit redoes");
    assert_eq!(ed.lines, vec!["a", "x", "b"]);
    assert_eq!(
        ed.provenance.seat(1),
        Some(&Seat::Navigator),
        "redo restores the edit's seat"
    );
    assert_eq!(
        ed.provenance.seat(2),
        Some(&Seat::Navigator),
        "and the split half's, exactly as the edit left it"
    );
}

/// Splitting a line attributes BOTH halves to the seat that split it,
/// and that is deliberate (#349).
///
/// It looks wrong on first read — the tail is someone else's text — but
/// the line they wrote no longer exists. Neither half is a line they
/// wrote, so crediting them would be the wrong answer in the other
/// direction. Pinned with this note so the next reader finds the
/// reasoning rather than filing it as a bug.
#[test]
fn splitting_a_line_gives_both_halves_to_the_splitting_seat() {
    let mut ed = Editor::new();
    ed.lines = vec![String::from("hello world")];
    ed.provenance
        .record(0..1, crate::provenance::Seat::Peer(String::from("ada")));
    ed.cursor_row = 0;
    ed.cursor_col = 5;
    ed.insert_str_as("\n", crate::provenance::Seat::Me);

    assert_eq!(
        ed.lines,
        vec![String::from("hello"), String::from(" world")]
    );
    assert_eq!(ed.provenance.seat(0), Some(&crate::provenance::Seat::Me));
    assert_eq!(
        ed.provenance.seat(1),
        Some(&crate::provenance::Seat::Me),
        "the tail is a NEW line, not ada's old one"
    );
}

/// Inserted text is attributed to the seat that inserted it, and
/// nothing else in the buffer is (#349).
///
/// Asserted as a contrast: an untouched line beside the written ones,
/// because "the new lines are attributed" passes just as well against an
/// implementation that attributes the whole buffer to whoever edited
/// last — which is precisely the wrong answer this feature exists to
/// avoid giving.
#[test]
fn inserted_text_carries_the_seat_that_wrote_it() {
    let mut ed = Editor::new();
    ed.lines = vec![String::from("existing"), String::from("tail")];
    ed.cursor_row = 1;
    ed.cursor_col = 0;

    ed.insert_str_as("one\ntwo", crate::provenance::Seat::Navigator);

    assert_eq!(
        ed.provenance.seat(1),
        Some(&crate::provenance::Seat::Navigator),
        "the first inserted line is the navigator's"
    );
    assert_eq!(
        ed.provenance.seat(2),
        Some(&crate::provenance::Seat::Navigator),
        "and so is the second"
    );
    // The line that was already there is NOT attributed: croft did not
    // watch it being written.
    assert_eq!(
        ed.provenance.seat(0),
        None,
        "a pre-existing line must not inherit the editing seat"
    );

    // An insert into an EMPTY buffer: `insert_char_raw` pushes the first
    // line, so the line count moves 0 -> 1 and the recorded range covers
    // exactly the line that now exists. Worth pinning because reasoning
    // about it from the arithmetic alone gets it wrong — `added` looks
    // like 0 until you notice the push.
    let mut empty = Editor::new();
    empty.insert_str_as("hello", crate::provenance::Seat::Me);
    assert_eq!(empty.lines.len(), 1, "staging: the buffer has one line");
    assert_eq!(
        empty.provenance.seat(0),
        Some(&crate::provenance::Seat::Me),
        "the only line must be attributed"
    );
    assert_eq!(
        empty.provenance.attributed(),
        1,
        "and nothing beyond it: {:?}",
        empty.provenance
    );

    // Replacing a MULTI-LINE selection. `delete_selection_inner` runs
    // before the line-count snapshot, so `added` measures only the
    // insertion — but the lines the selection removed must lose their
    // attribution rather than carry it onto the replacement, which is
    // the invariant in its most tempting-to-break form.
    let mut sel = Editor::new();
    sel.lines = vec![
        String::from("keep"),
        String::from("gone one"),
        String::from("gone two"),
        String::from("tail"),
    ];
    // FOUR DISTINCT SEATS, deliberately. With one seat on every line a
    // misattribution is invisible — the wrong seat and the right one are
    // the same value — so the assertions below would pass against the
    // very bug they exist to catch. Only the count would fail.
    sel.provenance.record(0..1, crate::provenance::Seat::Me);
    sel.provenance
        .record(1..2, crate::provenance::Seat::Navigator);
    sel.provenance
        .record(2..3, crate::provenance::Seat::Agent(String::from("pane 2")));
    sel.provenance
        .record(3..4, crate::provenance::Seat::Peer(String::from("ada")));
    sel.cursor_row = 1;
    sel.cursor_col = 0;
    sel.selection = Some(EditorSelection {
        anchor: (1, 0),
        head: (2, 8),
    });
    sel.insert_str_as("replaced", crate::provenance::Seat::Generated);

    assert_eq!(
        sel.provenance.seat(0),
        Some(&crate::provenance::Seat::Me),
        "the line above the selection keeps its own seat"
    );
    assert_eq!(
        sel.provenance.seat(1),
        Some(&crate::provenance::Seat::Generated),
        "the replacement belongs to the seat that made it"
    );
    // The surviving `tail` was ada's. Before the fix it painted as the
    // AGENT — the seat of a line the selection destroyed — which is the
    // bug in the form that matters, and the form a same-seat fixture
    // cannot see.
    assert_eq!(
        sel.provenance.seat(2),
        Some(&crate::provenance::Seat::Peer(String::from("ada"))),
        "the surviving line kept its own seat, not a destroyed line's"
    );
    assert_eq!(
        sel.provenance.attributed(),
        sel.lines.len(),
        "one seat per line, none past the end: {:?}",
        sel.provenance
    );

    // A second seat writing elsewhere does not repaint the first's work.
    ed.cursor_row = 0;
    ed.cursor_col = 0;
    ed.insert_str_as("mine", crate::provenance::Seat::Me);
    assert_eq!(ed.provenance.seat(0), Some(&crate::provenance::Seat::Me));
    assert_eq!(
        ed.provenance.seat(2),
        Some(&crate::provenance::Seat::Navigator),
        "the navigator's lines kept their seat through another edit"
    );
}

#[test]
fn toggle_breakpoint_adds_then_removes_on_cursor_line() {
    let mut e = Editor::new();
    e.path = Some(PathBuf::from("/x/a.py"));
    e.lines = vec!["a".into(), "b".into(), "c".into()];
    e.cursor_row = 1; // 0-based row 1 => 1-based line 2
    // First toggle adds it (now_set = true).
    assert_eq!(
        e.toggle_breakpoint(),
        Some((PathBuf::from("/x/a.py"), 2, true))
    );
    assert_eq!(e.breakpoint_lines(Path::new("/x/a.py")), vec![2u32]);
    // Second toggle removes it (now_set = false).
    assert_eq!(
        e.toggle_breakpoint(),
        Some((PathBuf::from("/x/a.py"), 2, false))
    );
    assert!(e.breakpoint_lines(Path::new("/x/a.py")).is_empty());
}

#[test]
fn toggle_breakpoint_without_open_file_is_noop() {
    let mut e = Editor::new();
    assert_eq!(e.toggle_breakpoint(), None);
}

#[test]
fn removing_a_breakpoint_discards_its_logpoint_and_condition() {
    // F9 twice on a logpoint line reads as "remove, then set a plain
    // breakpoint" — but the orphaned message survived in
    // `breakpoint_logs` and re-attached on the next set, resurrecting a
    // logpoint that never pauses. Same leak for conditions.
    let path = PathBuf::from("/x/a.py");
    let mut e = Editor::new();
    e.path = Some(path.clone());
    e.lines = vec!["a".into(), "b".into(), "c".into()];
    e.toggle_breakpoint_line(2);
    e.breakpoint_logs
        .entry(path.clone())
        .or_default()
        .insert(2, String::from("x is {x}"));
    e.breakpoint_conditions
        .entry(path.clone())
        .or_default()
        .insert(2, String::from("x > 1"));
    e.toggle_breakpoint_line(2); // remove
    e.toggle_breakpoint_line(2); // re-add: a PLAIN breakpoint
    let lines = e.breakpoints.get(&path).cloned().unwrap();
    let specs = e.source_breakpoints(&path, &lines);
    assert_eq!(specs.len(), 1);
    assert_eq!(
        specs[0].log_message, None,
        "a re-added breakpoint must pause, not print"
    );
    assert_eq!(
        specs[0].condition, None,
        "a re-added breakpoint must be unconditional"
    );
}

#[test]
fn removing_a_breakpoint_drops_its_hit_condition() {
    let mut e = Editor::new();
    let p = PathBuf::from("/x/a.py");
    e.path = Some(p.clone());
    e.lines = vec!["a".into(), "b".into()];
    e.toggle_breakpoint_line(1);
    e.breakpoint_hit_conditions
        .entry(p.clone())
        .or_default()
        .insert(1, String::from(">= 5"));
    e.toggle_breakpoint_line(1);
    e.toggle_breakpoint_line(1);
    assert!(
        !e.breakpoint_hit_conditions.contains_key(&p),
        "a fresh breakpoint on the line must not inherit the old hit count"
    );
}

#[test]
fn toggle_breakpoint_line_targets_an_explicit_line_not_the_cursor() {
    let mut e = Editor::new();
    e.path = Some(PathBuf::from("/x/a.py"));
    e.lines = vec!["a".into(), "b".into(), "c".into()];
    e.cursor_row = 0; // cursor on line 1, but we target line 3
    assert_eq!(
        e.toggle_breakpoint_line(3),
        Some((PathBuf::from("/x/a.py"), 3, true))
    );
    assert_eq!(e.breakpoint_lines(Path::new("/x/a.py")), vec![3u32]);
    // Cursor never moved.
    assert_eq!(e.cursor_row, 0);
    // Toggling the same line removes it again.
    assert_eq!(
        e.toggle_breakpoint_line(3),
        Some((PathBuf::from("/x/a.py"), 3, false))
    );
    assert!(e.breakpoint_lines(Path::new("/x/a.py")).is_empty());
}

#[test]
fn gutter_line_at_maps_gutter_clicks_and_ignores_the_body() {
    let mut e = editor_with("a\nbb\nccc\ndddd\neeeee");
    e.last_inner = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 25,
    };
    e.last_gutter_width = 2; // text_x = 0 + 2 + 1 = 3
    // A click in the glyph margin / line-number columns maps to the line.
    assert_eq!(e.gutter_line_at(0, 0), Some(0), "col 0 is the glyph margin");
    assert_eq!(e.gutter_line_at(2, 2), Some(2), "col 2 is the line number");
    // The body (col >= text_x) is NOT the gutter.
    assert_eq!(e.gutter_line_at(3, 0), None, "col 3 is the text body");
    assert_eq!(e.gutter_line_at(10, 1), None, "deep in the body");
    // Past the last line there is no gutter target.
    assert_eq!(e.gutter_line_at(0, 20), None, "row past content");
}

#[test]
fn gutter_line_at_follows_vertical_scroll() {
    let mut e = editor_with("a\nbb\nccc\ndddd\neeeee");
    e.last_inner = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 25,
    };
    e.last_gutter_width = 2;
    e.scroll = 2;
    assert_eq!(
        e.gutter_line_at(1, 0),
        Some(2),
        "the top visible gutter row is buffer line scroll = 2"
    );
}

/// A bookmark editor over a real temp file: bookmarks are keyed by path,
/// so an unsaved scratch buffer cannot carry any.
fn bookmark_editor(lines: usize) -> (Editor, tempfile::NamedTempFile) {
    let f = NamedTempFile::new().unwrap();
    let body: String = (1..=lines).map(|i| format!("line {i}\n")).collect();
    std::fs::write(f.path(), &body).unwrap();
    let mut e = Editor::new();
    e.open(f.path()).unwrap();
    (e, f)
}

#[test]
fn toggling_a_bookmark_adds_then_removes_the_cursor_line() {
    let (mut e, _f) = bookmark_editor(10);
    e.cursor_row = 3; // 0-based; the mark is stored 1-based
    assert_eq!(e.toggle_bookmark(), Some(true), "first toggle sets it");
    assert_eq!(e.bookmarked_lines(), vec![4]);
    assert_eq!(e.toggle_bookmark(), Some(false), "second toggle clears it");
    assert!(e.bookmarked_lines().is_empty());
}

#[test]
fn a_bookmark_moves_with_its_line_on_move_line_down_and_back() {
    let (mut e, _f) = bookmark_editor(10);
    e.cursor_row = 3;
    e.toggle_bookmark();
    e.move_lines_down();
    assert_eq!(e.lines[4], "line 4");
    assert_eq!(e.bookmarked_lines(), vec![5]);
    e.move_lines_up();
    assert_eq!(e.bookmarked_lines(), vec![4]);
    // An in-place edit keeps the mark on its row.
    e.cursor_col = 0;
    e.insert_char('x');
    assert_eq!(e.bookmarked_lines(), vec![4]);
}

#[test]
fn breakpoints_and_their_conditions_follow_edits_like_bookmarks() {
    let (mut e, _tmp) = bookmark_editor(10);
    let f = e.path.clone().unwrap();
    e.toggle_breakpoint_line(4);
    e.breakpoint_conditions
        .entry(f.clone())
        .or_default()
        .insert(4, String::from("x > 1"));
    // Enter at the very top pushes every line down one.
    e.cursor_row = 0;
    e.cursor_col = 0;
    e.insert_newline();
    assert_eq!(e.breakpoint_lines(&f), vec![5]);
    assert_eq!(
        e.breakpoint_conditions[&f].get(&5).map(String::as_str),
        Some("x > 1")
    );
    // Moving the line carries its breakpoint along.
    e.cursor_row = 4;
    e.move_lines_down();
    assert_eq!(e.breakpoint_lines(&f), vec![6]);
    // Deleting the line drops it rather than leaving it on a neighbour.
    e.cursor_row = 5;
    e.selection = Some(EditorSelection {
        anchor: (5, 0),
        head: (6, 0),
    });
    e.backspace();
    assert!(
        e.breakpoint_lines(&f).is_empty(),
        "{:?}",
        e.breakpoint_lines(&f)
    );
}

#[test]
fn mirrored_edits_above_move_the_extra_carets_too() {
    let (mut e, _f) = bookmark_editor(5);
    e.cursor_row = 4;
    e.carets = vec![EditorSelection {
        anchor: (3, 0),
        head: (3, 0),
    }];
    let mut new = e.lines.clone();
    new.drain(0..2);
    assert!(e.mirror_lines_from(&new, 1, (0, 0)));
    assert_eq!(e.cursor_row, 2);
    assert_eq!(e.carets[0].head, (1, 0));
}

#[test]
fn clearing_the_last_bookmark_drops_the_files_entry_entirely() {
    let (mut e, f) = bookmark_editor(10);
    e.cursor_row = 0;
    e.toggle_bookmark();
    e.toggle_bookmark();
    assert!(
        !e.bookmarks.contains_key(f.path()),
        "an emptied set must be removed, not left as a hollow entry"
    );
}

#[test]
fn a_scratch_buffer_with_no_path_cannot_be_bookmarked() {
    let mut e = editor_with("a\nb\n");
    assert_eq!(e.toggle_bookmark(), None);
    assert!(e.bookmarked_lines().is_empty());
}

#[test]
fn bookmarks_report_ascending_regardless_of_the_order_they_were_set() {
    let (mut e, _f) = bookmark_editor(30);
    for row in [20, 4, 11] {
        e.cursor_row = row;
        e.toggle_bookmark();
    }
    assert_eq!(e.bookmarked_lines(), vec![5, 12, 21]);
}

#[test]
fn next_bookmark_walks_down_and_wraps_to_the_first() {
    let (mut e, _f) = bookmark_editor(40);
    for row in [4, 14, 29] {
        e.cursor_row = row;
        e.toggle_bookmark();
    }
    e.cursor_row = 0;
    assert_eq!(e.goto_next_bookmark(), Some(5));
    assert_eq!(e.cursor_row, 4);
    assert_eq!(e.goto_next_bookmark(), Some(15));
    assert_eq!(e.goto_next_bookmark(), Some(30));
    assert_eq!(
        e.goto_next_bookmark(),
        Some(5),
        "past the last mark it wraps to the top of the file"
    );
    assert_eq!(e.cursor_row, 4);
}

#[test]
fn previous_bookmark_walks_up_and_wraps_to_the_last() {
    let (mut e, _f) = bookmark_editor(40);
    for row in [4, 14, 29] {
        e.cursor_row = row;
        e.toggle_bookmark();
    }
    e.cursor_row = 39;
    assert_eq!(e.goto_prev_bookmark(), Some(30));
    assert_eq!(e.goto_prev_bookmark(), Some(15));
    assert_eq!(e.goto_prev_bookmark(), Some(5));
    assert_eq!(
        e.goto_prev_bookmark(),
        Some(30),
        "above the first mark it wraps to the bottom of the file"
    );
}

#[test]
fn navigating_with_no_bookmarks_reports_none_and_leaves_the_cursor() {
    let (mut e, _f) = bookmark_editor(10);
    e.cursor_row = 6;
    assert_eq!(e.goto_next_bookmark(), None);
    assert_eq!(e.goto_prev_bookmark(), None);
    assert_eq!(e.cursor_row, 6, "a failed jump must not move the cursor");
}

#[test]
fn a_lone_bookmark_on_the_cursors_own_line_still_reports_itself() {
    let (mut e, _f) = bookmark_editor(10);
    e.cursor_row = 5;
    e.toggle_bookmark();
    assert_eq!(
        e.goto_next_bookmark(),
        Some(6),
        "wrapping onto the only mark must report it, not look broken"
    );
    assert_eq!(e.goto_prev_bookmark(), Some(6));
}

#[test]
fn jumping_to_a_bookmark_drops_any_selection() {
    let (mut e, _f) = bookmark_editor(20);
    e.cursor_row = 15;
    e.toggle_bookmark();
    e.cursor_row = 0;
    e.selection = Some(EditorSelection {
        anchor: (0, 0),
        head: (0, 3),
    });
    e.goto_next_bookmark();
    assert!(
        e.selection.is_none(),
        "a jump is a navigation, not an extend"
    );
}

#[test]
fn clear_bookmarks_reports_how_many_it_removed() {
    let (mut e, _f) = bookmark_editor(20);
    for row in [1, 5, 9] {
        e.cursor_row = row;
        e.toggle_bookmark();
    }
    assert_eq!(e.clear_bookmarks(), 3);
    assert!(e.bookmarked_lines().is_empty());
    assert_eq!(e.clear_bookmarks(), 0, "clearing twice is a no-op");
}

#[test]
fn bookmarks_are_per_file_so_switching_tabs_keeps_each_files_marks() {
    let (mut e, first) = bookmark_editor(10);
    e.cursor_row = 2;
    e.toggle_bookmark();
    let second = NamedTempFile::new().unwrap();
    std::fs::write(second.path(), "x\ny\nz\n").unwrap();
    e.open(second.path()).unwrap();
    assert!(
        e.bookmarked_lines().is_empty(),
        "the second file starts unmarked"
    );
    e.cursor_row = 0;
    e.toggle_bookmark();
    e.open(first.path()).unwrap();
    assert_eq!(
        e.bookmarked_lines(),
        vec![3],
        "coming back restores the first file's marks"
    );
}

/// Set a bookmark on each 0-based row through the real toggle path.
fn mark_rows(e: &mut Editor, rows: &[usize]) {
    for &row in rows {
        e.cursor_row = row;
        e.cursor_col = 0;
        e.toggle_bookmark();
    }
}

/// Several edit paths change lines in more than one place and sync once
/// (multi-cursor line delete, LSP formatting, Replace All with newlines,
/// and their undo). A mark between two such regions must move by the
/// lines removed above it, not stay put because a single prefix/suffix
/// span swallowed it.
#[test]
fn bookmark_shift_follows_lines_through_a_multi_region_edit() {
    let old: Vec<String> = (1..=10).map(|i| format!("line {i}")).collect();
    let new: Vec<String> = old
        .iter()
        .filter(|l| *l != "line 2" && *l != "line 7")
        .cloned()
        .collect();
    let marks: std::collections::BTreeSet<usize> = [2, 5, 7, 9].into_iter().collect();
    let moved: Vec<usize> = shift_bookmark_lines(&marks, &old, &new)
        .into_iter()
        .collect();
    // `line 5` is now row 4 and `line 9` row 7; the marks on the two
    // deleted lines go with them.
    assert_eq!(moved, vec![4, 7]);
    assert_eq!(new[3], "line 5");
    assert_eq!(new[6], "line 9");
}

#[test]
fn bookmark_shift_a_newline_above_a_mark_pushes_it_down() {
    let (mut e, _f) = bookmark_editor(10);
    mark_rows(&mut e, &[1, 5]); // lines 2 and 6
    e.cursor_row = 2;
    e.cursor_col = 0;
    e.insert_newline();
    assert_eq!(
        e.bookmarked_lines(),
        vec![2, 7],
        "the mark above the edit stays, the one below follows its line"
    );
    assert!(e.is_bookmarked(6), "row 6 now holds `line 6`");
    assert_eq!(e.lines[6], "line 6");
}

#[test]
fn bookmark_shift_a_multi_line_paste_moves_marks_by_the_pasted_count() {
    let (mut e, _f) = bookmark_editor(10);
    mark_rows(&mut e, &[5]); // `line 6`
    e.cursor_row = 0;
    e.cursor_col = 0;
    e.insert_str("a\nb\nc\n");
    assert_eq!(e.lines[8], "line 6");
    assert_eq!(e.bookmarked_lines(), vec![9]);
}

#[test]
fn bookmark_shift_deleting_lines_drops_marks_inside_and_pulls_up_the_rest() {
    let (mut e, _f) = bookmark_editor(10);
    mark_rows(&mut e, &[2, 4, 8]); // lines 3, 5, 9
    e.cursor_row = 3;
    e.delete_lines(3); // removes lines 4, 5, 6
    assert_eq!(e.lines[5], "line 9");
    assert_eq!(
        e.bookmarked_lines(),
        vec![3, 6],
        "line 5 went with its line; line 9 is now line 6"
    );
}

#[test]
fn bookmark_shift_undo_and_redo_of_a_line_delete_move_marks_back_and_forth() {
    let (mut e, _f) = bookmark_editor(10);
    mark_rows(&mut e, &[8]); // `line 9`
    e.cursor_row = 3;
    e.delete_lines(3);
    assert_eq!(e.bookmarked_lines(), vec![6]);
    assert!(e.undo());
    assert_eq!(e.lines[8], "line 9");
    assert_eq!(e.bookmarked_lines(), vec![9], "undo restores the line");
    assert!(e.redo());
    assert_eq!(e.bookmarked_lines(), vec![6], "redo deletes it again");
}

#[test]
fn bookmark_shift_a_selection_delete_collapses_inner_marks_onto_the_join_line() {
    let (mut e, _f) = bookmark_editor(10);
    mark_rows(&mut e, &[3, 7]); // lines 4 and 8
    e.selection = Some(EditorSelection {
        anchor: (2, 2),
        head: (5, 1),
    });
    e.cursor_row = 5;
    e.cursor_col = 1;
    assert!(e.delete_selection());
    assert_eq!(e.lines[4], "line 8");
    assert_eq!(
        e.bookmarked_lines(),
        vec![3, 5],
        "line 4 folded into the join line 3; line 8 moved up by 3"
    );
}

#[test]
fn bookmark_shift_backspace_joining_lines_keeps_the_mark_on_the_join_line() {
    let (mut e, _f) = bookmark_editor(10);
    mark_rows(&mut e, &[4, 6]); // lines 5 and 7
    e.cursor_row = 4;
    e.cursor_col = 0;
    e.backspace();
    assert_eq!(e.lines[3], "line 4line 5");
    assert_eq!(e.bookmarked_lines(), vec![4, 6]);
}

#[test]
fn bookmark_shift_an_external_reload_that_shrinks_the_file_clamps_marks() {
    let (mut e, f) = bookmark_editor(10);
    mark_rows(&mut e, &[1, 9]); // lines 2 and 10
    std::fs::write(f.path(), "line 1\nline 2\nline 3\n").unwrap();
    assert!(matches!(e.reload_if_clean(), Some(Ok(()))));
    assert_eq!(e.lines.len(), 3);
    let marks = e.bookmarked_lines();
    assert!(
        marks.iter().all(|&l| l <= e.lines.len()),
        "no mark may outlive the buffer; was {marks:?}"
    );
    assert!(marks.contains(&2), "the untouched line keeps its mark");
    assert!(
        e.bookmarks.values().flatten().all(|&l| l <= 3),
        "the stored set is clamped too, not only the view"
    );
}

#[test]
fn bookmark_nav_next_wraps_after_a_delete_pulled_the_last_mark_up() {
    let (mut e, _f) = bookmark_editor(10);
    mark_rows(&mut e, &[1, 9]); // lines 2 and 10
    e.cursor_row = 3;
    e.delete_lines(3);
    e.cursor_row = e.lines.len() - 1; // `line 10`, now line 7
    assert_eq!(
        e.goto_next_bookmark(),
        Some(2),
        "from the last mark, next wraps to the first"
    );
    assert_eq!(e.cursor_row, 1);
}

#[test]
fn bookmark_nav_ignores_a_mark_past_the_end_of_the_buffer() {
    // Defence in depth for navigation itself: a stale mark beyond the
    // last line (however it got there) must not become a target the
    // jump clamps back onto the cursor's own row, which froze the ring.
    let (mut e, f) = bookmark_editor(10);
    mark_rows(&mut e, &[1]);
    e.bookmarks
        .get_mut(f.path())
        .expect("marked above")
        .insert(50);
    e.cursor_row = 9;
    assert_eq!(e.goto_next_bookmark(), Some(2), "wraps past the stale mark");
    e.cursor_row = 0;
    assert_eq!(
        e.goto_prev_bookmark(),
        Some(2),
        "prev wraps to the last in-range mark, not line 50"
    );
    assert_eq!(e.bookmarked_lines(), vec![2]);
}

#[test]
fn is_bookmarked_is_zero_based_to_match_the_render_loop() {
    let (mut e, _f) = bookmark_editor(10);
    e.cursor_row = 4; // stored as line 5
    e.toggle_bookmark();
    assert!(e.is_bookmarked(4), "0-based row 4 is the bookmarked line");
    assert!(
        !e.is_bookmarked(5),
        "off-by-one would light up the wrong row"
    );
}

/// #1625: a ghost continuation row of wide (CJK) characters is clipped
/// by terminal cells, so it never paints past the editor's own area.
#[test]
fn a_wide_character_ghost_row_stays_inside_the_editor() {
    let mut e = editor_with("x = 1");
    e.focused = true;
    e.cursor_row = 0;
    e.cursor_col = 5;
    let wide: String = "\u{4e2d}".repeat(60);
    e.ghost = Some(crate::inline_complete::Ghost::insert(
        0,
        5,
        e.edit_seq,
        format!("\n{wide}"),
    ));
    let area = Rect {
        x: 0,
        y: 0,
        width: 30,
        height: 10,
    };
    let mut buf = ratatui::buffer::Buffer::empty(Rect { width: 80, ..area });
    ratatui::widgets::Widget::render(&mut e, area, &mut buf);
    let row = (0..area.height)
        .find(|&y| (0..area.width).any(|x| buf[(x, y)].symbol() == "\u{4e2d}"))
        .expect("the ghost row is painted");
    for x in area.width..80 {
        assert_eq!(buf[(x, row)].symbol(), " ", "nothing painted at column {x}");
    }
}

#[test]
fn the_bookmark_flag_is_painted_in_the_left_glyph_margin() {
    let (mut e, _f) = bookmark_editor(5);
    e.cursor_row = 1; // line 2
    e.toggle_bookmark();
    let area = Rect {
        x: 0,
        y: 0,
        width: 30,
        height: 10,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut e, area, &mut buf);
    let row = e.last_inner.y + 1; // line 2 is the 2nd content row
    assert_eq!(
        buf[(e.last_inner.x, row)].symbol(),
        "\u{2691}",
        "the flag belongs in the sign margin at inner.x"
    );
    let unmarked = e.last_inner.y + 2; // line 3 carries no mark
    assert_ne!(buf[(e.last_inner.x, unmarked)].symbol(), "\u{2691}");
}

#[test]
fn a_breakpoint_outranks_a_bookmark_in_the_shared_sign_cell() {
    let (mut e, f) = bookmark_editor(5);
    e.cursor_row = 1;
    e.toggle_bookmark();
    e.breakpoints
        .entry(f.path().to_path_buf())
        .or_default()
        .insert(2); // same line
    let area = Rect {
        x: 0,
        y: 0,
        width: 30,
        height: 10,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut e, area, &mut buf);
    assert_eq!(
        buf[(e.last_inner.x, e.last_inner.y + 1)].symbol(),
        "\u{25cf}",
        "debugger state must never be hidden behind a reader's own mark"
    );
}

#[test]
fn breakpoint_glyph_sits_in_left_margin_not_over_the_line_number() {
    let f = NamedTempFile::new().unwrap();
    std::fs::write(f.path(), "aaa\nbbb\nccc\n").unwrap();
    let mut ed = Editor::new();
    ed.open(f.path()).unwrap();
    ed.breakpoints
        .entry(f.path().to_path_buf())
        .or_default()
        .insert(2); // breakpoint on line 2
    let area = Rect {
        x: 0,
        y: 0,
        width: 30,
        height: 10,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut ed, area, &mut buf);
    // The editor insets by a 1-col border, so inner.x == 1 and content rows
    // start at y == 1; line 2 is therefore at y == 2.
    let inner_x = ed.last_inner.x;
    let gutter = ed.last_gutter_width;
    let row = ed.last_inner.y + 1; // line 2 is the 2nd content row
    let mut line = String::new();
    for x in 0..area.width {
        line.push_str(buf[(x, row)].symbol());
    }
    // The dot lives in the far-left glyph margin...
    assert_eq!(
        buf[(inner_x, row)].symbol(),
        "●",
        "breakpoint dot must be in the left glyph margin; row was {line:?}"
    );
    // ...the line number is right-aligned after the margin (its last digit
    // sits one cell before the gutter's trailing space) and is intact...
    assert_eq!(
        buf[(inner_x + gutter - 2, row)].symbol(),
        "2",
        "line number must survive after the margin; row was {line:?}"
    );
    // ...and the margin never overwrites a digit (the cell after the dot is
    // a blank, not a number).
    assert_eq!(
        buf[(inner_x + 1, row)].symbol(),
        " ",
        "a space separates the dot from the line number; row was {line:?}"
    );
}

#[test]
fn source_breakpoints_attach_conditions() {
    let mut e = Editor::new();
    let p = PathBuf::from("/x/a.py");
    let mut lines = std::collections::BTreeSet::new();
    lines.insert(3usize);
    lines.insert(5usize);
    e.breakpoint_conditions
        .entry(p.clone())
        .or_default()
        .insert(5, String::from("i == 10"));
    let specs = e.source_breakpoints(&p, &lines);
    assert_eq!(specs.len(), 2);
    assert_eq!(specs[0].line, 3);
    assert_eq!(specs[0].condition, None);
    assert_eq!(specs[1].line, 5);
    assert_eq!(specs[1].condition.as_deref(), Some("i == 10"));
}

#[test]
fn source_breakpoints_attach_log_messages() {
    let mut e = Editor::new();
    let p = PathBuf::from("/x/a.py");
    let mut lines = std::collections::BTreeSet::new();
    lines.insert(3usize);
    lines.insert(5usize);
    e.breakpoint_logs
        .entry(p.clone())
        .or_default()
        .insert(5, String::from("value is {v}"));
    let specs = e.source_breakpoints(&p, &lines);
    assert_eq!(specs[0].log_message, None);
    assert_eq!(specs[1].log_message.as_deref(), Some("value is {v}"));
}

#[test]
fn logpoint_wears_an_amber_diamond_in_the_gutter() {
    let mut e = editor_with("a\nb\nc");
    let p = PathBuf::from("/x/lp.py");
    e.path = Some(p.clone());
    e.breakpoints.entry(p.clone()).or_default().insert(2);
    e.breakpoint_logs
        .entry(p)
        .or_default()
        .insert(2, String::from("hit {x}"));
    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 6,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut e as &mut Editor).render(area, &mut buf);
    let cell = &buf[(e.last_inner.x, e.last_inner.y + 1)];
    assert_eq!(
        cell.symbol(),
        "◆",
        "a logpoint paints the diamond in the glyph margin"
    );
    assert_eq!(
        cell.fg,
        Color::Rgb(0xe5, 0xc0, 0x7b),
        "the logpoint diamond is amber, distinct from the conditional's red"
    );
}

#[test]
fn word_string_at_reads_identifier_under_cursor() {
    let mut e = Editor::new();
    e.lines = vec!["    total = count + 1".into()];
    // cursor inside "total" (chars 4..9)
    assert_eq!(e.word_string_at(0, 6).as_deref(), Some("total"));
    // cursor inside "count"
    assert_eq!(e.word_string_at(0, 13).as_deref(), Some("count"));
    // over the '=' (non-word) => None
    assert_eq!(e.word_string_at(0, 10), None);
}

// ---- Paste keeps relative indentation (#1218) ----

const SHOP: &str = "class Shop:\n    def total(self, items):\n        if items:\n            n = 0\n            \n        return 0";
const LOOP: &str = "for x in items:\n    if x:\n        print(x)";

fn shop() -> Editor {
    let mut e = editor_with(SHOP);
    e.lang = Some(LangKind::Python);
    e.cursor_row = 4;
    e.cursor_col = 12;
    e
}

#[test]
fn a_pasted_block_keeps_its_shape_under_the_caret() {
    let mut e = shop();
    e.paste_str(LOOP, true);
    assert_eq!(
        &e.lines[4..8],
        &[
            "            for x in items:",
            "                if x:",
            "                    print(x)",
            "        return 0",
        ]
    );
    assert_eq!(
        (e.cursor_row, e.cursor_col),
        (6, 28),
        "the caret ends after print(x)"
    );
}

#[test]
fn one_undo_restores_the_verbatim_paste_and_a_second_removes_it() {
    let mut e = shop();
    e.paste_str(LOOP, true);
    assert!(e.undo());
    assert_eq!(
        &e.lines[4..7],
        &[
            "            for x in items:",
            "    if x:",
            "        print(x)"
        ]
    );
    assert!(e.undo());
    assert_eq!(e.lines.join("\n"), SHOP);
}

#[test]
fn a_pasted_block_copied_with_its_own_indentation_lands_at_the_caret() {
    let mut e = shop();
    e.paste_str(
        "    for x in items:\n        if x:\n            print(x)\n",
        true,
    );
    assert_eq!(
        &e.lines[4..8],
        &[
            "            for x in items:",
            "                if x:",
            "                    print(x)",
            "",
        ]
    );
}

#[test]
fn tabs_in_the_paste_follow_the_buffers_spaces() {
    let mut e = shop();
    e.paste_str("for x in items:\n\tif x:\n\t\tprint(x)", true);
    assert_eq!(e.lines[5], "                if x:");
    assert_eq!(e.lines[6], "                    print(x)");
}

#[test]
fn a_paste_at_column_zero_is_unchanged() {
    let mut e = editor_with("x = 1\n\nz");
    e.lang = Some(LangKind::Python);
    e.cursor_row = 1;
    e.cursor_col = 0;
    e.paste_str(LOOP, true);
    assert_eq!(
        &e.lines[1..4],
        &["for x in items:", "    if x:", "        print(x)"]
    );
    assert!(e.undo());
    assert_eq!(
        e.lines[1], "",
        "a paste needing no re-indent is one undo step"
    );
}

#[test]
fn an_indented_paste_at_column_zero_keeps_its_indentation() {
    let mut e = editor_with("x = 1\n\nz");
    e.lang = Some(LangKind::Python);
    e.cursor_row = 1;
    e.cursor_col = 0;
    e.paste_str("    if x:\n        run()", true);
    assert_eq!(&e.lines[1..3], &["    if x:", "        run()"]);
}

#[test]
fn a_first_line_deeper_than_a_later_one_keeps_the_difference() {
    let mut e = shop();
    e.paste_str("    if cond:\n        body()\nelse:", true);
    assert_eq!(
        &e.lines[4..7],
        &[
            "                if cond:",
            "                    body()",
            "            else:",
        ]
    );
}

#[test]
fn a_paste_inside_a_docstring_is_left_verbatim() {
    let mut e = editor_with("def f():\n    \"\"\"\n    \n    \"\"\"");
    e.lang = Some(LangKind::Python);
    e.recompute_highlights();
    e.cursor_row = 2;
    e.cursor_col = 4;
    e.paste_str("hello\nworld", true);
    assert_eq!(&e.lines[2..4], &["    hello", "world"]);
    let mut e = editor_with("def f():\n    x = \"\"\"a");
    e.lang = Some(LangKind::Python);
    e.cursor_row = 1;
    e.cursor_col = 14;
    e.paste_str("hello\nworld", true);
    assert_eq!(&e.lines[1..3], &["    x = \"\"\"ahello", "world"]);
}

#[test]
fn paste_reindent_off_or_in_plain_text_pastes_verbatim() {
    let mut e = shop();
    e.paste_str(LOOP, false);
    assert_eq!(e.lines[5], "    if x:");
    let mut e = shop();
    e.lang = None;
    e.paste_str(LOOP, true);
    assert_eq!(
        e.lines[5], "    if x:",
        "plain text keeps the clipboard's spacing"
    );
}

#[test]
fn a_one_line_paste_is_never_touched() {
    let mut e = shop();
    e.paste_str("    total = 1", true);
    assert_eq!(e.lines[4], "                total = 1");
}

fn editor_with(text: &str) -> Editor {
    let mut e = Editor::new();
    e.lines = if text.is_empty() {
        vec![String::new()]
    } else {
        text.lines().map(|s| s.to_string()).collect()
    };
    if e.lines.is_empty() {
        e.lines.push(String::new());
    }
    e
}

fn head(lines: &[&str]) -> Option<PackedLines> {
    Some(PackedLines::from(lines))
}

#[test]
fn git_gutter_marks_added_modified_and_deleted_lines() {
    let mut e = editor_with("a\nB2\nINSERTED\nc");
    // HEAD: a / b / c / d  → b became B2 (modified), INSERTED is new (added),
    // and d was removed before EOF.
    e.set_git_head_lines(PathBuf::from("f.rs"), head(&["a", "b", "c", "d"]));
    e.refresh_git_marks();
    assert_eq!(e.git_mark_at(0), None, "unchanged line carries no mark");
    assert_eq!(e.git_mark_at(1), Some(GitMark::Modified));
    assert_eq!(e.git_mark_at(2), Some(GitMark::Added));
    // `c` survives at line 3 with the deletion of `d` just below it.
    assert_eq!(e.git_mark_at(3), Some(GitMark::Deleted));
}

#[test]
fn git_gutter_no_baseline_means_no_marks() {
    let mut e = editor_with("a\nb\nc");
    e.refresh_git_marks();
    assert_eq!(e.git_mark_at(0), None);
    assert_eq!(e.git_mark_at(1), None);
}

#[test]
fn git_gutter_recomputes_after_an_edit() {
    let mut e = editor_with("a\nb");
    e.set_git_head_lines(PathBuf::from("f.rs"), head(&["a", "b"]));
    e.refresh_git_marks();
    assert_eq!(e.git_mark_at(1), None, "buffer matches HEAD: clean");
    // Edit line 1, then a fresh render-time refresh must light it up.
    e.cursor_row = 1;
    e.cursor_col = 1;
    e.insert_char('X');
    e.refresh_git_marks();
    assert_eq!(e.git_mark_at(1), Some(GitMark::Modified));
}

/// The provenance lens paints a bar in the seat's hue in the gutter's
/// spacer cell (#349): two seats, two colours, and a line with no record
/// paints nothing there. Off, the lane is the git gutter's alone. On
/// Croft Loch (the default) the bar is the one-eighth block, the quarter
/// block being the change bar's there.
#[test]
fn provenance_overlay_paints_a_seat_coloured_bar_per_line() {
    use crate::provenance::Seat;
    use ratatui::buffer::Buffer;
    let mut e = editor_with("a\nb\nc");
    e.provenance.record(0..1, Seat::Me);
    e.provenance
        .record(1..2, Seat::Agent(String::from("pane 2")));
    let area = Rect {
        x: 0,
        y: 0,
        width: 30,
        height: 5,
    };
    let mut buf = Buffer::empty(area);
    (&mut e).render(area, &mut buf);
    let bar_x = e.last_inner.x + e.last_gutter_width;
    assert_ne!(
        buf[(bar_x, e.last_inner.y)].symbol(),
        "\u{258f}",
        "off, the lens paints nothing"
    );
    e.provenance_overlay = true;
    let mut buf = Buffer::empty(area);
    (&mut e).render(area, &mut buf);
    let me = &buf[(bar_x, e.last_inner.y)];
    let agent = &buf[(bar_x, e.last_inner.y + 1)];
    let unknown = &buf[(bar_x, e.last_inner.y + 2)];
    assert_eq!(me.symbol(), "\u{258f}", "a line you typed gets a bar");
    assert_eq!(
        agent.symbol(),
        "\u{258f}",
        "a line an agent typed gets a bar"
    );
    assert_ne!(me.style().fg, agent.style().fg, "in distinct hues");
    assert_ne!(
        unknown.symbol(),
        "\u{258f}",
        "a line with no record is never guessed"
    );
}

/// Inside a conflict block the annotation stays off with the lens on
/// (#349): the [Accept …] actions own the header row's cells, and a seat
/// standing alone there would paint over the affordance that resolves
/// the conflict.
#[test]
fn provenance_overlay_stays_silent_on_a_conflict_header() {
    use crate::provenance::Seat;
    let mut e = editor_with("<<<<<<< HEAD\na\n=======\nb\n>>>>>>> x");
    let _ = e.conflicts();
    e.provenance.record(0..1, Seat::Me);
    e.provenance_overlay = true;
    e.blame_enabled = true;
    e.cursor_row = 0;
    assert!(
        e.current_line_blame_annotation().is_none(),
        "the accept affordance still owns the header row"
    );
}

/// With the lens on, the inline-blame annotation names the seat that
/// typed the cursor's line (#349), and says nothing extra for a line
/// with no record.
#[test]
fn provenance_overlay_names_the_seat_in_the_blame_annotation() {
    use crate::provenance::Seat;
    let mut e = editor_with("one\ntwo\n");
    e.path = Some(PathBuf::from("f.rs"));
    e.set_blame(
        PathBuf::from("f.rs"),
        Some(blame_of(vec![
            blame_line("Vitali", "fix: the thing", 90, false),
            blame_line("Alice", "feat: two", 7200, false),
        ])),
    );
    e.provenance
        .record(0..1, Seat::Agent(String::from("pane 2")));
    e.cursor_row = 0;
    assert_eq!(
        e.current_line_blame_annotation().as_deref(),
        Some("Vitali, 1 minute ago • fix: the thing"),
        "off, the annotation is git's alone"
    );
    e.provenance_overlay = true;
    assert_eq!(
        e.current_line_blame_annotation().as_deref(),
        Some("Vitali, 1 minute ago • fix: the thing • agent (pane 2)"),
        "on, the seat joins it"
    );
    e.cursor_row = 1;
    assert_eq!(
        e.current_line_blame_annotation().as_deref(),
        Some("Alice, 2 hours ago • feat: two"),
        "a line with no record adds nothing"
    );
}

#[test]
fn git_gutter_render_paints_bar_in_the_spacer_cell() {
    use ratatui::buffer::Buffer;
    let mut e = editor_with("a\nb");
    e.set_git_head_lines(PathBuf::from("f.rs"), head(&["a", "DIFFERENT"]));
    let area = Rect {
        x: 0,
        y: 0,
        width: 30,
        height: 5,
    };
    let mut buf = Buffer::empty(area);
    (&mut e).render(area, &mut buf);
    // Line 1 (`b` vs `DIFFERENT`) is modified; the bar sits in the spacer
    // cell at `inner.x + gutter_width`, one cell left of the text.
    let bar_x = e.last_inner.x + e.last_gutter_width;
    let cell = &buf[(bar_x, e.last_inner.y + 1)];
    // Croft Loch (the default) draws the bar as a quarter block on the
    // cell's left edge; a boxed theme keeps the heavy centred bar.
    assert_eq!(
        cell.symbol(),
        "\u{258e}",
        "modified line shows a gutter bar"
    );
    e.theme = crate::theme::Theme::BLACK;
    let mut buf = Buffer::empty(area);
    (&mut e).render(area, &mut buf);
    assert_eq!(buf[(bar_x, e.last_inner.y + 1)].symbol(), "\u{2503}");
}

/// The gutter of a rendered `e` (#1576): each screen row's number
/// column, trimmed, and the cell style of each row's number.
fn gutter_column(e: &mut Editor, rows: u16) -> (Vec<String>, Vec<ratatui::style::Style>) {
    use ratatui::buffer::Buffer;
    let area = Rect::new(0, 0, 40, rows + 2);
    let mut buf = Buffer::empty(area);
    e.render(area, &mut buf);
    let (x0, x1) = (e.last_inner.x + 2, e.last_inner.x + e.last_gutter_width);
    let ys = e.last_inner.y..e.last_inner.y + rows;
    let text = ys
        .clone()
        .map(|y| (x0..x1).map(|x| buf[(x, y)].symbol()).collect::<String>())
        .map(|t| t.trim().to_string())
        .collect();
    let styles = ys.map(|y| buf[(x1 - 2, y)].style()).collect();
    (text, styles)
}

fn ten_lines() -> Editor {
    editor_with(
        &(1..=10)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// #1576: relative numbers count from the caret's line, which shows its
/// own number, as the issue's gutter "4 3 2 1 5 1 2 3 4 5" does.
#[test]
fn relative_line_numbers_count_from_the_caret() {
    let mut e = ten_lines();
    e.cursor_row = 4;
    e.line_numbers = LineNumbers::Relative;
    let (numbers, _) = gutter_column(&mut e, 10);
    assert_eq!(numbers, ["4", "3", "2", "1", "5", "1", "2", "3", "4", "5"]);
}

/// #1576: a collapsed fold counts as one line, as in Vim, so the
/// number after it is what `j` needs to reach it.
#[test]
fn relative_line_numbers_count_a_fold_as_one_line() {
    let mut e = editor_with("fn a() {\n    one\n    two\n}\nafter\nend");
    e.line_numbers = LineNumbers::Relative;
    e.toggle_fold(0);
    e.cursor_row = 0;
    assert_eq!(
        e.gutter_number(4),
        Some(2),
        "the brace line is one, `after` two"
    );
    assert_eq!(e.gutter_number(5), Some(3));
    e.cursor_row = 5;
    assert_eq!(e.gutter_number(0), Some(3), "upwards too");
}

/// #1576: interval numbers every tenth line and the caret's line; off
/// shows no numbers and narrows the gutter by the number column.
#[test]
fn interval_and_off_line_numbers() {
    let mut e = ten_lines();
    e.cursor_row = 2;
    e.line_numbers = LineNumbers::Interval;
    let (numbers, _) = gutter_column(&mut e, 10);
    assert_eq!(numbers, ["", "", "3", "", "", "", "", "", "", "10"]);
    let on_width = e.last_gutter_width;
    e.line_numbers = LineNumbers::Off;
    let (numbers, _) = gutter_column(&mut e, 10);
    assert!(numbers.iter().all(String::is_empty), "{numbers:?}");
    assert_eq!(e.last_gutter_width, on_width - 2, "two digits fewer");
}

/// #1576: the caret's line number, and each extra caret's, is in the
/// active colour; the others stay grey.
#[test]
fn the_caret_line_number_is_brighter() {
    let mut e = ten_lines();
    e.cursor_row = 4;
    e.carets.push(EditorSelection::new(7, 0));
    let (numbers, styles) = gutter_column(&mut e, 10);
    assert_eq!(numbers[4], "5", "absolute numbers by default");
    let active = Some(e.theme.line_number_active());
    assert_eq!(styles[4].fg, active);
    assert_eq!(styles[7].fg, active, "an extra caret's line too");
    assert_eq!(styles[3].fg, Some(e.theme.line_number_fg()));
    assert_ne!(active, Some(e.theme.line_number_fg()));
}

/// Croft Loch's gutter is in its own inks, not the host's named greys:
/// the faint tone for the numbers, body text for the caret's; a boxed
/// theme keeps the terminal's dark grey.
#[test]
fn a_surfaces_gutter_numbers_in_the_themes_inks() {
    let mut e = ten_lines();
    e.cursor_row = 4;
    let (_, styles) = gutter_column(&mut e, 10);
    let t = crate::widgets::surface::tones(e.theme).expect("Loch is the default");
    assert_eq!(styles[3].fg, Some(t.faint()));
    assert_eq!(styles[4].fg, Some(t.text()));
    let mut e = ten_lines();
    e.theme = crate::theme::Theme::BLACK;
    e.cursor_row = 4;
    let (_, styles) = gutter_column(&mut e, 10);
    assert_eq!(styles[3].fg, Some(Color::DarkGray));
}

/// Negative (#1576): the default is absolute numbers, and an unknown
/// pref falls back to it.
#[test]
fn line_numbers_default_to_on() {
    let mut e = ten_lines();
    e.cursor_row = 4;
    let (numbers, _) = gutter_column(&mut e, 10);
    assert_eq!(numbers, ["1", "2", "3", "4", "5", "6", "7", "8", "9", "10"]);
    assert_eq!(LineNumbers::from_pref(""), LineNumbers::On);
    assert_eq!(LineNumbers::from_pref("absolute"), LineNumbers::On);
    assert_eq!(LineNumbers::from_pref("relative"), LineNumbers::Relative);
    let mut mode = LineNumbers::On;
    let cycle: Vec<&str> = (0..4)
        .map(|_| {
            mode = mode.next();
            mode.pref_id()
        })
        .collect();
    assert_eq!(cycle, ["relative", "interval", "off", "on"]);
}

#[test]
fn save_on_a_preview_tab_never_touches_the_file() {
    // #185: the save choke point used to serialise the placeholder
    // text buffer over the previewed file, truncating a PNG to zero
    // bytes. Every preview kind must refuse at the choke point so no
    // caller (explicit save, force save, auto save, format-on-save)
    // can route around the guard.
    let tmp = tempfile::tempdir().unwrap();

    let png = tmp.path().join("pic.png");
    image::RgbaImage::new(2, 2).save(&png).unwrap();
    let png_bytes = std::fs::read(&png).unwrap();
    let mut e = Editor::new();
    e.open(&png).unwrap();
    assert!(e.image.is_some());
    assert!(e.save_to_disk().is_err(), "image tab save must refuse");
    assert!(e.save_to_disk_force().is_err(), "force save too");
    assert_eq!(std::fs::read(&png).unwrap(), png_bytes, "file intact");

    let bin = tmp.path().join("blob.bin");
    std::fs::write(&bin, b"\x00\x01\x02\x03").unwrap();
    let mut e = Editor::new();
    e.open(&bin).unwrap();
    assert!(e.hex.is_some());
    assert!(e.save_to_disk().is_err(), "hex tab save must refuse");
    assert_eq!(std::fs::read(&bin).unwrap(), b"\x00\x01\x02\x03");

    let csv = tmp.path().join("t.csv");
    std::fs::write(&csv, "a,b\n1,2\n").unwrap();
    let mut e = Editor::new();
    e.open(&csv).unwrap();
    assert!(e.sheet.is_some());
    assert!(e.save_to_disk().is_err(), "sheet tab save must refuse");
    assert_eq!(std::fs::read(&csv).unwrap(), b"a,b\n1,2\n");
}

#[test]
fn save_on_a_scratch_tab_writes_nothing_until_save_as() {
    // #951: a scratch tab's path is its label, so the choke point
    // refuses it for every caller, force save included. The label is
    // absolute here so a regression writes into the tempdir, not into
    // the test's working directory.
    let tmp = tempfile::tempdir().unwrap();
    let label = tmp.path().join("stats.py @ 4affdca");
    let mut tabs = EditorTabs::default();
    tabs.open_text_buffer(&label, "def mean(xs):\n").unwrap();
    tabs.insert_str("x");
    assert!(tabs.scratch && tabs.dirty);
    assert!(tabs.save_to_disk().is_err(), "explicit save refuses");
    assert!(tabs.auto_save_to_disk().is_err(), "auto save too");
    assert!(tabs.save_to_disk_force().is_err(), "and force save");
    assert!(!label.exists(), "nothing was written");
    let kept = tmp.path().join("stats.py");
    assert_eq!(tabs.save_as(&kept).unwrap(), SaveOutcome::Saved);
    assert!(!tabs.scratch, "the tab is that file now");
    let written = std::fs::read_to_string(&kept).unwrap();
    assert!(written.starts_with("xdef mean(xs):"), "{written:?}");
}

#[test]
fn a_real_file_opened_into_a_scratch_tab_saves() {
    // The tab stops being scratch once it holds a file: kept, the flag
    // refused the file's saves and let its edits close unasked.
    let tmp = tempfile::tempdir().unwrap();
    let real = tmp.path().join("query.ql");
    std::fs::write(&real, "select 1\n").unwrap();
    let mut tabs = EditorTabs::default();
    tabs.open_text_buffer(&tmp.path().join("Search: mean"), "hits\n")
        .unwrap();
    (*tabs).open(&real).unwrap();
    assert!(!tabs.scratch);
    tabs.insert_str("x");
    tabs.save_to_disk().expect("the file saves");
    assert_eq!(std::fs::read_to_string(&real).unwrap(), "xselect 1\n");
}
#[test]
fn is_binary_detects_nul() {
    assert!(is_binary(b"hello\0world"));
    assert!(!is_binary(b"hello world"));
    assert!(!is_binary(b""));
}

#[test]
fn binary_file_opens_in_the_hex_viewer_instead_of_erroring() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("blob.bin");
    std::fs::write(&p, b"\x00\x01\x02payload\xff\xfe").unwrap();
    let mut e = Editor::new();
    e.open(&p).unwrap();
    assert!(e.hex.is_some(), "binary files land in the hex viewer");
    assert_eq!(
        e.lines,
        vec![String::new()],
        "the whole-buffer-swap convention: one empty line, like image/sheet tabs"
    );
    assert_eq!(e.path.as_deref(), Some(p.as_path()));
    assert!(!e.dirty, "hex tabs are read-only");
}

#[test]
fn oversized_binary_opens_in_hex_but_oversized_text_still_errors() {
    // Only the head is sniffed, so the hex route must not read the
    // whole file — an over-limit BINARY opens fine (windowed IO),
    // while an over-limit TEXT file keeps the too-large error (the
    // text editor genuinely would have to load it whole).
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join("huge.bin");
    let f = std::fs::File::create(&bin).unwrap();
    f.set_len(MAX_FILE_BYTES + 5).unwrap(); // sparse: leading NULs = binary
    drop(f);
    let mut e = Editor::new();
    e.open(&bin).unwrap();
    assert!(e.hex.is_some(), "an over-limit binary opens in hex");

    let txt = tmp.path().join("huge.log");
    let f = std::fs::File::create(&txt).unwrap();
    use std::io::Write as _;
    let mut w = std::io::BufWriter::new(f);
    w.write_all(&vec![b'a'; 8192]).unwrap();
    drop(w);
    std::fs::OpenOptions::new()
        .append(true)
        .open(&txt)
        .unwrap()
        .set_len(MAX_FILE_BYTES + 5)
        .unwrap();
    let mut e2 = Editor::new();
    let err = e2.open(&txt).unwrap_err().to_string();
    assert!(
        err.contains("too large"),
        "text keeps the size guard: {err}"
    );
}

#[test]
fn hex_tab_renders_offsets_hex_bytes_ascii_and_status() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("blob.bin");
    let mut data = b"MZ\x90\x00".to_vec();
    data.extend_from_slice(b"Hello!\x00\xde\xad\xbe\xef");
    std::fs::write(&p, &data).unwrap();
    let mut ed = Editor::new();
    ed.open(&p).unwrap();
    assert!(ed.hex.is_some());
    let area = Rect {
        x: 0,
        y: 0,
        width: 92,
        height: 12,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut ed, area, &mut buf);
    let mut rows = Vec::new();
    for y in 0..area.height {
        let mut line = String::new();
        for x in 0..area.width {
            line.push_str(buf[(x, y)].symbol());
        }
        rows.push(line);
    }
    let all = rows.join("\n");
    assert!(all.contains("blob.bin"), "header names the file: {all}");
    assert!(all.contains("00000000"), "offset column paints: {all}");
    assert!(
        all.contains("4D 5A 90 00"),
        "hex bytes paint uppercase: {all}"
    );
    assert!(
        all.contains("MZ·"),
        "ascii gutter: printables verbatim, non-printables as ·: {all}"
    );
    assert!(
        all.contains("0x00000000"),
        "status row shows the cursor offset: {all}"
    );
    let view = ed.hex.as_ref().unwrap();
    assert_eq!(view.bytes_per_row, 16, "a 92-col pane fits 16 bytes/row");
    assert!(
        view.layout.data_rows > 0,
        "layout written back for mouse hit-testing"
    );
}

/// #1548: on High Contrast Light the hex editor's selected bytes are
/// black on the pale selection, not white; other themes keep white.
#[test]
fn hex_selected_bytes_use_a_readable_ink_on_high_contrast_light() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("blob.bin");
    std::fs::write(&p, (0u8..32).collect::<Vec<_>>()).unwrap();
    for (id, want) in [
        ("hc-light", Color::Rgb(0, 0, 0)),
        ("hc-black", Color::Rgb(0xff, 0xff, 0xff)),
        ("dark", Color::White),
    ] {
        let mut ed = Editor::new();
        ed.theme = crate::theme::Theme::from_id(id);
        ed.open(&p).unwrap();
        {
            let view = ed.hex.as_mut().unwrap();
            view.sel_anchor = Some(0);
            view.cursor = 3;
        }
        let area = Rect {
            x: 0,
            y: 0,
            width: 92,
            height: 12,
        };
        let mut buf = ratatui::buffer::Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut ed, area, &mut buf);
        let l = ed.hex.as_ref().unwrap().layout;
        // Byte 1 is selected and is not the cursor.
        let cell = &buf[(l.hex_x + 3, l.data_top)];
        assert_eq!(cell.symbol(), "0", "{id}: byte 1's hex cell");
        assert_eq!(cell.fg, want, "{id}: selected byte ink");
        assert_eq!(cell.bg, ed.theme.selection(), "{id}: selection fill");
    }
}

#[test]
fn hex_mouse_hit_test_round_trips_through_the_painted_layout() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("blob.bin");
    // 60 bytes: the last 16-byte row is PARTIAL, so its tail cells
    // genuinely sit past EOF.
    std::fs::write(&p, (0u8..60).collect::<Vec<_>>()).unwrap();
    let mut ed = Editor::new();
    ed.open(&p).unwrap();
    let area = Rect {
        x: 0,
        y: 0,
        width: 92,
        height: 12,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut ed, area, &mut buf);
    let view = ed.hex.as_ref().unwrap();
    let l = view.layout;
    // Row 1 (second data row), byte 3: base 16 + 3 = 19.
    let y = l.data_top + 1;
    assert_eq!(view.hit_test(l.hex_x + 3 * 3, y), Some(19), "hex cell");
    assert_eq!(view.hit_test(l.ascii_x + 3, y), Some(19), "ascii cell");
    // Byte 10 sits past the 8-byte group gap in the hex grid.
    assert_eq!(
        view.hit_test(l.hex_x + 10 * 3 + 1, y),
        Some(26),
        "post-gap hex cell accounts for the group divider"
    );
    // Cells past EOF, the offset gutter, and the header all miss.
    let last_row_y = l.data_top + 3;
    assert_eq!(
        view.hit_test(l.ascii_x + 15, last_row_y),
        None,
        "past EOF misses"
    );
    assert_eq!(view.hit_test(area.x + 1, y), None, "offset gutter misses");
    assert_eq!(
        view.hit_test(l.hex_x, l.data_top - 1),
        None,
        "header misses"
    );
}

/// Smallest xlsx calamine accepts: the four structural parts plus one
/// worksheet, stored uncompressed. Used by the content-routing tests.
fn write_minimal_xlsx(p: &Path) {
    use std::io::Write as _;
    let f = std::fs::File::create(p).unwrap();
    let mut z = zip::ZipWriter::new(f);
    let opts =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let parts: &[(&str, &str)] = &[
        (
            "[Content_Types].xml",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
</Types>"#,
        ),
        (
            "_rels/.rels",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"#,
        ),
        (
            "xl/workbook.xml",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
<sheets><sheet name="Data" sheetId="1" r:id="rId1"/></sheets>
</workbook>"#,
        ),
        (
            "xl/_rels/workbook.xml.rels",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
</Relationships>"#,
        ),
        (
            "xl/worksheets/sheet1.xml",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
<sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>hello</t></is></c><c r="B1"><v>42</v></c></row></sheetData>
</worksheet>"#,
        ),
    ];
    for (name, body) in parts {
        z.start_file(*name, opts).unwrap();
        z.write_all(body.as_bytes()).unwrap();
    }
    z.finish().unwrap();
}

#[test]
fn svg_opens_as_a_rendered_preview_and_reopen_as_text_edits_the_source() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("logo.svg");
    std::fs::write(
        &p,
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50">
<rect width="100" height="50" fill="#3366ff"/></svg>"##,
    )
    .unwrap();
    let mut e = Editor::new();
    e.open(&p).unwrap();
    let img = e.image.as_ref().expect("svg opens rendered");
    assert_eq!(img.format_label, "SVG");
    assert!(img.pixel_w > 0 && img.pixel_h > 0);
    assert!(
        image::load_from_memory(&img.bytes).is_ok(),
        "the overlay pipeline gets a decodable raster"
    );

    // Reopen as Text: the per-tab override lands in the XML source
    // and STICKS across the same-path reload behind FS sync.
    e.force_text = true;
    e.open(&p).unwrap();
    assert!(e.image.is_none());
    assert!(e.lines[0].contains("<svg"), "editing the source");
    e.open(&p).unwrap();
    assert!(
        e.image.is_none(),
        "the override survives a same-path reload (FS sync)"
    );
    // Reopen as Preview: clearing the override routes normally
    // again (the command's dispatch does exactly this).
    e.force_text = false;
    e.open(&p).unwrap();
    assert!(
        e.image.is_some(),
        "clearing the override restores the render"
    );
    // A DIFFERENT file drops the override: routing is per tab, not
    // per session.
    e.force_text = true;
    e.open(&p).unwrap();
    let q = tmp.path().join("other.svg");
    std::fs::copy(&p, &q).unwrap();
    e.open(&q).unwrap();
    assert!(e.image.is_some(), "a path change resets force_text");
}

/// #1008: the header names the SVG's own size. croft rasterises toward
/// a fixed long edge, so the raster's 1600×800 says nothing about a
/// 200×100 drawing.
#[test]
fn the_svg_header_shows_the_drawings_size_not_the_rasters() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("diagram.svg");
    std::fs::write(
        &p,
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100">
<rect width="200" height="100" fill="#0000ff"/></svg>"##,
    )
    .unwrap();
    let mut e = Editor::new();
    e.open(&p).unwrap();
    let img = e.image.as_ref().expect("svg opens rendered");
    assert_eq!((img.pixel_w, img.pixel_h), (1600, 800), "the raster");
    let area = Rect::new(0, 0, 60, 4);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    render_image_placeholder(
        img,
        Some(&p),
        area,
        &mut buf,
        Color::Reset,
        crate::theme::Theme::default(),
    );
    let header: String = (0..area.width)
        .map(|x| buf[(x, 0)].symbol().to_string())
        .collect();
    assert!(header.contains("diagram.svg · 200×100 ·"), "{header}");
    assert!(!header.contains("1600"), "{header}");
    assert_eq!(css_px_size((37.795_277, 96.0)), "37.8×96");
    assert_eq!(css_px_size((199.98, 0.25)), "200×0.3");
}

/// #257: a colour-bearing log opens rendered, escapes never reach the
/// text, and "Reopen as Text" round-trips back to the raw bytes.
#[test]
fn a_color_log_opens_rendered_and_reopen_as_text_round_trips() {
    let tmp = tempfile::TempDir::new().unwrap();
    let p = tmp.path().join("build.log");
    std::fs::write(
        &p,
        "plain\n\u{1b}[31mERROR\u{1b}[0m boom\n\u{1b}[32mok\u{1b}[0m\n",
    )
    .unwrap();
    let mut e = Editor::new();
    e.open(&p).unwrap();

    let log = e.log.as_mut().expect("a colour log opens rendered");
    assert_eq!(log.len(), 3);
    log.ensure(0, 3).unwrap();
    assert_eq!(
        log.visible_text(1),
        Some("ERROR boom"),
        "escapes are stripped from what the user sees"
    );
    assert_eq!(
        log.line(1).unwrap().spans[0].style.fg,
        Some(crate::ansi_text::AnsiColor::Indexed(1)),
        "the theme palette resolves the colour, so it stays symbolic"
    );

    // Reopen as Text: raw escapes, and the override sticks across the
    // same-path reload the FS sweep performs.
    e.force_text = true;
    e.open(&p).unwrap();
    assert!(e.log.is_none());
    assert!(
        e.lines[1].contains('\u{1b}'),
        "the text view shows the real bytes"
    );
    e.open(&p).unwrap();
    assert!(e.log.is_none(), "the override survives a same-path reload");
    e.force_text = false;
    e.open(&p).unwrap();
    assert!(e.log.is_some(), "clearing the override renders again");
}

/// #257: the find highlight must reach the rendered log. The search runs
/// on the stripped text, and the renderer paints spans from that same
/// text, so the highlight columns land on the match the user searched
/// for rather than being shifted by the escapes in the raw bytes.
#[test]
fn the_find_highlight_paints_over_a_rendered_log_line() {
    use ratatui::buffer::Buffer;
    let tmp = tempfile::TempDir::new().unwrap();
    let p = tmp.path().join("build.log");
    // The escape before `boom` would shift the columns by five if the
    // highlight were computed against the raw bytes.
    std::fs::write(&p, "plain\n\u{1b}[31mboom\u{1b}[0m tail\n").unwrap();
    let mut e = Editor::new();
    // The boxed themes' orange active match; the columns are the point.
    e.theme = crate::theme::Theme::BLACK;
    e.open(&p).unwrap();
    e.set_search_highlight(Some(String::from("boom")));
    e.active_search_match = Some((1, 0, 4));

    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 6,
    };
    let mut buf = Buffer::empty(area);
    (&mut e).render(area, &mut buf);

    // Body row 1 is file line 1; the log body starts below the header.
    let y = e.last_inner.y + 2;
    let cell = &buf[(e.last_inner.x, y)];
    assert_eq!(cell.symbol(), "b", "the match starts at column 0");
    assert_eq!(
        cell.style().bg,
        Some(Color::Rgb(0xff, 0x8c, 0x2a)),
        "the ACTIVE match paints orange over the log's own colour"
    );
    let after = &buf[(e.last_inner.x + 4, y)];
    assert_ne!(
        after.style().bg,
        Some(Color::Rgb(0xff, 0x8c, 0x2a)),
        "the highlight stops at the end of the match"
    );
}

/// The sniff must not hijack ordinary files: a plain log routes to text,
/// while ANSI inside a `.txt` still renders. Extension alone is not the
/// signal, and neither is the mere presence of an escape byte.
#[test]
fn only_files_carrying_sgr_sequences_route_to_the_log_view() {
    let tmp = tempfile::TempDir::new().unwrap();
    let plain = tmp.path().join("plain.log");
    std::fs::write(&plain, "no colours here\njust text\n").unwrap();
    let mut e = Editor::new();
    e.open(&plain).unwrap();
    assert!(
        e.log.is_none(),
        "a log without colours is an ordinary editable file"
    );

    let txt = tmp.path().join("pytest.txt");
    std::fs::write(&txt, "\u{1b}[32mPASSED\u{1b}[0m\n").unwrap();
    e.open(&txt).unwrap();
    assert!(e.log.is_some(), "the sniff catches ANSI under a .txt name");

    // A screen recording (cursor movement, no SGR) is not a colour log.
    let rec = tmp.path().join("session.log");
    std::fs::write(&rec, "\u{1b}[2J\u{1b}[10;1Hmoved\n").unwrap();
    e.open(&rec).unwrap();
    assert!(e.log.is_none(), "cursor movement alone does not render");
}

/// The fg of the highlight span covering byte `at` of line `row`.
fn fg_at(e: &Editor, row: usize, at: usize) -> Option<Color> {
    e.highlights
        .get(row)?
        .iter()
        .find(|s| s.range().contains(&at))
        .and_then(|s| s.style.fg)
}

/// #1537: a plain `.log` with no escapes stays an editable text tab but
/// reads as Log and wears tailspin's colours: ERROR is not INFO.
#[test]
fn a_plain_log_gets_log_colours_in_the_text_editor() {
    let _exclusive = crate::log_view::DEFAULT_HIGHLIGHT_TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let before = crate::log_view::default_highlight();
    crate::log_view::set_default_highlight(true);
    let tmp = tempfile::TempDir::new().unwrap();
    let p = tmp.path().join("app.log");
    std::fs::write(
        &p,
        "2026-10-08T12:00:01Z ERROR worker-1 took 120ms\n2026-10-08T12:00:02Z INFO worker-2 ok\n",
    )
    .unwrap();
    let mut e = Editor::new();
    e.open(&p).unwrap();
    crate::log_view::set_default_highlight(before);
    assert!(e.log.is_none(), "still the editable text tab");
    assert_eq!(e.language_label(), "Log");
    let error = fg_at(&e, 0, e.lines[0].find("ERROR").unwrap());
    let info = fg_at(&e, 1, e.lines[1].find("INFO").unwrap());
    assert!(error.is_some(), "ERROR is coloured: {:?}", e.highlights[0]);
    assert!(info.is_some(), "INFO is coloured: {:?}", e.highlights[1]);
    assert_ne!(error, info, "and ERROR does not look like INFO");
    assert!(
        fg_at(&e, 0, 0).is_some(),
        "the timestamp is coloured too: {:?}",
        e.highlights[0]
    );
    let plain = fg_at(&e, 0, e.lines[0].find("worker").unwrap());
    assert!(plain.is_none(), "a bare word keeps the editor's colour");
}

/// #1537: a line typed into a plain log is coloured like the rest, and
/// one whose text changed loses the spans of the text it replaced.
#[test]
fn an_edit_to_a_plain_log_recolours_the_edited_line() {
    let _exclusive = crate::log_view::DEFAULT_HIGHLIGHT_TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let before = crate::log_view::default_highlight();
    crate::log_view::set_default_highlight(true);
    let tmp = tempfile::TempDir::new().unwrap();
    let p = tmp.path().join("app.log");
    std::fs::write(&p, "ERROR one\n").unwrap();
    let mut e = Editor::new();
    e.open(&p).unwrap();
    assert!(fg_at(&e, 0, 0).is_some(), "ERROR is coloured on open");
    e.cursor_row = 0;
    e.cursor_col = 0;
    for c in "xx ".chars() {
        e.insert_char(c);
    }
    let shifted = fg_at(&e, 0, 3);
    e.cursor_col = 12;
    e.insert_newline();
    for c in "WARN two".chars() {
        e.insert_char(c);
    }
    crate::log_view::set_default_highlight(before);
    assert_eq!(e.lines, vec!["xx ERROR one", "WARN two"]);
    assert!(shifted.is_some(), "ERROR keeps its colour where it moved");
    assert!(fg_at(&e, 0, 0).is_none(), "the typed prefix is plain");
    assert!(fg_at(&e, 1, 0).is_some(), "the typed WARN line is coloured");
}

/// #1537 negatives: the colours belong to `.log` files, and to the
/// "Log: Highlighting (tailspin)" setting. The same lines under a
/// `.txt` name, or with the setting off, stay plain.
#[test]
fn only_a_log_with_highlighting_on_gets_log_colours() {
    let _exclusive = crate::log_view::DEFAULT_HIGHLIGHT_TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let before = crate::log_view::default_highlight();
    let tmp = tempfile::TempDir::new().unwrap();
    let body = "2026-10-08T12:00:01Z ERROR worker-1 took 120ms\n";
    let txt = tmp.path().join("notes.txt");
    std::fs::write(&txt, body).unwrap();
    let log = tmp.path().join("app.log");
    std::fs::write(&log, body).unwrap();

    crate::log_view::set_default_highlight(true);
    let mut t = Editor::new();
    t.open(&txt).unwrap();
    assert_eq!(t.language_label(), "Plain Text");
    assert!(t.highlights.iter().all(Vec::is_empty), "a .txt is plain");

    crate::log_view::set_default_highlight(false);
    let mut l = Editor::new();
    l.open(&log).unwrap();
    assert_eq!(l.language_label(), "Log");
    assert!(
        l.highlights.iter().all(Vec::is_empty),
        "the setting off leaves the log plain"
    );
    crate::log_view::set_default_highlight(true);
    l.refresh_log_highlight();
    assert!(fg_at(&l, 0, 0).is_some(), "and on again colours it");
    crate::log_view::set_default_highlight(before);
}

/// #1537: a line that would need its text changed to colour it gets no
/// spans rather than spans at the wrong offsets.
#[test]
fn plain_line_spans_cover_the_raw_text_only() {
    let line = "2026-10-08 ERROR id=7";
    let spans = crate::log_view::plain_line_spans(line);
    assert!(!spans.is_empty());
    assert!(spans.iter().all(|s| s.end <= line.len() && s.start < s.end));
    assert!(crate::log_view::plain_line_spans("").is_empty());
    assert!(crate::log_view::plain_line_spans("just words here").is_empty());
    assert!(
        crate::log_view::plain_line_spans("\u{1b}[31mERROR\u{1b}[0m").is_empty(),
        "a line with its own escapes is not re-coloured"
    );
}

/// #257 acceptance: a log far past the text size guard still opens,
/// because the view is windowed rather than loaded whole. Under the old
/// route this exact file was a "File too large" dead end.
#[test]
fn a_huge_color_log_opens_rendered_instead_of_hitting_the_size_guard() {
    let tmp = tempfile::TempDir::new().unwrap();
    let p = tmp.path().join("huge-color.log");
    // Real colour bytes up front so the sniff fires, then a sparse tail
    // that pushes the file well past MAX_FILE_BYTES without writing it.
    std::fs::write(&p, "\u{1b}[31mERROR\u{1b}[0m first line\n").unwrap();
    let f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
    f.set_len(MAX_FILE_BYTES + 4096).unwrap();
    drop(f);

    let mut e = Editor::new();
    e.open(&p).expect("a windowed log open has no size ceiling");
    let log = e.log.as_mut().expect("opens rendered, not refused");
    assert!(log.file_len > MAX_FILE_BYTES);
    log.ensure(0, 1).unwrap();
    assert_eq!(
        log.visible_text(0),
        Some("ERROR first line"),
        "the first window parses without reading the whole file"
    );
}

/// #257 acceptance: the view must scroll past its first screen. It could
/// not before — `scroll_view_to` clamped against `lines`, which is a
/// one-line stub for a log, pinning scroll at 0 — so the windowing this
/// feature exists for was unreachable in the shipped UI.
#[test]
fn a_rendered_log_scrolls_past_the_first_screen() {
    let tmp = tempfile::TempDir::new().unwrap();
    let p = tmp.path().join("big.log");
    let mut body = String::from("\u{1b}[31mstart\u{1b}[0m\n");
    for i in 1..2_000 {
        body.push_str(&format!("line{i}\n"));
    }
    std::fs::write(&p, body).unwrap();

    let mut e = Editor::new();
    e.open(&p).unwrap();
    let area = Rect::new(0, 0, 40, 12);
    let mut buf = Buffer::empty(area);
    e.render(area, &mut buf);
    assert_eq!(e.scroll, 0);

    e.scroll_down(500);
    assert_eq!(e.scroll, 500, "the log scrolls by file line");
    e.render(area, &mut buf);
    let total = {
        let log = e.log.as_ref().unwrap();
        assert_eq!(
            log.visible_text(500),
            Some("line500"),
            "and the window follows the viewport"
        );
        log.len()
    };

    // Clamped at the tail rather than scrolling into blank space.
    e.scroll_down(usize::MAX / 2);
    assert!(e.scroll < total, "scroll stays inside the file");
    e.scroll_up(usize::MAX / 2);
    assert_eq!(e.scroll, 0, "and back to the top");
}

/// A rendered log of `n` lines (`line1` .. `line{n}`) behind a colour
/// escape, so it routes to the log view, opened and painted once.
fn open_rendered_log(dir: &Path, n: usize) -> (Editor, PathBuf, Rect) {
    let p = dir.join("app.log");
    let mut body = String::from("\u{1b}[32mINFO\u{1b}[0m start\n");
    for i in 1..=n {
        body.push_str(&format!("line{i}\n"));
    }
    std::fs::write(&p, body).unwrap();
    let mut e = Editor::new();
    e.open(&p).unwrap();
    assert!(e.log.is_some(), "the fixture opens as a rendered log");
    let area = Rect::new(0, 0, 40, 12);
    let mut buf = Buffer::empty(area);
    e.render(area, &mut buf);
    (e, p, area)
}

fn append_log_lines(p: &Path, from: usize, to: usize) {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().append(true).open(p).unwrap();
    for i in from..=to {
        writeln!(f, "line{i}").unwrap();
    }
}

/// The rows the log body shows under its header.
fn log_body_rows(e: &Editor) -> usize {
    (e.last_inner.height as usize).saturating_sub(1).max(1)
}

/// #1538: a log that grew on disk reloaded with the view back on line 1,
/// so a reader at line 240 lost their place on every append.
#[test]
fn a_grown_log_keeps_its_scroll_through_an_external_reload() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (mut e, p, _) = open_rendered_log(tmp.path(), 2_000);
    e.scroll_down(240);
    assert_eq!(e.scroll, 240);
    append_log_lines(&p, 2_001, 2_010);
    assert_eq!(e.reload_or_flag_conflict(), ExternalChange::Reloaded);
    assert_eq!(
        e.log.as_ref().unwrap().len(),
        2_011,
        "the reload saw the append"
    );
    assert_eq!(e.scroll, 240, "the reader stays where they were");
}

/// #1538: a view at the end of the log follows it, so new lines come into
/// view on each reload the way `tail -f` shows them.
#[test]
fn a_log_viewed_at_its_end_follows_new_lines_on_reload() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (mut e, p, area) = open_rendered_log(tmp.path(), 2_000);
    e.scroll_down(usize::MAX / 2);
    let rows = log_body_rows(&e);
    assert_eq!(e.scroll + rows, 2_001, "the view is on the last line");
    append_log_lines(&p, 2_001, 2_005);
    assert_eq!(e.reload_or_flag_conflict(), ExternalChange::Reloaded);
    assert_eq!(e.scroll + rows, 2_006, "the new last line is in view");
    let mut buf = Buffer::empty(area);
    e.render(area, &mut buf);
    let text: String = buf.content().iter().map(|c| c.symbol()).collect();
    assert!(text.contains("line2005"), "and painted");
}

/// #1538: a log too large to index on open (past the synchronous head)
/// still follows its end: the place is settled again as the background
/// index catches up, rather than clamped to the head's line count.
#[test]
fn a_large_log_viewed_at_its_end_follows_it_once_the_index_catches_up() {
    let tmp = tempfile::TempDir::new().unwrap();
    let lines = (crate::log_view::HEAD_INDEX_BYTES as usize) / 9 + 50_000;
    let (mut e, p, _) = open_rendered_log(tmp.path(), lines);
    e.log.as_mut().unwrap().finish_index();
    e.scroll_down(usize::MAX / 2);
    let rows = log_body_rows(&e);
    assert_eq!(e.scroll + rows, lines + 1);
    append_log_lines(&p, lines + 1, lines + 3);
    assert_eq!(e.reload_or_flag_conflict(), ExternalChange::Reloaded);
    assert!(
        e.log.as_ref().unwrap().indexing(),
        "the tail indexes in the background"
    );
    e.log.as_mut().unwrap().finish_index();
    e.settle_log_reload();
    assert_eq!(e.scroll + rows, lines + 4, "the view lands on the new end");
}

/// Negative: a reader who scrolls while the reload's index is still
/// running keeps the place they scrolled to; the settle does not drag
/// them back to where the reload left them.
#[test]
fn scrolling_during_a_reload_index_cancels_the_pending_place() {
    let tmp = tempfile::TempDir::new().unwrap();
    let lines = (crate::log_view::HEAD_INDEX_BYTES as usize) / 9 + 50_000;
    let (mut e, p, _) = open_rendered_log(tmp.path(), lines);
    e.log.as_mut().unwrap().finish_index();
    e.scroll_down(usize::MAX / 2);
    append_log_lines(&p, lines + 1, lines + 3);
    assert_eq!(e.reload_or_flag_conflict(), ExternalChange::Reloaded);
    e.scroll_view_to(10);
    e.log.as_mut().unwrap().finish_index();
    e.settle_log_reload();
    assert_eq!(e.scroll, 10, "the reader's own scroll wins");
}

/// #1538: a second change to a large log while the first reload is still
/// indexing keeps following the end, rather than pinning the view to the
/// scroll the first reload's partial index clamped it to.
#[test]
fn a_second_reload_during_indexing_keeps_following_the_end() {
    let tmp = tempfile::TempDir::new().unwrap();
    let lines = (crate::log_view::HEAD_INDEX_BYTES as usize) / 9 + 50_000;
    let (mut e, p, _) = open_rendered_log(tmp.path(), lines);
    e.log.as_mut().unwrap().finish_index();
    e.scroll_down(usize::MAX / 2);
    let rows = log_body_rows(&e);
    append_log_lines(&p, lines + 1, lines + 3);
    assert_eq!(e.reload_or_flag_conflict(), ExternalChange::Reloaded);
    assert!(e.log.as_ref().unwrap().indexing());
    append_log_lines(&p, lines + 4, lines + 6);
    assert_eq!(e.reload_or_flag_conflict(), ExternalChange::Reloaded);
    assert!(e.log.as_ref().unwrap().indexing());
    e.log.as_mut().unwrap().finish_index();
    e.settle_log_reload();
    assert_eq!(
        e.scroll + rows,
        lines + 7,
        "the view lands on the newest end"
    );
}

/// Negative: a log truncated under the view (rotation, `> app.log`)
/// clamps to what is left instead of scrolling into blank rows, and one
/// shorter than a screen goes back to the top.
#[test]
fn a_truncated_log_clamps_its_scroll_on_reload() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (mut e, p, _) = open_rendered_log(tmp.path(), 2_000);
    let rows = log_body_rows(&e);
    e.scroll_down(1_500);
    let mut body = String::from("\u{1b}[32mINFO\u{1b}[0m start\n");
    for i in 1..=99 {
        body.push_str(&format!("line{i}\n"));
    }
    std::fs::write(&p, &body).unwrap();
    assert_eq!(e.reload_or_flag_conflict(), ExternalChange::Reloaded);
    assert_eq!(e.log.as_ref().unwrap().len(), 100);
    assert_eq!(e.scroll, 100 - rows, "clamped to the last screen");

    std::fs::write(&p, "\u{1b}[32mINFO\u{1b}[0m rotated\n").unwrap();
    assert_eq!(e.reload_or_flag_conflict(), ExternalChange::Reloaded);
    assert_eq!(
        e.scroll, 0,
        "a log shorter than a screen shows from the top"
    );
}

/// A reload keeps the reader's selection and colour toggle, and drops a
/// selection that no longer points into the file.
#[test]
fn a_log_reload_keeps_the_selection_and_highlight_toggle() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (mut e, p, _) = open_rendered_log(tmp.path(), 2_000);
    let flipped = !e.log.as_ref().unwrap().highlight();
    {
        let log = e.log.as_mut().unwrap();
        log.set_highlight(flipped);
        log.selection = Some(((10, 0), (12, 3)));
    }
    append_log_lines(&p, 2_001, 2_002);
    assert_eq!(e.reload_or_flag_conflict(), ExternalChange::Reloaded);
    let log = e.log.as_ref().unwrap();
    assert_eq!(log.highlight(), flipped);
    assert_eq!(log.selection, Some(((10, 0), (12, 3))));

    e.log.as_mut().unwrap().selection = Some(((1_900, 0), (1_950, 2)));
    std::fs::write(&p, "\u{1b}[32mINFO\u{1b}[0m rotated\nline1\n").unwrap();
    assert_eq!(e.reload_or_flag_conflict(), ExternalChange::Reloaded);
    assert_eq!(e.log.as_ref().unwrap().selection, None, "past the new end");
}

/// Colours resolve through the ACTIVE theme's ANSI palette, so a rendered
/// log matches the terminal panes rather than a hardcoded table.
#[test]
fn rendered_log_paints_colors_from_the_theme_palette() {
    let tmp = tempfile::TempDir::new().unwrap();
    let p = tmp.path().join("c.log");
    std::fs::write(&p, "\u{1b}[31mRED\u{1b}[0m\n").unwrap();
    let mut e = Editor::new();
    e.open(&p).unwrap();

    let area = Rect::new(0, 0, 40, 4);
    let mut buf = Buffer::empty(area);
    e.render(area, &mut buf);

    let (r, g, b) = e.theme.ansi()[1];
    // Find the painted "RED" rather than assuming where the frame and
    // header put it.
    let mut found = None;
    for y in 0..area.height {
        for x in 0..area.width {
            if buf[(x, y)].symbol() == "R" && buf[(x, y)].style().fg == Some(Color::Rgb(r, g, b)) {
                found = Some((x, y));
            }
        }
    }
    let (x, y) = found.expect("the red R paints somewhere in the body");
    assert_eq!(buf[(x + 1, y)].symbol(), "E", "the stripped text paints");
    assert_eq!(
        buf[(x + 1, y)].style().fg,
        Some(Color::Rgb(r, g, b)),
        "SGR 31 resolves through the theme's ANSI slot 1 across the span"
    );
}

/// SGR 9 reaches the painter as CROSSED_OUT, so a pane's struck-through
/// cells stay struck through in a rendered log.
#[test]
fn rendered_log_paints_strikeout() {
    let tmp = tempfile::TempDir::new().unwrap();
    let p = tmp.path().join("s.log");
    std::fs::write(&p, "\u{1b}[9mSTRUCK\u{1b}[0m plain\n").unwrap();
    let mut e = Editor::new();
    e.open(&p).unwrap();
    assert!(e.log.is_some(), "the fixture opens rendered");
    let area = Rect::new(0, 0, 40, 4);
    let mut buf = Buffer::empty(area);
    e.render(area, &mut buf);
    let mut struck = None;
    for y in 0..area.height {
        for x in 0..area.width {
            if buf[(x, y)].symbol() == "S"
                && buf[(x, y)]
                    .style()
                    .add_modifier
                    .contains(Modifier::CROSSED_OUT)
            {
                struck = Some((x, y));
            }
        }
    }
    let (x, y) = struck.expect("the struck S paints somewhere in the body");
    assert!(
        !buf[(x + 7, y)]
            .style()
            .add_modifier
            .contains(Modifier::CROSSED_OUT),
        "the reset ends the run before `plain`"
    );
}

#[test]
fn broken_svg_falls_through_to_the_text_editor() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("broken.svg");
    std::fs::write(&p, "this is not xml").unwrap();
    let mut e = Editor::new();
    e.open(&p).unwrap();
    assert!(e.image.is_none());
    assert_eq!(e.lines, vec!["this is not xml".to_string()]);
}

#[test]
fn extensionless_png_routes_to_the_image_preview_by_content() {
    let tmp = tempfile::tempdir().unwrap();
    let png = tmp.path().join("pic.png");
    image::RgbaImage::new(2, 2).save(&png).unwrap();
    let bare = tmp.path().join("logo");
    std::fs::copy(&png, &bare).unwrap();
    let mut e = Editor::new();
    e.open(&bare).unwrap();
    assert!(
        e.image.is_some(),
        "content routing must recognise the PNG without an extension"
    );
    assert!(e.hex.is_none());
}

#[test]
fn misnamed_image_extension_falls_through_to_the_real_content() {
    let tmp = tempfile::tempdir().unwrap();
    // Plain text wearing .png: the decode failure must fall through
    // to the text editor, not fail the whole open.
    let fake = tmp.path().join("notes.png");
    std::fs::write(&fake, "just words\n").unwrap();
    let mut e = Editor::new();
    e.open(&fake).unwrap();
    assert!(e.image.is_none());
    assert_eq!(e.lines, vec!["just words".to_string()]);

    // Binary garbage wearing .png: decode fails, content is binary,
    // the hex fallback catches it — never a dead-end.
    let junk = tmp.path().join("junk.png");
    std::fs::write(&junk, b"\x00\x01\x02\x03garbage").unwrap();
    let mut e = Editor::new();
    e.open(&junk).unwrap();
    assert!(e.hex.is_some(), "misnamed binary lands in hex");
}

#[test]
fn extensionless_workbook_routes_to_the_sheet_viewer_by_content() {
    let tmp = tempfile::tempdir().unwrap();
    let bare = tmp.path().join("report");
    write_minimal_xlsx(&bare);
    let mut e = Editor::new();
    e.open(&bare).unwrap();
    let sheet = e
        .sheet
        .as_ref()
        .expect("zip content routed to the sheet viewer");
    assert_eq!(sheet.kind, crate::sheet::SheetKind::Xlsx);
    let data = &sheet.sheets[0];
    let all: String = data
        .headers
        .iter()
        .chain(data.rows.iter().flatten())
        .cloned()
        .collect::<Vec<_>>()
        .join(",");
    assert!(all.contains("hello") && all.contains("42"), "cells: {all}");
}

/// #1002: the grid shows a SQL NULL as a dimmed `NULL`, apart from an
/// empty string, and a cell's newline and tab as `⏎` and `→` instead of
/// dropping them.
#[test]
fn the_sheet_grid_shows_null_and_control_characters() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("shop.db");
    rusqlite::Connection::open(&p)
        .unwrap()
        .execute_batch(
            "CREATE TABLE c (country TEXT, notes TEXT);
                 INSERT INTO c VALUES (NULL, 'a' || char(10) || 'b' || char(9) || 'c'), ('', 'x');",
        )
        .unwrap();
    let mut view = crate::sqlite_view::open_database(&p).unwrap();
    // The cursor sits on row 2's first cell, so row 1 draws plainly.
    view.sheets[0].cur_row = 1;
    let area = Rect::new(0, 0, 40, 6);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    render_sheet(
        &mut view,
        Some(&p),
        area,
        &mut buf,
        Color::Reset,
        crate::theme::Theme::default(),
    );
    let row = |y: u16| -> String { (0..area.width).map(|x| buf[(x, y)].symbol()).collect() };
    // Header pill, column headers, then the body.
    let first = row(2);
    assert!(first.contains("NULL"), "{first}");
    assert!(first.contains("a\u{23ce}b\u{2192}c"), "{first}");
    let null_x = first.find("NULL").unwrap() as u16;
    let cell = &buf[(null_x, 2)];
    assert!(
        cell.modifier.contains(Modifier::ITALIC),
        "NULL is styled apart"
    );
    assert_eq!(cell.fg, Color::DarkGray);
    assert!(!row(3).contains("NULL"), "'' stays empty: {}", row(3));
}

/// #1138: CJK, Hangul and emoji take two columns each. The grid sized
/// and filled cells in chars, so `서울특별시` showed as `서울특` and ran
/// into the next column (`大阪府2750000`).
#[test]
fn wide_characters_fit_their_column_and_keep_the_gap() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("cities.csv");
    std::fs::write(
        &p,
        "city,pref,pop\n東京都,東京,13960000\n大阪市,大阪府,2750000\n\
             Seoul,서울특별시,9500000\nemoji 🎉🎉🎉,x,1\n",
    )
    .unwrap();
    let mut e = Editor::new();
    e.open(&p).unwrap();
    let view = e.sheet.as_mut().unwrap();
    // The caret sits on the header-less last row, so the rows above
    // draw plainly.
    view.sheets[0].cur_row = 3;
    let area = Rect::new(0, 0, 60, 8);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    render_sheet(
        view,
        Some(&p),
        area,
        &mut buf,
        Color::Reset,
        crate::theme::Theme::default(),
    );
    // What a terminal shows on row `y`: a wide glyph's covered cell is
    // skipped, as the terminal draws the glyph over it.
    let row = |y: u16| -> String {
        let mut out = String::new();
        let mut x = 0;
        while x < area.width {
            let sym = buf[(x, y)].symbol();
            out.push_str(sym);
            x += unicode_width::UnicodeWidthStr::width(sym).max(1) as u16;
        }
        out
    };
    let screen: Vec<String> = (0..area.height).map(row).collect();
    let body = screen.join("\n");
    for whole in ["東京都", "大阪府", "서울특별시", "emoji 🎉🎉🎉"] {
        assert!(body.contains(whole), "{whole} is drawn whole:\n{body}");
    }
    assert!(
        !body.contains("大阪府2750000"),
        "a gap between columns:\n{body}"
    );
    assert!(
        !body.contains("시9500000"),
        "a gap between columns:\n{body}"
    );
    // Every value of a column starts at the same screen column.
    let col_of = |needle: &str| -> usize {
        let line = screen.iter().find(|l| l.contains(needle)).unwrap();
        let byte = line.find(needle).unwrap();
        unicode_width::UnicodeWidthStr::width(&line[..byte])
    };
    assert_eq!(col_of("13960000"), col_of("2750000"));
    assert_eq!(col_of("2750000"), col_of("9500000"));
    assert_eq!(col_of("大阪府"), col_of("서울특별시"));
}

/// #1138: the in-grid editor windows its text in screen columns too. A
/// value of wide characters longer than the column shows its tail with
/// the caret inside the slot, not twice the slot running over the next
/// column.
#[test]
fn the_in_grid_editor_keeps_wide_text_inside_its_column() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("cities.csv");
    std::fs::write(&p, "city,pref,pop\nSeoul,서울특별시,9500000\nx,y,1\n").unwrap();
    let mut e = Editor::new();
    e.open(&p).unwrap();
    let view = e.sheet.as_mut().unwrap();
    let value = "서울특별시서울특별시".to_string();
    view.sheets[0].cur_row = 0;
    view.sheets[0].cur_col = 1;
    view.editing = Some(crate::sheet::CellEdit {
        cursor: value.len(),
        value,
    });
    let area = Rect::new(0, 0, 50, 6);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    render_sheet(
        view,
        Some(&p),
        area,
        &mut buf,
        Color::Reset,
        crate::theme::Theme::default(),
    );
    let line = (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
        })
        .find(|l| l.contains("Seoul"))
        .unwrap();
    assert!(
        line.contains("9500000"),
        "the next column is intact: {line:?}"
    );
    // The tail that fits: 4 of the 10 glyphs (8 columns), then the caret.
    assert!(line.contains("울 특 별 시"), "{line:?}");
    assert!(!line.contains("서 울 특 별 시 서"), "{line:?}");
}

/// #1138 review: 20 CJK characters make a 40-column-wide column. In a
/// pane narrower than that, the grid drew no column at all; it now
/// draws that one clipped to the pane.
#[test]
fn a_column_wider_than_a_narrow_pane_is_drawn_clipped() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("long.csv");
    let long = "東京都".repeat(7);
    std::fs::write(&p, format!("name,n\n{long},1\nx,2\n")).unwrap();
    let mut e = Editor::new();
    e.open(&p).unwrap();
    let view = e.sheet.as_mut().unwrap();
    view.sheets[0].cur_row = 1;
    let area = Rect::new(0, 0, 30, 6);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    render_sheet(
        view,
        Some(&p),
        area,
        &mut buf,
        Color::Reset,
        crate::theme::Theme::default(),
    );
    let body = (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(body.contains("name"), "the header is drawn:\n{body}");
    assert!(
        body.contains("東 京 都 東 京 都"),
        "the value, clipped:\n{body}"
    );
}

#[test]
fn a_wide_glyph_that_would_straddle_the_cell_edge_is_left_out() {
    let area = Rect::new(0, 0, 6, 1);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    buf.set_string(0, 0, "zzzzzz", Style::default());
    // Three columns hold one wide glyph; the second would need 4.
    write_cell(&mut buf, 0, 0, 3, "表表", Style::default());
    assert_eq!(buf[(0, 0)].symbol(), "表");
    assert_eq!(buf[(2, 0)].symbol(), " ", "the edge column is blank");
    assert_eq!(
        buf[(3, 0)].symbol(),
        "z",
        "nothing past the slot is touched"
    );
}

#[test]
fn oversized_csv_keeps_its_size_refusal_instead_of_silently_opening() {
    // #188 review: the parse-failure fallthrough must not swallow
    // the sheet viewer's SIZE refusal — pre-#174 an over-cap CSV
    // errored loudly, and rerouting it into the text/binary path
    // silently bypassed the cap.
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("big.csv");
    let f = std::fs::File::create(&p).unwrap();
    f.set_len(26 * 1024 * 1024).unwrap(); // sparse: over the 25MB cap
    drop(f);
    let mut e = Editor::new();
    let err = e.open(&p).unwrap_err().to_string();
    assert!(
        err.contains("too large"),
        "size refusal must surface: {err}"
    );
    assert!(e.sheet.is_none() && e.hex.is_none());
}

#[test]
fn workbook_misnamed_as_csv_still_reaches_the_sheet_viewer() {
    // #188 review: a real xlsx wearing .csv fails the CSV parse and
    // falls through; the zip retry guard must only skip the ONE
    // route that already ran (.xlsx), not every sheet extension.
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("data.csv");
    write_minimal_xlsx(&p);
    let mut e = Editor::new();
    e.open(&p).unwrap();
    let sheet = e.sheet.as_ref().expect("zip content retried as xlsx");
    assert_eq!(sheet.kind, crate::sheet::SheetKind::Xlsx);
}

#[test]
fn extensionless_pdf_and_plain_zip_never_dead_end() {
    let tmp = tempfile::tempdir().unwrap();
    // A real-enough PDF head: whether poppler is installed decides
    // pdf-vs-hex, but the open must SUCCEED either way.
    let pdf = tmp.path().join("doc");
    std::fs::write(
        &pdf,
        b"%PDF-1.4\n1 0 obj\n<<>>\nendobj\ntrailer\n<<>>\n%%EOF\n\x00",
    )
    .unwrap();
    let mut e = Editor::new();
    e.open(&pdf).unwrap();
    assert!(
        e.image.as_ref().is_some_and(|i| i.pdf.is_some()) || e.hex.is_some(),
        "a sniffed PDF opens in the reader or falls to hex, never errors"
    );

    // A zip that is NOT a workbook: the xlsx attempt fails, the hex
    // fallback catches it.
    let plain = tmp.path().join("bundle");
    let f = std::fs::File::create(&plain).unwrap();
    let mut z = zip::ZipWriter::new(f);
    let opts =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    use std::io::Write as _;
    z.start_file("readme.txt", opts).unwrap();
    z.write_all(b"inside").unwrap();
    z.finish().unwrap();
    let mut e = Editor::new();
    e.open(&plain).unwrap();
    assert!(
        e.archive.is_some(),
        "a non-workbook zip lands in the archive browser (#179)"
    );
    assert!(e.sheet.is_none() && e.hex.is_none());
}

fn gzip_to(path: &Path, data: &[u8]) {
    use std::io::Write as _;
    let mut gz = flate2::write::GzEncoder::new(
        std::fs::File::create(path).unwrap(),
        flate2::Compression::fast(),
    );
    gz.write_all(data).unwrap();
    gz.finish().unwrap();
}

fn log_lines(e: &mut Editor, n: usize) -> Vec<String> {
    let view = e.log.as_mut().expect("the log view");
    view.ensure(0, n).unwrap();
    (0..n)
        .filter_map(|i| view.visible_text(i).map(str::to_string))
        .collect()
}

/// #1469: a rotated log, gzipped alone, opens as its text, read-only,
/// under its own name; it opened as a hex dump of the gzip bytes.
#[test]
fn a_gzipped_log_opens_as_its_text() {
    let tmp = tempfile::tempdir().unwrap();
    let gz = tmp.path().join("app.log.1.gz");
    gzip_to(&gz, b"2026-10-04 INFO request id=1\n2026-10-04 WARN slow\n");
    let mut e = Editor::new();
    e.open(&gz).unwrap();
    assert!(e.hex.is_none(), "not hex");
    assert_eq!(e.path.as_deref(), Some(gz.as_path()), "the tab is the .gz");
    assert_eq!(
        log_lines(&mut e, 2),
        ["2026-10-04 INFO request id=1", "2026-10-04 WARN slow"]
    );
    assert!(e.status.contains("read-only"), "{}", e.status);
}

/// #1469: any text compresses the same way: a `data.json.gz` opens as
/// its JSON.
#[test]
fn a_gzipped_json_file_opens_as_its_text() {
    let tmp = tempfile::tempdir().unwrap();
    let gz = tmp.path().join("data.json.gz");
    gzip_to(&gz, b"{\"a\": 1}\n");
    let mut e = Editor::new();
    e.open(&gz).unwrap();
    assert_eq!(log_lines(&mut e, 1), ["{\"a\": 1}"]);
}

/// #1469 negative: the decompressed text is never saved over the
/// archive, and the .gz on disk is unchanged.
#[test]
fn a_gunzipped_tab_is_never_saved_over_the_archive() {
    let tmp = tempfile::tempdir().unwrap();
    let gz = tmp.path().join("dump.sql.gz");
    gzip_to(&gz, b"select 1;\n");
    let before = std::fs::read(&gz).unwrap();
    let mut e = Editor::new();
    e.open(&gz).unwrap();
    assert!(
        e.save_to_disk_force().is_err(),
        "a read-only view saves nothing"
    );
    assert_eq!(std::fs::read(&gz).unwrap(), before);
}

/// #1469 negative: a gzip of binary data stays a hex dump, and a tar.gz
/// still opens the archive browser.
#[test]
fn binary_gzip_stays_hex_and_a_tar_gz_stays_an_archive() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join("blob.bin.gz");
    gzip_to(&bin, &[0u8, 1, 2, 3, 0, 0, 0xff, 0xfe, 0, 7]);
    let mut e = Editor::new();
    e.open(&bin).unwrap();
    assert!(e.hex.is_some() && e.log.is_none());

    let tgz = tmp.path().join("bundle.gz");
    let gz = flate2::write::GzEncoder::new(
        std::fs::File::create(&tgz).unwrap(),
        flate2::Compression::fast(),
    );
    let mut t = tar::Builder::new(gz);
    let data = b"tar payload";
    let mut hdr = tar::Header::new_gnu();
    hdr.set_size(data.len() as u64);
    hdr.set_mode(0o644);
    hdr.set_cksum();
    t.append_data(&mut hdr, "inner/file.txt", &data[..])
        .unwrap();
    t.into_inner().unwrap().finish().unwrap();
    let mut e = Editor::new();
    e.open(&tgz).unwrap();
    assert!(e.archive.is_some() && e.log.is_none());
}

/// #1469 review: a UTF-16 payload, which the UTF-8 log view would
/// render as NULs and replacement characters, stays hex.
#[test]
fn a_gzipped_utf16_file_stays_hex() {
    let tmp = tempfile::tempdir().unwrap();
    let gz = tmp.path().join("notes.txt.gz");
    let mut data = vec![0xff, 0xfe];
    for u in "hello\n".encode_utf16() {
        data.extend_from_slice(&u.to_le_bytes());
    }
    gzip_to(&gz, &data);
    let mut e = Editor::new();
    e.open(&gz).unwrap();
    assert!(e.hex.is_some() && e.log.is_none());
}

/// #1469 review: a gzip larger than the in-memory text cap still opens
/// as its text when it decompresses under the gunzip cap; the size
/// guard used to send it to hex before the gzip route ran.
#[test]
fn a_gzip_past_the_text_cap_still_opens_as_its_text() {
    use std::io::Write as _;
    let tmp = tempfile::tempdir().unwrap();
    let gz = tmp.path().join("big.log.gz");
    let mut enc = flate2::write::GzEncoder::new(
        std::fs::File::create(&gz).unwrap(),
        flate2::Compression::none(),
    );
    let line = b"2026-10-04 INFO a line of a big rotated log\n";
    let mut written = 0u64;
    while written <= MAX_FILE_BYTES {
        enc.write_all(line).unwrap();
        written += line.len() as u64;
    }
    enc.finish().unwrap();
    assert!(std::fs::metadata(&gz).unwrap().len() > MAX_FILE_BYTES);
    let mut e = Editor::new();
    e.open(&gz).unwrap();
    assert!(e.hex.is_none(), "not hex");
    assert_eq!(
        log_lines(&mut e, 1),
        ["2026-10-04 INFO a line of a big rotated log"]
    );
}

/// #1469 review: a link planted at the scratch name is replaced, never
/// followed, so opening a gzip cannot overwrite the link's target.
#[cfg(unix)]
#[test]
fn a_link_planted_at_the_gunzip_scratch_name_is_not_followed() {
    let tmp = tempfile::tempdir().unwrap();
    let gz = tmp.path().join("planted.log.gz");
    gzip_to(&gz, b"decompressed text\n");
    let victim = tmp.path().join("victim.txt");
    std::fs::write(&victim, b"keep me").unwrap();
    let key = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        gz.as_path().hash(&mut h);
        h.finish()
    };
    let scratch = gunzip_scratch_dir()
        .unwrap()
        .join(format!("{key:016x}-planted.log"));
    let _ = std::fs::remove_file(&scratch);
    std::os::unix::fs::symlink(&victim, &scratch).unwrap();
    let mut e = Editor::new();
    e.open(&gz).unwrap();
    assert_eq!(std::fs::read(&victim).unwrap(), b"keep me");
    assert!(!std::fs::symlink_metadata(&scratch).unwrap().is_symlink());
    assert_eq!(log_lines(&mut e, 1), ["decompressed text"]);
    let _ = std::fs::remove_file(&scratch);
}

#[test]
fn preview_openers_supersede_a_diff_view_and_its_arrow_rects() {
    // #187 review: only `open`'s TEXT tail cleared `diff`, and the
    // preview arms return before the render's arrow-rect clearing —
    // a diff tab reopened as a preview kept its "left ↔ right"
    // label, its caret, and clickable hunk arrows.
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join("blob.bin");
    std::fs::write(&bin, b"\x00\x01\x02").unwrap();
    let mut e = Editor::new();
    e.diff = Some(crate::widgets::diff::DiffData::build(
        std::path::PathBuf::from("left.txt"),
        std::path::PathBuf::from("right.txt"),
        vec![String::from("a")],
        vec![String::from("b")],
    ));
    e.diff_prev_arrow = Rect {
        x: 1,
        y: 1,
        width: 3,
        height: 1,
    };
    e.diff_next_arrow = e.diff_prev_arrow;
    e.open(&bin).unwrap();
    assert!(e.hex.is_some());
    assert!(e.diff.is_none(), "the diff view is superseded");
    assert_eq!(e.diff_prev_arrow, Rect::default(), "stale hit rect cleared");
    assert_eq!(e.diff_next_arrow, Rect::default());

    let png = tmp.path().join("pic.png");
    image::RgbaImage::new(2, 2).save(&png).unwrap();
    let mut e = Editor::new();
    e.diff = Some(crate::widgets::diff::DiffData::build(
        std::path::PathBuf::from("left.txt"),
        std::path::PathBuf::from("right.txt"),
        vec![String::from("a")],
        vec![String::from("b")],
    ));
    e.open(&png).unwrap();
    assert!(e.image.is_some());
    assert!(e.diff.is_none(), "image opener supersedes the diff too");
}

#[test]
fn hex_render_narrower_than_the_8_byte_layout_paints_no_half_grid() {
    // #187 review: between the old 24-col floor and the 8-byte
    // layout's real need, the per-cell clip painted offsets with no
    // bytes. The empty state is the clamped header alone.
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("blob.bin");
    std::fs::write(&p, vec![0u8; 64]).unwrap();
    let mut ed = Editor::new();
    ed.open(&p).unwrap();
    let area = Rect {
        x: 0,
        y: 0,
        width: 34, // inner 32: >= 24, < the 44 the 8-byte layout needs
        height: 10,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut ed, area, &mut buf);
    let mut all = String::new();
    for y in 0..area.height {
        for x in 0..area.width {
            all.push_str(buf[(x, y)].symbol());
        }
        all.push('\n');
    }
    assert!(all.contains("HEX"), "header still names the tab: {all}");
    assert!(
        !all.contains("00000000"),
        "no offset column without bytes to go with it: {all}"
    );
    assert!(!all.contains("00 "), "no lone byte cells: {all}");
    let view = ed.hex.as_ref().unwrap();
    assert_eq!(
        view.layout.data_rows, 0,
        "empty state publishes no hit-test rows"
    );
}

#[test]
fn pending_hex_edits_survive_a_reclick_and_die_on_revert() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("blob.bin");
    std::fs::write(&p, vec![0u8; 8]).unwrap();
    let mut e = Editor::new();
    e.open(&p).unwrap();
    e.hex.as_mut().unwrap().apply_edit(0, 0x77);
    e.dirty = true;
    // A same-path re-open (tree re-click) must not drop the edits.
    e.open(&p).unwrap();
    assert_eq!(e.hex.as_ref().unwrap().effective_byte(0), Some(0x77));
    // Revert is the explicit discard: disk truth returns.
    e.revert_to_disk().unwrap();
    assert!(!e.hex.as_ref().unwrap().has_edits());
    assert_eq!(e.hex.as_ref().unwrap().effective_byte(0), Some(0));
    assert!(!e.dirty);
}

#[test]
fn hex_render_drops_to_8_bytes_per_row_in_a_narrow_pane() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("blob.bin");
    std::fs::write(&p, vec![0x00u8; 64]).unwrap();
    let mut ed = Editor::new();
    ed.open(&p).unwrap();
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 10,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut ed, area, &mut buf);
    assert_eq!(ed.hex.as_ref().unwrap().bytes_per_row, 8);
}

#[test]
fn hex_view_swaps_away_cleanly_when_a_text_file_reopens() {
    // The same tab object is reused across opens (the sheet/image
    // precedent): a hex tab navigated away to a text file must drop
    // the hex state, and vice versa.
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join("blob.bin");
    std::fs::write(&bin, b"\x00\x01\x02").unwrap();
    let txt = tmp.path().join("notes.txt");
    std::fs::write(&txt, "hello\n").unwrap();
    let mut e = Editor::new();
    e.open(&bin).unwrap();
    assert!(e.hex.is_some());
    e.open(&txt).unwrap();
    assert!(e.hex.is_none(), "text reopen drops the hex state");
    assert_eq!(e.lines, vec!["hello".to_string()]);
    e.open(&bin).unwrap();
    assert!(e.hex.is_some(), "and back again");
    assert_eq!(e.lines, vec![String::new()]);
}

fn diag(
    start_line: u32,
    start_char: u32,
    end_line: u32,
    end_char: u32,
    severity: crate::lsp::manager::DiagnosticSeverity,
) -> crate::lsp::manager::Diagnostic {
    crate::lsp::manager::Diagnostic {
        start_line,
        start_char,
        end_line,
        end_char,
        severity,
        message: String::from("test diagnostic"),
        tags: Default::default(),
        code: None,
        origin: None,
    }
}

// --- Overview ruler ---------------------------------------------------

/// A 100-line buffer with a path, so git marks and diagnostics can attach.
fn ruler_editor() -> Editor {
    let body: String = (1..=100).map(|i| format!("line {i}\n")).collect();
    let mut e = editor_with(&body);
    e.path = Some(std::path::PathBuf::from("/tmp/ruler.rs"));
    e
}

#[test]
fn an_unmarked_file_produces_an_empty_ruler() {
    let mut e = ruler_editor();
    let rows = e.overview_ruler_rows(20);
    assert_eq!(rows.len(), 20, "one slot per track row");
    assert!(rows.iter().all(|r| r.is_none()));
}

#[test]
fn a_zero_height_track_asks_for_no_slots() {
    let mut e = ruler_editor();
    assert!(e.overview_ruler_rows(0).is_empty());
}

#[test]
fn an_empty_buffer_does_not_divide_by_its_line_count() {
    let mut e = Editor::new();
    e.lines.clear();
    assert_eq!(e.overview_ruler_rows(10).len(), 10);
}

/// #1030: a quick-fix request hands a server its diagnostic as sent, but
/// not once the buffer has changed since: the fix in its `data` would
/// land where the text used to be.
#[test]
fn a_diagnostic_loses_its_origin_once_the_buffer_changes() {
    let mut e = editor_with("import os\n");
    let wire = lsp_types::Diagnostic {
        message: "`os` imported but unused".into(),
        data: Some(serde_json::json!({ "fix": "remove" })),
        ..Default::default()
    };
    let d = crate::lsp::client::convert_diagnostic("ruff", &wire);
    e.apply_diagnostics(PathBuf::from("/w/a.py"), vec![d]);
    let sent = e.diagnostics_in_line_range(0, 0);
    assert!(sent[0].origin.is_some(), "unchanged: sent as published");
    e.insert_char('x');
    let sent = e.diagnostics_in_line_range(0, 0);
    assert_eq!(sent.len(), 1);
    assert!(sent[0].origin.is_none(), "edited: rebuilt, without `data`");
}

#[test]
fn a_diagnostic_lands_on_the_track_row_its_line_maps_to() {
    use crate::lsp::manager::DiagnosticSeverity;
    let mut e = ruler_editor();
    let p = e.path.clone().unwrap();
    // Line 51 (0-based 50) of 100 lines on a 20-row track => row 10.
    e.apply_diagnostics(p, vec![diag(50, 0, 50, 4, DiagnosticSeverity::Error)]);
    let rows = e.overview_ruler_rows(20);
    assert_eq!(rows[10], Some(OverviewMark::Error));
    assert_eq!(
        rows.iter().filter(|r| r.is_some()).count(),
        1,
        "one diagnostic must not smear across the track"
    );
}

#[test]
fn the_last_line_maps_inside_the_track_rather_than_one_past_it() {
    use crate::lsp::manager::DiagnosticSeverity;
    let mut e = ruler_editor();
    let p = e.path.clone().unwrap();
    e.apply_diagnostics(p, vec![diag(99, 0, 99, 1, DiagnosticSeverity::Error)]);
    let rows = e.overview_ruler_rows(20);
    assert_eq!(
        rows[19],
        Some(OverviewMark::Error),
        "the final line belongs on the final track row, not out of bounds"
    );
}

#[test]
fn an_error_outranks_a_warning_sharing_a_track_row() {
    use crate::lsp::manager::DiagnosticSeverity;
    let mut e = ruler_editor();
    let p = e.path.clone().unwrap();
    // Lines 0 and 1 both compress onto track row 0 (100 lines / 10 rows).
    e.apply_diagnostics(
        p,
        vec![
            diag(0, 0, 0, 1, DiagnosticSeverity::Warning),
            diag(1, 0, 1, 1, DiagnosticSeverity::Error),
        ],
    );
    assert_eq!(
        e.overview_ruler_rows(10)[0],
        Some(OverviewMark::Error),
        "a broken build must not be hidden behind a warning on the next line"
    );
}

#[test]
fn hints_and_information_stay_off_the_ruler() {
    use crate::lsp::manager::DiagnosticSeverity;
    let mut e = ruler_editor();
    let p = e.path.clone().unwrap();
    e.apply_diagnostics(
        p,
        vec![
            diag(10, 0, 10, 1, DiagnosticSeverity::Hint),
            diag(30, 0, 30, 1, DiagnosticSeverity::Information),
        ],
    );
    assert!(
        e.overview_ruler_rows(20).iter().all(|r| r.is_none()),
        "advisory diagnostics would crowd the one column real problems use"
    );
}

#[test]
fn search_matches_are_ticked_only_where_the_highlighter_agrees() {
    let mut e = ruler_editor();
    e.lines[20] = String::from("needle here");
    e.lines[60] = String::from("NEEDLE shouting");
    e.search_highlight = Some(String::from("needle"));
    // Case-insensitive by default: both lines match.
    let rows = e.overview_ruler_rows(10);
    assert_eq!(rows[2], Some(OverviewMark::SearchMatch));
    assert_eq!(rows[6], Some(OverviewMark::SearchMatch));

    e.search_highlight_opts.case_sensitive = true;
    let rows = e.overview_ruler_rows(10);
    assert_eq!(rows[2], Some(OverviewMark::SearchMatch));
    assert_eq!(
        rows[6], None,
        "the ruler must agree with the yellow runs in the text, not guess"
    );
}

#[test]
fn an_empty_needle_ticks_nothing() {
    let mut e = ruler_editor();
    e.search_highlight = Some(String::new());
    assert!(e.overview_ruler_rows(10).iter().all(|r| r.is_none()));
}

#[test]
fn a_diagnostic_outranks_a_search_hit_on_the_same_row() {
    use crate::lsp::manager::DiagnosticSeverity;
    let mut e = ruler_editor();
    let p = e.path.clone().unwrap();
    e.lines[5] = String::from("needle");
    e.search_highlight = Some(String::from("needle"));
    e.apply_diagnostics(p, vec![diag(5, 0, 5, 1, DiagnosticSeverity::Warning)]);
    assert_eq!(e.overview_ruler_rows(10)[0], Some(OverviewMark::Warning));
}

#[test]
fn the_mark_priority_order_is_the_declared_one() {
    // The render relies on `Ord`, so pin it rather than the comparison
    // silently inverting when a variant is inserted in the middle.
    assert!(OverviewMark::Error > OverviewMark::Warning);
    assert!(OverviewMark::Warning > OverviewMark::SearchMatch);
    assert!(OverviewMark::SearchMatch > OverviewMark::GitDeleted);
    assert!(OverviewMark::GitDeleted > OverviewMark::GitModified);
    assert!(OverviewMark::GitModified > OverviewMark::GitAdded);
}

#[test]
fn git_marks_reach_the_ruler_in_their_own_three_colours() {
    let mut e = ruler_editor();
    // A HEAD baseline that differs from the buffer on known lines.
    let mut head: Vec<String> = (1..=100).map(|i| format!("line {i}")).collect();
    head[10] = String::from("line 11 as committed");
    e.git_head_lines = Some(PackedLines::from(head));
    e.git_baseline_for = e.path.clone();
    let rows = e.overview_ruler_rows(100);
    assert_eq!(
        rows[10],
        Some(OverviewMark::GitModified),
        "an edited line must show as modified on the ruler; rows were {:?}",
        rows.iter()
            .enumerate()
            .filter(|(_, r)| r.is_some())
            .collect::<Vec<_>>()
    );
}

/// Render `e` into a 40x12 buffer and return it with the track-relative
/// rows that carry a ruler tick.
fn rendered_ruler_ticks(e: &mut Editor) -> (ratatui::buffer::Buffer, Vec<u16>) {
    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 12,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut *e, area, &mut buf);
    let bar = e.last_scrollbar;
    let ticks = (0..bar.height)
        .filter(|&r| buf[(bar.x, bar.y + r)].symbol() == "\u{2501}")
        .collect();
    (buf, ticks)
}

#[test]
fn the_ticks_paint_over_the_track_without_erasing_the_thumb() {
    use crate::lsp::manager::DiagnosticSeverity;
    let mut e = ruler_editor();
    let p = e.path.clone().unwrap();
    // Scrolled to the top, line 0's tick sits on the thumb and line 99's
    // on the bare track: each must keep the background it was painted on.
    e.apply_diagnostics(
        p,
        vec![
            diag(0, 0, 0, 1, DiagnosticSeverity::Error),
            diag(99, 0, 99, 1, DiagnosticSeverity::Error),
        ],
    );
    let (buf, ticks) = rendered_ruler_ticks(&mut e);
    let bar = e.last_scrollbar;
    assert_eq!(ticks, vec![0, bar.height - 1], "one tick per error");
    let thumb = if e.focused {
        e.theme.scrollbar_thumb_focused()
    } else {
        e.theme.scrollbar_thumb()
    };
    assert_eq!(
        buf[(bar.x, bar.y)].style().bg,
        Some(thumb),
        "a tick on the thumb must not erase it"
    );
    assert_eq!(
        buf[(bar.x, bar.y + bar.height - 1)].style().bg,
        Some(e.theme.scrollbar_track()),
        "a tick on the track keeps the track colour"
    );
}

#[test]
fn a_wrapped_file_places_ticks_where_the_thumb_would_reveal_them() {
    use crate::lsp::manager::DiagnosticSeverity;
    let mut e = ruler_editor();
    e.wrap_override = Some(true);
    // The first half wraps to many rows each, so line 50 starts near the
    // bottom of the content the thumb travels over — not at its middle.
    for l in 0..50 {
        e.lines[l] = "x".repeat(400);
    }
    let p = e.path.clone().unwrap();
    e.apply_diagnostics(p, vec![diag(50, 0, 50, 1, DiagnosticSeverity::Error)]);
    let (_, ticks) = rendered_ruler_ticks(&mut e);
    let bar = e.last_scrollbar;
    assert_eq!(
        ticks,
        vec![bar.height - 1],
        "line 50 starts past 90% of the visual rows, so on the last track row; \
             a line-count mapping puts it halfway, at {}",
        bar.height / 2
    );
}

#[test]
fn a_zero_width_regex_match_ticks_nothing() {
    let mut e = ruler_editor();
    e.search_highlight_opts.use_regex = true;
    // Each matches every line at zero width; the text highlighter paints
    // no cell for any of them, so the ruler must not either.
    for needle in ["^", "\\b", "q*", "needle|"] {
        e.search_highlight = Some(String::from(needle));
        assert!(
            e.overview_ruler_rows(10).iter().all(|r| r.is_none()),
            "{needle} highlights no text"
        );
    }
}

#[test]
fn an_edit_after_a_search_moves_the_search_ticks() {
    let mut e = ruler_editor();
    e.search_highlight = Some(String::from("needle"));
    assert!(e.overview_ruler_rows(10).iter().all(|r| r.is_none()));
    // Through the real insert path, so a result cached per frame must
    // notice the buffer changed under it.
    e.cursor_row = 30;
    e.cursor_col = 0;
    for c in "needle ".chars() {
        e.insert_char(c);
    }
    assert_eq!(
        e.overview_ruler_rows(10)[3],
        Some(OverviewMark::SearchMatch)
    );
}

/// The search ticks are cached per `(edit, needle, options)`, so a new
/// needle or a flipped option must be a miss, not a stale answer.
#[test]
fn a_new_needle_or_option_recomputes_the_cached_search_ticks() {
    let mut e = ruler_editor();
    e.lines[30] = String::from("Needle");
    e.search_highlight = Some(String::from("needle"));
    assert_eq!(
        e.overview_ruler_rows(10)[3],
        Some(OverviewMark::SearchMatch)
    );
    e.search_highlight_opts.case_sensitive = true;
    assert!(
        e.overview_ruler_rows(10).iter().all(|r| r.is_none()),
        "case-sensitive `needle` does not match `Needle`"
    );
    e.search_highlight = Some(String::from("Needle"));
    assert_eq!(
        e.overview_ruler_rows(10)[3],
        Some(OverviewMark::SearchMatch)
    );
}

#[test]
fn toggling_the_ruler_off_leaves_a_plain_scrollbar() {
    use crate::lsp::manager::DiagnosticSeverity;
    let mut e = ruler_editor();
    let p = e.path.clone().unwrap();
    e.apply_diagnostics(p, vec![diag(99, 0, 99, 1, DiagnosticSeverity::Error)]);
    e.overview_ruler = false;
    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 12,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut e, area, &mut buf);
    let bar_x = e.last_scrollbar.x;
    assert!(
        (e.last_scrollbar.y..e.last_scrollbar.y + e.last_scrollbar.height)
            .all(|y| buf[(bar_x, y)].symbol() != "\u{2501}"),
        "the toggle must actually stop the paint, not just dim it"
    );
}

#[test]
fn apply_diagnostics_decodes_into_per_line_char_spans() {
    use crate::lsp::manager::DiagnosticSeverity;
    let mut e = editor_with("const x = 1;");
    let p = std::path::PathBuf::from("/tmp/diag.ts");
    e.path = Some(p.clone());
    // Underline the `x` (chars 6..7).
    e.apply_diagnostics(p, vec![diag(0, 6, 0, 7, DiagnosticSeverity::Error)]);
    let spans = e.diagnostic_spans_for_test();
    assert_eq!(
        spans[0],
        vec![(6, 7, DiagnosticSeverity::Error)],
        "the diagnostic must decode to a single-char run over `x`"
    );
}

#[test]
fn zero_width_diagnostic_is_widened_to_one_cell() {
    use crate::lsp::manager::DiagnosticSeverity;
    let mut e = editor_with("const x = 1;");
    let p = std::path::PathBuf::from("/tmp/diag.ts");
    e.path = Some(p.clone());
    // A point diagnostic (start == end) must still show one cell.
    e.apply_diagnostics(p, vec![diag(0, 6, 0, 6, DiagnosticSeverity::Warning)]);
    assert_eq!(
        e.diagnostic_spans_for_test()[0],
        vec![(6, 7, DiagnosticSeverity::Warning)],
        "a zero-width diagnostic must widen to one cell so it stays visible"
    );
}

#[test]
fn diagnostics_for_another_file_do_not_decode() {
    use crate::lsp::manager::DiagnosticSeverity;
    let mut e = editor_with("const x = 1;");
    e.path = Some(std::path::PathBuf::from("/tmp/open.ts"));
    // Batch is for a different file than the one loaded.
    e.apply_diagnostics(
        std::path::PathBuf::from("/tmp/other.ts"),
        vec![diag(0, 0, 0, 5, DiagnosticSeverity::Error)],
    );
    assert!(
        e.diagnostic_spans_for_test().iter().all(|l| l.is_empty()),
        "a batch for a different file must not underline the loaded one"
    );
}

#[test]
fn empty_diagnostics_batch_clears_the_underlines() {
    use crate::lsp::manager::DiagnosticSeverity;
    let mut e = editor_with("const x = 1;");
    let p = std::path::PathBuf::from("/tmp/diag.ts");
    e.path = Some(p.clone());
    e.apply_diagnostics(p.clone(), vec![diag(0, 6, 0, 7, DiagnosticSeverity::Error)]);
    assert!(!e.diagnostic_spans_for_test()[0].is_empty());
    // An "all clear" republish carries an empty list.
    e.apply_diagnostics(p, Vec::new());
    assert!(
        e.diagnostic_spans_for_test().iter().all(|l| l.is_empty()),
        "an empty batch must erase the squiggles"
    );
}

#[test]
fn diagnostics_at_returns_messages_covering_the_point() {
    use crate::lsp::manager::{Diagnostic, DiagnosticSeverity};
    let mut e = editor_with("const x = 1;");
    let p = std::path::PathBuf::from("/tmp/diag.ts");
    e.path = Some(p.clone());
    // Underline the `x` (chars 6..7) with a real message.
    e.apply_diagnostics(
        p,
        vec![Diagnostic {
            start_line: 0,
            start_char: 6,
            end_line: 0,
            end_char: 7,
            severity: DiagnosticSeverity::Error,
            message: String::from("Type 'number' is not assignable to type 'string'."),
            tags: Default::default(),
            code: None,
            origin: None,
        }],
    );
    assert_eq!(
        e.diagnostics_at(0, 6, Path::new("/tmp")),
        vec![(
            DiagnosticSeverity::Error,
            String::from("Type 'number' is not assignable to type 'string'.")
        )],
        "a point inside the diagnostic range must surface its message"
    );
    assert!(
        e.diagnostics_at(0, 0, Path::new("/tmp")).is_empty(),
        "a point outside every diagnostic range must surface nothing"
    );
}

/// #1540: the hover message carries the rule as `source(code)` and its
/// link on the next line; a diagnostic without a code reads as before.
#[test]
fn diagnostics_at_adds_the_rule_code_and_its_link() {
    let mut e = editor_with("import os, json");
    e.path = Some(PathBuf::from("/tmp/app.py"));
    let diag = |start: u32, end: u32, message: &str, code| crate::lsp::manager::Diagnostic {
        start_line: 0,
        start_char: start,
        end_line: 0,
        end_char: end,
        severity: crate::lsp::manager::DiagnosticSeverity::Warning,
        message: message.into(),
        tags: Default::default(),
        code,
        origin: None,
    };
    e.apply_diagnostics(
        PathBuf::from("/tmp/app.py"),
        vec![
            diag(
                7,
                9,
                "`os` imported but unused",
                Some(crate::lsp::manager::DiagnosticCode {
                    value: String::from("F401"),
                    source: Some(String::from("Ruff")),
                    href: Some(String::from(
                        "https://docs.astral.sh/ruff/rules/unused-import",
                    )),
                }),
            ),
            diag(11, 15, "`json` is unused", None),
        ],
    );
    assert_eq!(
        e.diagnostics_at(0, 8, Path::new("/tmp")),
        vec![(
            crate::lsp::manager::DiagnosticSeverity::Warning,
            String::from(
                "`os` imported but unused  Ruff(F401)\nhttps://docs.astral.sh/ruff/rules/unused-import"
            )
        )]
    );
    assert_eq!(
        e.diagnostics_at(0, 12, Path::new("/tmp")),
        vec![(
            crate::lsp::manager::DiagnosticSeverity::Warning,
            String::from("`json` is unused")
        )]
    );
}

/// #1523: the places the server points at to explain a diagnostic
/// follow it in the hover as `file(line, col): message`, one-based, the
/// file relative to the workspace.
#[test]
fn diagnostics_at_lists_the_related_locations_under_the_message() {
    let mut e = editor_with("let first = &v[0]; v.push(2);");
    e.path = Some(PathBuf::from("/w/src/main.rs"));
    let at = |line, character, message: &str| lsp_types::DiagnosticRelatedInformation {
        location: lsp_types::Location {
            uri: lsp_types::Url::parse("file:///w/src/main.rs").unwrap(),
            range: lsp_types::Range::new(
                lsp_types::Position::new(line, character),
                lsp_types::Position::new(line, character + 1),
            ),
        },
        message: message.into(),
    };
    let wire = lsp_types::Diagnostic {
        range: lsp_types::Range::new(
            lsp_types::Position::new(0, 19),
            lsp_types::Position::new(0, 28),
        ),
        message: "cannot borrow `v` as mutable".into(),
        related_information: Some(vec![
            at(0, 13, "immutable borrow occurs here"),
            at(4, 15, "immutable borrow later used here"),
        ]),
        ..Default::default()
    };
    let d = crate::lsp::client::convert_diagnostic("rust-analyzer", &wire);
    e.apply_diagnostics(PathBuf::from("/w/src/main.rs"), vec![d]);
    assert_eq!(
        e.diagnostics_at(0, 20, Path::new("/w"))[0].1,
        "cannot borrow `v` as mutable\n\
             src/main.rs(1, 14): immutable borrow occurs here\n\
             src/main.rs(5, 16): immutable borrow later used here"
    );
}

#[test]
fn diagnostics_at_widens_a_zero_width_diagnostic_to_one_cell() {
    use crate::lsp::manager::{Diagnostic, DiagnosticSeverity};
    let mut e = editor_with("const x = 1;");
    let p = std::path::PathBuf::from("/tmp/diag.ts");
    e.path = Some(p.clone());
    e.apply_diagnostics(
        p,
        vec![Diagnostic {
            start_line: 0,
            start_char: 6,
            end_line: 0,
            end_char: 6,
            severity: DiagnosticSeverity::Warning,
            message: String::from("missing semicolon"),
            tags: Default::default(),
            code: None,
            origin: None,
        }],
    );
    assert_eq!(
        e.diagnostics_at(0, 6, Path::new("/tmp")),
        vec![(
            DiagnosticSeverity::Warning,
            String::from("missing semicolon")
        )],
        "a zero-width diagnostic must still be hoverable over its one widened cell"
    );
}

#[test]
fn hover_region_at_prefers_the_word_then_falls_back_to_the_diagnostic_span() {
    use crate::lsp::manager::{Diagnostic, DiagnosticSeverity};
    // `a.b` — the `.` at char 1 is not a word char, but a diagnostic covers it.
    let mut e = editor_with("a.b");
    let p = std::path::PathBuf::from("/tmp/dot.ts");
    e.path = Some(p.clone());
    e.apply_diagnostics(
        p,
        vec![Diagnostic {
            start_line: 0,
            start_char: 1,
            end_line: 0,
            end_char: 2,
            severity: DiagnosticSeverity::Error,
            message: String::from("unexpected token"),
            tags: Default::default(),
            code: None,
            origin: None,
        }],
    );
    assert_eq!(
        e.hover_region_at(0, 0),
        Some((0, 0, 1)),
        "over the `a` identifier the region is the word range"
    );
    assert_eq!(
        e.hover_region_at(0, 1),
        Some((0, 1, 2)),
        "over the punctuation `.` the region falls back to the covering diagnostic span"
    );
}

#[test]
fn render_paints_severity_coloured_underline_over_the_diagnostic_span() {
    use crate::lsp::manager::DiagnosticSeverity;
    let mut e = editor_with("const x = 1;");
    let p = std::path::PathBuf::from("/tmp/diag.ts");
    e.path = Some(p.clone());
    e.apply_diagnostics(p, vec![diag(0, 6, 0, 7, DiagnosticSeverity::Error)]);
    e.focused = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 5,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut e as &mut Editor).render(area, &mut buf);
    let red = Color::Rgb(0xf1, 0x4c, 0x4c);
    let mut underlined: Vec<(u16, u16, String)> = Vec::new();
    for y in 0..area.height {
        for x in 0..area.width {
            let cell = &buf[(x, y)];
            if cell.modifier.contains(Modifier::UNDERLINED) && cell.underline_color == red {
                underlined.push((x, y, cell.symbol().to_string()));
            }
        }
    }
    assert_eq!(
        underlined.len(),
        1,
        "exactly one cell (the `x`) must carry a red error underline; got {underlined:?}"
    );
    assert_eq!(
        underlined[0].2, "x",
        "the underline must sit under the offending `x` glyph"
    );
}

/// Render `text` with `diags` applied and report, per cell of the first
/// text row, its glyph and the modifiers the diagnostics added (#1522).
fn diagnostic_looks(
    text: &str,
    diags: Vec<crate::lsp::manager::Diagnostic>,
) -> Vec<(String, Modifier, Color)> {
    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 5,
    };
    let draw = |diags: Vec<crate::lsp::manager::Diagnostic>| {
        let mut e = editor_with(text);
        let p = std::path::PathBuf::from("/tmp/diag.ts");
        e.path = Some(p.clone());
        e.apply_diagnostics(p, diags);
        e.focused = true;
        let mut buf = ratatui::buffer::Buffer::empty(area);
        (&mut e as &mut Editor).render(area, &mut buf);
        buf
    };
    let plain = draw(Vec::new());
    let marked = draw(diags);
    let row = (0..area.height)
        .find(|&y| {
            let line: String = (0..area.width).map(|x| plain[(x, y)].symbol()).collect();
            line.contains(text)
        })
        .expect("the text row is drawn");
    (0..area.width)
        .map(|x| {
            let (was, now) = (&plain[(x, row)], &marked[(x, row)]);
            (
                now.symbol().to_string(),
                now.modifier - was.modifier,
                now.underline_color,
            )
        })
        .collect()
}

fn with_tags(
    d: crate::lsp::manager::Diagnostic,
    unnecessary: bool,
    deprecated: bool,
) -> crate::lsp::manager::Diagnostic {
    crate::lsp::manager::Diagnostic {
        tags: crate::lsp::manager::DiagnosticTags {
            unnecessary,
            deprecated,
        },
        ..d
    }
}

fn glyphs_with(looks: &[(String, Modifier, Color)], m: Modifier) -> String {
    looks
        .iter()
        .filter(|(_, added, _)| added.contains(m))
        .map(|(g, _, _)| g.as_str())
        .collect()
}

#[test]
fn an_unnecessary_hint_is_faded_and_not_underlined() {
    use crate::lsp::manager::DiagnosticSeverity;
    let looks = diagnostic_looks(
        "const tmp = 42;",
        vec![with_tags(
            diag(0, 6, 0, 9, DiagnosticSeverity::Hint),
            true,
            false,
        )],
    );
    assert_eq!(glyphs_with(&looks, Modifier::DIM), "tmp", "{looks:?}");
    assert_eq!(
        glyphs_with(&looks, Modifier::UNDERLINED),
        "",
        "unused code is faded, never underlined like a problem"
    );
}

#[test]
fn a_deprecated_warning_is_struck_through_and_keeps_its_underline() {
    use crate::lsp::manager::DiagnosticSeverity;
    let looks = diagnostic_looks(
        "old(); fresh();",
        vec![with_tags(
            diag(0, 0, 0, 3, DiagnosticSeverity::Warning),
            false,
            true,
        )],
    );
    assert_eq!(glyphs_with(&looks, Modifier::CROSSED_OUT), "old");
    assert_eq!(glyphs_with(&looks, Modifier::UNDERLINED), "old");
    assert_eq!(
        glyphs_with(&looks, Modifier::DIM),
        "",
        "deprecated is not faded"
    );
}

#[test]
fn a_plain_hint_is_marked_under_its_first_two_characters_in_grey() {
    use crate::lsp::manager::DiagnosticSeverity;
    let looks = diagnostic_looks(
        "let total = add(a, b);",
        vec![diag(0, 4, 0, 9, DiagnosticSeverity::Hint)],
    );
    assert_eq!(glyphs_with(&looks, Modifier::UNDERLINED), "to");
    assert!(
        looks
            .iter()
            .filter(|(_, added, _)| added.contains(Modifier::UNDERLINED))
            .all(|(_, _, colour)| *colour
                == crate::theme::Theme::default().ui(Color::Rgb(0x80, 0x80, 0x80))),
        "a hint's marker is grey, not the information blue: {looks:?}"
    );
    assert_eq!(
        glyphs_with(&looks, Modifier::DIM),
        "",
        "an untagged hint is not faded"
    );
}

#[test]
fn errors_warnings_and_information_keep_their_full_underline_and_no_fade() {
    use crate::lsp::manager::DiagnosticSeverity;
    let looks = diagnostic_looks(
        "bad; odd; note;",
        vec![
            diag(0, 0, 0, 3, DiagnosticSeverity::Error),
            diag(0, 5, 0, 8, DiagnosticSeverity::Warning),
            diag(0, 10, 0, 14, DiagnosticSeverity::Information),
        ],
    );
    assert_eq!(glyphs_with(&looks, Modifier::UNDERLINED), "badoddnote");
    assert_eq!(glyphs_with(&looks, Modifier::DIM), "");
    assert_eq!(glyphs_with(&looks, Modifier::CROSSED_OUT), "");
}

#[test]
fn a_tagged_error_is_both_underlined_and_faded() {
    use crate::lsp::manager::DiagnosticSeverity;
    let looks = diagnostic_looks(
        "import os",
        vec![with_tags(
            diag(0, 7, 0, 9, DiagnosticSeverity::Error),
            true,
            false,
        )],
    );
    assert_eq!(glyphs_with(&looks, Modifier::UNDERLINED), "os");
    assert_eq!(glyphs_with(&looks, Modifier::DIM), "os");
}

#[test]
fn a_zero_width_tagged_hint_is_still_marked() {
    use crate::lsp::manager::DiagnosticSeverity;
    let looks = diagnostic_looks(
        "let tmp = 1;",
        vec![with_tags(
            diag(0, 4, 0, 4, DiagnosticSeverity::Hint),
            true,
            false,
        )],
    );
    assert_eq!(glyphs_with(&looks, Modifier::DIM), "t", "{looks:?}");
    assert_eq!(glyphs_with(&looks, Modifier::UNDERLINED), "");
    // At the end of the line there is nothing to fade, so the hint keeps
    // its short underline rather than vanishing.
    let mut e = editor_with("let tmp = 1;");
    let p = std::path::PathBuf::from("/tmp/diag.ts");
    e.path = Some(p.clone());
    e.apply_diagnostics(
        p,
        vec![with_tags(
            diag(0, 12, 0, 12, DiagnosticSeverity::Hint),
            false,
            true,
        )],
    );
    assert!(e.diagnostic_tag_spans[0].is_empty());
    assert_eq!(e.diagnostic_spans_for_test()[0].len(), 1);
}

#[test]
fn a_multi_line_unnecessary_hint_fades_every_line_it_covers() {
    use crate::lsp::manager::DiagnosticSeverity;
    let mut e = editor_with("return 1;\nlog(\"never\");\ndone();");
    let p = std::path::PathBuf::from("/tmp/diag.ts");
    e.path = Some(p.clone());
    e.apply_diagnostics(
        p,
        vec![with_tags(
            diag(1, 0, 2, 7, DiagnosticSeverity::Hint),
            true,
            false,
        )],
    );
    assert!(e.diagnostic_spans_for_test().iter().all(|l| l.is_empty()));
    let tagged: Vec<_> = e
        .diagnostic_tag_spans
        .iter()
        .map(|l| l.iter().map(|&(s, e, _)| (s, e)).collect::<Vec<_>>())
        .collect();
    assert_eq!(tagged, vec![vec![], vec![(0, 13)], vec![(0, 7)]]);
}

#[test]
fn focused_editor_gradient_border_is_gated_on_focus_gradient() {
    use crate::gradient::{GRAD_TL, rgb_color};
    let mut e = editor_with("hi");
    // A boxed theme: Croft Loch marks focus with a rule instead.
    e.theme = crate::theme::Theme::BLACK;
    e.focused = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 5,
    };

    // Black theme: rounded gradient corner in the gradient's TL colour.
    e.focus_gradient = true;
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut e as &mut Editor).render(area, &mut buf);
    assert_eq!(buf[(0, 0)].symbol(), "\u{256d}");
    assert_eq!(buf[(0, 0)].fg, rgb_color(GRAD_TL));

    // Croft Dark: square corner, solid blue (the historical highlight).
    e.focus_gradient = false;
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut e as &mut Editor).render(area, &mut buf);
    assert_eq!(buf[(0, 0)].symbol(), "\u{250c}");
    assert_eq!(buf[(0, 0)].fg, Color::Rgb(0x4e, 0x9a, 0xff));
}

/// #248: the cursor line wears the theme's accent wash (VS Code
/// `editor.lineHighlightBackground`), gutter through code, in the
/// focused editor only, and it follows the cursor.
#[test]
fn current_line_wash_tracks_the_cursor_and_focus() {
    let mut e = editor_with("alpha\nbeta\ngamma");
    e.focused = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 6,
    };
    let wash = e.theme.current_line_bg();
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut e as &mut Editor).render(area, &mut buf);
    // Line 0 (rendered at y=1 inside the border) holds the cursor: its
    // row wears the wash from the gutter through the code cells, and its
    // neighbour does not.
    assert_eq!(buf[(1, 1)].bg, wash, "gutter cell wears the wash");
    assert_eq!(buf[(8, 1)].bg, wash, "code cell wears the wash");
    assert_ne!(buf[(8, 2)].bg, wash, "the next row stays unwashed");
    // The wash follows the cursor.
    e.cursor_row = 2;
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut e as &mut Editor).render(area, &mut buf);
    assert_ne!(buf[(8, 1)].bg, wash, "the old row is released");
    assert_eq!(buf[(8, 3)].bg, wash, "the new cursor row is washed");
    // An unfocused editor paints no wash at all.
    e.focused = false;
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut e as &mut Editor).render(area, &mut buf);
    assert_ne!(buf[(8, 3)].bg, wash, "no wash without focus");
}

fn inlay(line: u32, character: u32, label: &str) -> crate::lsp::manager::InlayHintItem {
    crate::lsp::manager::InlayHintItem {
        line,
        character,
        label: label.to_string(),
    }
}

/// Render `e` into a fresh 60x5 buffer and return the full text of the
/// first content row (y = 1, inside the border).
fn first_row_text(e: &mut Editor) -> String {
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 5,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (e as &mut Editor).render(area, &mut buf);
    (0..area.width)
        .map(|x| buf[(x, 1)].symbol().to_string())
        .collect()
}

#[test]
fn inlay_hint_splices_dim_italic_label_into_the_row() {
    let mut e = editor_with("let x = f(y);");
    let p = std::path::PathBuf::from("/tmp/hints.rs");
    e.path = Some(p.clone());
    e.apply_inlay_hints(p, vec![inlay(0, 5, ": i32"), inlay(0, 10, "n: ")]);
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 5,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut e as &mut Editor).render(area, &mut buf);
    let row: String = (0..area.width)
        .map(|x| buf[(x, 1)].symbol().to_string())
        .collect();
    assert!(
        row.contains("let x: i32 = f(n: y);"),
        "hints must splice into the rendered row; got {row:?}"
    );
    // The hint cells are dim italic; the real code cells are not.
    let hint_fg = e.theme.ignored_fg();
    let hint_x = (0..area.width)
        .find(|&x| {
            buf[(x, 1)].symbol() == "i"
                && buf[(x + 1, 1)].symbol() == "3"
                && buf[(x + 2, 1)].symbol() == "2"
        })
        .expect("the i32 hint must be on the row");
    for dx in 0..3 {
        let cell = &buf[(hint_x + dx, 1)];
        assert!(
            cell.modifier.contains(Modifier::ITALIC),
            "hint cells must be italic"
        );
        assert_eq!(cell.fg, hint_fg, "hint cells must use the muted grey");
    }
    let code_x = (0..area.width)
        .find(|&x| buf[(x, 1)].symbol() == "x")
        .expect("the x binding must be on the row");
    assert!(
        !buf[(code_x, 1)].modifier.contains(Modifier::ITALIC),
        "real code cells must stay non-italic"
    );
}

#[test]
fn gutter_play_glyph_marks_test_fns_and_maps_clicks_to_the_name() {
    let mut e = editor_with("#[test]\nfn my_case() {}\nfn helper() {}");
    // Beads need a saved .rs file: an unsaved buffer has nothing a
    // runner could target.
    e.path = Some(PathBuf::from("/x/a.rs"));
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 6,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut e as &mut Editor).render(area, &mut buf);
    let sign_x = e.last_inner.x;
    // Buffer line 1 (`fn my_case`) paints on content row y = 2.
    assert_eq!(
        buf[(sign_x, 2)].symbol(),
        "\u{eb2c}",
        "a test fn's first row must wear the play glyph in the sign margin"
    );
    assert_ne!(
        buf[(sign_x, 3)].symbol(),
        "\u{eb2c}",
        "a plain fn must not wear the glyph"
    );
    assert_eq!(
        e.test_glyph_at(sign_x, 2).as_deref(),
        Some("my_case"),
        "clicking the glyph must resolve the test's name"
    );
    assert_eq!(
        e.test_glyph_at(sign_x, 3),
        None,
        "clicking the sign margin of a non-test line resolves nothing"
    );
    assert_eq!(
        e.test_glyph_at(sign_x + 1, 2),
        None,
        "the fold-chevron column is not the play glyph"
    );
}

/// The AI-stream stop button: while `stream_stop_line` is set, that
/// line's sign cell wears the stop square (outranking the test play
/// glyph), and `stream_stop_at` maps clicks on exactly that cell.
#[test]
fn gutter_stream_stop_glyph_renders_maps_clicks_and_outranks_the_play_glyph() {
    let mut e = editor_with("#[test]\nfn my_case() {}\nfn helper() {}");
    e.path = Some(PathBuf::from("/x/a.rs")); // beads need a saved .rs file
    e.stream_stop_line = Some(1); // 0-based: the test fn's line
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 6,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut e as &mut Editor).render(area, &mut buf);
    let sign_x = e.last_inner.x;
    // Buffer line 1 paints on content row y = 2 (breadcrumb row above).
    assert_eq!(
        buf[(sign_x, 2)].symbol(),
        "■",
        "the streamed row's sign cell wears the stop square, not the play glyph"
    );
    assert!(e.stream_stop_at(sign_x, 2), "the stop cell is clickable");
    assert!(
        !e.stream_stop_at(sign_x + 1, 2),
        "the fold-chevron column is not the button"
    );
    assert!(
        !e.stream_stop_at(sign_x, 3),
        "other rows are not the button"
    );

    // Stream over: the glyph clears and the play glyph returns.
    e.stream_stop_line = None;
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut e as &mut Editor).render(area, &mut buf);
    assert_eq!(
        buf[(sign_x, 2)].symbol(),
        "\u{eb2c}",
        "without a stream the test play glyph owns the cell again"
    );
    assert!(!e.stream_stop_at(sign_x, 2));
}

/// A comment box anchored to line 0 renders as an unnumbered block
/// between line 0 and line 1: title row naming the author, body, and a
/// footer with the reply field and Ignore button. The gutter's number
/// column stays blank on box rows, and every following line paints
/// (and reports its screen row) shifted down by the box height.
#[test]
fn comment_box_renders_unnumbered_rows_between_lines() {
    let mut e = editor_with("first\nsecond\nthird");
    e.comment_boxes = vec![CommentBox {
        id: 7,
        line: 0,
        author: "navigator".into(),
        body: "tighten this".into(),
    }];
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 10,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut e as &mut Editor).render(area, &mut buf);

    let row_text = |y: u16| -> String {
        (0..area.width)
            .map(|x| buf[(x, y)].symbol().to_string())
            .collect()
    };
    // Breadcrumb row at y=0; line 0 at y=1 with its number.
    assert!(row_text(1).contains("first"));
    assert!(row_text(1).contains('1'), "line 0 keeps its number");
    // Box: title (author), body, footer (reply + Ignore) at y=2..=4.
    assert!(row_text(2).contains("navigator"), "title names the author");
    assert!(row_text(3).contains("tighten this"), "body renders");
    assert!(row_text(4).contains("Ignore"), "footer has the button");
    let gutter = e.last_gutter_width;
    for y in 2..=4u16 {
        let g: String = (0..gutter)
            .map(|x| buf[(x, y)].symbol().to_string())
            .collect();
        assert!(
            g.chars().all(|c| !c.is_ascii_digit()),
            "box rows are unnumbered, got gutter {g:?} at y={y}"
        );
    }
    // Line 1 shifted below the box, with its own number.
    assert!(row_text(5).contains("second"));
    assert!(row_text(5).contains('2'));
    assert_eq!(
        e.screen_row_of_line(1),
        Some(5),
        "screen row mapping shifts by the box height"
    );
}

/// Box rows belong to no buffer line: a click inside the box maps to no
/// buffer position, and a click on the line below the box maps to that
/// line at its shifted screen row.
#[test]
fn clicks_map_through_comment_box_rows() {
    let mut e = editor_with("first\nsecond\nthird");
    e.comment_boxes = vec![CommentBox {
        id: 7,
        line: 0,
        author: "navigator".into(),
        body: "tighten this".into(),
    }];
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 10,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut e as &mut Editor).render(area, &mut buf);
    let text_x = e.last_inner.x + e.last_gutter_width + 1;
    assert_eq!(
        e.buffer_pos_at(text_x, 3),
        None,
        "a box row is not a buffer cell"
    );
    assert_eq!(
        e.buffer_pos_at(text_x, 5),
        Some((1, 0)),
        "the line below the box maps at its shifted row"
    );
}

/// The box's mouse surface: the footer's ✕ Ignore cells hit Ignore, the
/// rest of the footer hits Reply, body rows hit Body, and buffer-text
/// rows hit nothing.
#[test]
fn comment_box_hit_finds_reply_ignore_and_body() {
    let mut e = editor_with("first\nsecond\nthird");
    e.comment_boxes = vec![CommentBox {
        id: 7,
        line: 0,
        author: "navigator".into(),
        body: "tighten this".into(),
    }];
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 10,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut e as &mut Editor).render(area, &mut buf);
    // Find the ✕ on the footer row rather than hard-coding geometry.
    let ignore_x = (0..area.width)
        .find(|&x| buf[(x, 4)].symbol() == "\u{2715}")
        .expect("the footer renders the ✕ Ignore button");
    assert_eq!(
        e.comment_box_hit(ignore_x, 4),
        Some((7, CommentHit::Ignore))
    );
    let text_x = e.last_inner.x + e.last_gutter_width + 1;
    assert_eq!(
        e.comment_box_hit(text_x + 2, 4),
        Some((7, CommentHit::Reply)),
        "the rest of the footer is the reply field"
    );
    assert_eq!(
        e.comment_box_hit(text_x + 2, 3),
        Some((7, CommentHit::Body))
    );
    assert_eq!(e.comment_box_hit(text_x + 2, 1), None, "text rows miss");
    assert_eq!(e.comment_box_hit(text_x + 2, 5), None);
}

/// In wrap mode (visual-row cursor motion) the caret steps over a box:
/// down from line 0 lands on line 1, never inside the block.
#[test]
fn cursor_skips_comment_box_rows_in_wrap_mode() {
    let mut e = editor_with("first\nsecond\nthird");
    e.wrap_override = Some(true);
    e.comment_boxes = vec![CommentBox {
        id: 7,
        line: 0,
        author: "navigator".into(),
        body: "tighten this".into(),
    }];
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 10,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut e as &mut Editor).render(area, &mut buf);
    e.cursor_row = 0;
    e.cursor_col = 0;
    e.move_down();
    assert_eq!(e.cursor_row, 1, "the caret skips the box");
    e.move_up();
    assert_eq!(e.cursor_row, 0);
}

/// A comment box on a line hidden inside a collapsed fold is never
/// painted, so it must not count toward the scroll extent either: it
/// used to inflate the content length, keeping a scrollbar alive for
/// invisible content and letting the wheel scroll into blank space.
#[test]
fn a_box_hidden_by_a_fold_does_not_extend_the_scroll_geometry() {
    let mut e = editor_with("fn a() {\n    body();\n}\nlast");
    e.comment_boxes = vec![CommentBox {
        id: 1,
        line: 1,
        author: "navigator".into(),
        body: (0..20)
            .map(|i| format!("p{i}"))
            .collect::<Vec<_>>()
            .join("\n"),
    }];
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 10,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut e as &mut Editor).render(area, &mut buf);
    e.toggle_fold(0);
    assert!(e.is_line_hidden(1), "the box's line folds away");
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut e as &mut Editor).render(area, &mut buf);
    assert_eq!(
        e.last_scrollbar.width, 0,
        "4 lines minus a fold fit the viewport; nothing left to scroll"
    );
    e.scroll_down(3);
    assert_eq!(
        (e.scroll, e.scroll_sub),
        (0, 0),
        "scrolling must not walk into folded-away box rows"
    );
}

/// A comment box taller than the viewport must be reachable in non-wrap
/// mode: scrolling used to be line-granular (a 3-line file cannot
/// scroll at all), so a tall box was truncated at the viewport edge and
/// its footer - the Reply field and ✕ Ignore - could never be seen.
#[test]
fn nonwrap_scrolling_reaches_a_tall_comment_boxes_footer() {
    let mut e = editor_with("first\nsecond\nthird");
    e.comment_boxes = vec![CommentBox {
        id: 1,
        line: 0,
        author: "navigator".into(),
        body: (0..30)
            .map(|i| format!("point {i}"))
            .collect::<Vec<_>>()
            .join("\n"),
    }];
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 10,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut e as &mut Editor).render(area, &mut buf);
    let screen = |buf: &ratatui::buffer::Buffer| -> String {
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert!(
        !screen(&buf).contains("Ignore"),
        "the tall box overflows: its footer starts off-screen"
    );
    // Scroll far enough that the footer row must be inside the viewport.
    for _ in 0..12 {
        e.scroll_down(3);
    }
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut e as &mut Editor).render(area, &mut buf);
    assert!(
        screen(&buf).contains("Ignore"),
        "scrolling must reach the box footer; got:\n{}",
        screen(&buf)
    );
}

/// A reply longer than the footer field must window around the caret:
/// the frozen first-N-chars rendering meant typing past the field width
/// went blind - no caret anywhere and the new text never appearing.
#[test]
fn a_long_reply_windows_so_the_caret_and_tail_stay_visible() {
    let mut e = editor_with("first\nsecond\nthird");
    e.comment_boxes = vec![CommentBox {
        id: 7,
        line: 0,
        author: "navigator".into(),
        body: "tighten this".into(),
    }];
    let reply = "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ_TAIL";
    e.comment_focus = Some(CommentFocus {
        id: 7,
        reply: reply.into(),
        cursor: reply.chars().count(),
    });
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 10,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut e as &mut Editor).render(area, &mut buf);
    let footer: String = (0..area.width)
        .map(|x| buf[(x, 4)].symbol().to_string())
        .collect();
    assert!(
        footer.contains("_TAIL"),
        "the field must window to keep the tail under the caret visible: {footer:?}"
    );
    let caret_cells = (0..area.width)
        .filter(|&x| {
            buf[(x, 4)]
                .style()
                .add_modifier
                .contains(ratatui::style::Modifier::REVERSED)
        })
        .count();
    assert_eq!(caret_cells, 1, "exactly one caret cell renders: {footer:?}");
}

/// The focused box renders the typed reply in its footer field.
#[test]
fn focused_comment_box_renders_the_reply_draft() {
    let mut e = editor_with("first\nsecond\nthird");
    e.comment_boxes = vec![CommentBox {
        id: 7,
        line: 0,
        author: "navigator".into(),
        body: "tighten this".into(),
    }];
    e.comment_focus = Some(CommentFocus {
        id: 7,
        reply: String::from("why here?"),
        cursor: 9,
    });
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 10,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut e as &mut Editor).render(area, &mut buf);
    let footer: String = (0..area.width)
        .map(|x| buf[(x, 4)].symbol().to_string())
        .collect();
    assert!(
        footer.contains("why here?"),
        "the reply draft renders in the footer: {footer:?}"
    );
}

#[test]
fn find_highlights_stay_readable_over_occurrence_tints() {
    // Cmd+F then Enter parks the caret ON a match; the idle-caret
    // documentHighlight then returns every occurrence of that word —
    // exactly the cells the find layer just painted black-on-gold. The
    // find layer must stay on top: an occurrence bg painted over it
    // keeps the BLACK foreground on a dark grey, which is unreadable,
    // and kills the orange active-match cue.
    let mut e = editor_with("config here");
    // The boxed themes' black-on-gold block; Croft Loch's twin is
    // `croft_loch_find_tints_stay_on_top_of_occurrence_tints`.
    e.theme = crate::theme::Theme::BLACK;
    e.path = Some(PathBuf::from("/x/a.rs"));
    e.set_search_highlight(Some(String::from("config")));
    e.occurrences = vec![(0, 0, 6, false)];
    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 5,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut e as &mut Editor).render(area, &mut buf);
    let gold = Color::Rgb(0xff, 0xd7, 0x4a);
    let occ = crate::theme::Theme::BLACK.occurrence_bg();
    let mut gold_cells = 0;
    for y in 0..area.height {
        for x in 0..area.width {
            let cell = &buf[(x, y)];
            if cell.bg == gold {
                gold_cells += 1;
            }
            assert!(
                !(cell.fg == Color::Black && cell.bg == occ),
                "cell ({x},{y}) '{}' wears the find layer's black fg on the occurrence bg — unreadable",
                cell.symbol()
            );
        }
    }
    assert!(
        gold_cells >= 6,
        "the find highlight must survive the occurrence pass (got {gold_cells} gold cells)"
    );
}

#[test]
fn a_claimed_sign_cell_does_not_hit_test_as_the_play_bead() {
    // Render gives the sign cell to the stop arrow / breakpoint glyph /
    // AI-stream square before the play bead; the CLICK hit-test must
    // agree, or the natural gesture of clicking a red ● (VS Code toggles
    // a breakpoint there) silently starts a test run instead.
    let mut e = editor_with("#[test]\nfn my_case() {}");
    e.path = Some(PathBuf::from("/x/a.rs"));
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 6,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut e as &mut Editor).render(area, &mut buf);
    let sign_x = e.last_inner.x;
    assert_eq!(e.test_glyph_at(sign_x, 2).as_deref(), Some("my_case"));
    e.toggle_breakpoint_line(2);
    assert_eq!(
        e.test_glyph_at(sign_x, 2),
        None,
        "the breakpoint dot's cell must not run tests on click"
    );
    e.toggle_breakpoint_line(2);
    assert_eq!(
        e.test_glyph_at(sign_x, 2).as_deref(),
        Some("my_case"),
        "removing the dot hands the cell back to the bead"
    );
    e.stop_line = Some((e.path.clone().unwrap(), 2));
    assert_eq!(
        e.test_glyph_at(sign_x, 2),
        None,
        "the paused stop arrow owns its cell"
    );
    e.stop_line = None;
    e.stream_stop_line = Some(1);
    assert_eq!(
        e.test_glyph_at(sign_x, 2),
        None,
        "the AI-stream stop square owns its cell"
    );
}

#[test]
fn gutter_play_glyph_yields_to_a_breakpoint_dot() {
    let mut e = editor_with("#[test]\nfn my_case() {}");
    let p = std::path::PathBuf::from("/tmp/bp.rs");
    e.path = Some(p.clone());
    e.breakpoints.entry(p).or_default().insert(2); // 1-based: the fn line
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 5,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut e as &mut Editor).render(area, &mut buf);
    assert_eq!(
        buf[(e.last_inner.x, 2)].symbol(),
        "●",
        "a breakpoint on the fn line owns the shared sign cell"
    );
}

fn occ(
    start_line: u32,
    start_char: u32,
    end_line: u32,
    end_char: u32,
    write: bool,
) -> crate::lsp::manager::OccurrenceItem {
    crate::lsp::manager::OccurrenceItem {
        start_line,
        start_char,
        end_line,
        end_char,
        write,
    }
}

#[test]
fn lsp_occurrences_paint_theme_tints_with_utf16_columns_and_clear_on_edit() {
    // "a𐐀x = x;" — 𐐀 is one char but two UTF-16 code units, so the
    // server's UTF-16 columns for the second `x` (units 7..8) land on
    // char column 6, one LESS than a naive unit-as-char mapping.
    let mut e = editor_with("a𐐀x = x;");
    let p = std::path::PathBuf::from("/tmp/occ.rs");
    e.path = Some(p.clone());
    e.apply_occurrences(vec![occ(0, 0, 0, 4, true), occ(0, 7, 0, 8, false)]);
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 5,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut e as &mut Editor).render(area, &mut buf);
    let write_bg = e.theme.occurrence_write_bg();
    let read_bg = e.theme.occurrence_bg();
    let a_x = (0..area.width)
        .find(|&x| buf[(x, 1)].symbol() == "a")
        .expect("the identifier must be on the row");
    for dx in 0..3 {
        assert_eq!(
            buf[(a_x + dx, 1)].bg,
            write_bg,
            "the write occurrence must tint all three identifier chars (dx={dx})"
        );
    }
    let second_x = (a_x + 3..area.width)
        .find(|&x| buf[(x, 1)].symbol() == "x")
        .expect("the second x must be on the row");
    assert_eq!(
        buf[(second_x, 1)].bg,
        read_bg,
        "the read occurrence must tint exactly the second x"
    );
    assert_ne!(
        buf[(second_x - 1, 1)].bg,
        read_bg,
        "the cell before the second x must stay untinted"
    );
    // Any edit invalidates the server's columns: the tints must vanish
    // until the app's next idle-cursor request answers.
    e.mark_buffer_changed();
    let mut buf2 = ratatui::buffer::Buffer::empty(area);
    (&mut e as &mut Editor).render(area, &mut buf2);
    assert_ne!(
        buf2[(second_x, 1)].bg,
        read_bg,
        "an edit must clear the occurrence tints"
    );
}

#[test]
fn inlay_hints_for_another_file_do_not_paint() {
    let mut e = editor_with("let x = f(y);");
    e.path = Some(std::path::PathBuf::from("/tmp/current.rs"));
    e.apply_inlay_hints(
        std::path::PathBuf::from("/tmp/other.rs"),
        vec![inlay(0, 5, ": i32")],
    );
    let row = first_row_text(&mut e);
    assert!(
        row.contains("let x = f(y);") && !row.contains("i32"),
        "hints for a different file must not paint; got {row:?}"
    );
}

#[test]
fn wrap_mode_suppresses_inlay_hints() {
    let mut e = editor_with("let x = f(y);");
    let p = std::path::PathBuf::from("/tmp/hints.rs");
    e.path = Some(p.clone());
    e.apply_inlay_hints(p, vec![inlay(0, 5, ": i32")]);
    e.wrap_override = Some(true);
    let row = first_row_text(&mut e);
    assert!(
        row.contains("let x = f(y);") && !row.contains("i32"),
        "wrap mode must render the raw text unshifted; got {row:?}"
    );
}

#[test]
fn cursor_screen_pos_shifts_past_inlay_hints() {
    let mut e = editor_with("let x = f(y);");
    let p = std::path::PathBuf::from("/tmp/hints.rs");
    e.path = Some(p.clone());
    e.apply_inlay_hints(p, vec![inlay(0, 5, ": i32")]);
    e.focused = true;
    let _ = first_row_text(&mut e);
    let text_x = e.last_inner.x + e.last_gutter_width + 1;
    // Caret on the `=` (buffer col 6): five hint cells sit before it.
    e.cursor_row = 0;
    e.cursor_col = 6;
    assert_eq!(
        e.cursor_screen_pos(),
        Some((text_x + 11, e.last_inner.y)),
        "the caret must account for hint cells before it"
    );
    // Caret exactly at the hint's anchor (buffer col 5) stays LEFT of the
    // hint, like VS Code: typing there pushes the hint right.
    e.cursor_col = 5;
    assert_eq!(
        e.cursor_screen_pos(),
        Some((text_x + 5, e.last_inner.y)),
        "the caret at the anchor must sit before the hint"
    );
}

#[test]
fn secondary_and_ghost_carets_sit_left_of_an_inlay_hint_like_the_primary() {
    // Change All Occurrences puts every extra caret at a word end —
    // exactly where rust-analyzer anchors a binding's type hint. The
    // primary caret deliberately sits LEFT of such a hint (see
    // cursor_screen_pos_shifts_past_inlay_hints); the block painted for
    // a secondary caret, and a collaborator's ghost caret, must agree
    // instead of landing a hint-width to the right, outside their own
    // selection band.
    let mut e = editor_with("let x = f(y);");
    let p = std::path::PathBuf::from("/tmp/hints.rs");
    e.path = Some(p.clone());
    e.apply_inlay_hints(p, vec![inlay(0, 5, ": i32")]);
    e.focused = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 5,
    };

    // Secondary caret at the hint's anchor (buffer col 5).
    e.carets = vec![EditorSelection::new(0, 5)];
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut e as &mut Editor).render(area, &mut buf);
    let text_x = e.last_inner.x + e.last_gutter_width + 1;
    let caret_bg = e.theme.secondary_caret_bg();
    let block_x = (0..area.width).find(|&x| buf[(x, 1)].bg == caret_bg);
    assert_eq!(
        block_x,
        Some(text_x + 5),
        "the secondary caret's block must sit where the primary would"
    );

    // Ghost caret at the same anchor, painted in the participant color.
    e.carets.clear();
    e.ghost_carets = vec![(0, 5, Color::Red)];
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut e as &mut Editor).render(area, &mut buf);
    let ghost_x = (0..area.width).find(|&x| buf[(x, 1)].bg == Color::Red);
    assert_eq!(
        ghost_x,
        Some(text_x + 5),
        "a collaborator's ghost caret must sit where the primary would"
    );
}

#[test]
fn click_and_word_select_map_through_inlay_hints() {
    // Repro: with a type hint spliced into the row, the caret / selection
    // landed `hint` cells right of the pointer while hover (which goes
    // through `buffer_pos_at`) resolved the symbol under it correctly.
    let mut e = editor_with("let x = f(y);");
    let p = std::path::PathBuf::from("/tmp/hints.rs");
    e.path = Some(p.clone());
    e.apply_inlay_hints(p, vec![inlay(0, 5, ": i32")]);
    let _ = first_row_text(&mut e);
    let text_x = e.last_inner.x + e.last_gutter_width + 1;
    let y = e.last_inner.y;

    // `f` displays at col 13 (buffer col 8 plus the five hint cells).
    e.click(text_x + 13, y);
    assert_eq!(
        (e.cursor_row, e.cursor_col),
        (0, 8),
        "the caret must land where the pointer is, not past the hint"
    );

    // Double-click on the shifted `y` selects `y`, not a neighbour.
    e.select_word_at(text_x + 15, y);
    assert_eq!(
        e.selection.map(|s| (s.anchor, s.head)),
        Some(((0, 10), (0, 11))),
        "word select must resolve the word under the pointer"
    );
}

#[test]
fn buffer_pos_at_maps_clicks_through_inlay_hints() {
    let mut e = editor_with("let x = f(y);");
    let p = std::path::PathBuf::from("/tmp/hints.rs");
    e.path = Some(p.clone());
    e.apply_inlay_hints(p, vec![inlay(0, 5, ": i32")]);
    let _ = first_row_text(&mut e);
    let text_x = e.last_inner.x + e.last_gutter_width + 1;
    let y = e.last_inner.y;
    // A click on the shifted `=` (display col 11) lands on buffer col 6.
    assert_eq!(e.buffer_pos_at(text_x + 11, y), Some((0, 6)));
    // A click inside the hint's cells snaps to its anchor column.
    assert_eq!(e.buffer_pos_at(text_x + 7, y), Some((0, 5)));
    // Cells before the hint are unaffected.
    assert_eq!(e.buffer_pos_at(text_x + 2, y), Some((0, 2)));
}

#[test]
fn opening_a_file_drops_the_previous_files_inlay_hints() {
    let mut e = editor_with("let x = f(y);");
    let p = std::path::PathBuf::from("/tmp/hints.rs");
    e.path = Some(p.clone());
    e.apply_inlay_hints(p, vec![inlay(0, 5, ": i32")]);
    let dir = std::env::temp_dir().join("croft_inlay_open_test");
    std::fs::create_dir_all(&dir).unwrap();
    let other = dir.join("other.rs");
    std::fs::write(&other, "fn g() {}\n").unwrap();
    e.open(&other).unwrap();
    let row = first_row_text(&mut e);
    assert!(
        !row.contains("i32"),
        "a newly opened file must not inherit stale hints; got {row:?}"
    );
}

#[test]
fn markdown_preview_toggles_renders_and_returns_to_source() {
    let mut e = editor_with("# Title\n\nSome body text.");
    e.lang = Some(LangKind::Markdown);
    assert!(e.toggle_markdown_preview(), "a Markdown tab must toggle");
    let text = first_row_screen(&mut e, 60, 8);
    assert!(
        text.contains("Title") && !text.contains("# Title"),
        "the preview must render the heading without its # marker; got:\n{text}"
    );
    assert!(text.contains("Some body text."), "got:\n{text}");
    assert!(e.toggle_markdown_preview(), "toggling again returns");
    assert!(e.markdown_preview.is_none());
    let text = first_row_screen(&mut e, 60, 8);
    assert!(
        text.contains("# Title"),
        "the source view must show the raw markdown again; got:\n{text}"
    );
}

#[test]
fn markdown_preview_opens_where_the_source_was_and_returns_there() {
    // #619: the preview scrolls to the source's top line, and toggling
    // back puts the source at the preview's top line.
    let mut body = String::from("# Top\n\n");
    for i in 0..40 {
        body.push_str(&format!("para {i}\n\n"));
    }
    body.push_str("## Deep heading\n\n");
    for i in 0..20 {
        body.push_str(&format!("after {i}\n\n"));
    }
    let mut e = editor_with(&body);
    e.lang = Some(LangKind::Markdown);
    let deep = e.lines.iter().position(|l| l == "## Deep heading").unwrap();
    e.scroll = deep;
    assert!(e.toggle_markdown_preview());
    let text = first_row_screen(&mut e, 60, 8);
    // The first painted content row, inside the pane border.
    let top = text
        .lines()
        .skip(1)
        .find(|l| {
            !l.trim_matches(|c: char| c == '\u{2502}' || c.is_whitespace())
                .is_empty()
        })
        .unwrap_or("");
    assert!(top.contains("Deep heading"), "preview top: {top:?}\n{text}");
    e.scroll = 0;
    assert!(e.toggle_markdown_preview(), "back to source");
    assert_eq!(e.scroll, deep, "the source comes back to the same place");
}

#[test]
fn markdown_preview_refuses_non_markdown_tabs() {
    let mut e = editor_with("fn main() {}");
    e.lang = Some(LangKind::Rust);
    assert!(!e.toggle_markdown_preview());
    assert!(e.markdown_preview.is_none());
}

#[test]
fn markdown_preview_rebuilds_when_the_buffer_moves() {
    let mut e = editor_with("# Old heading");
    e.lang = Some(LangKind::Markdown);
    assert!(e.toggle_markdown_preview());
    e.lines[0] = String::from("# New heading");
    e.edit_seq = e.edit_seq.wrapping_add(1);
    let text = first_row_screen(&mut e, 60, 8);
    assert!(
        text.contains("New heading") && !text.contains("Old heading"),
        "a stale preview must rebuild against the edited buffer; got:\n{text}"
    );
}

/// #1605: the same-path `open` behind every FS-sync reload (a save in
/// another pane, an agent rewriting the file) keeps the tab rendered,
/// showing the new text; another file still opens as source.
#[test]
fn markdown_preview_survives_a_same_path_reload() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("doc.md");
    std::fs::write(&path, "# Old heading\n").unwrap();
    let mut e = Editor::new();
    e.open(&path).unwrap();
    assert!(e.toggle_markdown_preview());
    std::fs::write(&path, "# Rewritten heading\n\nAnd a paragraph.\n").unwrap();
    e.open(&path).unwrap();
    assert!(
        e.markdown_preview.is_some(),
        "a reload of the same file must stay a preview"
    );
    let text = first_row_screen(&mut e, 60, 8);
    assert!(
        text.contains("Rewritten heading") && !text.contains("# Rewritten"),
        "the preview must render the reloaded text; got:\n{text}"
    );
    let other = tmp.path().join("other.md");
    std::fs::write(&other, "# Other\n").unwrap();
    e.open(&other).unwrap();
    assert!(e.markdown_preview.is_none(), "another file opens as source");
}

/// #1605 review: a preview rebuilt from another pane's text keeps its own
/// `built_seq`, so its row caches must still be rebuilt, or scroll sync,
/// link and play-glyph clicks use the first build's rows.
#[test]
fn a_preview_rebuilt_from_another_pane_refreshes_its_row_caches() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("doc.md");
    std::fs::write(&path, "# Title\n").unwrap();
    let mut e = Editor::new();
    e.open(&path).unwrap();
    assert!(e.toggle_markdown_preview());
    first_row_screen(&mut e, 60, 20);
    let typed = "# Title\n\nA new paragraph.\n\n```sh\necho hi\n```\n";
    e.rebuild_markdown_preview_from(typed, Some((7, 7)));
    first_row_screen(&mut e, 60, 20);
    let md = e.markdown_preview.as_ref().unwrap();
    assert_eq!(md.row_of_line.len(), md.lines.len(), "scroll-sync rows");
    assert_eq!(md.runnables.len(), 1);
    assert_eq!(md.run_rows.len(), 1, "the new block's play glyph has a row");
}

/// Render into a fresh buffer and return the whole screen as one string.
fn first_row_screen(e: &mut Editor, width: u16, height: u16) -> String {
    let area = Rect {
        x: 0,
        y: 0,
        width,
        height,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (e as &mut Editor).render(area, &mut buf);
    let mut out = String::new();
    for y in 0..height {
        for x in 0..width {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

#[test]
fn range_batch_does_not_overwrite_a_full_batch_for_the_same_file() {
    // The viewport range request races the whole-document request on open.
    // If a range reply lands AFTER the full reply, it must be dropped so it
    // cannot blank the off-screen colour the full reply painted.
    let mut e = editor_with("def f(x):\n    return x\n");
    let p = std::path::PathBuf::from("/tmp/sem_guard.py");
    e.path = Some(p.clone());
    let legend = std::sync::Arc::new(vec!["parameter".to_string()]);
    // Full batch: signature param (line 0) AND body reference (line 1).
    let full = vec![0, 6, 1, 0, 0, 1, 11, 1, 0, 0];
    e.apply_semantic_tokens(p.clone(), full, legend.clone(), true);
    let body_spans = e.semantic_overlay_for_test()[1].len();
    assert!(body_spans > 0, "full batch must colour the body reference");

    // A late range batch covering only line 0 must be rejected.
    let range_only_line0 = vec![0, 6, 1, 0, 0];
    e.apply_semantic_tokens(p.clone(), range_only_line0, legend, false);
    assert_eq!(
        e.semantic_overlay_for_test()[1].len(),
        body_spans,
        "a range batch must not erase off-screen colour from the full batch"
    );
}

#[test]
fn range_batch_paints_first_then_full_replaces() {
    // The intended first-paint order: range colours the viewport, then the
    // full reply arrives and extends colour to off-screen lines.
    let mut e = editor_with("def f(x):\n    return x\n");
    let p = std::path::PathBuf::from("/tmp/sem_first.py");
    e.path = Some(p.clone());
    let legend = std::sync::Arc::new(vec!["parameter".to_string()]);
    // Range first: only line 0 (the viewport) is coloured.
    e.apply_semantic_tokens(p.clone(), vec![0, 6, 1, 0, 0], legend.clone(), false);
    let line1_before = e
        .semantic_overlay_for_test()
        .get(1)
        .map(|s| s.len())
        .unwrap_or(0);
    assert_eq!(
        line1_before, 0,
        "range batch leaves the off-screen line bare"
    );
    // Full reply lands and colours the body reference on line 1.
    e.apply_semantic_tokens(p.clone(), vec![0, 6, 1, 0, 0, 1, 11, 1, 0, 0], legend, true);
    assert!(
        !e.semantic_overlay_for_test()[1].is_empty(),
        "full reply must colour the previously-bare off-screen line"
    );
}

/// #1558: plain-text matching finds `oo` inside words, left to right
/// without overlapping, and nothing for an empty or longer needle.
#[test]
fn find_text_occurrences_matches_inside_words() {
    let chars: Vec<char> = "foo food oooo".chars().collect();
    let oo: Vec<char> = "oo".chars().collect();
    assert_eq!(find_text_occurrences(&chars, &oo), vec![1, 5, 9, 11]);
    assert_eq!(find_word_occurrences(&chars, &oo), Vec::<usize>::new());
    assert!(find_text_occurrences(&chars, &[]).is_empty());
    let long: Vec<char> = "foo food oooo!".chars().collect();
    assert!(find_text_occurrences(&chars, &long).is_empty());
}

#[test]
fn find_word_occurrences_matches_whole_words_only() {
    let chars: Vec<char> = "foo foobar foo_ foo".chars().collect();
    let word: Vec<char> = "foo".chars().collect();
    // Only the standalone "foo" at col 0 and the final "foo" qualify;
    // "foobar" and "foo_" are larger identifiers.
    assert_eq!(find_word_occurrences(&chars, &word), vec![0, 16]);
}

#[test]
fn select_all_occurrences_picks_cursor_match_as_primary() {
    let mut e = editor_with("foo bar foo\nfoo");
    e.cursor_row = 0;
    e.cursor_col = 8; // second "foo" on line 0
    let n = e.select_all_occurrences_of_word_at_cursor();
    assert_eq!(n, 3);
    // Primary selection covers the occurrence under the cursor.
    assert_eq!(e.selection.unwrap().normalised(), ((0, 8), (0, 11)));
    // The other two become secondary carets.
    assert_eq!(e.carets.len(), 2);
    assert!(e.has_multi_cursor());
}

#[test]
fn multi_insert_replaces_every_occurrence_in_one_undo_step() {
    let mut e = editor_with("foo bar foo\nfoo");
    e.cursor_row = 0;
    e.cursor_col = 0;
    assert_eq!(e.select_all_occurrences_of_word_at_cursor(), 3);
    e.multi_insert_char('X');
    assert_eq!(e.lines, vec!["X bar X".to_string(), "X".to_string()]);
    // One undo restores the whole batch at once.
    assert!(e.undo());
    assert_eq!(e.lines, vec!["foo bar foo".to_string(), "foo".to_string()]);
}

/// #913: a word typed at several carets undoes in one step, as one
/// caret's typing burst does, and brings the selections back.
#[test]
fn multi_caret_typing_burst_undoes_in_one_step() {
    let mut e = editor_with("total = 0\ntotal += 5\nprint(total)");
    for _ in 0..3 {
        e.select_next_occurrence();
    }
    let (selection, carets) = (e.selection, e.carets.clone());
    assert_eq!(carets.len(), 2);
    for c in "grand".chars() {
        e.multi_insert_char(c);
    }
    assert_eq!(e.lines, ["grand = 0", "grand += 5", "print(grand)"]);
    e.collapse_carets();
    assert!(e.undo());
    assert_eq!(e.lines, ["total = 0", "total += 5", "print(total)"]);
    assert_eq!(e.selection, selection);
    assert_eq!(e.carets, carets);
    assert!(!e.undo(), "the rename was the only step");
}

/// #913: the burst ends at a Backspace and when the carets change, as
/// one caret's ends at a Backspace or a move.
#[test]
fn multi_caret_typing_burst_ends_at_backspace_or_caret_change() {
    let mut e = editor_with("a\nb");
    e.cursor_col = 1;
    e.carets = vec![EditorSelection::new(1, 1)];
    e.multi_insert_char('x');
    e.multi_insert_char('y');
    e.multi_backspace();
    e.multi_insert_char('z');
    assert_eq!(e.lines, ["axz", "bxz"]);
    e.carets = vec![EditorSelection::new(1, 0)];
    e.multi_insert_char('w');
    assert_eq!(e.lines, ["axzw", "wbxz"]);
    let mut undone = Vec::new();
    while e.undo() {
        undone.push(e.lines.join("|"));
    }
    assert_eq!(undone, ["axz|bxz", "ax|bx", "axy|bxy", "a|b"]);
}

/// #913: collapsing the carets and adding them back where they were
/// (Cmd+Alt+Down does both) ends the burst like any caret command, so
/// the typing either side of it undoes in two steps.
#[test]
fn multi_caret_typing_burst_ends_when_the_carets_are_rebuilt_in_place() {
    let mut e = editor_with("a\nb");
    e.cursor_col = 1;
    e.add_cursor_below();
    e.multi_insert_char('x');
    e.collapse_carets();
    e.add_cursor_below();
    e.multi_insert_char('y');
    assert_eq!(e.lines, ["axy", "bxy"]);
    assert!(e.undo());
    assert_eq!(e.lines, ["ax", "bx"]);
    assert!(e.undo());
    assert_eq!(e.lines, ["a", "b"]);
}

#[test]
fn carets_on_one_line_keep_their_places_across_keystrokes() {
    let mut e = editor_with("abcd");
    e.cursor_row = 0;
    e.cursor_col = 1;
    e.carets = vec![EditorSelection::new(0, 3)];
    e.multi_insert_char('X');
    e.multi_insert_char('Y');
    assert_eq!(e.lines, vec!["aXYbcXYd".to_string()]);
    // A backspace that joins lines moves the carets below it up.
    let mut e = editor_with("ab\ncd\nef");
    e.cursor_row = 1;
    e.cursor_col = 0;
    e.carets = vec![EditorSelection::new(2, 1)];
    e.multi_backspace();
    e.multi_insert_char('X');
    assert_eq!(e.lines, vec!["abXcd".to_string(), "Xf".to_string()]);
}

#[test]
fn multi_backspace_deletes_at_every_caret() {
    let mut e = editor_with("ab ab ab");
    e.cursor_row = 0;
    e.cursor_col = 0;
    assert_eq!(e.select_all_occurrences_of_word_at_cursor(), 3);
    // Selections are active; backspace deletes each selected "ab".
    e.multi_backspace();
    assert_eq!(e.lines, vec!["  ".to_string()]);
}

fn three_foos() -> Editor {
    editor_with("foo = 1\nfoo = 2\nfoo = 3")
}

fn carets_after_foo(e: &mut Editor) {
    e.cursor_row = 0;
    e.cursor_col = 3;
    e.carets = vec![EditorSelection::new(1, 3), EditorSelection::new(2, 3)];
}

/// #1597: a quote typed over several Ctrl+D selections wraps each one
/// and keeps it selected, in one undo step, instead of replacing them.
#[test]
fn a_quote_wraps_every_selection() {
    let mut e = three_foos();
    e.cursor_col = 0;
    assert_eq!(e.select_all_occurrences_of_word_at_cursor(), 3);
    e.multi_insert_char('"');
    assert_eq!(e.lines, vec!["\"foo\" = 1", "\"foo\" = 2", "\"foo\" = 3"]);
    let selected: Vec<String> = e
        .selection
        .iter()
        .chain(e.carets.iter())
        .filter(|s| s.has_area())
        .map(|s| {
            let ((r, a), (_, b)) = s.normalised();
            e.lines[r].chars().skip(a).take(b - a).collect()
        })
        .collect();
    assert_eq!(selected, vec!["foo", "foo", "foo"], "still selected");
    e.multi_insert_char('(');
    assert_eq!(e.lines[2], "\"(foo)\" = 3", "and wrap again");
    assert!(e.undo());
    assert!(e.undo());
    assert_eq!(e.lines, vec!["foo = 1", "foo = 2", "foo = 3"]);
}

/// #1597: an opener typed at several carets closes at each, the carets
/// between the pairs.
#[test]
fn an_opener_closes_at_every_caret() {
    let mut e = three_foos();
    carets_after_foo(&mut e);
    e.multi_insert_char('[');
    assert_eq!(e.lines, vec!["foo[] = 1", "foo[] = 2", "foo[] = 3"]);
    e.multi_insert_char('0');
    assert_eq!(e.lines, vec!["foo[0] = 1", "foo[0] = 2", "foo[0] = 3"]);
}

/// #1597: a closer typed before the same closer steps over it at every
/// caret.
#[test]
fn a_closer_types_over_at_every_caret() {
    let mut e = editor_with("print(x)\nprint(y)");
    e.cursor_col = 7;
    e.carets = vec![EditorSelection::new(1, 7)];
    e.multi_insert_char(')');
    assert_eq!(e.lines, vec!["print(x)", "print(y)"]);
    assert_eq!((e.cursor_row, e.cursor_col), (0, 8));
    assert_eq!(e.carets, vec![EditorSelection::new(1, 8)]);
}

/// #1597: Backspace right after pairing at several carets takes both
/// sides of every empty pair.
#[test]
fn backspace_after_pairing_at_every_caret_takes_both_sides() {
    let mut e = three_foos();
    carets_after_foo(&mut e);
    e.multi_insert_char('(');
    e.multi_backspace();
    assert_eq!(e.lines, vec!["foo = 1", "foo = 2", "foo = 3"]);
}

/// Negative (#1597): a pair already in the file keeps its closer when
/// Backspace runs at several carets inside it.
#[test]
fn backspace_at_every_caret_keeps_a_pair_already_there() {
    let mut e = editor_with("f()\ng()");
    e.cursor_col = 2;
    e.carets = vec![EditorSelection::new(1, 2)];
    e.multi_backspace();
    assert_eq!(e.lines, vec!["f)", "g)"]);
}

/// Negative (#1597): a quote that pairs at one caret but steps over a
/// quote already in the file at another arms only the caret that
/// paired; the next Backspace keeps the other's pre-existing quote.
#[test]
fn backspace_after_mixed_pairing_keeps_the_pair_already_there() {
    let mut e = editor_with("\n\"\"");
    e.cursor_col = 0;
    e.carets = vec![EditorSelection::new(1, 0)];
    e.multi_insert_char('"');
    assert_eq!(e.lines, vec!["\"\"", "\"\""]);
    e.multi_backspace();
    assert_eq!(e.lines, vec!["", "\""]);
}

/// #1597: stepping over the closer at every caret records no undo step,
/// so one Undo reverts the edit before it.
#[test]
fn type_over_at_every_caret_records_no_undo_step() {
    let mut e = editor_with("print(x)\nprint(y)");
    e.cursor_col = 7;
    e.carets = vec![EditorSelection::new(1, 7)];
    e.multi_insert_char('z');
    e.multi_insert_char(')');
    assert_eq!(e.lines, vec!["print(xz)", "print(yz)"]);
    assert!(e.undo());
    assert_eq!(e.lines, vec!["print(x)", "print(y)"]);
}

/// Negative (#1597): carets moved after the pairing disarm the pair
/// Backspace, so it deletes one character as usual.
#[test]
fn carets_moved_after_pairing_backspace_one_character() {
    let mut e = three_foos();
    carets_after_foo(&mut e);
    e.multi_insert_char('(');
    e.cursor_col = 6;
    e.carets = vec![EditorSelection::new(1, 6), EditorSelection::new(2, 6)];
    e.multi_backspace();
    assert_eq!(e.lines, vec!["foo()= 1", "foo()= 2", "foo()= 3"]);
}

/// Negative (#1597): with auto-closing pairs off, several carets type
/// the plain character.
#[test]
fn without_auto_close_every_caret_types_the_plain_character() {
    let mut e = three_foos();
    e.auto_close_pairs = false;
    carets_after_foo(&mut e);
    e.multi_insert_char('[');
    assert_eq!(e.lines, vec!["foo[ = 1", "foo[ = 2", "foo[ = 3"]);
}

/// Negative (#1597): a closer typed over several selections replaces
/// them, as it does one selection.
#[test]
fn a_closer_replaces_every_selection() {
    let mut e = three_foos();
    e.cursor_col = 0;
    e.select_all_occurrences_of_word_at_cursor();
    e.multi_insert_char(')');
    assert_eq!(e.lines, vec![") = 1", ") = 2", ") = 3"]);
}

#[test]
fn collapse_carets_drops_secondary_cursors() {
    let mut e = editor_with("x x x");
    e.cursor_col = 0;
    e.select_all_occurrences_of_word_at_cursor();
    assert!(e.has_multi_cursor());
    e.collapse_carets();
    assert!(!e.has_multi_cursor());
    assert!(e.carets.is_empty());
}

#[test]
fn apply_span_edits_applies_bottom_to_top() {
    let mut lines = vec!["foo bar foo".to_string()];
    let edits = vec![
        TextSpanEdit {
            start: (0, 0),
            end: (0, 3),
            new_text: "baz".to_string(),
            utf16: false,
        },
        TextSpanEdit {
            start: (0, 8),
            end: (0, 11),
            new_text: "baz".to_string(),
            utf16: false,
        },
    ];
    assert_eq!(apply_span_edits_to_lines(&mut lines, &edits), 2);
    assert_eq!(lines, vec!["baz bar baz".to_string()]);
}

/// An insert of `text` at `(row, col)`, in UTF-16 columns as a
/// language server sends it.
fn lsp_insert(row: usize, col: usize, text: &str) -> TextSpanEdit {
    TextSpanEdit {
        start: (row, col),
        end: (row, col),
        new_text: text.to_string(),
        utf16: true,
    }
}

/// vtsls 0.3.0's Format Document reply for
/// `export function f(a: number) {if(a>1){return  0;}`, captured with a
/// minimal LSP client (#1633). The first two edits share a position.
fn vtsls_format_reply() -> Vec<TextSpanEdit> {
    let mut edits = vec![
        lsp_insert(0, 30, "\n"),
        lsp_insert(0, 30, "  "),
        lsp_insert(0, 32, " "),
        lsp_insert(0, 34, " "),
        lsp_insert(0, 35, " "),
        lsp_insert(0, 37, " "),
        lsp_insert(0, 38, " "),
    ];
    edits.push(TextSpanEdit {
        start: (0, 44),
        end: (0, 46),
        new_text: " ".to_string(),
        utf16: true,
    });
    edits.push(lsp_insert(0, 48, " "));
    edits
}

/// #1633: edits that start at one position land in the order the server
/// sent them, as the LSP spec says.
#[test]
fn inserts_at_one_position_land_in_the_order_sent() {
    let mut lines = vec!["ab".to_string()];
    let edits = vec![lsp_insert(0, 1, "\n"), lsp_insert(0, 1, "  ")];
    assert_eq!(apply_span_edits_to_lines(&mut lines, &edits), 2);
    assert_eq!(lines, vec!["a".to_string(), "  b".to_string()]);
    // An insert sent before a replacement of the text after it.
    let mut lines = vec!["foobar".to_string()];
    let edits = vec![
        lsp_insert(0, 0, "X"),
        TextSpanEdit {
            start: (0, 0),
            end: (0, 3),
            new_text: String::new(),
            utf16: true,
        },
    ];
    assert_eq!(apply_span_edits_to_lines(&mut lines, &edits), 2);
    assert_eq!(lines, vec!["Xbar".to_string()]);
}

/// #1633: the issue's TypeScript sample formats to an indented line with
/// no trailing spaces.
#[test]
fn a_vtsls_format_reply_indents_the_line_it_breaks() {
    let mut lines: Vec<String> = [
        "export function f(a: number) {if(a>1){return  0;}",
        "  return a;",
        "}",
    ]
    .map(String::from)
    .to_vec();
    assert_eq!(
        apply_span_edits_to_lines(&mut lines, &vtsls_format_reply()),
        9
    );
    assert_eq!(
        lines,
        [
            "export function f(a: number) {",
            "  if (a > 1) { return 0; }",
            "  return a;",
            "}"
        ]
        .map(String::from)
        .to_vec()
    );
}

/// #1633 negative: edits at different positions, and a replacement sent
/// before an insert at its start, keep their meaning.
#[test]
fn edits_at_different_positions_keep_their_places() {
    let mut lines = vec!["a b c".to_string()];
    let edits = vec![
        lsp_insert(0, 4, "Z"),
        lsp_insert(0, 0, "X"),
        lsp_insert(0, 2, "Y"),
    ];
    assert_eq!(apply_span_edits_to_lines(&mut lines, &edits), 3);
    assert_eq!(lines, vec!["Xa Yb Zc".to_string()]);
    let mut lines = vec!["foobar".to_string()];
    let edits = vec![
        TextSpanEdit {
            start: (0, 0),
            end: (0, 3),
            new_text: String::new(),
            utf16: true,
        },
        lsp_insert(0, 0, "X"),
    ];
    assert_eq!(apply_span_edits_to_lines(&mut lines, &edits), 2);
    assert_eq!(lines, vec!["Xbar".to_string()]);
}

#[test]
fn apply_span_edits_splits_multiline_new_text_into_separate_lines() {
    // Ruff's "Organize Imports" returns ONE edit whose new_text spans several
    // lines. The result must become several Vec entries, not one line with
    // embedded '\n' (the bug that mashed the imports onto a single line).
    let mut lines = vec![
        "import typing".to_string(),
        "import pandas".to_string(),
        "import logging".to_string(),
        "import re".to_string(),
    ];
    let edits = vec![TextSpanEdit {
        start: (0, 0),
        end: (3, 9),
        new_text: "import logging\nimport re\nimport typing\n\nimport pandas".to_string(),
        utf16: false,
    }];
    assert_eq!(apply_span_edits_to_lines(&mut lines, &edits), 1);
    assert_eq!(
        lines,
        vec![
            "import logging".to_string(),
            "import re".to_string(),
            "import typing".to_string(),
            String::new(),
            "import pandas".to_string(),
        ]
    );
}

#[test]
fn apply_span_edits_single_line_range_with_multiline_new_text() {
    // A same-line range whose new_text introduces a newline must split the
    // line in two, keeping the untouched prefix and suffix.
    let mut lines = vec!["abXYef".to_string()];
    let edits = vec![TextSpanEdit {
        start: (0, 2),
        end: (0, 4),
        new_text: "C\nD".to_string(),
        utf16: false,
    }];
    assert_eq!(apply_span_edits_to_lines(&mut lines, &edits), 1);
    assert_eq!(lines, vec!["abC".to_string(), "Def".to_string()]);
}

/// An insert of `text` at the end of row 0, as a remote span.
fn append_span(e: &Editor, text: &str) -> TextSpanEdit {
    let col = e.lines[0].chars().count();
    TextSpanEdit {
        start: (0, col),
        end: (0, col),
        new_text: text.to_string(),
        utf16: false,
    }
}

/// #1207: every span of one remote group (the fragments of an AI
/// stream) is one undo step.
#[test]
fn remote_spans_of_one_group_undo_as_one_step() {
    let mut e = editor_with("def add(a, b):");
    for frag in [" return", " a", " +", " b"] {
        let span = append_span(&e, frag);
        e.apply_remote_span_edits(&[span], 5);
    }
    assert_eq!(e.lines[0], "def add(a, b): return a + b");
    assert!(e.undo());
    assert_eq!(
        e.lines[0], "def add(a, b):",
        "one undo drops the whole group"
    );
    assert!(!e.undo(), "and it was the only step");
    assert!(e.redo());
    assert_eq!(e.lines[0], "def add(a, b): return a + b");
}

/// Negative (#1207): another group, a local keystroke in between, or
/// ungrouped span edits still open steps of their own.
#[test]
fn a_local_edit_or_another_group_splits_remote_steps() {
    let mut e = editor_with("x");
    let span = append_span(&e, "1");
    e.apply_remote_span_edits(&[span], 5);
    let span = append_span(&e, "2");
    e.apply_remote_span_edits(&[span], 6);
    e.cursor_col = e.lines[0].chars().count();
    e.insert_char('3');
    let span = append_span(&e, "4");
    e.apply_remote_span_edits(&[span], 6);
    for _ in 0..2 {
        let span = append_span(&e, "5");
        e.apply_span_edits(&[span]);
    }
    let mut seen = vec![e.lines[0].clone()];
    while e.undo() {
        seen.push(e.lines[0].clone());
    }
    assert_eq!(
        seen,
        ["x123455", "x12345", "x1234", "x123", "x12", "x1", "x"]
    );
}

/// #1207: a group whose edits restored the text it started from (a
/// cancelled stream's revert) leaves no undo step once it ends; one
/// that changed the text keeps its step.
#[test]
fn an_ended_group_that_restored_its_text_leaves_no_step() {
    let mut e = editor_with("return a - b");
    let span = append_span(&e, "\n    raise TypeError(");
    e.apply_remote_span_edits(&[span], 7);
    let len = e.lines[1].chars().count();
    let revert = TextSpanEdit {
        start: (0, 12),
        end: (1, len),
        new_text: String::new(),
        utf16: false,
    };
    e.apply_remote_span_edits(&[revert], 7);
    assert_eq!(e.lines, vec!["return a - b".to_string()]);
    e.end_remote_undo_group(7);
    assert!(
        !e.undo(),
        "nothing to undo: the stream left the text as it was"
    );

    let span = append_span(&e, " + c");
    e.apply_remote_span_edits(&[span], 8);
    e.end_remote_undo_group(8);
    let span = append_span(&e, " + d");
    e.apply_remote_span_edits(&[span], 8);
    assert!(e.undo());
    assert_eq!(e.lines[0], "return a - b + c", "an ended group is closed");
    assert!(e.undo());
    assert_eq!(e.lines[0], "return a - b");
}

/// #934: a delete, a new line and the typing after it, made inside an
/// undo group, undo as one step; the edit after the group is its own.
#[test]
fn an_undo_group_undoes_its_delete_newline_and_typing_at_once() {
    let mut e = editor_with("one two");
    e.begin_undo_group();
    for _ in 0..3 {
        e.delete_forward();
    }
    e.insert_char('A');
    e.insert_newline();
    e.insert_char('B');
    e.end_undo_group();
    e.insert_char('C');
    assert_eq!(e.lines, vec!["A", "BC two"]);
    assert!(e.undo());
    assert_eq!(e.lines, vec!["A", "B two"], "the edit after the group");
    assert!(e.undo());
    assert_eq!(e.lines, vec!["one two"], "the whole group at once");
    assert!(!e.undo());
}

/// Negative (#934): an undo inside a group, or a remote edit, ends the
/// step the group was filling; later edits open a new one.
#[test]
fn an_undo_or_a_peer_edit_inside_a_group_starts_a_new_step() {
    let mut e = editor_with("x");
    e.cursor_col = 1;
    e.insert_char('a');
    e.begin_undo_group();
    e.insert_char('1');
    e.insert_newline();
    assert!(e.undo());
    e.insert_char('2');
    let span = append_span(&e, "3");
    e.apply_remote_span_edits(&[span], 9);
    e.cursor_col = e.lines[0].chars().count();
    e.insert_char('4');
    e.end_undo_group();
    let mut seen = vec![e.lines.join("|")];
    while e.undo() {
        seen.push(e.lines.join("|"));
    }
    assert_eq!(seen, ["xa234", "xa23", "xa2", "xa", "x"]);
}

/// #919: a peer's edit opens no undo step. Undo reverts this user's own
/// typing and keeps the peer's lines, redo brings the typing back beside
/// them, and the restored caret sits where the typing was although the
/// peer added a line above it.
#[test]
fn undo_and_redo_keep_a_peers_edit() {
    let mut e = editor_with("def f():\n    return 1\n\ndef g():\n    return 2");
    e.cursor_row = 1;
    e.cursor_col = 12;
    e.insert_str("  # A-edit");
    assert_eq!(
        e.apply_peer_span_edit(&span((0, 0), (0, 0), "import os\n")),
        1
    );
    e.apply_peer_span_edit(&span((5, 12), (5, 12), "  # B-edit"));
    let typed = e.lines.clone();
    assert!(e.undo());
    assert_eq!(
        e.lines,
        [
            "import os",
            "def f():",
            "    return 1",
            "",
            "def g():",
            "    return 2  # B-edit"
        ],
        "only this user's typing is undone"
    );
    assert_eq!((e.cursor_row, e.cursor_col), (2, 12));
    assert!(!e.undo(), "the peer's edits are not this user's steps");
    assert!(e.redo());
    assert_eq!(e.lines, typed);
    // A peer edit while a step waits in redo lands in it too.
    assert!(e.undo());
    e.apply_peer_span_edit(&span((0, 0), (1, 0), ""));
    assert!(e.redo());
    assert_eq!(e.lines[0], "def f():");
    assert_eq!(e.lines[1], "    return 1  # A-edit");
}

/// #919: a peer's edit lands in every held state, so a large paste over
/// a deep history is trimmed back under the cap, not kept many times.
#[test]
fn a_peers_large_edit_keeps_the_history_under_its_cap() {
    let mut e = editor_with("x");
    for _ in 0..40 {
        e.insert_str("y");
        e.break_undo_coalescing();
    }
    let paste = "z".repeat(50 * 1024);
    e.apply_peer_span_edit(&span((0, 0), (0, 0), &paste));
    let (steps, bytes) = e.undo_history_size();
    assert!(
        bytes <= UNDO_BYTES_LIMIT,
        "{steps} steps hold {bytes} bytes"
    );
    assert!(e.undo_stack.len() >= UNDO_MIN_STEPS);
    assert!(e.undo());
    assert!(
        e.lines[0].starts_with(&paste),
        "the kept steps took the paste"
    );
}

fn span(start: (usize, usize), end: (usize, usize), new_text: &str) -> TextSpanEdit {
    TextSpanEdit {
        start,
        end,
        new_text: new_text.to_string(),
        utf16: false,
    }
}

/// #1603: lines added above the caret carry it down with its text.
#[test]
fn an_edit_adding_lines_above_the_caret_moves_it_down_with_its_text() {
    let mut e = editor_with("0\n1\n2\n3\n4\n\tname := strings.ToUpper()\n6");
    e.cursor_row = 5;
    e.cursor_col = 8;
    e.apply_span_edits(&[span((2, 0), (2, 0), "a\nb\nc\n")]);
    assert_eq!((e.cursor_row, e.cursor_col), (8, 8));
    e.insert_char('m');
    assert_eq!(e.lines[8], "\tname :=m strings.ToUpper()");
}

/// #1603: a longer name earlier on the caret's row moves its column,
/// once per occurrence before it.
#[test]
fn a_longer_name_earlier_on_the_caret_s_row_moves_its_column() {
    let mut e = editor_with("x = x + 1");
    e.cursor_col = 8;
    e.apply_span_edits(&[span((0, 0), (0, 1), "total"), span((0, 4), (0, 5), "total")]);
    assert_eq!(e.lines[0], "total = total + 1");
    assert_eq!((e.cursor_row, e.cursor_col), (0, 16));
}

/// #1603: columns a language server sends in UTF-16 units move the
/// caret by chars, past an emoji on its row.
#[test]
fn a_utf16_edit_before_the_caret_moves_it_by_chars() {
    let mut e = editor_with("😀x = 1");
    e.cursor_col = 5;
    let mut edit = span((0, 2), (0, 3), "total");
    edit.utf16 = true;
    e.apply_span_edits(&[edit]);
    assert_eq!(e.lines[0], "😀total = 1");
    assert_eq!(e.cursor_col, 9, "still before the 1");
}

/// #1603: a caret inside a deleted range lands where the range began.
#[test]
fn a_caret_inside_a_deleted_range_lands_at_its_start() {
    let mut e = editor_with("keep\nab-gone\ngone\ngone-cd\nlast");
    e.cursor_row = 2;
    e.cursor_col = 3;
    e.apply_span_edits(&[span((1, 2), (3, 5), "")]);
    assert_eq!(e.lines, vec!["keep", "abcd", "last"]);
    assert_eq!((e.cursor_row, e.cursor_col), (1, 2));
}

/// #1603: the selection and every extra caret follow their text too.
#[test]
fn the_selection_and_extra_carets_follow_an_edit_above_them() {
    let mut e = editor_with("top\nfoo\nbar\nbaz");
    e.cursor_row = 1;
    e.cursor_col = 3;
    e.selection = Some(EditorSelection {
        anchor: (1, 0),
        head: (1, 3),
    });
    e.carets = vec![EditorSelection::new(2, 1), EditorSelection::new(3, 2)];
    e.apply_span_edits(&[span((0, 0), (0, 0), "use x;\nuse y;\n")]);
    assert_eq!((e.cursor_row, e.cursor_col), (3, 3));
    assert_eq!(
        e.selection,
        Some(EditorSelection {
            anchor: (3, 0),
            head: (3, 3),
        })
    );
    assert_eq!(
        e.carets,
        vec![EditorSelection::new(4, 1), EditorSelection::new(5, 2)]
    );
}

/// #1603: a peer's lines arriving above the caret keep it on its text.
#[test]
fn a_remote_edit_above_the_caret_moves_it_with_its_text() {
    let mut e = editor_with("one\ntwo");
    e.cursor_row = 1;
    e.cursor_col = 2;
    e.apply_remote_span_edits(&[span((0, 0), (0, 0), "zero\n")], 3);
    assert_eq!((e.cursor_row, e.cursor_col), (2, 2));
}

/// #918: a peer's lines added or removed above the view move its top
/// line with them, so the text on screen stays where it was. Negative:
/// text inserted right at the top line, below it, or above a view at the
/// top of the file leaves the top where it is.
#[test]
fn a_remote_edit_above_the_view_keeps_the_text_on_screen() {
    let text: Vec<String> = (0..40).map(|i| format!("line {i}")).collect();
    let mut e = editor_with(&text.join("\n"));
    e.scroll = 20;
    e.cursor_row = 25;
    e.apply_remote_span_edits(&[span((3, 0), (3, 0), "a\nb\n")], 1);
    assert_eq!((e.scroll, e.cursor_row), (22, 27));
    assert_eq!(e.lines[e.scroll], "line 20");
    e.apply_remote_span_edits(&[span((2, 4), (5, 4), "")], 2);
    assert_eq!(e.scroll, 19);
    assert_eq!(e.lines[e.scroll], "line 20");
    e.apply_remote_span_edits(&[span((19, 0), (19, 0), "new\n")], 3);
    e.apply_remote_span_edits(&[span((30, 0), (30, 0), "x\ny\n")], 4);
    assert_eq!(e.scroll, 19);
    assert_eq!(e.lines[e.scroll], "new");
    e.scroll = 0;
    e.apply_remote_span_edits(&[span((0, 0), (0, 0), "import os\n")], 5);
    assert_eq!(e.scroll, 0);
}

/// Negative (#1603): edits after the caret, later on its row or on rows
/// below, leave it where it is.
#[test]
fn edits_after_the_caret_leave_it_where_it_is() {
    let mut e = editor_with("let a = b;\nlet c = d;");
    e.cursor_col = 4;
    e.apply_span_edits(&[
        span((0, 8), (0, 9), "bee\nbee"),
        span((1, 0), (1, 0), "// note\n"),
    ]);
    assert_eq!((e.cursor_row, e.cursor_col), (0, 4));
}

/// #1603: a formatter's one edit replacing the whole document leaves
/// the caret on its line, as before, not at the end of the file.
#[test]
fn a_whole_document_replacement_keeps_the_caret_on_its_line() {
    let mut e = editor_with("fn a() {\nlet x=1;\nlet y=2;\n}\nfn b() {}");
    e.cursor_row = 2;
    e.cursor_col = 4;
    e.apply_span_edits(&[span(
        (0, 0),
        (4, 9),
        "fn a() {\n    let x = 1;\n    let y = 2;\n}\nfn b() {}\n",
    )]);
    assert_eq!(e.lines.len(), 6);
    assert_eq!((e.cursor_row, e.cursor_col), (2, 4));
}

/// #1603: an out-of-range edit is skipped, so it must not move the
/// caret either; only the edit that landed does.
#[test]
fn a_skipped_out_of_range_edit_does_not_move_the_caret() {
    let mut e = editor_with("one\ntwo\nthree");
    e.cursor_row = 2;
    e.cursor_col = 1;
    let n = e.apply_span_edits(&[
        span((0, 0), (0, 0), "zero\n"),
        span((1, 0), (9, 0), "stale"),
    ]);
    assert_eq!(n, 1);
    assert_eq!(e.lines, vec!["zero", "one", "two", "three"]);
    assert_eq!((e.cursor_row, e.cursor_col), (3, 1));
}

/// Negative (#1603): a caret at the start of a replaced word stays at
/// its start, not carried to the end of the new name.
#[test]
fn a_caret_at_the_start_of_a_replaced_word_stays_there() {
    let mut e = editor_with("print(name)");
    e.cursor_col = 6;
    e.apply_span_edits(&[span((0, 6), (0, 10), "label")]);
    assert_eq!(e.lines[0], "print(label)");
    assert_eq!(e.cursor_col, 6);
}

#[test]
fn editor_apply_span_edits_marks_dirty_and_one_undo() {
    let mut e = editor_with("name = 1\nprint(name)");
    let edits = vec![
        TextSpanEdit {
            start: (0, 0),
            end: (0, 4),
            new_text: "label".to_string(),
            utf16: false,
        },
        TextSpanEdit {
            start: (1, 6),
            end: (1, 10),
            new_text: "label".to_string(),
            utf16: false,
        },
    ];
    assert_eq!(e.apply_span_edits(&edits), 2);
    assert!(e.dirty);
    assert_eq!(
        e.lines,
        vec!["label = 1".to_string(), "print(label)".to_string()]
    );
    assert!(e.undo());
    assert_eq!(
        e.lines,
        vec!["name = 1".to_string(), "print(name)".to_string()]
    );
}

#[test]
fn is_binary_detects_high_nontext_ratio() {
    let mut data = vec![0x01u8; 100];
    data.extend_from_slice(b"abc");
    assert!(is_binary(&data));
}

#[test]
fn is_binary_accepts_normal_text() {
    let txt = "fn main() { println!(\"hello\"); }\n// this is fine\n";
    assert!(!is_binary(txt.as_bytes()));
}

#[test]
fn open_loads_a_real_world_sized_log_file_above_the_old_5mb_cap() {
    // Regression for the user's "I can't open .croft/lsp.log" report:
    // the LSP log grew past 5MB during normal use (their copy was
    // 7.4MB) and the old cap silently bailed with "File too large".
    // The current cap MUST accommodate at least 8MB of ASCII text so
    // a typical LSP log session round-trips.
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("big.log");
    let line = "1778548312.915 lsp[ruff] stderr: 2026-05-12 02:11:52 INFO some workspace setting\n";
    let line_bytes = line.len();
    let target_bytes = 8 * 1024 * 1024;
    let line_count = target_bytes / line_bytes + 1;
    let mut buf = String::with_capacity(line_count * line_bytes);
    for _ in 0..line_count {
        buf.push_str(line);
    }
    std::fs::write(&path, &buf).unwrap();
    assert!(
        std::fs::metadata(&path).unwrap().len() > 5 * 1024 * 1024,
        "test setup must produce a file larger than the historical 5MB cap"
    );
    let mut e = Editor::new();
    e.open(&path)
        .expect("editor.open must accept an 8MB plain-text log; the user reported lsp.log (7.4MB) being unopenable");
    assert_eq!(e.lines.len(), line_count);
}

#[test]
fn line_char_len_counts_chars_not_bytes() {
    let mut e = editor_with("héllo");
    assert_eq!(e.line_char_len(0), 5);
    e.lines[0] = String::from("日本語");
    assert_eq!(e.line_char_len(0), 3);
}

// #1196: the caret and single-character deletes step by extended
// grapheme cluster, so a ZWJ emoji, a flag or a decomposed accent is
// never split.
const FAMILY: &str = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
const FLAG: &str = "\u{1F1EB}\u{1F1F7}";

#[test]
fn move_right_steps_over_a_zwj_emoji_whole() {
    let mut e = editor_with(&format!("a{FAMILY}b"));
    e.move_right();
    assert_eq!(e.cursor_col, 1);
    e.move_right();
    assert_eq!(e.cursor_col, 6, "one Right crosses all five code points");
    e.move_right();
    assert_eq!(e.cursor_col, 7);
}

#[test]
fn move_left_steps_over_a_flag_whole() {
    let mut e = editor_with(&format!("{FLAG} France"));
    e.cursor_col = 2;
    e.move_left();
    assert_eq!(e.cursor_col, 0, "one Left crosses both regional indicators");
}

#[test]
fn typing_after_two_rights_lands_after_the_emoji() {
    let mut e = editor_with(&format!("a{FAMILY}b"));
    e.move_right();
    e.move_right();
    e.insert_char('Z');
    assert_eq!(e.lines[0], format!("a{FAMILY}Zb"));
}

#[test]
fn typing_after_a_decomposed_accent_keeps_it_on_its_letter() {
    let mut e = editor_with("cafe\u{301}!");
    for _ in 0..4 {
        e.move_right();
    }
    assert_eq!(e.cursor_col, 5, "the fourth Right crosses e and its accent");
    e.insert_char('Y');
    assert_eq!(e.lines[0], "cafe\u{301}Y!");
}

#[test]
fn backspace_removes_a_whole_flag() {
    let mut e = editor_with(&format!("{FLAG} France"));
    e.cursor_col = 2;
    e.backspace();
    assert_eq!(e.lines[0], " France");
    assert_eq!(e.cursor_col, 0);
}

#[test]
fn delete_forward_removes_a_whole_zwj_emoji() {
    let mut e = editor_with(&format!("a{FAMILY}b"));
    e.cursor_col = 1;
    e.delete_forward();
    assert_eq!(e.lines[0], "ab");
    assert_eq!(e.cursor_col, 1);
}

#[test]
fn move_down_snaps_out_of_a_cluster() {
    let mut e = editor_with(&format!("abcdef\na{FAMILY}b"));
    e.cursor_col = 3;
    e.move_down();
    assert_eq!(
        (e.cursor_row, e.cursor_col),
        (1, 1),
        "col 3 is inside the emoji"
    );
    e.cursor_row = 0;
    e.cursor_col = 3;
    e.wrap_override = Some(true);
    let _ = first_row_text(&mut e);
    e.move_down();
    assert_eq!(
        (e.cursor_row, e.cursor_col),
        (1, 1),
        "wrapped lines snap the same way"
    );
}

#[test]
fn click_inside_a_cluster_snaps_to_its_start() {
    let mut e = editor_with(&format!("x{FAMILY}y\ncafe\u{301}!"));
    let _ = first_row_text(&mut e);
    let text_x = e.last_inner.x + e.last_gutter_width + 1;
    let y = e.last_inner.y;
    for dx in 0..8 {
        e.click(text_x + dx, y);
        assert!(
            [0, 1, 6, 7].contains(&e.cursor_col),
            "a click at cell {dx} left the caret at col {}, inside the emoji",
            e.cursor_col
        );
    }
}

#[test]
fn shift_right_selects_a_whole_emoji() {
    let mut e = editor_with(&format!("a{FAMILY}b"));
    e.cursor_col = 1;
    e.start_selection_at_cursor();
    e.move_right();
    e.extend_selection_to_cursor();
    assert_eq!(e.selection_text(), FAMILY);
}

#[test]
fn plain_and_cjk_text_still_step_one_char() {
    let mut e = editor_with("ab\u{65E5}\u{672C}c");
    for want in 1..=5 {
        e.move_right();
        assert_eq!(e.cursor_col, want);
    }
    for want in (0..5).rev() {
        e.move_left();
        assert_eq!(e.cursor_col, want);
    }
    e.cursor_col = 3;
    e.backspace();
    assert_eq!(e.lines[0], "ab\u{672C}c");
    e.delete_forward();
    assert_eq!(e.lines[0], "abc");
}

#[test]
fn backspace_and_delete_still_join_lines_at_the_edges() {
    let mut e = editor_with(&format!("{FLAG}\nx"));
    e.cursor_row = 1;
    e.cursor_col = 0;
    e.backspace();
    assert_eq!(e.lines, vec![format!("{FLAG}x")]);
    assert_eq!(e.cursor_col, 2);
    let mut e = editor_with(&format!("{FLAG}\nx"));
    e.cursor_col = 2;
    e.delete_forward();
    assert_eq!(e.lines, vec![format!("{FLAG}x")]);
}

#[test]
fn a_lone_combining_mark_and_a_lone_indicator_are_one_step_each() {
    let mut e = editor_with("\u{301}\u{1F1EB}x");
    e.move_right();
    assert_eq!(e.cursor_col, 1);
    e.move_right();
    assert_eq!(e.cursor_col, 2);
    e.backspace();
    assert_eq!(e.lines[0], "\u{301}x");
}

#[test]
fn byte_index_ascii() {
    let e = editor_with("abcdef");
    assert_eq!(e.byte_index(0, 0), 0);
    assert_eq!(e.byte_index(0, 3), 3);
    assert_eq!(e.byte_index(0, 6), 6);
    assert_eq!(e.byte_index(0, 99), 6); // saturates at end
}

#[test]
fn byte_index_multibyte() {
    let e = editor_with("héllo");
    // 'h'=1 byte, 'é'=2 bytes, 'l'=1 byte
    assert_eq!(e.byte_index(0, 0), 0);
    assert_eq!(e.byte_index(0, 1), 1); // before 'é'
    assert_eq!(e.byte_index(0, 2), 3); // after 'é'
    assert_eq!(e.byte_index(0, 3), 4);
}

#[test]
fn insert_char_at_end() {
    let mut e = editor_with("abc");
    e.cursor_col = 3;
    e.insert_char('d');
    assert_eq!(e.lines[0], "abcd");
    assert_eq!(e.cursor_col, 4);
    assert!(e.dirty);
}

#[test]
fn insert_char_at_start() {
    let mut e = editor_with("bc");
    e.cursor_col = 0;
    e.insert_char('a');
    assert_eq!(e.lines[0], "abc");
    assert_eq!(e.cursor_col, 1);
}

#[test]
fn insert_char_in_middle() {
    let mut e = editor_with("ac");
    e.cursor_col = 1;
    e.insert_char('b');
    assert_eq!(e.lines[0], "abc");
    assert_eq!(e.cursor_col, 2);
}

#[test]
fn insert_char_multibyte_position() {
    let mut e = editor_with("aé");
    e.cursor_col = 2; // after 'é'
    e.insert_char('z');
    assert_eq!(e.lines[0], "aéz");
}

#[test]
fn insert_newline_splits_line() {
    let mut e = editor_with("hello world");
    e.cursor_col = 5;
    e.insert_newline();
    assert_eq!(e.lines, vec!["hello".to_string(), " world".to_string()]);
    assert_eq!(e.cursor_row, 1);
    assert_eq!(e.cursor_col, 0);
}

#[test]
fn insert_newline_at_end_creates_blank_line() {
    let mut e = editor_with("abc");
    e.cursor_col = 3;
    e.insert_newline();
    assert_eq!(e.lines, vec!["abc".to_string(), String::new()]);
    assert_eq!(e.cursor_row, 1);
}

#[test]
fn insert_newline_copies_previous_indent_for_unknown_language() {
    let mut e = editor_with("    abc");
    e.cursor_col = 7;
    e.insert_newline();
    assert_eq!(e.lines, vec!["    abc".to_string(), "    ".to_string()]);
    assert_eq!(e.cursor_row, 1);
    assert_eq!(e.cursor_col, 4);
}

/// #1443: `text` with the caret at `|`, in `lang`, after one Enter.
fn enter_at(lang: LangKind, text: &str) -> (Vec<String>, (usize, usize)) {
    let (row, line) = text
        .lines()
        .enumerate()
        .find(|(_, l)| l.contains('|'))
        .unwrap();
    let mut e = editor_with(&text.replace('|', ""));
    e.lang = Some(lang);
    e.cursor_row = row;
    e.cursor_col = line.chars().position(|c| c == '|').unwrap();
    e.insert_newline();
    (e.lines.clone(), (e.cursor_row, e.cursor_col))
}

#[test]
fn enter_after_a_doc_opener_starts_the_block_and_closes_it() {
    assert_eq!(
        enter_at(LangKind::JavaScript, "/**|"),
        (vec!["/**".into(), " * ".into(), " */".into()], (1, 3))
    );
    assert_eq!(
        enter_at(LangKind::Java, "    /**|"),
        (
            vec!["    /**".into(), "     * ".into(), "     */".into()],
            (1, 7)
        )
    );
}

#[test]
fn enter_between_a_doc_opener_and_its_closer_splits_them() {
    assert_eq!(
        enter_at(LangKind::TypeScript, "  /**| */"),
        (vec!["  /**".into(), "   * ".into(), "   */".into()], (1, 5))
    );
}

#[test]
fn enter_after_an_opener_already_closed_below_adds_no_closer() {
    assert_eq!(
        enter_at(LangKind::C, "/**|\n * Adds.\n */"),
        (
            vec!["/**".into(), " * ".into(), " * Adds.".into(), " */".into()],
            (1, 3)
        )
    );
}

#[test]
fn enter_after_an_opener_closed_further_down_adds_no_closer() {
    assert_eq!(
        enter_at(LangKind::JavaScript, "/**|\nDescription.\n */"),
        (
            vec![
                "/**".into(),
                " * ".into(),
                "Description.".into(),
                " */".into()
            ],
            (1, 3)
        )
    );
    assert_eq!(
        enter_at(LangKind::JavaScript, "/**|\n\nf();\n/** g */"),
        (
            vec![
                "/**".into(),
                " * ".into(),
                " */".into(),
                "".into(),
                "f();".into(),
                "/** g */".into()
            ],
            (1, 3)
        )
    );
}

#[test]
fn enter_on_a_star_line_continues_it_at_the_same_column() {
    assert_eq!(
        enter_at(LangKind::JavaScript, "/**\n * Adds two numbers.|\n */"),
        (
            vec![
                "/**".into(),
                " * Adds two numbers.".into(),
                " * ".into(),
                " */".into()
            ],
            (2, 3)
        )
    );
    assert_eq!(
        enter_at(LangKind::Cpp, "    * a|b"),
        (vec!["    * a".into(), "    * b".into()], (1, 6))
    );
}

#[test]
fn enter_after_a_closer_steps_back_to_the_opener_column() {
    assert_eq!(
        enter_at(LangKind::CSharp, "    /**\n     * x\n     */|"),
        (
            vec![
                "    /**".into(),
                "     * x".into(),
                "     */".into(),
                "    ".into()
            ],
            (3, 4)
        )
    );
}

#[test]
fn enter_on_a_rust_doc_line_continues_it() {
    assert_eq!(
        enter_at(LangKind::Rust, "/// Adds two numbers.|"),
        (vec!["/// Adds two numbers.".into(), "/// ".into()], (1, 4))
    );
    assert_eq!(
        enter_at(LangKind::Rust, "    //! Crate docs.|"),
        (
            vec!["    //! Crate docs.".into(), "    //! ".into()],
            (1, 8)
        )
    );
    assert_eq!(
        enter_at(LangKind::Rust, "/// Adds| two"),
        (vec!["/// Adds".into(), "///  two".into()], (1, 4))
    );
}

#[test]
fn enter_leaves_code_and_plain_comments_alone() {
    // Multiplication, a plain `//`, a four-slash rule, `/*` and the caret
    // just before a `*/`.
    assert_eq!(
        enter_at(LangKind::JavaScript, "x = a *|"),
        (vec!["x = a *".into(), String::new()], (1, 0))
    );
    assert_eq!(
        enter_at(LangKind::Rust, "// a|"),
        (vec!["// a".into(), String::new()], (1, 0))
    );
    assert_eq!(
        enter_at(LangKind::Rust, "//// a|"),
        (vec!["//// a".into(), String::new()], (1, 0))
    );
    assert_eq!(
        enter_at(LangKind::JavaScript, "/*|"),
        (vec!["/*".into(), String::new()], (1, 0))
    );
    assert_eq!(
        enter_at(LangKind::JavaScript, "/** a */|"),
        (vec!["/** a */".into(), String::new()], (1, 0))
    );
    assert_eq!(
        enter_at(LangKind::JavaScript, " * text |*/"),
        (vec![" * text ".into(), " */".into()], (1, 1))
    );
    assert_eq!(
        enter_at(LangKind::JavaScript, "*|"),
        (vec!["*".into(), String::new()], (1, 0))
    );
}

#[test]
fn enter_in_languages_without_these_comments_is_unchanged() {
    assert_eq!(
        enter_at(LangKind::Python, "/**|"),
        (vec!["/**".into(), String::new()], (1, 0))
    );
    assert_eq!(
        enter_at(LangKind::Python, " * a|"),
        (vec![" * a".into(), " ".into()], (1, 1))
    );
    assert_eq!(
        enter_at(LangKind::Markdown, "/// a|"),
        (vec!["/// a".into(), String::new()], (1, 0))
    );
    assert_eq!(
        enter_at(LangKind::JavaScript, "/// a|"),
        (vec!["/// a".into(), String::new()], (1, 0))
    );
}

#[test]
fn a_comment_continuation_undoes_in_one_step() {
    let mut e = editor_with("/**");
    e.lang = Some(LangKind::JavaScript);
    e.cursor_col = 3;
    e.insert_newline();
    assert_eq!(e.lines, vec!["/**", " * ", " */"]);
    assert!(e.undo());
    assert_eq!(e.lines, vec!["/**"]);
}

#[test]
fn insert_newline_python_indents_one_step_after_colon() {
    let mut e = editor_with("def hello():");
    e.lang = Some(LangKind::Python);
    e.cursor_col = e.line_char_len(0);
    e.insert_newline();
    assert_eq!(
        e.lines,
        vec!["def hello():".to_string(), "    ".to_string()]
    );
    assert_eq!(e.cursor_row, 1);
    assert_eq!(e.cursor_col, 4);
}

#[test]
fn insert_newline_python_no_extra_indent_without_colon() {
    let mut e = editor_with("    print(x)");
    e.lang = Some(LangKind::Python);
    e.cursor_col = e.line_char_len(0);
    e.insert_newline();
    assert_eq!(
        e.lines,
        vec!["    print(x)".to_string(), "    ".to_string()]
    );
    assert_eq!(e.cursor_col, 4);
}

#[test]
fn insert_newline_python_stacks_indent_on_nested_colon() {
    let mut e = editor_with("    if x:");
    e.lang = Some(LangKind::Python);
    e.cursor_col = e.line_char_len(0);
    e.insert_newline();
    assert_eq!(
        e.lines,
        vec!["    if x:".to_string(), "        ".to_string()]
    );
    assert_eq!(e.cursor_col, 8);
}

#[test]
fn shift_tab_dedents_current_line_when_no_selection() {
    let mut e = editor_with("        pass");
    e.lang = Some(LangKind::Python);
    e.cursor_row = 0;
    e.cursor_col = 8;
    e.dedent_lines();
    assert_eq!(e.lines, vec!["    pass".to_string()]);
    assert_eq!(e.cursor_col, 4, "cursor follows the four stripped spaces");
}

#[test]
fn shift_tab_dedent_aligns_to_previous_tab_stop() {
    // Six leading spaces drop to four (the previous width-4 tab stop),
    // removing two, not a flat four.
    let mut e = editor_with("      x = 1");
    e.lang = Some(LangKind::Python);
    e.cursor_col = 6;
    e.dedent_lines();
    assert_eq!(e.lines, vec!["    x = 1".to_string()]);
    assert_eq!(e.cursor_col, 4);
}

#[test]
fn shift_tab_dedent_partial_indent_removes_all_leading_spaces() {
    let mut e = editor_with("   y");
    e.lang = Some(LangKind::Python);
    e.cursor_col = 3;
    e.dedent_lines();
    assert_eq!(e.lines, vec!["y".to_string()]);
    assert_eq!(e.cursor_col, 0);
}

#[test]
fn shift_tab_dedent_on_flush_left_line_is_a_noop() {
    let mut e = editor_with("x");
    e.lang = Some(LangKind::Python);
    e.cursor_col = 1;
    e.dedent_lines();
    assert_eq!(e.lines, vec!["x".to_string()]);
    assert_eq!(e.cursor_col, 1);
}

#[test]
fn shift_tab_dedent_strips_a_single_leading_tab() {
    let mut e = editor_with("\tpass");
    e.lang = Some(LangKind::Python);
    e.cursor_col = 1;
    e.dedent_lines();
    assert_eq!(e.lines, vec!["pass".to_string()]);
    assert_eq!(e.cursor_col, 0);
}

#[test]
fn shift_tab_dedents_every_line_a_selection_touches() {
    let mut e = editor_with("    a\n    b\n    c");
    e.lang = Some(LangKind::Python);
    e.cursor_row = 2;
    e.cursor_col = 5;
    e.selection = Some(EditorSelection {
        anchor: (0, 4),
        head: (2, 5),
    });
    e.dedent_lines();
    assert_eq!(
        e.lines,
        vec!["a".to_string(), "b".to_string(), "c".to_string()]
    );
    assert_eq!(
        e.selection.unwrap(),
        EditorSelection {
            anchor: (0, 0),
            head: (2, 1),
        }
    );
}

#[test]
fn tab_indents_every_line_a_multiline_selection_touches() {
    let mut e = editor_with("a\nb");
    e.lang = Some(LangKind::Python);
    e.cursor_row = 1;
    e.cursor_col = 1;
    e.selection = Some(EditorSelection {
        anchor: (0, 0),
        head: (1, 1),
    });
    e.indent_lines();
    assert_eq!(e.lines, vec!["    a".to_string(), "    b".to_string()]);
    assert_eq!(
        e.selection.unwrap(),
        EditorSelection {
            anchor: (0, 4),
            head: (1, 5),
        }
    );
}

#[test]
fn tab_indent_block_leaves_empty_lines_untouched() {
    let mut e = editor_with("a\n\nb");
    e.lang = Some(LangKind::Python);
    e.selection = Some(EditorSelection {
        anchor: (0, 0),
        head: (2, 1),
    });
    e.indent_lines();
    assert_eq!(
        e.lines,
        vec!["    a".to_string(), String::new(), "    b".to_string()]
    );
}

#[test]
fn tab_at_cursor_pads_to_the_next_tab_stop() {
    // Cursor at column 2 inserts two spaces to reach column 4, not four.
    let mut e = editor_with("ab");
    e.lang = Some(LangKind::Python);
    e.cursor_col = 2;
    e.indent_at_cursor();
    assert_eq!(e.lines, vec!["ab  ".to_string()]);
    assert_eq!(e.cursor_col, 4);
}

#[test]
fn dedent_then_undo_restores_the_buffer_in_one_step() {
    let mut e = editor_with("        pass");
    e.lang = Some(LangKind::Python);
    e.cursor_col = 8;
    e.dedent_lines();
    assert_eq!(e.lines, vec!["    pass".to_string()]);
    assert!(e.undo());
    assert_eq!(e.lines, vec!["        pass".to_string()]);
}

/// #694: undo history is bounded in bytes as well as steps, keeping the
/// newest steps (and at least `UNDO_MIN_STEPS` of them).
#[test]
fn undo_history_of_a_large_buffer_is_bounded_in_bytes() {
    let big = vec!["x".repeat(255); 1024].join("\n");
    let mut e = editor_with(&big);
    for i in 0..40 {
        e.push_undo(EditKind::Paste);
        e.lines[0] = format!("edit {i}");
    }
    let held: usize = e.undo_stack.iter().map(|s| s.bytes).sum();
    assert_eq!(e.undo_stack.len(), UNDO_MIN_STEPS, "held {held} bytes");
    // The newest steps survive: one undo brings back the text before
    // the last edit.
    assert!(e.undo());
    assert_eq!(e.lines[0], "edit 38");
    // A small buffer keeps its full step count.
    let mut small = editor_with("a");
    for _ in 0..40 {
        small.push_undo(EditKind::Paste);
    }
    assert_eq!(small.undo_stack.len(), 40);
}

/// An undo step holds the buffer's text packed (#1707): about the text's
/// own size, not the ~45 bytes a line more a `Vec<String>` copy took.
#[test]
fn an_undo_step_holds_about_its_texts_size() {
    let text: String = (0..1000).map(|i| format!("line {i}\n")).collect();
    let mut e = editor_with(&text);
    e.push_undo(EditKind::Paste);
    let held = e.undo_stack[0].bytes;
    let text_bytes: usize = e.lines.iter().map(|l| l.len() + 1).sum();
    assert!(
        held <= text_bytes + 8 * e.lines.len(),
        "{held} bytes held for {text_bytes} bytes of text"
    );
    e.lines[0] = String::from("changed");
    assert!(e.undo());
    assert_eq!(
        e.lines,
        text.lines().collect::<Vec<_>>(),
        "and undoes to it"
    );
}

#[test]
fn redo_reapplies_an_undone_edit() {
    let mut e = editor_with("");
    e.insert_char('a');
    e.insert_char('b');
    assert_eq!(e.lines, vec!["ab".to_string()]);
    assert!(e.undo());
    assert_eq!(e.lines, vec![String::new()]);
    assert!(e.redo());
    assert_eq!(e.lines, vec!["ab".to_string()]);
    assert_eq!(e.cursor_col, 2);
}

#[test]
fn redo_is_a_noop_on_an_empty_redo_stack() {
    let mut e = editor_with("x");
    assert!(!e.redo());
}

#[test]
fn undo_and_redo_bump_edit_seq_so_the_app_resyncs() {
    // The app drives LSP did_change / git-gutter refresh off edit_seq; a
    // buffer restore must bump it or diagnostics go stale until the next
    // keystroke.
    let mut e = editor_with("");
    e.insert_char('a');
    let after_edit = e.edit_seq;
    assert!(e.undo());
    assert!(e.edit_seq > after_edit, "undo must bump edit_seq");
    let after_undo = e.edit_seq;
    assert!(e.redo());
    assert!(e.edit_seq > after_undo, "redo must bump edit_seq");
}

/// "Reopen with Encoding" swaps the whole buffer for a re-decode of the
/// file on disk, like every other whole-buffer replacement — and like
/// them it must clear BOTH history stacks: a redo popped after the swap
/// used to reinstate the entire buffer as decoded under the OLD
/// encoding (plus its dirty flag), silently discarding the re-decode.
#[test]
fn reopening_with_an_encoding_clears_both_history_stacks() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("enc.txt");
    // 0xC2 0xA2 decodes to one char as UTF-8 ("\u{a2}") and two as
    // Windows-1252 ("\u{c2}\u{a2}"), so the assertions below prove the
    // buffer really was re-decoded, not merely re-read.
    std::fs::write(&path, [0xC2_u8, 0xA2, b'\n']).unwrap();
    let mut e = Editor::new();
    e.open(&path).unwrap();
    assert_eq!(e.lines, vec!["\u{a2}".to_string()], "staging: UTF-8 decode");
    e.insert_char('x');
    assert!(e.undo());
    assert_eq!(e.lines, vec!["\u{a2}".to_string()], "staging: edit undone");
    e.reopen_with_encoding(encoding_rs::WINDOWS_1252).unwrap();
    assert_eq!(
        e.lines,
        vec!["\u{c2}\u{a2}".to_string()],
        "the buffer holds the Windows-1252 decode"
    );
    assert!(
        !e.redo(),
        "redo must not resurrect the buffer decoded under the old encoding"
    );
    assert!(
        !e.undo(),
        "undo history from before the re-decode is equally stale"
    );
    assert_eq!(e.lines, vec!["\u{c2}\u{a2}".to_string()]);
}

#[test]
fn a_new_edit_after_undo_discards_the_redo_branch() {
    let mut e = editor_with("");
    e.insert_char('a');
    assert!(e.undo());
    assert_eq!(e.lines, vec![String::new()]);
    // A fresh edit branches history: the redo of "a" must be discarded.
    e.insert_char('b');
    assert!(!e.redo(), "a fresh edit must clear the redo stack");
    assert_eq!(e.lines, vec!["b".to_string()]);
}

#[test]
fn insert_newline_rust_indents_after_open_brace() {
    let mut e = editor_with("fn main() {");
    e.lang = Some(LangKind::Rust);
    e.cursor_col = e.line_char_len(0);
    e.insert_newline();
    assert_eq!(e.lines, vec!["fn main() {".to_string(), "    ".to_string()]);
    assert_eq!(e.cursor_col, 4);
}

#[test]
fn insert_newline_typescript_indents_after_open_paren() {
    let mut e = editor_with("foo(");
    e.lang = Some(LangKind::TypeScript);
    e.cursor_col = e.line_char_len(0);
    e.insert_newline();
    assert_eq!(e.lines, vec!["foo(".to_string(), "    ".to_string()]);
    assert_eq!(e.cursor_col, 4);
}

#[test]
fn insert_newline_rust_bracket_pair_split_places_close_on_own_line() {
    let mut e = editor_with("fn main() {}");
    e.lang = Some(LangKind::Rust);
    e.cursor_col = 11;
    e.insert_newline();
    assert_eq!(
        e.lines,
        vec![
            "fn main() {".to_string(),
            "    ".to_string(),
            "}".to_string()
        ]
    );
    assert_eq!(e.cursor_row, 1);
    assert_eq!(e.cursor_col, 4);
}

#[test]
fn insert_newline_rust_bracket_pair_split_preserves_outer_indent() {
    let mut e = editor_with("    let v = vec![];");
    e.lang = Some(LangKind::Rust);
    e.cursor_col = 17;
    e.insert_newline();
    assert_eq!(
        e.lines,
        vec![
            "    let v = vec![".to_string(),
            "        ".to_string(),
            "    ];".to_string(),
        ]
    );
    assert_eq!(e.cursor_row, 1);
    assert_eq!(e.cursor_col, 8);
}

#[test]
fn insert_newline_python_bracket_pair_split_places_close_on_own_line() {
    let mut e = editor_with("foo()");
    e.lang = Some(LangKind::Python);
    e.cursor_col = 4;
    e.insert_newline();
    assert_eq!(
        e.lines,
        vec!["foo(".to_string(), "    ".to_string(), ")".to_string()]
    );
    assert_eq!(e.cursor_row, 1);
    assert_eq!(e.cursor_col, 4);
}

#[test]
fn insert_newline_yaml_uses_two_space_indent() {
    let mut e = editor_with("  key:");
    e.lang = Some(LangKind::Yaml);
    e.cursor_col = e.line_char_len(0);
    e.insert_newline();
    assert_eq!(e.lines, vec!["  key:".to_string(), "    ".to_string()]);
    assert_eq!(e.cursor_col, 4);
}

/// Enter at the end of a YAML line; returns the new line's indent.
fn yaml_enter(line: &str) -> String {
    let mut e = editor_with(line);
    e.lang = Some(LangKind::Yaml);
    e.cursor_col = e.line_char_len(0);
    e.insert_newline();
    assert_eq!(e.lines.len(), 2, "one new line for {line:?}");
    assert_eq!(e.cursor_row, 1);
    assert_eq!(e.cursor_col, e.lines[1].chars().count());
    e.lines[1].clone()
}

#[test]
fn yaml_enter_after_a_key_that_opens_a_mapping_indents_one_step() {
    assert_eq!(yaml_enter("services:"), "  ");
    assert_eq!(yaml_enter("  web:  # the app"), "    ");
    assert_eq!(yaml_enter("run: |"), "  ");
    assert_eq!(yaml_enter("  script: >-"), "    ");
    assert_eq!(yaml_enter("\"quoted key\":"), "  ");
    assert_eq!(yaml_enter("args: ["), "  ");
}

#[test]
fn yaml_enter_after_an_anchored_or_tagged_key_indents_one_step() {
    // #1309: the anchor names the mapping typed below it.
    assert_eq!(yaml_enter(".defaults: &defaults"), "  ");
    assert_eq!(yaml_enter("x-common: &common  # shared"), "  ");
    assert_eq!(yaml_enter("  data: !!map"), "    ");
    assert_eq!(yaml_enter("base: &a !!map"), "  ");
    assert_eq!(yaml_enter("script: &steps |"), "  ");
    assert_eq!(yaml_enter("- step: &s"), "    ");
}

#[test]
fn yaml_enter_after_an_anchored_or_tagged_value_keeps_the_indent() {
    // Negative: the anchor or tag is on a value already written, and an
    // alias is a value.
    assert_eq!(yaml_enter("image: &img python:3.12"), "");
    assert_eq!(yaml_enter("  port: !!int 80"), "  ");
    assert_eq!(yaml_enter("<<: *defaults"), "");
    assert_eq!(yaml_enter("name: a&b"), "");
}

#[test]
fn yaml_enter_in_a_list_item_lines_up_with_the_item_content() {
    assert_eq!(yaml_enter("  - name: x"), "    ");
    assert_eq!(yaml_enter("- name:"), "    ");
    assert_eq!(yaml_enter("  - run: |"), "      ");
}

#[test]
fn yaml_enter_after_a_plain_value_keeps_the_indent() {
    // Negative: nothing opens a block, so the caret stays in the column.
    assert_eq!(yaml_enter("image: nginx"), "");
    assert_eq!(yaml_enter("  ports:  [80]"), "  ");
    assert_eq!(yaml_enter("  - \"80:80\""), "  ");
    assert_eq!(yaml_enter("- plain item"), "");
    assert_eq!(yaml_enter("url: http://x.io/a"), "");
    assert_eq!(yaml_enter("  note: 'ends with a colon:'"), "  ");
    assert_eq!(yaml_enter("# todo:"), "");
    assert_eq!(yaml_enter("cmd: echo hi # really:"), "");
}

/// Enter at byte-and-char column `col` of row `row` in `text`; returns
/// the lines and the caret.
fn enter_in(lang: LangKind, text: &str, row: usize, col: usize) -> (Vec<String>, (usize, usize)) {
    let mut e = editor_with(text);
    e.lang = Some(lang);
    e.cursor_row = row;
    e.cursor_col = col;
    e.insert_newline();
    (e.lines.clone(), (e.cursor_row, e.cursor_col))
}

/// Enter at the end of a one-line Markdown buffer; returns the new line
/// and checks the caret ends it.
fn md_enter(line: &str) -> String {
    let (lines, caret) = enter_in(LangKind::Markdown, line, 0, line.chars().count());
    assert_eq!(lines.len(), 2, "one new line for {line:?}: {lines:?}");
    assert_eq!(lines[0], line);
    assert_eq!(caret, (1, lines[1].chars().count()), "{line:?}");
    lines[1].clone()
}

// #1495: Enter after a Markdown list item, task item or quote starts the
// next one, as GitHub, Obsidian and VS Code's Markdown All in One do.
#[test]
fn markdown_enter_continues_bullets_numbers_tasks_and_quotes() {
    assert_eq!(md_enter("- first"), "- ");
    assert_eq!(md_enter("* a"), "* ");
    assert_eq!(md_enter("  + nested"), "  + ");
    assert_eq!(md_enter("1. one"), "2. ");
    assert_eq!(md_enter("9) nine"), "10) ");
    assert_eq!(md_enter("3.  wide gap"), "4.  ");
    assert_eq!(md_enter("- [x] done"), "- [ ] ");
    assert_eq!(md_enter("- [ ] todo"), "- [ ] ");
    assert_eq!(md_enter("> quoted"), "> ");
    assert_eq!(md_enter("> - in a quote"), "> - ");
    assert_eq!(md_enter("- caf\u{e9}"), "- ");
    // Mid-item, the rest of the item moves to the new one.
    let (lines, caret) = enter_in(LangKind::Markdown, "- foo bar", 0, 5);
    assert_eq!(lines, ["- foo", "- bar"]);
    assert_eq!(caret, (1, 2));
    // One undo takes the whole continuation back.
    let mut e = editor_with("- first");
    e.lang = Some(LangKind::Markdown);
    e.cursor_col = 7;
    e.insert_newline();
    assert!(e.undo());
    assert_eq!(e.lines, ["- first"]);
}

// #1495: Enter on an item that is only its marker ends the list: the
// marker goes and the line is left blank.
#[test]
fn markdown_enter_on_an_empty_item_ends_the_list() {
    for line in ["- ", "3. ", "- [ ] ", "- [ ]", "> ", ">"] {
        let (lines, caret) = enter_in(LangKind::Markdown, line, 0, line.len());
        assert_eq!(lines, [""], "{line:?}");
        assert_eq!(caret, (0, 0), "{line:?}");
    }
    let (lines, caret) = enter_in(LangKind::Markdown, "- a\n- ", 1, 2);
    assert_eq!(lines, ["- a", ""]);
    assert_eq!(caret, (1, 0));
}

// Negatives (#1495): lines that are not list items, a caret before the
// marker, a fenced code block and other languages keep the plain Enter.
#[test]
fn markdown_enter_leaves_other_lines_alone() {
    for line in [
        "plain",
        "**bold**",
        "---",
        "* * *",
        "1.5 apples",
        "-dash",
        "#1 fan",
    ] {
        assert_eq!(md_enter(line), "", "{line:?}");
    }
    let (lines, _) = enter_in(LangKind::Markdown, "- a", 0, 0);
    assert_eq!(lines, ["", "- a"], "a caret before the marker");
    let (lines, _) = enter_in(LangKind::Markdown, "```\n- a", 1, 3);
    assert_eq!(lines, ["```", "- a", ""], "inside a code fence");
    let (lines, _) = enter_in(LangKind::Markdown, "```\nx\n```\n- a", 3, 3);
    assert_eq!(lines[4], "- ", "after a closed fence");
    let (lines, _) = enter_in(LangKind::Markdown, "```\n~~~\n- a", 2, 3);
    assert_eq!(
        lines,
        ["```", "~~~", "- a", ""],
        "an unlike fence closes nothing"
    );
    let (lines, _) = enter_in(LangKind::Markdown, "````\n```\n- a", 2, 3);
    assert_eq!(
        lines,
        ["````", "```", "- a", ""],
        "a shorter fence closes nothing"
    );
    let (lines, _) = enter_in(LangKind::Markdown, "```rust\nx\n````\n- a", 3, 3);
    assert_eq!(lines[4], "- ", "a longer fence of the same kind closes");
    let (lines, _) = enter_in(LangKind::Rust, "- a", 0, 3);
    assert_eq!(lines, ["- a", ""], "Markdown only");
}

// #1443: `/**` then Enter opens a JSDoc / Doxygen block and closes it;
// a ` * ` line inside a block comment continues; Rust `///` and `//!`
// lines continue.
#[test]
fn enter_continues_doc_comments() {
    let (lines, caret) = enter_in(LangKind::JavaScript, "/**", 0, 3);
    assert_eq!(lines, ["/**", " * ", " */"]);
    assert_eq!(caret, (1, 3));
    let (lines, caret) = enter_in(LangKind::Java, "  /**\n  void f();", 0, 5);
    assert_eq!(lines, ["  /**", "   * ", "   */", "  void f();"]);
    assert_eq!(caret, (1, 5));
    let (lines, caret) = enter_in(LangKind::TypeScript, "/** */", 0, 3);
    assert_eq!(lines, ["/**", " * ", " */"]);
    assert_eq!(caret, (1, 3));
    // Already closed below: no second closer.
    let (lines, _) = enter_in(LangKind::C, "/**\n * Adds.\n */", 0, 3);
    assert_eq!(lines, ["/**", " * ", " * Adds.", " */"]);
    let (lines, caret) = enter_in(LangKind::Tsx, "/**\n * Adds two.\n */", 1, 12);
    assert_eq!(lines, ["/**", " * Adds two.", " * ", " */"]);
    assert_eq!(caret, (2, 3));
    let (lines, _) = enter_in(LangKind::Java, "/**\ndescription\n\n */", 0, 3);
    assert_eq!(
        lines,
        ["/**", " * ", "description", "", " */"],
        "a plain body line"
    );
    let (lines, _) = enter_in(LangKind::C, "/**\nint f();\n/** g */", 0, 3);
    assert_eq!(
        lines,
        ["/**", " * ", " */", "int f();", "/** g */"],
        "a later comment"
    );
    let (lines, _) = enter_in(LangKind::Cpp, "/*\n * plain block", 1, 14);
    assert_eq!(lines[2], " * ");
    let (lines, caret) = enter_in(LangKind::Rust, "/// Adds two numbers.", 0, 21);
    assert_eq!(lines, ["/// Adds two numbers.", "/// "]);
    assert_eq!(caret, (1, 4));
    let (lines, _) = enter_in(LangKind::Rust, "    //! crate docs", 0, 18);
    assert_eq!(lines[1], "    //! ");
}

// Negatives (#1443): a `*` in code, plain `//` comments, a caret before
// `*/`, a closed one-line doc and other languages keep the plain Enter.
#[test]
fn enter_outside_a_doc_comment_adds_no_leader() {
    let plain = |lang, text: &str, row, col| enter_in(lang, text, row, col).0;
    assert_eq!(
        plain(LangKind::JavaScript, "x = a *", 0, 7),
        ["x = a *", ""]
    );
    assert_eq!(plain(LangKind::Rust, "// a", 0, 4), ["// a", ""]);
    assert_eq!(
        plain(LangKind::Rust, "//// banner", 0, 11),
        ["//// banner", ""]
    );
    assert_eq!(
        plain(LangKind::C, "/*\n * text */", 1, 8),
        ["/*", " * text ", " */"],
        "a caret before the closer"
    );
    assert_eq!(
        plain(LangKind::Java, "/** done */", 0, 11),
        ["/** done */", ""]
    );
    assert_eq!(plain(LangKind::Java, "/**/", 0, 4), ["/**/", ""]);
    assert_eq!(plain(LangKind::Python, "/**", 0, 3), ["/**", ""]);
    assert_eq!(plain(LangKind::Markdown, "/**", 0, 3), ["/**", ""]);
    assert_eq!(plain(LangKind::Go, "/// x", 0, 5), ["/// x", ""]);
}

#[test]
fn enter_after_a_colon_does_not_indent_outside_yaml_and_python() {
    let mut e = editor_with("key:");
    e.lang = Some(LangKind::Rust);
    e.cursor_col = e.line_char_len(0);
    e.insert_newline();
    assert_eq!(e.lines, vec!["key:".to_string(), String::new()]);
}

#[test]
fn backspace_mid_line() {
    let mut e = editor_with("abcd");
    e.cursor_col = 3;
    e.backspace();
    assert_eq!(e.lines[0], "abd");
    assert_eq!(e.cursor_col, 2);
}

#[test]
fn backspace_at_col_zero_joins_with_previous_line() {
    let mut e = editor_with("hello\nworld");
    e.cursor_row = 1;
    e.cursor_col = 0;
    e.backspace();
    assert_eq!(e.lines, vec!["helloworld".to_string()]);
    assert_eq!(e.cursor_row, 0);
    assert_eq!(e.cursor_col, 5);
}

#[test]
fn backspace_at_origin_does_nothing_destructive() {
    let mut e = editor_with("abc");
    e.cursor_row = 0;
    e.cursor_col = 0;
    e.backspace();
    assert_eq!(e.lines, vec!["abc".to_string()]);
}

#[test]
fn delete_forward_mid_line() {
    let mut e = editor_with("abcd");
    e.cursor_col = 1;
    e.delete_forward();
    assert_eq!(e.lines[0], "acd");
    assert_eq!(e.cursor_col, 1);
}

#[test]
fn delete_forward_at_end_joins_with_next_line() {
    let mut e = editor_with("hello\nworld");
    e.cursor_row = 0;
    e.cursor_col = 5;
    e.delete_forward();
    assert_eq!(e.lines, vec!["helloworld".to_string()]);
    assert_eq!(e.cursor_row, 0);
    assert_eq!(e.cursor_col, 5);
}

// #909: Ctrl+Backspace / Ctrl+Delete delete to Ctrl+Left / Ctrl+Right's
// stop, never past the line, and each press is one undo step.
#[test]
fn delete_word_left_takes_the_word_and_the_gap_before_the_caret() {
    let mut e = editor_with("alpha beta.gamma  ");
    e.cursor_col = 18;
    e.delete_word_left();
    assert_eq!((e.lines[0].as_str(), e.cursor_col), ("alpha beta.", 11));
    e.delete_word_left();
    assert_eq!((e.lines[0].as_str(), e.cursor_col), ("alpha ", 6));
    e.delete_word_left();
    assert_eq!((e.lines[0].as_str(), e.cursor_col), ("", 0));
    assert!(e.undo());
    assert_eq!(e.lines[0], "alpha ");
}

/// #909: a combining accent in decomposed text stays with its letter:
/// Ctrl+Left / Ctrl+Right step over `e\u{301}x` whole, and Ctrl+Backspace
/// and Ctrl+Delete take the letter and its accent together.
#[test]
fn word_motion_and_deletes_keep_a_combining_accent_with_its_letter() {
    let mut e = editor_with("e\u{301}x yz");
    e.move_word_right();
    assert_eq!(e.cursor_col, 3, "past the whole word");
    e.delete_word_left();
    assert_eq!((e.lines[0].as_str(), e.cursor_col), (" yz", 0));
    assert!(e.undo());
    e.cursor_col = 0;
    e.delete_word_right();
    assert_eq!(e.lines[0], " yz");
    let mut e = editor_with("ab e\u{301}");
    e.cursor_col = 5;
    e.move_word_left();
    assert_eq!(
        e.cursor_col, 3,
        "back to the letter, not between it and its accent"
    );
}

#[test]
fn delete_word_left_at_line_start_only_joins_the_line_above() {
    let mut e = editor_with("one two\nthree");
    e.cursor_row = 1;
    e.cursor_col = 0;
    e.delete_word_left();
    assert_eq!(e.lines, vec!["one twothree".to_string()]);
    assert_eq!((e.cursor_row, e.cursor_col), (0, 7));
}

#[test]
fn delete_word_left_takes_an_accented_word_and_stays_on_its_line() {
    let mut e = editor_with("x\n  cafe\u{301}");
    e.cursor_row = 1;
    e.cursor_col = 7;
    e.delete_word_left();
    assert_eq!((e.lines[1].as_str(), e.cursor_col), ("  ", 2));
    e.delete_word_left();
    assert_eq!(e.lines, vec!["x".to_string(), String::new()]);
    assert_eq!((e.cursor_row, e.cursor_col), (1, 0));
}

#[test]
fn delete_word_right_takes_the_gap_and_the_word_after_the_caret() {
    let mut e = editor_with("alpha  beta gamma\nnext");
    e.cursor_col = 5;
    e.delete_word_right();
    assert_eq!((e.lines[0].as_str(), e.cursor_col), ("alpha gamma", 5));
    e.cursor_col = 11;
    e.delete_word_right();
    assert_eq!(e.lines, vec!["alpha gammanext".to_string()]);
    e.cursor_col = 1;
    e.delete_word_right();
    assert_eq!(e.lines[0], "a gammanext");
}

/// Decomposed `e\u{301}x` is the one word `éx` reads as, so the whole
/// word goes, as it does for the precomposed `éx`.
#[test]
fn delete_word_right_never_splits_a_letter_from_its_accent() {
    for word in ["e\u{301}x", "\u{e9}x"] {
        let mut e = editor_with(word);
        e.delete_word_right();
        assert_eq!(e.lines[0], "", "{word:?}");
    }
}

#[test]
fn delete_word_with_a_selection_deletes_just_the_selection() {
    let mut e = editor_with("alpha beta");
    e.selection = Some(EditorSelection {
        anchor: (0, 1),
        head: (0, 3),
    });
    e.cursor_col = 3;
    e.delete_word_left();
    assert_eq!((e.lines[0].as_str(), e.cursor_col), ("aha beta", 1));
    e.selection = Some(EditorSelection {
        anchor: (0, 4),
        head: (0, 5),
    });
    e.cursor_col = 5;
    e.delete_word_right();
    assert_eq!(e.lines[0], "aha eta");
}

#[test]
fn buffer_pos_at_maps_cells_to_line_and_char() {
    let mut e = editor_with("fn main() {}\nlet x = 1;");
    e.last_inner = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 25,
    };
    e.last_gutter_width = 2;
    assert_eq!(
        e.buffer_pos_at(3, 0),
        Some((0, 0)),
        "first text cell (text_x = x + gutter + 1 = 3) is char 0 of line 0"
    );
    assert_eq!(
        e.buffer_pos_at(6, 0),
        Some((0, 3)),
        "three cells into the text is char 3"
    );
    assert_eq!(
        e.buffer_pos_at(3, 1),
        Some((1, 0)),
        "the second screen row is the second buffer line"
    );
}

#[test]
fn buffer_pos_at_returns_none_over_the_gutter() {
    let mut e = editor_with("hello");
    e.last_inner = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 25,
    };
    e.last_gutter_width = 2;
    assert_eq!(e.buffer_pos_at(0, 0), None, "column 0 is in the gutter");
    assert_eq!(e.buffer_pos_at(2, 0), None, "still left of text_x");
}

#[test]
fn buffer_pos_at_accounts_for_vertical_scroll() {
    let mut e = editor_with("a\nbb\nccc\ndddd\neeeee");
    e.last_inner = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 25,
    };
    e.last_gutter_width = 2;
    e.scroll = 2;
    assert_eq!(
        e.buffer_pos_at(3, 0),
        Some((2, 0)),
        "the top visible row is buffer line scroll = 2"
    );
}

#[test]
fn buffer_pos_at_accounts_for_horizontal_scroll() {
    let mut e = editor_with("abcdefghijklmnop");
    e.last_inner = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 25,
    };
    e.last_gutter_width = 2;
    e.scroll_col = 5;
    assert_eq!(
        e.buffer_pos_at(3, 0),
        Some((0, 5)),
        "the leftmost text cell maps to char scroll_col, not char 0"
    );
    assert_eq!(
        e.buffer_pos_at(5, 0),
        Some((0, 7)),
        "two cells right of the left edge is char scroll_col + 2"
    );
}

#[test]
fn buffer_pos_at_honours_pane_offset() {
    let mut e = editor_with("hello");
    e.last_inner = Rect {
        x: 10,
        y: 4,
        width: 40,
        height: 10,
    };
    e.last_gutter_width = 3;
    assert_eq!(e.buffer_pos_at(14, 4), Some((0, 0)), "text_x = 10 + 3 + 1");
    assert_eq!(e.buffer_pos_at(16, 4), Some((0, 2)));
    assert_eq!(e.buffer_pos_at(13, 4), None, "one cell left of text_x");
}

#[test]
fn buffer_pos_at_returns_none_outside_viewport_and_past_content() {
    let mut e = editor_with("one\ntwo");
    e.last_inner = Rect {
        x: 0,
        y: 2,
        width: 80,
        height: 4,
    };
    e.last_gutter_width = 2;
    assert_eq!(e.buffer_pos_at(3, 1), None, "row above the pane");
    assert_eq!(e.buffer_pos_at(3, 6), None, "row below the pane");
    assert_eq!(
        e.buffer_pos_at(3, 4),
        None,
        "row maps past the last line of content"
    );
}

#[test]
fn buffer_pos_at_clamps_past_end_of_line() {
    let mut e = editor_with("hi");
    e.last_inner = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 25,
    };
    e.last_gutter_width = 2;
    assert_eq!(
        e.buffer_pos_at(40, 0),
        Some((0, 2)),
        "a cell past the line's text clamps to the line length"
    );
}

#[test]
fn word_at_returns_identifier_bounds() {
    let e = editor_with("fn main() {}");
    assert_eq!(e.word_at(0, 0), Some((0, 2)), "fn");
    assert_eq!(e.word_at(0, 1), Some((0, 2)), "still within fn");
    assert_eq!(e.word_at(0, 3), Some((3, 7)), "main starts at char 3");
    assert_eq!(e.word_at(0, 6), Some((3, 7)), "last char of main");
}

#[test]
fn word_at_returns_none_off_a_word() {
    let e = editor_with("fn main() {}");
    assert_eq!(e.word_at(0, 2), None, "the space between fn and main");
    assert_eq!(e.word_at(0, 7), None, "the open paren");
    assert_eq!(e.word_at(0, 99), None, "past the end of the line");
}

#[test]
fn word_at_includes_underscores_and_digits() {
    let e = editor_with("let foo_bar2 = 1");
    assert_eq!(
        e.word_at(0, 4),
        Some((4, 12)),
        "foo_bar2 is a single identifier"
    );
    assert_eq!(
        e.word_at(0, 11),
        Some((4, 12)),
        "a trailing digit is part of the identifier"
    );
}

#[test]
fn word_at_handles_missing_line() {
    let e = editor_with("only one line");
    assert_eq!(e.word_at(5, 0), None, "no such line");
}

#[test]
fn page_down_advances_one_full_viewport_and_puts_first_unseen_line_at_top() {
    // Simulate a 100-line file with the editor's viewport rendering 25
    // lines. After PageDown the cursor should land on row 25 (line 26 in
    // 1-indexed terms) and that row should be the new top of the view.
    let mut e = editor_with_lines(100);
    e.last_inner = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 25,
    };
    assert_eq!(e.scroll, 0);
    assert_eq!(e.cursor_row, 0);
    e.page_down_one_screen();
    assert_eq!(
        e.cursor_row, 25,
        "cursor should jump to first previously-unseen row"
    );
    assert_eq!(
        e.scroll, 25,
        "scroll should align with new cursor at top of viewport"
    );
}

#[test]
fn page_down_repeats_advance_one_viewport_at_a_time() {
    let mut e = editor_with_lines(100);
    e.last_inner = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 20,
    };
    e.page_down_one_screen();
    e.page_down_one_screen();
    assert_eq!(e.cursor_row, 40);
    assert_eq!(e.scroll, 40);
}

#[test]
fn page_down_clamps_at_end_of_file() {
    let mut e = editor_with_lines(30);
    e.last_inner = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 25,
    };
    // Realistic state: scroll = 4 means rows 4..=28 are on screen, with
    // line 29 (cursor_row 28) visible at the bottom.
    e.scroll = 4;
    e.cursor_row = 28;
    e.page_down_one_screen();
    // 4 + 25 = 29 → last row.  Cursor and scroll land there.
    assert_eq!(e.cursor_row, 29);
    assert_eq!(e.scroll, 29);
}

#[test]
fn page_up_rewinds_one_full_viewport() {
    let mut e = editor_with_lines(200);
    e.last_inner = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 25,
    };
    e.scroll = 100;
    e.cursor_row = 100;
    e.page_up_one_screen();
    assert_eq!(e.cursor_row, 75);
    assert_eq!(e.scroll, 75);
}

#[test]
fn page_up_clamps_at_top_of_file() {
    let mut e = editor_with_lines(50);
    e.last_inner = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 25,
    };
    e.scroll = 5;
    e.cursor_row = 5;
    e.page_up_one_screen();
    assert_eq!(e.cursor_row, 0);
    assert_eq!(e.scroll, 0);
}

#[test]
fn page_size_falls_back_when_viewport_is_unknown() {
    // Before the first render last_inner is zero-sized; PageDown should
    // still advance by some sensible default rather than no-op.
    let mut e = editor_with_lines(100);
    e.last_inner = Rect::default();
    e.page_down_one_screen();
    assert!(e.cursor_row > 0, "should advance even with zero last_inner");
}

fn editor_with_lines(n: usize) -> Editor {
    let mut e = Editor::new();
    e.lines = (0..n).map(|i| format!("line {i}")).collect();
    e
}

#[test]
fn move_word_right_jumps_to_end_of_current_word() {
    let mut e = editor_with("hello world");
    e.cursor_row = 0;
    e.cursor_col = 0;
    e.move_word_right();
    assert_eq!((e.cursor_row, e.cursor_col), (0, 5));
}

#[test]
fn move_word_right_skips_intervening_whitespace_and_lands_at_end_of_next_word() {
    let mut e = editor_with("hello   world!!!end");
    e.cursor_row = 0;
    e.cursor_col = 5;
    e.move_word_right();
    assert_eq!(
        (e.cursor_row, e.cursor_col),
        (0, 13),
        "from end of 'hello' the next forward-word skips the spaces and stops at end of 'world'"
    );
    e.move_word_right();
    assert_eq!(
        (e.cursor_row, e.cursor_col),
        (0, 19),
        "second forward-word skips '!!!' and lands at end of 'end'"
    );
}

#[test]
fn move_word_right_at_eol_advances_to_first_word_boundary_on_next_line() {
    let mut e = editor_with("foo\n  bar baz");
    e.cursor_row = 0;
    e.cursor_col = 3;
    e.move_word_right();
    assert_eq!((e.cursor_row, e.cursor_col), (1, 5));
}

#[test]
fn move_word_right_at_eof_is_a_noop() {
    let mut e = editor_with("hi");
    e.cursor_row = 0;
    e.cursor_col = 2;
    e.move_word_right();
    assert_eq!((e.cursor_row, e.cursor_col), (0, 2));
}

#[test]
fn move_word_right_treats_underscore_as_part_of_the_word() {
    let mut e = editor_with("snake_case rest");
    e.cursor_row = 0;
    e.cursor_col = 0;
    e.move_word_right();
    assert_eq!(
        (e.cursor_row, e.cursor_col),
        (0, 10),
        "underscore is a word char so the whole identifier is one word"
    );
}

#[test]
fn move_word_left_jumps_to_start_of_current_word() {
    let mut e = editor_with("hello world");
    e.cursor_row = 0;
    e.cursor_col = 11;
    e.move_word_left();
    assert_eq!((e.cursor_row, e.cursor_col), (0, 6));
}

#[test]
fn move_word_left_skips_back_over_whitespace_and_punctuation() {
    let mut e = editor_with("hello   world!!!end");
    e.cursor_row = 0;
    e.cursor_col = 19;
    e.move_word_left();
    assert_eq!(
        (e.cursor_row, e.cursor_col),
        (0, 16),
        "from end of 'end' the backward-word lands at start of 'end'"
    );
    e.move_word_left();
    assert_eq!(
        (e.cursor_row, e.cursor_col),
        (0, 8),
        "next backward-word skips '!!!' and lands at start of 'world'"
    );
}

#[test]
fn move_word_left_at_bol_jumps_to_end_of_previous_line() {
    let mut e = editor_with("foo bar\nbaz");
    e.cursor_row = 1;
    e.cursor_col = 0;
    e.move_word_left();
    assert_eq!(
        (e.cursor_row, e.cursor_col),
        (0, 4),
        "backward-word from start of line 1 should cross the boundary and land at start of 'bar' on line 0"
    );
}

#[test]
fn move_word_left_at_bof_is_a_noop() {
    let mut e = editor_with("hi");
    e.cursor_row = 0;
    e.cursor_col = 0;
    e.move_word_left();
    assert_eq!((e.cursor_row, e.cursor_col), (0, 0));
}

#[test]
fn duplicate_lines_down_with_no_selection_copies_the_current_line_below_and_moves_cursor_to_it() {
    let mut e = editor_with("alpha\nbeta\ngamma");
    e.cursor_row = 1;
    e.cursor_col = 2;
    e.duplicate_lines_down();
    assert_eq!(e.lines, vec!["alpha", "beta", "beta", "gamma"]);
    assert_eq!((e.cursor_row, e.cursor_col), (2, 2));
    assert!(e.dirty);
}

#[test]
fn duplicate_lines_up_with_no_selection_copies_the_current_line_above_and_keeps_cursor_on_the_copy()
{
    let mut e = editor_with("alpha\nbeta\ngamma");
    e.cursor_row = 1;
    e.cursor_col = 3;
    e.duplicate_lines_up();
    assert_eq!(e.lines, vec!["alpha", "beta", "beta", "gamma"]);
    assert_eq!(
        (e.cursor_row, e.cursor_col),
        (1, 3),
        "cursor stays at the same row index — that row is now the upper copy, the original was pushed to row 2"
    );
}

#[test]
fn duplicate_lines_down_with_multiline_selection_duplicates_the_whole_block() {
    let mut e = editor_with("a\nb\nc\nd");
    e.cursor_row = 2;
    e.cursor_col = 1;
    e.selection = Some(EditorSelection {
        anchor: (1, 0),
        head: (2, 1),
    });
    e.duplicate_lines_down();
    assert_eq!(e.lines, vec!["a", "b", "c", "b", "c", "d"]);
    assert_eq!(
        (e.cursor_row, e.cursor_col),
        (4, 1),
        "cursor follows the duplicated block — it was at (2,1) over the original 'c', now it's at (4,1) over the duplicate"
    );
    assert_eq!(
        e.selection.unwrap(),
        EditorSelection {
            anchor: (3, 0),
            head: (4, 1),
        },
        "the selection migrates onto the duplicated block too so the next gesture acts on the copy"
    );
}

#[test]
fn duplicate_lines_up_with_multiline_selection_keeps_the_cursor_on_the_upper_copy() {
    let mut e = editor_with("a\nb\nc\nd");
    e.cursor_row = 2;
    e.cursor_col = 0;
    e.selection = Some(EditorSelection {
        anchor: (1, 0),
        head: (2, 1),
    });
    e.duplicate_lines_up();
    assert_eq!(e.lines, vec!["a", "b", "c", "b", "c", "d"]);
    assert_eq!(
        (e.cursor_row, e.cursor_col),
        (2, 0),
        "the duplicate sits at rows 1-2 and the original was pushed down to rows 3-4; cursor at (2,0) is on the upper copy"
    );
}

#[test]
fn duplicate_lines_down_then_undo_restores_the_buffer_in_one_step() {
    let mut e = editor_with("alpha\nbeta\ngamma");
    e.cursor_row = 1;
    e.cursor_col = 0;
    e.duplicate_lines_down();
    assert_eq!(e.lines.len(), 4);
    assert!(e.undo());
    assert_eq!(e.lines, vec!["alpha", "beta", "gamma"]);
    assert_eq!((e.cursor_row, e.cursor_col), (1, 0));
}

#[test]
fn duplicate_lines_up_at_first_row_inserts_the_copy_at_row_zero() {
    let mut e = editor_with("alpha\nbeta");
    e.cursor_row = 0;
    e.cursor_col = 4;
    e.duplicate_lines_up();
    assert_eq!(e.lines, vec!["alpha", "alpha", "beta"]);
    assert_eq!((e.cursor_row, e.cursor_col), (0, 4));
}

#[test]
fn goto_top_lands_on_row_zero_col_zero() {
    let mut e = editor_with("alpha\nbeta\ngamma");
    e.cursor_row = 2;
    e.cursor_col = 4;
    e.goto_top();
    assert_eq!((e.cursor_row, e.cursor_col), (0, 0));
}

#[test]
fn goto_bottom_lands_on_last_row_col_zero() {
    let mut e = editor_with("alpha\nbeta\ngamma");
    e.cursor_row = 0;
    e.cursor_col = 3;
    e.goto_bottom();
    assert_eq!((e.cursor_row, e.cursor_col), (2, 0));
}

#[test]
fn cursor_top_and_bottom_keep_the_selection_anchor() {
    let mut e = editor_with("alpha\nbeta\ngamma");
    e.cursor_row = 1;
    e.cursor_col = 2;
    e.start_selection_at_cursor();
    e.cursor_bottom();
    e.extend_selection_to_cursor();
    assert_eq!((e.cursor_row, e.cursor_col), (2, 5));
    assert_eq!(e.selection_text(), "ta\ngamma");
    e.cursor_top();
    e.extend_selection_to_cursor();
    assert_eq!(e.selection_text(), "alpha\nbe");
}

#[test]
fn goto_line_uses_one_based_indexing_and_clamps_to_last_row() {
    let mut e = editor_with("alpha\nbeta\ngamma");
    e.goto_line(2);
    assert_eq!((e.cursor_row, e.cursor_col), (1, 0));
    e.goto_line(999);
    assert_eq!((e.cursor_row, e.cursor_col), (2, 0));
    e.goto_line(0);
    assert_eq!((e.cursor_row, e.cursor_col), (0, 0));
}

#[test]
fn yank_lines_one_returns_current_line_with_trailing_newline() {
    let mut e = editor_with("alpha\nbeta\ngamma");
    e.cursor_row = 1;
    let yanked = e.yank_lines(1);
    assert_eq!(yanked, "beta\n");
    assert_eq!(e.lines, vec!["alpha", "beta", "gamma"]);
}

#[test]
fn yank_lines_n_grabs_current_plus_below() {
    let mut e = editor_with("alpha\nbeta\ngamma\ndelta");
    e.cursor_row = 1;
    let yanked = e.yank_lines(3);
    assert_eq!(yanked, "beta\ngamma\ndelta\n");
}

#[test]
fn delete_lines_one_drops_current_row() {
    let mut e = editor_with("alpha\nbeta\ngamma");
    e.cursor_row = 1;
    let yanked = e.delete_lines(1);
    assert_eq!(yanked, "beta\n");
    assert_eq!(e.lines, vec!["alpha", "gamma"]);
    assert_eq!((e.cursor_row, e.cursor_col), (1, 0));
}

#[test]
fn delete_lines_n_clamps_at_end_of_buffer() {
    let mut e = editor_with("alpha\nbeta\ngamma");
    e.cursor_row = 1;
    let yanked = e.delete_lines(99);
    assert_eq!(yanked, "beta\ngamma\n");
    assert_eq!(e.lines, vec!["alpha"]);
    assert_eq!((e.cursor_row, e.cursor_col), (0, 0));
}

#[test]
fn delete_lines_leaves_an_empty_line_when_buffer_emptied() {
    let mut e = editor_with("alpha\nbeta");
    e.cursor_row = 0;
    e.delete_lines(2);
    assert_eq!(e.lines, vec![""]);
    assert_eq!((e.cursor_row, e.cursor_col), (0, 0));
}

#[test]
fn delete_lines_is_undoable_in_one_step() {
    let mut e = editor_with("alpha\nbeta\ngamma");
    e.cursor_row = 1;
    e.delete_lines(1);
    assert!(e.undo());
    assert_eq!(e.lines, vec!["alpha", "beta", "gamma"]);
    assert_eq!((e.cursor_row, e.cursor_col), (1, 0));
}

#[test]
fn kill_to_bol_removes_head_of_current_line() {
    let mut e = editor_with("hello world\nnext");
    e.cursor_row = 0;
    e.cursor_col = 6;
    let killed = e.kill_to_bol();
    assert_eq!(killed, "hello ");
    assert_eq!(e.lines, vec!["world", "next"]);
    assert_eq!((e.cursor_row, e.cursor_col), (0, 0));
}

#[test]
fn kill_to_bol_at_col_zero_is_noop() {
    let mut e = editor_with("hello\nnext");
    e.cursor_row = 0;
    e.cursor_col = 0;
    let killed = e.kill_to_bol();
    assert_eq!(killed, "");
    assert_eq!(e.lines, vec!["hello", "next"]);
}

/// #1577: the columns of each row a find in a range highlights.
#[test]
fn find_window_covers_the_range_rows_only() {
    let range = Some(((2, 4), (4, 3)));
    assert_eq!(find_window(None, 9), None, "no range: the whole line");
    assert_eq!(find_window(range, 1), Some((0, 0)), "above");
    assert_eq!(find_window(range, 2), Some((4, usize::MAX)));
    assert_eq!(find_window(range, 3), Some((0, usize::MAX)));
    assert_eq!(find_window(range, 4), Some((0, 3)));
    assert_eq!(find_window(range, 5), Some((0, 0)), "below");
}

#[test]
fn open_line_below_inserts_empty_line_after_current() {
    let mut e = editor_with("alpha\nbeta");
    e.cursor_row = 0;
    e.cursor_col = 3;
    e.open_line_below();
    assert_eq!(e.lines, vec!["alpha", "", "beta"]);
    assert_eq!((e.cursor_row, e.cursor_col), (1, 0));
}

#[test]
fn open_line_below_inherits_indent_from_current_row() {
    let mut e = editor_with("    alpha\nbeta");
    e.cursor_row = 0;
    e.cursor_col = 6;
    e.open_line_below();
    assert_eq!(e.lines, vec!["    alpha", "    ", "beta"]);
    assert_eq!((e.cursor_row, e.cursor_col), (1, 4));
}

#[test]
fn open_line_above_inserts_empty_line_before_current() {
    let mut e = editor_with("alpha\nbeta");
    e.cursor_row = 1;
    e.cursor_col = 2;
    e.open_line_above();
    assert_eq!(e.lines, vec!["alpha", "", "beta"]);
    assert_eq!((e.cursor_row, e.cursor_col), (1, 0));
}

#[test]
fn open_line_above_inherits_indent_from_current_row() {
    let mut e = editor_with("alpha\n        beta");
    e.cursor_row = 1;
    e.cursor_col = 10;
    e.open_line_above();
    assert_eq!(e.lines, vec!["alpha", "        ", "        beta"]);
    assert_eq!((e.cursor_row, e.cursor_col), (1, 8));
}

#[test]
fn kill_to_eol_removes_tail_of_current_line() {
    let mut e = editor_with("hello world\nnext");
    e.cursor_row = 0;
    e.cursor_col = 5;
    let killed = e.kill_to_eol();
    assert_eq!(killed, " world");
    assert_eq!(e.lines, vec!["hello", "next"]);
    assert_eq!((e.cursor_row, e.cursor_col), (0, 5));
}

#[test]
fn kill_to_eol_at_eol_is_noop() {
    let mut e = editor_with("hello\nnext");
    e.cursor_row = 0;
    e.cursor_col = 5;
    let killed = e.kill_to_eol();
    assert_eq!(killed, "");
    assert_eq!(e.lines, vec!["hello", "next"]);
}

#[test]
fn move_left_crosses_line_boundary() {
    let mut e = editor_with("abc\ndef");
    e.cursor_row = 1;
    e.cursor_col = 0;
    e.move_left();
    assert_eq!(e.cursor_row, 0);
    assert_eq!(e.cursor_col, 3);
}

#[test]
fn move_right_crosses_line_boundary() {
    let mut e = editor_with("abc\ndef");
    e.cursor_row = 0;
    e.cursor_col = 3;
    e.move_right();
    assert_eq!(e.cursor_row, 1);
    assert_eq!(e.cursor_col, 0);
}

#[test]
fn move_up_clamps_column() {
    let mut e = editor_with("ab\nlongline");
    e.cursor_row = 1;
    e.cursor_col = 7;
    e.move_up();
    assert_eq!(e.cursor_row, 0);
    assert_eq!(e.cursor_col, 2);
}

#[test]
fn home_and_end() {
    let mut e = editor_with("hello world");
    e.cursor_col = 5;
    e.home_line();
    assert_eq!(e.cursor_col, 0);
    e.end_line();
    assert_eq!(e.cursor_col, 11);
}

/// The issue's paragraph, `Notes: word0 … word59`, wrapped in a 60-column
/// pane, with its wrap rows as `(start, end)` columns.
fn wrapped_paragraph(wrap: bool) -> (Editor, Vec<(usize, usize)>) {
    let words: Vec<String> = (0..60).map(|i| format!("word{i}")).collect();
    let mut e = editor_with(&format!("Notes: {}", words.join(" ")));
    e.wrap_override = Some(wrap);
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 20,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut e as &mut Editor).render(area, &mut buf);
    let segs = e.line_segments(0, e.visible_text_width().max(1));
    assert!(segs.len() >= 4, "the paragraph wraps: {segs:?}");
    (e, segs)
}

/// #1569: with wrap on, End went to the end of the whole line from any of
/// its rows. Like VS Code it now stops at the end of the caret's row (on
/// the space the row broke at), and a second End goes to the line's end.
#[test]
fn end_in_a_wrapped_line_stops_at_the_end_of_the_row_first() {
    let (mut e, segs) = wrapped_paragraph(true);
    let len = e.line_char_len(0);
    e.cursor_col = segs[1].0;
    e.end_key();
    assert_eq!(e.cursor_col, segs[1].1 - 1, "the end of row 2: {segs:?}");
    assert_eq!(e.lines[0].chars().nth(e.cursor_col), Some(' '));
    let width = e.visible_text_width();
    assert_eq!(
        e.segment_index_of_col(0, e.cursor_col, width),
        1,
        "still on row 2"
    );
    e.end_key();
    assert_eq!(
        e.cursor_col, len,
        "a second End goes to the end of the line"
    );
    e.end_key();
    assert_eq!(e.cursor_col, len);
}

/// #987: Home goes to the first non-whitespace character, then toggles
/// with the line's start; Shift+Home selects the same way.
#[test]
fn home_goes_past_the_indentation_then_toggles_with_the_line_start() {
    let mut e = editor_with("    x = 1\ny = 2\n   \n");
    e.cursor_col = 9;
    e.home_key();
    assert_eq!(e.cursor_col, 4, "the first non-whitespace character");
    e.home_key();
    assert_eq!(e.cursor_col, 0, "then the line's start");
    e.home_key();
    assert_eq!(e.cursor_col, 4, "and back");
    e.cursor_col = 2;
    e.home_key();
    assert_eq!(e.cursor_col, 4, "from inside the indentation too");
    (e.cursor_row, e.cursor_col) = (1, 5);
    e.home_key();
    assert_eq!(e.cursor_col, 0, "no indentation: the start");
    e.home_key();
    assert_eq!(e.cursor_col, 0);
    (e.cursor_row, e.cursor_col) = (2, 3);
    e.home_key();
    assert_eq!(e.cursor_col, 0, "a blank line toggles end and start");
    e.home_key();
    assert_eq!(e.cursor_col, 3);
    e.cursor_row = 0;
    e.cursor_col = 9;
    e.start_selection_at_cursor();
    e.home_key();
    e.extend_selection_to_cursor();
    assert_eq!(e.selection_text(), "x = 1");
    e.home_line();
    assert_eq!(e.cursor_col, 0, "Ctrl+A stays a hard line start");
}

/// #1569: Home from inside a wrapped row stops at that row's start first.
#[test]
fn home_in_a_wrapped_line_stops_at_the_start_of_the_row_first() {
    let (mut e, segs) = wrapped_paragraph(true);
    e.cursor_col = segs[2].0 + 5;
    e.home_key();
    assert_eq!(e.cursor_col, segs[2].0, "the start of row 3: {segs:?}");
    e.home_key();
    assert_eq!(
        e.cursor_col, 0,
        "a second Home goes to the start of the line"
    );
}

/// #1569: with `home_walks_rows`, Home on a continuation row's start goes
/// to the start of the row above, one row per press, and off it keeps
/// VS Code's jump to the line's own stops.
#[test]
fn home_walks_back_one_wrapped_row_per_press_when_asked() {
    let (mut e, segs) = wrapped_paragraph(true);
    e.home_walks_rows = true;
    e.cursor_col = segs[2].0 + 5;
    e.home_key();
    assert_eq!(e.cursor_col, segs[2].0, "the start of row 3: {segs:?}");
    e.home_key();
    assert_eq!(e.cursor_col, segs[1].0, "the start of row 2: {segs:?}");
    e.home_key();
    assert_eq!(e.cursor_col, segs[0].0, "the start of row 1: {segs:?}");
    e.home_key();
    assert_eq!(e.cursor_col, 0, "the line starts at column 0, so it stays");
    // Off: from a row's start straight to the line's start.
    let (mut e, segs) = wrapped_paragraph(true);
    e.cursor_col = segs[2].0;
    e.home_key();
    assert_eq!(e.cursor_col, 0, "not the row above: {segs:?}");
}

/// #1569: on an indented line the walk ends at the line's own stops: the
/// first non-blank, then column 0, then back.
#[test]
fn home_walk_ends_at_the_indentation_stops() {
    let text = format!("    {}", "word ".repeat(40));
    let mut e = editor_with(&text);
    e.wrap_override = Some(true);
    e.home_walks_rows = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 20,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut e as &mut Editor).render(area, &mut buf);
    let segs = e.line_segments(0, e.visible_text_width().max(1));
    assert!(segs.len() > 2, "the line wraps: {segs:?}");
    let (row_start, row_end) = segs[1];
    let chars: Vec<char> = e.lines[0].chars().collect();
    let word = (row_start + 1..row_end)
        .rfind(|&i| !chars[i].is_whitespace() && chars[i - 1].is_whitespace())
        .expect("a word starts inside row 2");
    e.cursor_col = word + 2;
    e.home_key();
    assert_eq!(e.cursor_col, row_start, "the start of row 2: {segs:?}");
    e.home_key();
    assert_eq!(e.cursor_col, segs[0].0, "the start of row 1: {segs:?}");
    e.home_key();
    assert_eq!(e.cursor_col, 4, "the line's first non-blank");
    e.home_key();
    assert_eq!(e.cursor_col, 0, "column 0");
    e.home_key();
    assert_eq!(e.cursor_col, 4);
}

/// #1569: a wrapped row can begin on a combining mark (the row before
/// broke after the space it combines with); Home must not leave the caret
/// inside that cluster.
#[test]
fn home_in_a_wrapped_line_stays_on_a_grapheme_boundary() {
    let (probe, _) = wrapped_paragraph(true);
    let width = probe.visible_text_width();
    let text = format!("{} \u{301}{}", "x".repeat(width - 1), "y".repeat(width + 5));
    let mut e = editor_with(&text);
    e.wrap_override = Some(true);
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 20,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut e as &mut Editor).render(area, &mut buf);
    let segs = e.line_segments(0, e.visible_text_width().max(1));
    assert_eq!(segs[1].0, width, "row 2 starts on the mark: {segs:?}");
    e.cursor_col = width + 3;
    e.home_key();
    assert_eq!(e.cursor_col, width - 1, "the space the mark sits on");
}

/// #1569: on the first and last rows the row's ends are the line's.
#[test]
fn home_and_end_on_the_outer_rows_of_a_wrapped_line_reach_the_line_ends() {
    let (mut e, segs) = wrapped_paragraph(true);
    let len = e.line_char_len(0);
    e.cursor_col = segs.last().unwrap().0 + 1;
    e.end_key();
    assert_eq!(e.cursor_col, len);
    e.cursor_col = 3;
    e.home_key();
    assert_eq!(e.cursor_col, 0);
}

/// #1569 negative: with wrap off, Home and End go to the line's ends.
#[test]
fn home_and_end_without_wrap_go_to_the_line_ends() {
    let (mut e, segs) = wrapped_paragraph(false);
    let len = e.line_char_len(0);
    e.cursor_col = segs[1].0;
    e.end_key();
    assert_eq!(e.cursor_col, len);
    e.cursor_col = segs[2].0 + 5;
    e.home_key();
    assert_eq!(e.cursor_col, 0);
}

#[test]
fn open_reads_file_and_splits_lines() {
    let mut tmp = NamedTempFile::new().unwrap();
    writeln!(tmp, "alpha").unwrap();
    writeln!(tmp, "beta").unwrap();
    write!(tmp, "gamma").unwrap();
    let mut e = Editor::new();
    e.open(tmp.path()).unwrap();
    assert_eq!(
        e.lines,
        vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()]
    );
    assert_eq!(e.cursor_row, 0);
    assert_eq!(e.cursor_col, 0);
    assert!(!e.dirty);
}

#[test]
fn open_detects_crlf_and_save_preserves_it() {
    use std::io::Read;
    let mut tmp = NamedTempFile::new().unwrap();
    tmp.write_all(b"alpha\r\nbeta\r\n").unwrap();
    let mut e = Editor::new();
    e.open(tmp.path()).unwrap();
    assert_eq!(e.eol, LineEnding::Crlf, "CRLF file detected");
    assert_eq!(e.lines, vec!["alpha".to_string(), "beta".to_string()]);
    e.save_to_disk().unwrap();
    let mut raw = Vec::new();
    std::fs::File::open(tmp.path())
        .unwrap()
        .read_to_end(&mut raw)
        .unwrap();
    assert_eq!(
        raw, b"alpha\r\nbeta\r\n",
        "save re-applies CRLF, final newline included"
    );
}

/// #936: one CRLF line pasted into an LF file. The file stays LF, so a
/// one-character edit saves as that edit plus the stray line brought
/// into line, not every line rewritten.
#[test]
fn one_crlf_line_in_an_lf_file_keeps_it_lf() {
    let mut text: Vec<String> = (0..200).map(|i| format!("setting_{i} = {i}\n")).collect();
    text[57] = "pasted_from_windows = True\r\n".into();
    let mut tmp = NamedTempFile::new().unwrap();
    tmp.write_all(text.concat().as_bytes()).unwrap();
    let mut e = Editor::new();
    e.open(tmp.path()).unwrap();
    assert_eq!(e.eol, LineEnding::Lf);
    e.cursor_col = e.lines[0].chars().count();
    e.insert_char('0');
    e.save_to_disk().unwrap();
    let saved = std::fs::read_to_string(tmp.path()).unwrap();
    assert!(!saved.contains('\r'), "no CRLF left");
    let changed: Vec<usize> = saved
        .lines()
        .zip(text.iter())
        .enumerate()
        .filter(|(_, (now, was))| *now != was.trim_end_matches(['\r', '\n']))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(changed, vec![0], "only the edited line's text changed");
}

/// The majority decides both ways, and a tie stays LF.
#[test]
fn the_line_ending_is_the_one_most_lines_use() {
    assert_eq!(LineEnding::detect("a\r\nb\r\nc\n"), LineEnding::Crlf);
    assert_eq!(LineEnding::detect("a\r\nb\nc\n"), LineEnding::Lf);
    assert_eq!(LineEnding::detect("a\r\nb\n"), LineEnding::Lf);
    assert_eq!(LineEnding::detect("a\r\n"), LineEnding::Crlf);
    assert_eq!(LineEnding::detect("no break"), LineEnding::Lf);
    // A lone CR is a break that counts toward CRLF, as in VS Code.
    assert_eq!(LineEnding::detect("a\n\rb\r\nc"), LineEnding::Crlf);
    assert_eq!(LineEnding::detect("a\rb\nc\n"), LineEnding::Lf);
}

#[test]
fn open_defaults_to_lf_for_unix_files() {
    let mut tmp = NamedTempFile::new().unwrap();
    tmp.write_all(b"a\nb\n").unwrap();
    let mut e = Editor::new();
    e.open(tmp.path()).unwrap();
    assert_eq!(e.eol, LineEnding::Lf);
}

#[test]
fn reopen_with_encoding_decodes_windows_1252() {
    use std::io::Write as _;
    // 0xE9 is 'é' in Windows-1252 but invalid UTF-8 (renders as the
    // replacement char when first opened as UTF-8).
    let mut tmp = NamedTempFile::new().unwrap();
    tmp.write_all(&[b'c', b'a', b'f', 0xE9]).unwrap();
    tmp.flush().unwrap();
    let mut e = Editor::new();
    e.open(tmp.path()).unwrap();
    assert_ne!(e.lines[0], "café", "UTF-8 decode mangles the 0xE9 byte");
    e.reopen_with_encoding(encoding_rs::WINDOWS_1252).unwrap();
    assert_eq!(e.lines[0], "café", "Windows-1252 decodes 0xE9 as é");
    assert_eq!(e.encoding, encoding_rs::WINDOWS_1252);
}

/// The issue's bytes: windows-1252 for "Grüße aus Köln\nCafé crème\n".
const LEGACY_1432: &[u8] = b"Gr\xfc\xdfe aus K\xf6ln\nCaf\xe9 cr\xe8me\n";

fn project_1432(editorconfig: Option<&str>, bytes: &[u8]) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    if let Some(ec) = editorconfig {
        std::fs::write(dir.path().join(".editorconfig"), ec).unwrap();
    }
    let file = dir.path().join("notes.txt");
    std::fs::write(&file, bytes).unwrap();
    (dir, file)
}

/// #1432: `charset = latin1` decodes a BOM-less file as windows-1252, and
/// saving it unchanged writes back exactly the bytes it was read from.
#[test]
fn an_editorconfig_charset_decodes_a_file_without_a_bom() {
    let (_dir, file) = project_1432(Some("root = true\n[*]\ncharset = latin1\n"), LEGACY_1432);
    let mut e = Editor::new();
    e.open(&file).unwrap();
    assert_eq!(e.lines[0], "Grüße aus Köln");
    assert_eq!(e.lines[1], "Café crème");
    assert_eq!(e.encoding, encoding_rs::WINDOWS_1252);
    assert!(!e.decode_lossy);
    e.save_to_disk().unwrap();
    assert_eq!(std::fs::read(&file).unwrap(), LEGACY_1432);
}

/// #1432: the issue's smallest case, `47 72 FC 44 65` under latin1.
#[test]
fn latin1_decodes_the_issue_s_five_bytes() {
    let (_dir, file) = project_1432(Some("[*]\ncharset = latin1\n"), b"\x47\x72\xFC\x44\x65");
    let mut e = Editor::new();
    e.open(&file).unwrap();
    assert_eq!(e.lines[0], "GrüDe");
}

/// #1432: `charset = utf-8-bom` gives a file without the mark one on
/// its next save.
#[test]
fn a_utf_8_bom_charset_adds_the_mark_on_save() {
    let (_dir, file) = project_1432(Some("[*.txt]\ncharset = utf-8-bom\n"), b"");
    let mut e = Editor::new();
    e.open(&file).unwrap();
    e.insert_char('h');
    e.insert_char('i');
    e.save_to_disk().unwrap();
    assert_eq!(std::fs::read(&file).unwrap(), b"\xEF\xBB\xBFhi");
}

/// #1432 negative: a BOM wins over the charset, which only speaks for
/// files that do not say what they are.
#[test]
fn a_bom_beats_the_editorconfig_charset() {
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice("Grüße\n".as_bytes());
    let (_dir, file) = project_1432(Some("[*]\ncharset = latin1\n"), &bytes);
    let mut e = Editor::new();
    e.open(&file).unwrap();
    assert_eq!(e.lines[0], "Grüße");
    assert_eq!(e.encoding, encoding_rs::UTF_8);
    assert!(e.bom);
}

/// #1432 negative: a charset for other files, a value naming no
/// encoding, or no `.editorconfig` at all leaves the file UTF-8.
#[test]
fn without_a_charset_for_the_file_it_stays_utf_8() {
    for ec in [
        Some("[*.cs]\ncharset = latin1\n"),
        Some("[*]\ncharset = latin9000\n"),
        None,
    ] {
        let (_dir, file) = project_1432(ec, LEGACY_1432);
        let mut e = Editor::new();
        e.open(&file).unwrap();
        assert_eq!(e.encoding, encoding_rs::UTF_8, "{ec:?}");
        assert!(e.decode_lossy, "{ec:?}");
        assert!(!e.bom, "{ec:?}");
    }
}

/// #1432: a workspace's `files_encoding` decodes the file when no
/// `.editorconfig` says otherwise, and a charset that does wins.
#[test]
fn the_workspace_encoding_applies_below_the_editorconfig_charset() {
    let (dir, file) = project_1432(None, LEGACY_1432);
    crate::file_encoding::set_workspace_default(
        dir.path(),
        crate::file_encoding::parse("windows1252"),
    );
    let mut e = Editor::new();
    e.open(&file).unwrap();
    assert_eq!(e.lines[0], "Grüße aus Köln");
    std::fs::write(dir.path().join(".editorconfig"), "[*]\ncharset = utf-8\n").unwrap();
    let mut e = Editor::new();
    e.open(&file).unwrap();
    assert_eq!(e.encoding, encoding_rs::UTF_8);
    crate::file_encoding::set_workspace_default(dir.path(), None);
}

/// #1432: Reopen with Encoding is remembered for the file, so closing the
/// tab and opening it again keeps Shift_JIS instead of mojibake.
#[test]
fn a_reopen_with_encoding_choice_is_kept_for_the_next_open() {
    // "日本語" in Shift_JIS.
    let sjis: &[u8] = b"\x93\xfa\x96\x7b\x8c\xea\n";
    let (dir, file) = project_1432(Some("[*]\ncharset = latin1\n"), sjis);
    let other = dir.path().join("other.txt");
    std::fs::write(&other, sjis).unwrap();
    let mut e = Editor::new();
    e.open(&file).unwrap();
    e.reopen_with_encoding(encoding_rs::SHIFT_JIS).unwrap();
    assert_eq!(e.lines[0], "日本語");
    let mut again = Editor::new();
    again.open(&file).unwrap();
    assert_eq!(
        again.encoding,
        encoding_rs::SHIFT_JIS,
        "the choice beats the charset"
    );
    assert_eq!(again.lines[0], "日本語");
    // Negative: only that file. Its neighbour follows the charset.
    let mut neighbour = Editor::new();
    neighbour.open(&other).unwrap();
    assert_eq!(neighbour.encoding, encoding_rs::WINDOWS_1252);
}

/// A same-path reload (the FS-sync sweep on a clean buffer, or an explicit
/// revert) re-enters `open`, which auto-detects. Without a guard that
/// throws away the encoding the user picked through "Reopen with
/// Encoding", and the bytes get decoded as UTF-8 — so the buffer fills
/// with replacement characters and the next save writes them over the
/// original file.
#[test]
fn a_reload_keeps_the_encoding_the_user_reopened_with() {
    use std::io::Write as _;
    let mut tmp = NamedTempFile::new().unwrap();
    tmp.write_all(&[b'c', b'a', b'f', 0xE9]).unwrap();
    tmp.flush().unwrap();
    let mut e = Editor::new();
    e.open(tmp.path()).unwrap();
    e.reopen_with_encoding(encoding_rs::WINDOWS_1252).unwrap();
    // The file changes on disk, still Windows-1252 and still BOM-less.
    std::fs::write(tmp.path(), [b't', b'h', 0xE9]).unwrap();
    e.reload_if_clean().unwrap().unwrap();
    assert_eq!(
        e.encoding,
        encoding_rs::WINDOWS_1252,
        "a same-path reload must keep the encoding the user chose"
    );
    assert_eq!(e.lines[0], "thé", "and decode the new bytes with it");
}

/// A UTF-16 file must survive the whole round trip: open, edit, save,
/// open again. Two things blocked it. `Encoding::encode` is decode-only
/// for UTF-16 per the WHATWG spec, so it silently emitted UTF-8; and
/// `is_binary` runs BEFORE the BOM sniff, so UTF-16's NUL bytes were
/// rejected as binary — meaning croft could offer UTF-16 in the encoding
/// picker and then never read back what it wrote.
#[test]
fn a_utf16_file_survives_open_edit_save_open() {
    let tmp = NamedTempFile::new().unwrap();
    let mut bytes = vec![0xFF, 0xFE]; // UTF-16LE BOM
    for u in "hi\nthere\n".encode_utf16() {
        bytes.extend_from_slice(&u.to_le_bytes());
    }
    std::fs::write(tmp.path(), &bytes).unwrap();

    let mut e = Editor::new();
    e.open(tmp.path()).unwrap();
    assert_eq!(e.encoding, encoding_rs::UTF_16LE, "opened as UTF-16LE");
    assert_eq!(e.lines[0], "hi");
    assert_eq!(e.lines[1], "there");
    e.cursor_col = 2;
    e.insert_char('!');
    e.save_to_disk().unwrap();

    let out = std::fs::read(tmp.path()).unwrap();
    assert_eq!(&out[..2], &[0xFF, 0xFE], "the BOM must survive");
    let mut e2 = Editor::new();
    e2.open(tmp.path())
        .expect("croft must be able to reopen what it just wrote");
    assert_eq!(e2.encoding, encoding_rs::UTF_16LE);
    assert_eq!(e2.lines[0], "hi!", "the edit round-tripped");
    assert_eq!(e2.lines[1], "there");
}

/// UTF-16 is self-describing only via its BOM: without one, the bytes are
/// NUL-laden and no tool (croft included) can tell them from binary. VS
/// Code always writes it for its UTF-16 entries, so a file that never had
/// one still gets one when saved as UTF-16.
#[test]
fn saving_as_utf16_always_writes_a_bom() {
    let tmp = NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), "plain\n").unwrap();
    let mut e = Editor::new();
    e.open(tmp.path()).unwrap();
    assert!(!e.bom, "the source file had no BOM");
    e.encoding = encoding_rs::UTF_16BE;
    e.dirty = true;
    e.save_to_disk().unwrap();
    let out = std::fs::read(tmp.path()).unwrap();
    assert_eq!(&out[..2], &[0xFE, 0xFF], "UTF-16BE BOM written anyway");
    let mut e2 = Editor::new();
    e2.open(tmp.path()).expect("and it reopens");
    assert_eq!(e2.lines[0], "plain");
}

/// A UTF-8 BOM is stripped by `decode` and nothing re-emitted it, so every
/// save quietly dropped it.
#[test]
fn saving_keeps_a_utf8_bom() {
    let tmp = NamedTempFile::new().unwrap();
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(b"hi\n");
    std::fs::write(tmp.path(), &bytes).unwrap();
    let mut e = Editor::new();
    e.open(tmp.path()).unwrap();
    assert_eq!(e.lines[0], "hi");
    e.cursor_col = 2;
    e.insert_char('!');
    e.save_to_disk().unwrap();
    let out = std::fs::read(tmp.path()).unwrap();
    assert_eq!(&out[..3], &[0xEF, 0xBB, 0xBF], "the UTF-8 BOM must survive");
    assert_eq!(&out[3..6], b"hi!");
}

/// #1632: a Vue or Svelte component opens as that language, coloured,
/// with HTML's block comment.
#[test]
fn vue_and_svelte_components_open_in_their_language() {
    let tmp = tempfile::tempdir().unwrap();
    for (name, label) in [("App.vue", "Vue"), ("Counter.svelte", "Svelte")] {
        let path = tmp.path().join(name);
        std::fs::write(&path, "<script>\nlet n = 1;\n</script>\n<p>{n}</p>\n").unwrap();
        let mut e = Editor::new();
        e.open(&path).unwrap();
        assert_eq!(e.language_label(), label, "{name}");
        assert!(
            !e.highlights[1].is_empty(),
            "{name}: the script is coloured"
        );
        e.cursor_row = 3;
        assert!(e.toggle_block_comment(), "{name}");
        assert_eq!(e.lines[3], "<!-- <p>{n}</p> -->", "{name}");
    }
}

/// #1226: a Dockerfile, a Makefile, SQL and Java open in their own
/// language, so Toggle Line Comment uses that language's marker.
#[test]
fn common_repository_files_open_in_their_language() {
    let tmp = tempfile::tempdir().unwrap();
    let cases = [
        ("Dockerfile", "FROM alpine", "Dockerfile", "# FROM alpine"),
        ("Makefile", "all: build", "Makefile", "# all: build"),
        ("q.sql", "SELECT 1;", "SQL", "-- SELECT 1;"),
        ("A.java", "int x = 1;", "Java", "// int x = 1;"),
        ("app.rb", "puts 1", "Ruby", "# puts 1"),
        ("P.cs", "int x = 1;", "C#", "// int x = 1;"),
    ];
    for (name, text, label, commented) in cases {
        let path = tmp.path().join(name);
        std::fs::write(&path, text).unwrap();
        let mut e = Editor::new();
        e.open(&path).unwrap();
        assert_eq!(e.language_label(), label, "{name}");
        assert!(e.toggle_line_comment(), "{name}");
        assert_eq!(e.lines, vec![commented], "{name}");
    }
    // Negative: an unknown file stays Plain Text with no comment marker.
    let path = tmp.path().join("notes.txt");
    std::fs::write(&path, "hello").unwrap();
    let mut e = Editor::new();
    e.open(&path).unwrap();
    assert_eq!(e.language_label(), "Plain Text");
    assert!(!e.toggle_line_comment());
}

/// #1532: the status bar names SCSS, Less and Sass the way VS Code does
/// (they share the CSS grammar), and plain CSS stays CSS.
#[test]
fn style_sheet_dialects_get_their_own_labels() {
    let tmp = tempfile::TempDir::new().unwrap();
    for (name, label) in [
        ("app.scss", "SCSS"),
        ("theme.less", "Less"),
        ("old.sass", "Sass"),
        ("plain.css", "CSS"),
    ] {
        let path = tmp.path().join(name);
        std::fs::write(&path, ".a { color: red; }\n").unwrap();
        let mut e = Editor::new();
        e.open(&path).unwrap();
        assert_eq!(e.language_label(), label, "{name}");
    }
    // A hand-picked language wins over the extension.
    let path = tmp.path().join("x.scss");
    std::fs::write(&path, "x = 1\n").unwrap();
    let mut e = Editor::new();
    e.open(&path).unwrap();
    e.set_language(Some(crate::highlight::LangKind::Python));
    assert_eq!(e.language_label(), "Python");
}

#[test]
fn set_language_updates_the_label() {
    let mut e = editor_with("x = 1");
    e.set_language(Some(crate::highlight::LangKind::Python));
    assert_eq!(e.language_label(), "Python");
    e.set_language(None);
    assert_eq!(e.language_label(), "Plain Text");
}

#[test]
fn open_splits_on_lone_cr_to_match_lsp_line_numbering() {
    // A file with a stray lone `\r` (mixed line endings). The LSP/VS Code
    // treat `\r`, `\r\n`, and `\n` all as line breaks, so its token
    // positions count this as a line boundary. Rust's `str::lines` only
    // breaks on `\n`, so croft must split the same way or every LSP
    // position past the stray CR lands one row off.
    let mut tmp = NamedTempFile::new().unwrap();
    // "a" <LF> "" (the lone CR) "b" <CRLF> "c"
    write!(tmp, "a\n\rb\r\nc").unwrap();
    let mut e = Editor::new();
    e.open(tmp.path()).unwrap();
    assert_eq!(
        e.lines,
        vec![
            "a".to_string(),
            String::new(),
            "b".to_string(),
            "c".to_string()
        ],
        "lone CR and CRLF must each split a line, matching the LSP"
    );
}

/// A 32x16 favicon: a PNG wrapped in an ICO container (#1652).
fn favicon_bytes() -> Vec<u8> {
    let img: image::RgbaImage =
        image::ImageBuffer::from_pixel(32, 16, image::Rgba([74, 144, 226, 255]));
    let mut buf: Vec<u8> = Vec::new();
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Ico)
        .unwrap();
    assert!(buf.starts_with(b"\0\0\x01\0"), "setup: an ICO container");
    buf
}

/// #1652: a `.ico` opens in the image viewer with its size, not as hex.
#[test]
fn a_favicon_opens_as_an_image() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("favicon.ico");
    std::fs::write(&path, favicon_bytes()).unwrap();
    let mut e = Editor::new();
    e.open(&path).unwrap();
    assert!(e.hex.is_none(), "not the hex viewer");
    let img = e.image.as_ref().expect("image-mode tab");
    assert_eq!((img.pixel_w, img.pixel_h), (32, 16));
    assert_eq!(img.format_label, "ICO");
}

/// #1652: an icon with no extension routes to the image viewer by its
/// bytes, like an extensionless PNG.
#[test]
fn an_extensionless_icon_routes_to_the_image_viewer_by_content() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("icon");
    std::fs::write(&path, favicon_bytes()).unwrap();
    let mut e = Editor::new();
    e.open(&path).unwrap();
    let img = e.image.as_ref().expect("image-mode tab");
    assert_eq!((img.pixel_w, img.pixel_h), (32, 16));
}

/// Negative (#1652): a truncated `.ico` that does not decode falls back
/// to the hex viewer, without a panic.
#[test]
fn a_broken_icon_falls_back_to_hex() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("broken.ico");
    std::fs::write(&path, &favicon_bytes()[..30]).unwrap();
    let mut e = Editor::new();
    e.open(&path).unwrap();
    assert!(e.image.is_none());
    assert!(e.hex.is_some(), "an undecodable icon opens as bytes");
}

/// A 24x10 TIFF, as a scanner or a print workflow writes one (#1652).
fn tiff_bytes() -> Vec<u8> {
    let img: image::RgbImage = image::ImageBuffer::from_pixel(24, 10, image::Rgb([200, 40, 90]));
    let mut buf: Vec<u8> = Vec::new();
    image::DynamicImage::ImageRgb8(img)
        .write_to(
            &mut std::io::Cursor::new(&mut buf),
            image::ImageFormat::Tiff,
        )
        .unwrap();
    assert!(
        buf.starts_with(b"II*\0") || buf.starts_with(b"MM\0*"),
        "setup: a TIFF header"
    );
    buf
}

/// #1652: `.tif` and `.tiff` open in the image viewer with their size,
/// not as hex.
#[test]
fn a_tiff_opens_as_an_image() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["scan.tif", "scan.tiff", "SCAN.TIFF"] {
        let path = dir.path().join(name);
        std::fs::write(&path, tiff_bytes()).unwrap();
        let mut e = Editor::new();
        e.open(&path).unwrap();
        assert!(e.hex.is_none(), "{name}: not the hex viewer");
        let img = e.image.as_ref().expect("image-mode tab");
        assert_eq!((img.pixel_w, img.pixel_h), (24, 10), "{name}");
        assert_eq!(img.format_label, "TIFF", "{name}");
    }
}

/// #1652: a TIFF with no extension routes to the image viewer by its
/// bytes.
#[test]
fn an_extensionless_tiff_routes_to_the_image_viewer_by_content() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("scan");
    std::fs::write(&path, tiff_bytes()).unwrap();
    let mut e = Editor::new();
    e.open(&path).unwrap();
    let img = e.image.as_ref().expect("image-mode tab");
    assert_eq!((img.pixel_w, img.pixel_h), (24, 10));
}

/// A TIFF whose header declares a `w`x`h` grayscale canvas with no pixels
/// behind it: a few dozen bytes a decoder would size its buffer from.
fn declared_tiff(w: u32, h: u32) -> Vec<u8> {
    // (tag, type, value): width, height, bits, no compression,
    // black-is-zero, strip offset, rows per strip, strip bytes.
    let entries: [(u16, u16, u32); 8] = [
        (256, 4, w),
        (257, 4, h),
        (258, 3, 8),
        (259, 3, 1),
        (262, 3, 1),
        (273, 4, 0),
        (278, 4, h),
        (279, 4, w.saturating_mul(h)),
    ];
    let mut tiff = b"II*\0\x08\0\0\0".to_vec();
    tiff.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    for (tag, kind, value) in entries {
        tiff.extend_from_slice(&tag.to_le_bytes());
        tiff.extend_from_slice(&kind.to_le_bytes());
        tiff.extend_from_slice(&1u32.to_le_bytes());
        tiff.extend_from_slice(&value.to_le_bytes());
    }
    tiff.extend_from_slice(&0u32.to_le_bytes());
    tiff
}

/// #1652 review: on a terminal without inline images the picture is drawn
/// in half blocks, and that decode is held to the same pixel budget as
/// the inline bake (#1160). A tiny file declaring 529 megapixels is
/// refused by its header, before any decode; an ordinary TIFF still draws.
#[test]
fn halfblocks_refuse_a_canvas_over_the_decode_budget_before_decoding() {
    let bomb = declared_tiff(23_000, 23_000);
    assert!(bomb.len() < 128, "the bomb is tiny: {} bytes", bomb.len());
    assert_eq!(
        image_dimensions_of(&bomb),
        Some((23_000, 23_000)),
        "setup: the header parses, so the refusal is the budget's"
    );
    let decodes = || HALFBLOCK_DECODES.with(std::cell::Cell::get);
    let before = decodes();
    assert!(halfblock_cells(&bomb, 80, 24, Color::Rgb(0, 0, 0)).is_none());
    assert_eq!(decodes(), before, "refused before any decode");
    assert!(halfblock_cells(&tiff_bytes(), 80, 24, Color::Rgb(0, 0, 0)).is_some());
    assert_eq!(decodes(), before + 1, "an ordinary picture is decoded");
}

/// Negative (#1652): a truncated `.tiff` that does not decode falls back
/// to the hex viewer, without a panic.
#[test]
fn a_broken_tiff_falls_back_to_hex() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("broken.tiff");
    std::fs::write(&path, &tiff_bytes()[..12]).unwrap();
    let mut e = Editor::new();
    e.open(&path).unwrap();
    assert!(e.image.is_none());
    assert!(e.hex.is_some(), "an undecodable TIFF opens as bytes");
}

#[test]
fn open_png_populates_image_view_and_skips_text_buffer() {
    // 1×1 transparent PNG, hand-crafted via the image crate.
    let img: image::RgbaImage = image::ImageBuffer::from_pixel(1, 1, image::Rgba([0, 0, 0, 0]));
    let mut buf: Vec<u8> = Vec::new();
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pic.png");
    std::fs::write(&path, &buf).unwrap();
    let mut e = Editor::new();
    e.open(&path).unwrap();
    let img = e.image.as_ref().expect("image-mode tab");
    assert_eq!(img.pixel_w, 1);
    assert_eq!(img.pixel_h, 1);
    assert_eq!(img.format_label, "PNG");
    // Text scaffolding should be inert.
    assert_eq!(e.lines, vec![String::new()]);
    assert!(e.path.is_some());
    assert!(e.lang.is_none());
    assert!(!e.dirty);
}

/// A file rebuilt on disk reloads to the same path (and, for a PDF, the
/// same page and rect) - the overlay's re-emit key tells the loads apart
/// only through the content generation, so every reload must stamp a
/// fresh one even when the bytes happen to be identical.
#[test]
fn a_reloaded_image_carries_a_fresh_generation() {
    let img: image::RgbaImage = image::ImageBuffer::from_pixel(1, 1, image::Rgba([0, 0, 0, 0]));
    let mut buf: Vec<u8> = Vec::new();
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pic.png");
    std::fs::write(&path, &buf).unwrap();
    let mut e = Editor::new();
    e.open(&path).unwrap();
    let first = e.image.as_ref().unwrap().generation;
    e.open(&path).unwrap();
    let second = e.image.as_ref().unwrap().generation;
    assert_ne!(
        first, second,
        "a reload must stamp fresh content, or the baked overlay stays stale"
    );
}

/// The scrollbar is drawn against `lines + comment-box rows`, so the
/// drag must map through the same content length. Mapping through bare
/// `lines.len()` made the bar of a short file with a tall navigator
/// comment completely dead (metrics said "no overflow"), and made a
/// long file's thumb run away from the pointer.
#[test]
fn dragging_the_scrollbar_of_a_file_with_a_comment_box_scrolls() {
    let mut e = editor_with("a\nb\nc\nd\ne");
    e.comment_boxes.push(CommentBox {
        id: 1,
        line: 1,
        author: String::from("navigator"),
        body: (0..20)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n"),
    });
    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 10,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut e as &mut Editor).render(area, &mut buf);
    assert!(
        e.last_scrollbar.width > 0,
        "the box overflows the viewport, so a bar must be drawn"
    );
    assert!(
        e.scroll_to_bar_y(e.last_scrollbar.y + e.last_scrollbar.height / 2),
        "a drag on the drawn bar must scroll, not be refused as no-overflow"
    );
}

/// End means "last page", which is unanswerable when the page count is
/// unknown (sips-only Macs where mdls reports nothing): the sentinel
/// must not be rendered literally, which sprayed
/// "PDF page 4294967295 failed" into the status bar.
#[test]
fn end_with_an_unknown_page_count_reports_instead_of_rendering_the_sentinel() {
    let mut e = Editor::new();
    e.image = Some(ImageView {
        bytes: Vec::new(),
        format_label: String::from("PDF"),
        pixel_w: 1,
        pixel_h: 1,
        natural_size: None,
        byte_size: 0,
        generation: 0,
        pdf: Some(PdfState {
            source_path: PathBuf::from("/nonexistent/doc.pdf"),
            current_page: 1,
            page_count: None,
            backend: crate::pdf::PdfBackend::SipsCli,
            source_byte_size: 0,
            links: None,
        }),
    });
    assert!(!e.set_pdf_page(u32::MAX));
    assert!(
        !e.status.contains("4294967295"),
        "the sentinel page must not leak into the status: {}",
        e.status
    );
    assert_eq!(e.pdf_page(), Some(1), "the page must not move");
}

/// A PNG of `w` × `h` pixels, `pixel(x, y)` each.
fn png_of(w: u32, h: u32, pixel: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
    let img: image::RgbaImage = image::ImageBuffer::from_fn(w, h, |x, y| image::Rgba(pixel(x, y)));
    let mut buf: Vec<u8> = Vec::new();
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .unwrap();
    buf
}

const RED: [u8; 4] = [255, 0, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];

/// #1591: a cell is `▀` over two pixels, the upper one in front.
#[test]
fn a_half_block_cell_shows_the_upper_pixel_over_the_lower() {
    let png = png_of(1, 2, |_, y| if y == 0 { RED } else { BLUE });
    let bg = Color::Rgb(9, 9, 9);
    assert_eq!(
        halfblock_cells(&png, 1, 1, bg),
        Some((1, 1, vec![(Color::Rgb(255, 0, 0), Color::Rgb(0, 0, 255))]))
    );
    // Transparent pixels take the background.
    let clear = png_of(1, 2, |_, _| [200, 200, 200, 0]);
    assert_eq!(
        halfblock_cells(&clear, 1, 1, bg),
        Some((1, 1, vec![(bg, bg)]))
    );
}

/// #1591: a large picture shrinks to the area keeping its shape; an odd
/// last pixel row sits over the background.
#[test]
fn a_large_image_shrinks_to_fit_keeping_its_shape() {
    let png = png_of(40, 20, |x, y| {
        if y < 10 {
            [x as u8 * 6, 0, 0, 255]
        } else {
            BLUE
        }
    });
    let bg = Color::Rgb(1, 2, 3);
    let (cols, rows, cells) = halfblock_cells(&png, 10, 10, bg).unwrap();
    assert_eq!((cols, rows), (10, 3), "40×20 to 10×5 pixels, 3 cell rows");
    assert_eq!(cells.len(), 30);
    assert!(cells[20..].iter().all(|&(_, lower)| lower == bg));
    let Color::Rgb(r, _, b) = cells[0].0 else {
        panic!("{:?}", cells[0]);
    };
    assert!(
        r < 60 && b < 40,
        "the upper left stays dark red: {:?}",
        cells[0]
    );
    let Color::Rgb(r, _, b) = cells[9].0 else {
        panic!("{:?}", cells[9]);
    };
    assert!(
        r > 150 && b < 40,
        "the upper right is bright red: {:?}",
        cells[9]
    );
    let Color::Rgb(r, _, b) = cells[20].0 else {
        panic!("{:?}", cells[20]);
    };
    assert!(b > 200 && r < 40, "the bottom is blue: {:?}", cells[20]);
}

/// #1591 negatives: a small image is not enlarged, and bytes that do
/// not decode or an empty area draw nothing.
#[test]
fn half_blocks_never_enlarge_and_refuse_what_they_cannot_draw() {
    let png = png_of(2, 2, |_, _| RED);
    assert_eq!(
        halfblock_cells(&png, 50, 50, Color::Black).map(|c| (c.0, c.1)),
        Some((2, 1))
    );
    assert_eq!(halfblock_cells(b"not an image", 10, 10, Color::Black), None);
    assert_eq!(halfblock_cells(&png, 0, 10, Color::Black), None);
    assert_eq!(halfblock_cells(&png, 10, 0, Color::Black), None);
}

/// #1591: an image tab with `cell_images` draws the picture under its
/// header, centred; without it (an inline-image terminal paints the
/// picture itself) the canvas stays empty.
#[test]
fn an_image_tab_draws_half_blocks_only_when_asked() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pic.png");
    std::fs::write(&path, png_of(4, 4, |_, y| if y < 2 { RED } else { BLUE })).unwrap();
    let area = Rect::new(0, 0, 30, 10);
    let draw = |cell_images: bool| {
        let mut e = Editor::new();
        e.open(&path).unwrap();
        e.cell_images = cell_images;
        let mut buf = Buffer::empty(area);
        Widget::render(&mut e, area, &mut buf);
        buf
    };
    let buf = draw(true);
    let blocks: Vec<(u16, u16)> = (0..area.height)
        .flat_map(|y| (0..area.width).map(move |x| (x, y)))
        .filter(|&(x, y)| buf[(x, y)].symbol() == "\u{2580}")
        .collect();
    assert_eq!(blocks.len(), 8, "4×4 pixels are 4×2 cells: {blocks:?}");
    let &(x, y) = blocks.first().unwrap();
    assert_eq!(buf[(x, y)].fg, Color::Rgb(255, 0, 0));
    assert_eq!(buf[(x, y + 1)].bg, Color::Rgb(0, 0, 255));
    let buf = draw(false);
    assert!(
        (0..area.height).all(|y| (0..area.width).all(|x| buf[(x, y)].symbol() != "\u{2580}")),
        "no half blocks where the terminal shows the picture"
    );
}

/// #1591: an image whose body does not decode is remembered as an
/// empty picture under its key, so the next frame does not decode the
/// bytes again, and nothing is drawn.
#[test]
fn an_undecodable_image_body_is_cached_not_retried_each_frame() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pic.png");
    let png = png_of(4, 4, |_, _| RED);
    std::fs::write(&path, &png).unwrap();
    let mut e = Editor::new();
    e.open(&path).unwrap();
    e.cell_images = true;
    e.image.as_mut().unwrap().bytes = png[..png.len() / 2].to_vec();
    let area = Rect::new(0, 0, 30, 10);
    let mut buf = Buffer::empty(area);
    Widget::render(&mut e, area, &mut buf);
    let cached = e.cell_image.clone().expect("the failed decode is cached");
    assert_eq!(cached.key.0, e.image.as_ref().unwrap().generation);
    assert!(cached.cells.is_empty());
    assert!(
        (0..area.height).all(|y| (0..area.width).all(|x| buf[(x, y)].symbol() != "\u{2580}")),
        "nothing is drawn for an undecodable body"
    );
    Widget::render(&mut e, area, &mut Buffer::empty(area));
    assert_eq!(e.cell_image, Some(cached), "the cache entry stays put");
}

#[test]
fn extension_is_image_recognises_common_formats() {
    for ext in [
        "png", "PNG", "jpg", "jpeg", "JPEG", "gif", "bmp", "webp", "ico", "ICO", "tif", "tiff",
        "TIFF",
    ] {
        assert!(extension_is_image(ext), "should recognise: {ext}");
    }
    for ext in ["txt", "rs", "md", "py", ""] {
        assert!(!extension_is_image(ext), "should not recognise: {ext}");
    }
}

#[test]
fn open_unrecognised_file_after_image_clears_image_view() {
    // Open an image, then a text file in the same Editor — `image`
    // must reset so the text-rendering path comes back.
    let img: image::RgbaImage = image::ImageBuffer::from_pixel(1, 1, image::Rgba([0, 0, 0, 0]));
    let mut buf: Vec<u8> = Vec::new();
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let img_path = dir.path().join("pic.png");
    std::fs::write(&img_path, &buf).unwrap();
    let txt_path = dir.path().join("hi.txt");
    std::fs::write(&txt_path, "hi\n").unwrap();
    let mut e = Editor::new();
    e.open(&img_path).unwrap();
    assert!(e.image.is_some());
    e.open(&txt_path).unwrap();
    assert!(e.image.is_none());
    assert_eq!(e.lines, vec!["hi".to_string()]);
}

#[test]
fn open_routes_binary_files_to_hex_not_text() {
    // The pre-#172 contract was a "Binary file" ERROR; the new one is
    // that binary content never lands in the text path — it routes to
    // the hex viewer, and no text state is populated.
    let mut tmp = NamedTempFile::new().unwrap();
    tmp.write_all(b"\x00\x01\x02binary garbage").unwrap();
    let mut e = Editor::new();
    e.open(tmp.path()).unwrap();
    assert!(e.hex.is_some(), "binary routes to hex");
    assert!(!e.dirty);
    assert_eq!(e.lines, vec![String::new()], "text path untouched");
}

#[test]
fn matches_open_path_handles_canonical_difference() {
    let mut tmp = NamedTempFile::new().unwrap();
    writeln!(tmp, "x").unwrap();
    let mut e = Editor::new();
    e.open(tmp.path()).unwrap();
    assert!(e.matches_open_path(tmp.path()));
    let bogus = std::path::Path::new("/definitely/not/the/same/path.txt");
    assert!(!e.matches_open_path(bogus));
}

#[test]
fn reload_if_clean_picks_up_external_changes() {
    let mut tmp = NamedTempFile::new().unwrap();
    writeln!(tmp, "def hello():").unwrap();
    writeln!(tmp, "    print(\"hi\")").unwrap();
    let mut e = Editor::new();
    e.open(tmp.path()).unwrap();
    assert!(e.lines[0].contains("hello"));

    // Simulate an external edit (vim, git pull, etc.).
    std::fs::write(tmp.path(), "def hi():\n    print(\"hi\")\n").unwrap();
    let outcome = e.reload_if_clean();
    assert!(matches!(outcome, Some(Ok(()))));
    assert_eq!(e.lines[0], "def hi():");
    assert!(!e.dirty);
}

#[test]
fn external_reload_bumps_edit_seq_and_drops_stale_semantic_overlay() {
    let tmp = NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), "def hello():\n    pass\n").unwrap();
    let mut e = Editor::new();
    e.open(tmp.path()).unwrap();
    let seq_before = e.edit_seq;
    let legend = std::sync::Arc::new(vec![String::from("function")]);
    e.apply_semantic_tokens(tmp.path().to_path_buf(), vec![0, 4, 5, 0, 0], legend, true);
    assert!(e.semantic_overlay_for_test().iter().any(|l| !l.is_empty()));

    std::fs::write(tmp.path(), "x = 1\n").unwrap();
    assert!(matches!(
        e.reload_or_flag_conflict(),
        ExternalChange::Reloaded
    ));
    // The LSP doc resync is keyed on edit_seq; if the reload doesn't move
    // it, the server never hears about the new content and never sends
    // fresh semantic tokens.
    assert_ne!(e.edit_seq, seq_before);
    // The old token batch was measured against the old text; decoding it
    // over the new lines paints wrong colors, so it must be dropped.
    assert!(e.semantic_overlay_for_test().iter().all(|l| l.is_empty()));
}

#[test]
fn reload_if_clean_refuses_when_dirty() {
    let tmp = NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), "original\n").unwrap();
    let mut e = Editor::new();
    e.open(tmp.path()).unwrap();
    e.insert_str("local edit");
    assert!(e.dirty);

    std::fs::write(tmp.path(), "external change\n").unwrap();
    let outcome = e.reload_if_clean();
    assert!(
        outcome.is_none(),
        "should refuse to reload over dirty buffer"
    );
    assert!(e.lines[0].contains("local edit"));
}

#[test]
fn saving_keeps_the_files_final_newline() {
    for (disk, want) in [
        ("a\nb\n", "xa\nb\n"),
        ("a\r\nb\r\n", "xa\r\nb\r\n"),
        ("a\n\n", "xa\n\n"),
        ("a", "xa"),
    ] {
        let tmp = NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), disk).unwrap();
        let mut e = Editor::new();
        e.open(tmp.path()).unwrap();
        e.insert_char('x');
        e.save_to_disk().unwrap();
        assert_eq!(
            std::fs::read_to_string(tmp.path()).unwrap(),
            want,
            "from {disk:?}"
        );
    }
}

/// #911: language servers read the text a save writes, final newline
/// included, so a formatter has no newline to add.
#[test]
fn the_document_text_is_what_a_save_writes() {
    for disk in ["a\nb\n", "a\n\n", "a", ""] {
        let tmp = NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), disk).unwrap();
        let mut e = Editor::new();
        e.open(tmp.path()).unwrap();
        assert_eq!(e.document_text(), disk);
    }
}

/// #911: the text the server reads ends in the final newline, so its
/// last position is on an empty row past the buffer's last. A
/// diagnostic there (a missing token at the end of the file) is drawn
/// and hovered at the end of the last row instead of dropped.
#[test]
fn a_diagnostic_at_the_end_of_the_document_marks_the_last_row() {
    use crate::lsp::manager::DiagnosticSeverity;
    let tmp = NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), "fn main() {\n").unwrap();
    let mut e = Editor::new();
    e.open(tmp.path()).unwrap();
    assert_eq!(e.lines, vec!["fn main() {"]);
    let path = tmp.path().to_path_buf();
    e.apply_diagnostics(path, vec![diag(1, 0, 1, 0, DiagnosticSeverity::Error)]);
    assert_eq!(
        e.diagnostic_spans[0],
        vec![(11, 12, DiagnosticSeverity::Error)]
    );
    assert_eq!(
        e.diagnostics_at(0, 11, Path::new("/tmp")).len(),
        1,
        "hovered at the row's end"
    );
}

/// #911: a formatter's edit up to the end of that text keeps the final
/// newline in the flag, not as a blank row the save writes a second
/// newline after. An edit that removes it saves none, and undo brings
/// it back.
#[test]
fn an_edit_reaching_the_final_newline_keeps_one() {
    let tmp = NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), "x=1\n").unwrap();
    let mut e = Editor::new();
    e.open(tmp.path()).unwrap();
    assert_eq!(e.apply_span_edits(&[span((0, 0), (1, 0), "x = 1\n")]), 1);
    assert_eq!(e.lines, vec!["x = 1"]);
    e.save_to_disk().unwrap();
    assert_eq!(std::fs::read_to_string(tmp.path()).unwrap(), "x = 1\n");

    assert_eq!(e.apply_span_edits(&[span((0, 5), (1, 0), "")]), 1);
    assert_eq!(e.lines, vec!["x = 1"]);
    assert_eq!(e.document_text(), "x = 1");
    assert!(e.undo());
    assert_eq!(e.document_text(), "x = 1\n");
}

#[test]
fn saving_over_invalid_bytes_asks_first() {
    let tmp = NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), b"caf\xe9 ok\n").unwrap();
    let mut e = Editor::new();
    e.open(tmp.path()).unwrap();
    assert!(e.decode_lossy, "a lone 0xE9 is not UTF-8");
    e.insert_char('x');
    assert_eq!(e.save_to_disk().unwrap(), SaveOutcome::EncodingLoss);
    assert_eq!(
        std::fs::read(tmp.path()).unwrap(),
        b"caf\xe9 ok\n",
        "untouched"
    );
    e.lossy_save_armed = true;
    assert_eq!(e.save_to_disk().unwrap(), SaveOutcome::Saved);
    assert!(!e.decode_lossy);
}

/// #1379: a file its owner `chmod 444`ed.
#[cfg(unix)]
fn protected_file(dir: &Path, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let path = dir.join("nginx.conf");
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
    path
}

#[cfg(unix)]
fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[cfg(unix)]
#[test]
fn a_write_protected_file_opens_locked_and_its_save_asks_first() {
    let dir = tempfile::tempdir().unwrap();
    let path = protected_file(dir.path(), "listen 80;\n");
    let mut e = Editor::new();
    e.open(&path).unwrap();
    assert!(e.write_protected, "flagged at open, before any edit");
    assert_eq!(tab_label(&e), "nginx.conf \u{f023}");
    e.insert_char('#');
    assert_eq!(e.save_to_disk().unwrap(), SaveOutcome::WriteProtected);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "listen 80;\n");
    assert!(e.dirty, "the edit is still only in the buffer");
    e.overwrite_protected_armed = true;
    assert_eq!(e.save_to_disk().unwrap(), SaveOutcome::Saved);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "#listen 80;\n");
    assert_eq!(mode_of(&path), 0o444, "the file stays protected");
    assert!(
        !e.overwrite_protected_armed,
        "the consent was for one write"
    );
    assert!(e.write_protected);
    assert!(!e.dirty);
}

#[cfg(unix)]
#[test]
fn overwriting_a_protected_file_through_a_symlink_keeps_the_link() {
    let dir = tempfile::tempdir().unwrap();
    let target = protected_file(dir.path(), "a\n");
    let link = dir.path().join("link.conf");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let mut e = Editor::new();
    e.open(&link).unwrap();
    assert!(e.write_protected);
    e.insert_char('b');
    e.overwrite_protected_armed = true;
    assert_eq!(e.save_to_disk().unwrap(), SaveOutcome::Saved);
    assert!(std::fs::symlink_metadata(&link).unwrap().is_symlink());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "ba\n");
    assert_eq!(mode_of(&target), 0o444);
}

#[cfg(unix)]
#[test]
fn a_writable_file_opens_unlocked_and_saves_at_once() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("site.conf");
    std::fs::write(&path, "a\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let mut e = Editor::new();
    e.open(&path).unwrap();
    assert!(!e.write_protected);
    assert_eq!(tab_label(&e), "site.conf");
    e.insert_char('b');
    assert_eq!(e.save_to_disk().unwrap(), SaveOutcome::Saved);
    assert_eq!(mode_of(&path), 0o644);
    assert_eq!(write_protection(&dir.path().join("new.conf")), None);
}

#[test]
fn only_a_permission_error_reads_as_permission_denied() {
    let denied = anyhow::Error::from(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
        .context("writing /etc/hosts");
    assert!(is_permission_denied(&denied));
    for other in [
        std::io::ErrorKind::NotFound,
        std::io::ErrorKind::StorageFull,
        std::io::ErrorKind::Other,
    ] {
        let e = anyhow::Error::from(std::io::Error::from(other));
        assert!(!is_permission_denied(&e), "{other:?}");
    }
    assert!(!is_permission_denied(&anyhow::anyhow!("permission denied")));
}

#[test]
fn a_save_made_elsewhere_settles_only_on_the_exact_bytes() {
    let tmp = NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), "old\n").unwrap();
    let mut e = Editor::new();
    e.open(tmp.path()).unwrap();
    e.insert_char('n');
    e.overwrite_protected_armed = true;
    let bytes = e.prepare_bytes_for_save().unwrap();
    assert_eq!(bytes, b"nold\n");
    std::fs::write(tmp.path(), "someone else\n").unwrap();
    assert!(!e.settle_save_made_elsewhere(&bytes));
    assert!(e.dirty);
    std::fs::write(tmp.path(), &bytes).unwrap();
    assert!(e.settle_save_made_elsewhere(&bytes));
    assert!(!e.dirty);
    assert!(
        !e.disk_changed_externally(),
        "the write is the buffer's own"
    );
    assert!(
        !e.overwrite_protected_armed,
        "a consent to overwrite does not outlive the save"
    );
}

#[test]
fn an_edit_made_while_a_save_was_elsewhere_stays_dirty() {
    let tmp = NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), "old\n").unwrap();
    let mut e = Editor::new();
    e.open(tmp.path()).unwrap();
    e.insert_char('n');
    let bytes = e.prepare_bytes_for_save().unwrap();
    e.insert_char('m');
    std::fs::write(tmp.path(), &bytes).unwrap();
    assert!(e.settle_save_made_elsewhere(&bytes));
    assert!(e.dirty, "the later edit is not on disk");
}

#[test]
fn a_language_servers_utf16_columns_land_after_an_emoji() {
    let mut lines = vec![String::from("let s = \"😀\"; x + 1")];
    let n = apply_span_edits_to_lines(
        &mut lines,
        &[TextSpanEdit {
            start: (0, 14),
            end: (0, 15),
            new_text: String::from("renamed"),
            utf16: true,
        }],
    );
    assert_eq!(n, 1);
    assert_eq!(lines[0], "let s = \"😀\"; renamed + 1");
}

#[test]
fn save_round_trips_content() {
    let tmp = NamedTempFile::new().unwrap();
    let mut e = Editor::new();
    e.open(tmp.path()).unwrap();
    e.insert_str("hello\nworld");
    assert!(e.dirty);
    e.save_to_disk().unwrap();
    assert!(!e.dirty);
    let written = std::fs::read_to_string(tmp.path()).unwrap();
    assert_eq!(written, "hello\nworld");
}

/// Run the test named `child` in a fresh copy of this test binary whose
/// files can't grow past `fsize` bytes (RLIMIT_FSIZE, with SIGXFSZ
/// ignored so a write past it fails with EFBIG instead of killing the
/// process). That is a disk that fills up mid-write, scoped to one
/// process, so it can't reach the other tests running in parallel.
#[cfg(target_os = "linux")]
fn run_with_file_size_limit(child: &str, file: &std::path::Path, fsize: u64) {
    use std::os::unix::process::CommandExt as _;
    let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
    cmd.args([child, "--exact", "--test-threads=1", "--nocapture"])
        .env("CROFT_TEST_FSIZE_FILE", file);
    // SAFETY: only async-signal-safe libc calls between fork and exec.
    unsafe {
        cmd.pre_exec(move || {
            let lim = libc::rlimit {
                rlim_cur: fsize,
                rlim_max: fsize,
            };
            if libc::setrlimit(libc::RLIMIT_FSIZE, &lim) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            libc::signal(libc::SIGXFSZ, libc::SIG_IGN);
            Ok(())
        });
    }
    let out = cmd.output().unwrap();
    let log = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{child} failed:\n{log}");
    assert!(log.contains("1 passed"), "{child} did not run:\n{log}");
}

/// #1124: a save that runs out of space must leave the file as it was,
/// and must not make croft's own failed write look like someone else's.
#[cfg(target_os = "linux")]
#[test]
fn a_save_that_runs_out_of_space_leaves_the_file_intact() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("notes.md");
    let original: String = (0..400).map(|i| format!("- entry {i:04}\n")).collect();
    std::fs::write(&f, &original).unwrap();
    // The file is ~5.6 KB; the child can write at most 4 KB of any file.
    run_with_file_size_limit(
        "widgets::editor::tests::child_saves_a_grown_buffer_under_a_file_size_limit",
        &f,
        4096,
    );
    assert_eq!(
        std::fs::read_to_string(&f).unwrap(),
        original,
        "the failed save cut the file short"
    );
    let stray: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .filter(|n| n != "notes.md")
        .collect();
    assert!(stray.is_empty(), "temp files left behind: {stray:?}");
}

/// The child half of the test above; a no-op unless run by it.
#[cfg(target_os = "linux")]
#[test]
fn child_saves_a_grown_buffer_under_a_file_size_limit() {
    let Some(f) = std::env::var_os("CROFT_TEST_FSIZE_FILE") else {
        return;
    };
    let mut e = Editor::new();
    e.open(std::path::Path::new(&f)).unwrap();
    e.insert_str("# A new entry at the top\n");
    assert!(
        e.save_to_disk().is_err(),
        "the write must fail past the limit"
    );
    assert!(e.dirty, "the edits are still only in the buffer");
    assert!(
        !e.disk_changed_externally(),
        "croft's own failed write must not read as an external change"
    );
}

/// Make the save path's copy aside fail as a full disk does, for the
/// rest of this thread.
#[cfg(target_os = "linux")]
fn fill_the_disk_for_copies() {
    crate::prefs::test_hooks::ASIDE_OUT_OF_SPACE.set(true);
}

/// #1614: with no room for a second copy of the file, a save writes the
/// file in place instead of failing with "No space left on device".
#[cfg(target_os = "linux")]
#[test]
fn a_save_with_no_room_for_a_copy_writes_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("notes.md");
    let original: String = (0..2000)
        .map(|i| format!("line {i} of the notes\n"))
        .collect();
    std::fs::write(&f, &original).unwrap();
    let mut e = Editor::new();
    e.open(&f).unwrap();
    e.insert_str("X");
    fill_the_disk_for_copies();
    let saved = e.save_to_disk();
    crate::prefs::test_hooks::ASIDE_OUT_OF_SPACE.set(false);
    assert_eq!(saved.unwrap(), SaveOutcome::Saved);
    assert_eq!(std::fs::read_to_string(&f).unwrap(), format!("X{original}"));
    assert!(!e.dirty);
    assert!(!e.disk_changed_externally());
    let stray: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .filter(|n| n != "notes.md")
        .collect();
    assert!(stray.is_empty(), "temp files left behind: {stray:?}");
}

/// #1614 negative: when the new contents don't fit in place either, the
/// save fails and the file is left whole (#1124).
#[cfg(target_os = "linux")]
#[test]
fn a_save_that_fits_neither_aside_nor_in_place_leaves_the_file_intact() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("notes.md");
    let original: String = (0..400).map(|i| format!("- entry {i:04}\n")).collect();
    std::fs::write(&f, &original).unwrap();
    run_with_file_size_limit(
        "widgets::editor::tests::child_saves_in_place_under_a_file_size_limit",
        &f,
        4096,
    );
    assert_eq!(
        std::fs::read_to_string(&f).unwrap(),
        original,
        "the failed save cut the file short"
    );
    let stray: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .filter(|n| n != "notes.md")
        .collect();
    assert!(stray.is_empty(), "temp files left behind: {stray:?}");
}

/// The child half of the test above; a no-op unless run by it.
#[cfg(target_os = "linux")]
#[test]
fn child_saves_in_place_under_a_file_size_limit() {
    let Some(f) = std::env::var_os("CROFT_TEST_FSIZE_FILE") else {
        return;
    };
    let mut e = Editor::new();
    e.open(std::path::Path::new(&f)).unwrap();
    e.insert_str("# A new entry at the top\n");
    fill_the_disk_for_copies();
    assert!(
        e.save_to_disk().is_err(),
        "the write must fail past the limit"
    );
    assert!(e.dirty, "the edits are still only in the buffer");
    assert!(
        !e.disk_changed_externally(),
        "croft's own failed write must not read as an external change"
    );
}

#[test]
fn saving_keeps_the_mode_and_writes_through_a_symlink() {
    // Negative: writing aside and renaming in must not change what a
    // save changes: the file keeps its mode, a symlink stays a link
    // to the file it named, and a new file is still created.
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("run.sh");
    std::fs::write(&real, "echo a\n").unwrap();
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o750)).unwrap();
    let link = dir.path().join("link.sh");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let mut e = Editor::new();
    e.open(&link).unwrap();
    e.insert_str("# hi\n");
    assert_eq!(e.save_to_disk().unwrap(), SaveOutcome::Saved);
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read_to_string(&real).unwrap(), "# hi\necho a\n");
    assert_eq!(
        std::fs::metadata(&real).unwrap().permissions().mode() & 0o777,
        0o750
    );
    assert!(!e.disk_changed_externally());

    let fresh = dir.path().join("new.txt");
    let mut e = Editor::new();
    e.path = Some(fresh.clone());
    e.insert_str("x");
    e.save_to_disk().unwrap();
    assert_eq!(std::fs::read_to_string(&fresh).unwrap(), "x");
}

/// #1190 review: a save that fails before the file is touched must
/// not refresh the disk stamp: an overwrite of an outside edit that
/// fails leaves that edit on disk, and the next plain save must still
/// stop at the conflict instead of silently replacing it.
#[cfg(target_os = "linux")]
#[test]
fn a_failed_save_keeps_an_external_edit_visible() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("notes.md");
    let original: String = (0..400).map(|i| format!("- entry {i:04}\n")).collect();
    std::fs::write(&f, &original).unwrap();
    run_with_file_size_limit(
        "widgets::editor::tests::child_fails_a_save_after_an_external_edit",
        &f,
        4096,
    );
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "edited elsewhere\n");
}

/// The child half of the test above; a no-op unless run by it.
#[cfg(target_os = "linux")]
#[test]
fn child_fails_a_save_after_an_external_edit() {
    let Some(f) = std::env::var_os("CROFT_TEST_FSIZE_FILE") else {
        return;
    };
    let f = std::path::PathBuf::from(f);
    let mut e = Editor::new();
    e.open(&f).unwrap();
    std::fs::write(&f, "edited elsewhere\n").unwrap();
    e.insert_str("# A new entry at the top\n");
    assert_eq!(e.save_to_disk().unwrap(), SaveOutcome::DiskConflict);
    assert!(
        e.save_to_disk_force().is_err(),
        "the overwrite must fail past the limit"
    );
    assert!(
        e.disk_changed_externally(),
        "the failed overwrite left the outside edit on disk, still unresolved"
    );
}

/// #1190 review: a hard link is written in place (a rename would split
/// it from its other names); running out of space there must still
/// leave the file whole.
#[cfg(target_os = "linux")]
#[test]
fn a_hard_linked_file_that_runs_out_of_space_stays_intact() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("notes.md");
    let original: String = (0..400).map(|i| format!("- entry {i:04}\n")).collect();
    std::fs::write(&f, &original).unwrap();
    std::fs::hard_link(&f, dir.path().join("other-name.md")).unwrap();
    run_with_file_size_limit(
        "widgets::editor::tests::child_saves_a_grown_buffer_under_a_file_size_limit",
        &f,
        4096,
    );
    assert_eq!(std::fs::read_to_string(&f).unwrap(), original);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("other-name.md")).unwrap(),
        original
    );
}

#[test]
fn a_file_with_a_long_name_still_saves() {
    // #1190 review: the temp name grew with the file's own, so a name
    // near the 255-byte limit could not be saved at all.
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join(format!("{}.md", "n".repeat(240)));
    std::fs::write(&f, "a\n").unwrap();
    let mut e = Editor::new();
    e.open(&f).unwrap();
    e.insert_str("b\n");
    assert_eq!(e.save_to_disk().unwrap(), SaveOutcome::Saved);
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "b\na\n");
}

#[test]
fn a_save_never_touches_a_file_named_like_a_temp_file() {
    // #1190 review: the temp name was predictable and a file already
    // at it was removed first, so a save could delete an unrelated one.
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("notes.md");
    std::fs::write(&f, "a\n").unwrap();
    let planted = dir
        .path()
        .join(format!(".notes.md.croft-save-{}", std::process::id()));
    std::fs::write(&planted, "not croft's\n").unwrap();
    let mut e = Editor::new();
    e.open(&f).unwrap();
    e.insert_str("b\n");
    assert_eq!(e.save_to_disk().unwrap(), SaveOutcome::Saved);
    assert_eq!(std::fs::read_to_string(&planted).unwrap(), "not croft's\n");
    let names: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names.len(), 2, "no temp file is left behind: {names:?}");
}

#[cfg(unix)]
#[test]
fn saving_keeps_the_files_group() {
    // #1190 review: the renamed-in file took the default group.
    use std::os::unix::fs::MetadataExt as _;
    let mut groups = vec![0 as libc::gid_t; 64];
    let n = unsafe { libc::getgroups(groups.len() as i32, groups.as_mut_ptr()) };
    let egid = unsafe { libc::getegid() };
    let Some(&other) = groups[..n.max(0) as usize].iter().find(|&&g| g != egid) else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("shared.txt");
    std::fs::write(&f, "a\n").unwrap();
    std::os::unix::fs::chown(&f, None, Some(other)).unwrap();
    let mut e = Editor::new();
    e.open(&f).unwrap();
    e.insert_str("b\n");
    assert_eq!(e.save_to_disk().unwrap(), SaveOutcome::Saved);
    assert_eq!(std::fs::metadata(&f).unwrap().gid(), other);
}

#[cfg(target_os = "linux")]
#[test]
fn saving_keeps_the_files_extended_attributes() {
    // #1190 review: xattrs, and the POSIX ACLs stored in them, were
    // left behind on the old inode.
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("tagged.txt");
    std::fs::write(&f, "a\n").unwrap();
    let c = std::ffi::CString::new(f.as_os_str().as_encoded_bytes()).unwrap();
    let name = c"user.croft-test";
    let set = unsafe { libc::setxattr(c.as_ptr(), name.as_ptr(), b"kept".as_ptr().cast(), 4, 0) };
    if set != 0 {
        return; // the filesystem has no user xattrs
    }
    let mut e = Editor::new();
    e.open(&f).unwrap();
    e.insert_str("b\n");
    assert_eq!(e.save_to_disk().unwrap(), SaveOutcome::Saved);
    let mut buf = [0u8; 16];
    let got = unsafe {
        libc::getxattr(
            c.as_ptr(),
            name.as_ptr(),
            buf.as_mut_ptr().cast(),
            buf.len(),
        )
    };
    assert_eq!(got, 4, "the xattr is gone");
    assert_eq!(&buf[..4], b"kept");
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "b\na\n");
}

#[test]
fn dirty_flag_lifecycle() {
    let mut e = editor_with("abc");
    assert!(!e.dirty);
    e.insert_char('z');
    assert!(e.dirty);
}

#[test]
fn switching_a_preview_tab_to_an_image_drops_the_stale_conflict_cache() {
    // open_image swaps the whole buffer; a conflict list memoised on
    // edit_seq from the previous text file must not survive, or a
    // palette merge-resolve slices a one-line buffer with stale
    // indices and panics.
    let tmp = tempfile::tempdir().unwrap();
    let mut e = Editor::new();
    e.lines = vec![
        "<<<<<<< HEAD".to_string(),
        "ours".to_string(),
        "=======".to_string(),
        "theirs".to_string(),
        ">>>>>>> branch".to_string(),
    ];
    e.mark_buffer_changed();
    assert_eq!(e.conflicts().len(), 1, "the conflict parses");
    let png = tmp.path().join("p.png");
    image::RgbaImage::new(1, 1).save(&png).unwrap();
    e.open_image(&png).unwrap();
    assert!(
        e.conflicts().is_empty(),
        "an image tab has no merge conflicts; a stale cache here is a panic waiting in resolution_lines"
    );
}

#[test]
fn undo_past_a_save_re_dirties_the_buffer() {
    // Auto save writes one second after a keystroke; Cmd+Z then restores
    // the pre-edit snapshot with dirty:false while disk keeps the edit.
    // dirty means "buffer differs from disk", so crossing a save point
    // backwards must re-dirty or the divergence is permanent and silent.
    let tmp = NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), "hello\n").unwrap();
    let mut e = Editor::new();
    e.open(tmp.path()).unwrap();
    e.insert_char('x');
    assert!(matches!(e.save_to_disk(), Ok(SaveOutcome::Saved)));
    assert!(!e.dirty);
    assert!(e.undo());
    assert!(
        e.dirty,
        "the undone buffer no longer matches disk and must say so"
    );
}

#[test]
fn a_save_breaks_insert_coalescing() {
    // Typing after a save must open a new undo step: coalescing across
    // the save point makes one Cmd+Z discard work from both sides of it.
    let tmp = NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), "").unwrap();
    let mut e = Editor::new();
    e.open(tmp.path()).unwrap();
    e.insert_char('x');
    assert!(matches!(e.save_to_disk(), Ok(SaveOutcome::Saved)));
    e.insert_char('y');
    assert!(e.undo());
    assert_eq!(
        e.lines[0], "x",
        "undo steps back to the save point, not past it"
    );
}

#[test]
fn a_replacement_carriage_return_does_not_survive_in_a_line() {
    let mut e = Editor::new();
    e.lines = vec!["abc".to_string()];
    e.replace_find_match(0, 1, 1, "x\r\ny");
    assert_eq!(e.lines, vec!["ax".to_string(), "yc".to_string()]);
    assert!(e.lines.iter().all(|l| !l.contains('\r')));
}

#[test]
fn a_multi_line_replacement_splices_into_separate_lines() {
    // A regex replacement containing a real newline (VS Code's \n) must
    // become two buffer lines, never an embedded control char in one.
    let mut e = Editor::new();
    e.lines = vec!["abc".to_string()];
    e.replace_find_match(0, 1, 1, "x\ny");
    assert_eq!(e.lines, vec!["ax".to_string(), "yc".to_string()]);
    assert_eq!(
        (e.cursor_row, e.cursor_col),
        (1, 1),
        "caret after the inserted text"
    );
}

#[test]
fn an_external_reload_moves_the_edit_seq_past_an_in_flight_token_request() {
    // Semantic-token replies now echo the `edit_seq` their request was
    // fired for, and the app drops a reply whose seq no longer matches.
    // That guard is only meaningful while a reload actually MOVES the
    // seq: drop the bump and a batch computed against the old text
    // matches again, paints the new buffer at the old offsets, and gets
    // persisted to the on-disk cache under the new content's key.
    let tmp = NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), "fn a() {}\n").unwrap();
    let mut e = Editor::new();
    e.open(tmp.path()).unwrap();
    e.apply_semantic_tokens(
        tmp.path().to_path_buf(),
        vec![0, 0, 2, 0, 0],
        std::sync::Arc::new(vec!["keyword".to_string()]),
        true,
    );
    let in_flight = e.edit_seq;
    std::fs::write(tmp.path(), "fn bbbb() {}\nfn c() {}\n").unwrap();
    e.reload_from_disk().unwrap();
    assert_ne!(
        e.edit_seq, in_flight,
        "a reload must invalidate every in-flight token request"
    );
    assert!(
        e.semantic_data.is_empty(),
        "and drop the batch measured against the old text"
    );
}

/// `open` also serves the same-path reload, so clearing the fold set there
/// unconditionally made every FS-sync sweep pop the user's blocks open:
/// a `git checkout`, an external formatter, or another split saving the
/// file. Only a DIFFERENT file arriving in this tab invalidates them.
#[test]
fn an_external_reload_of_the_same_file_keeps_its_folds() {
    let tmp = NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), "def f():\n    a\n    b\ntail\n").unwrap();
    let mut e = Editor::new();
    e.open(tmp.path()).unwrap();
    e.toggle_fold(0);
    assert!(e.is_line_hidden(1), "the block starts collapsed");
    // An external rewrite of the same shape: same line count, same length,
    // so only the mtime moves.
    std::fs::write(tmp.path(), "def f():\n    A\n    B\ntail\n").unwrap();
    let newer = std::time::SystemTime::now() + std::time::Duration::from_secs(2);
    std::fs::OpenOptions::new()
        .write(true)
        .open(tmp.path())
        .unwrap()
        .set_modified(newer)
        .unwrap();
    assert_eq!(e.reload_or_flag_conflict(), ExternalChange::Reloaded);
    assert_eq!(e.lines[1], "    A", "the reload landed");
    assert!(
        e.is_line_hidden(1),
        "the fold must survive a reload of the file it was set on"
    );
}

/// Keeping `folded` across a same-path reload is only half the job: the
/// cached spans were measured against the OLD text. A reload that keeps the
/// line count but moves the indentation (an external formatter, a
/// `git checkout`) slips past the render-time `fold_epoch_lines` guard, so
/// the stale cache goes on hiding lines the fold no longer covers.
#[test]
fn a_same_path_reload_remeasures_the_fold_spans() {
    let tmp = NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), "def f():\n    a\n    b\ntail\n").unwrap();
    let mut e = Editor::new();
    e.open(tmp.path()).unwrap();
    e.toggle_fold(0);
    assert!(e.is_line_hidden(1), "the block starts collapsed");
    // Same line count, same length: only the indentation moved, so the
    // header no longer has a body to fold.
    std::fs::write(tmp.path(), "def f():\na    \nb    \ntail\n").unwrap();
    let newer = std::time::SystemTime::now() + std::time::Duration::from_secs(2);
    std::fs::OpenOptions::new()
        .write(true)
        .open(tmp.path())
        .unwrap()
        .set_modified(newer)
        .unwrap();
    assert_eq!(e.reload_or_flag_conflict(), ExternalChange::Reloaded);
    assert_eq!(e.lines[1], "a    ", "the reload landed");
    assert!(
        !e.is_line_hidden(1),
        "a line the fold no longer covers must not stay hidden"
    );
}

#[test]
fn reload_or_flag_conflict_reloads_clean_buffer() {
    let tmp = NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), "old\n").unwrap();
    let mut e = Editor::new();
    e.open(tmp.path()).unwrap();
    // External edit grows the file so mtime and len both move.
    std::fs::write(tmp.path(), "new content here\n").unwrap();
    assert_eq!(e.reload_or_flag_conflict(), ExternalChange::Reloaded);
    assert_eq!(e.lines[0], "new content here");
    assert!(!e.disk_conflict);
    // A second sweep with no further change is a no-op.
    assert_eq!(e.reload_or_flag_conflict(), ExternalChange::Unchanged);
}

/// #1357: a clean tab reloaded after an external rewrite (an agent, a
/// formatter, a checkout) kept no history, so Ctrl+Z said "Nothing to
/// undo" and could take back neither the rewrite nor what came before.
#[test]
fn an_external_reload_is_one_undo_step_and_keeps_the_history_before_it() {
    let tmp = NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), "def total(xs):\n").unwrap();
    let mut e = Editor::new();
    e.open(tmp.path()).unwrap();
    e.cursor_row = 0;
    e.cursor_col = e.lines[0].len();
    e.insert_str(" # typed");
    assert_eq!(e.save_to_disk().unwrap(), SaveOutcome::Saved);
    std::fs::write(tmp.path(), "def total(xs):\n    return sum(xs)\n").unwrap();
    assert_eq!(e.reload_or_flag_conflict(), ExternalChange::Reloaded);
    assert_eq!(e.lines, vec!["def total(xs):", "    return sum(xs)"]);
    assert!(!e.dirty);

    assert!(e.undo(), "the reload itself is undoable");
    assert_eq!(e.lines, vec!["def total(xs): # typed"]);
    assert!(e.dirty, "the restored text no longer matches disk");
    assert!(e.undo(), "and so is what was typed before it");
    assert_eq!(e.lines, vec!["def total(xs):"]);
    assert!(e.dirty);

    assert!(e.redo() && e.redo(), "redo walks forward again");
    assert_eq!(e.lines, vec!["def total(xs):", "    return sum(xs)"]);
    assert!(
        !e.dirty,
        "back at the reloaded text, which is what disk holds"
    );
}

#[test]
fn a_reload_that_changes_no_text_pushes_no_undo_step() {
    let tmp = NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), "a\n").unwrap();
    let mut e = Editor::new();
    e.open(tmp.path()).unwrap();
    e.cursor_col = 1;
    e.insert_str("b");
    assert_eq!(e.save_to_disk().unwrap(), SaveOutcome::Saved);
    let steps = e.undo_stack.len();
    // Same bytes, new mtime: `touch`, or a formatter with nothing to do.
    let f = std::fs::File::options()
        .write(true)
        .open(tmp.path())
        .unwrap();
    f.set_modified(SystemTime::now() + std::time::Duration::from_secs(60))
        .unwrap();
    assert_eq!(e.reload_or_flag_conflict(), ExternalChange::Reloaded);
    assert_eq!(e.undo_stack.len(), steps, "no empty step to undo through");
    assert!(e.undo());
    assert_eq!(e.lines[0], "a", "the typing before it is one undo away");
}

#[test]
fn a_file_reloaded_as_a_different_view_starts_its_history_afresh() {
    let tmp = NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), "text\n").unwrap();
    let mut e = Editor::new();
    e.open(tmp.path()).unwrap();
    e.insert_str("x");
    assert_eq!(e.save_to_disk().unwrap(), SaveOutcome::Saved);
    std::fs::write(tmp.path(), [0u8, 159, 146, 150, 0, 1, 2, 3, 4, 5]).unwrap();
    assert_eq!(e.reload_or_flag_conflict(), ExternalChange::Reloaded);
    assert!(e.hex.is_some(), "precondition: the file is binary now");
    assert!(!e.undo(), "text history cannot be undone into a hex view");
}

#[test]
fn reload_or_flag_conflict_flags_dirty_buffer_without_clobbering() {
    let tmp = NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), "original\n").unwrap();
    let mut e = Editor::new();
    e.open(tmp.path()).unwrap();
    e.insert_str("my unsaved edit");
    std::fs::write(tmp.path(), "external change\n").unwrap();
    assert_eq!(e.reload_or_flag_conflict(), ExternalChange::Conflict);
    assert!(
        e.lines[0].contains("my unsaved edit"),
        "dirty buffer must NOT be reloaded over"
    );
    assert!(e.disk_conflict);
}

#[test]
fn reload_or_flag_conflict_fires_once_then_stays_quiet() {
    // A dirty buffer whose disk keeps differing must announce the conflict
    // exactly once so the confirm popup doesn't reopen on every FS poll.
    let tmp = NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), "original\n").unwrap();
    let mut e = Editor::new();
    e.open(tmp.path()).unwrap();
    e.insert_str("my unsaved edit");
    std::fs::write(tmp.path(), "external change\n").unwrap();
    assert_eq!(e.reload_or_flag_conflict(), ExternalChange::Conflict);
    // Disk still differs and the buffer is still dirty, but the conflict
    // has already been announced.
    assert_eq!(e.reload_or_flag_conflict(), ExternalChange::Unchanged);
    assert!(e.disk_conflict);
}

#[test]
fn revert_paths_to_disk_discards_edits_and_reloads() {
    let tmp = NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), "original\n").unwrap();
    let mut tabs = EditorTabs::new();
    tabs.open_pinned(tmp.path()).unwrap();
    tabs.insert_str("my unsaved edit"); // deref to the active editor
    std::fs::write(tmp.path(), "external change\n").unwrap();
    let reverted = tabs.revert_paths_to_disk(&[tmp.path().to_path_buf()]);
    assert_eq!(reverted, vec![tmp.path().to_path_buf()]);
    assert_eq!(tabs.lines[0], "external change");
    assert!(!tabs.dirty);
    assert!(!tabs.disk_conflict);
}

#[test]
fn reload_or_flag_conflict_unchanged_when_disk_matches() {
    let tmp = NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), "stable\n").unwrap();
    let mut e = Editor::new();
    e.open(tmp.path()).unwrap();
    assert_eq!(e.reload_or_flag_conflict(), ExternalChange::Unchanged);
}

#[test]
fn save_refuses_to_clobber_external_change_then_force_overwrites() {
    let tmp = NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), "original\n").unwrap();
    let mut e = Editor::new();
    e.open(tmp.path()).unwrap();
    e.insert_str("local");
    // Someone else writes the file after we opened it.
    std::fs::write(tmp.path(), "theirs\n").unwrap();
    assert_eq!(e.save_to_disk().unwrap(), SaveOutcome::DiskConflict);
    assert_eq!(
        std::fs::read_to_string(tmp.path()).unwrap(),
        "theirs\n",
        "a guarded save must NOT clobber the external change"
    );
    assert!(e.disk_conflict);
    // Explicit force overwrites with our buffer and clears the conflict.
    e.save_to_disk_force().unwrap();
    assert!(
        std::fs::read_to_string(tmp.path())
            .unwrap()
            .contains("local")
    );
    assert!(!e.disk_conflict);
}

/// A legacy encoding that cannot represent the buffer must never write
/// silently: `encoding_rs` substitutes HTML numeric character references
/// (NOT `?`), so the file ends up holding ASCII text that no longer
/// round-trips, while the buffer still shows the originals.
#[test]
fn saving_text_the_encoding_cannot_represent_is_refused_not_silently_mangled() {
    let tmp = NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), "cafe\n").unwrap();
    let mut e = Editor::new();
    e.open(tmp.path()).unwrap();
    e.reopen_with_encoding(encoding_rs::WINDOWS_1252).unwrap();
    // € lives at 0x80 in Windows-1252, so only the CJK is unmappable.
    e.lines = vec![String::from("日本語 costs 5€")];
    e.dirty = true;
    assert_eq!(e.save_to_disk().unwrap(), SaveOutcome::EncodingLoss);
    assert_eq!(
        std::fs::read_to_string(tmp.path()).unwrap(),
        "cafe\n",
        "a refused save must leave the file untouched"
    );
    assert!(e.encoding_loss, "the refusal latches so auto save skips it");
    assert_eq!(e.unmappable_chars(), vec!['日', '本', '語']);
    // Explicit consent (the second Cmd+S) writes the replacements.
    e.lossy_save_armed = true;
    assert_eq!(e.save_to_disk().unwrap(), SaveOutcome::Saved);
    assert_eq!(
        std::fs::read(tmp.path()).unwrap(),
        b"&#26085;&#26412;&#35486; costs 5\x80\n",
        "consent writes encoding_rs' references, and € as the 0x80 byte"
    );
    assert!(!e.encoding_loss);
    assert!(!e.lossy_save_armed, "consent is one-shot");
}

/// The refusal must not fire for a buffer the encoding covers, and must
/// not fire at all for UTF-8/UTF-16, which cover everything.
#[test]
fn a_representable_buffer_saves_without_a_prompt() {
    let tmp = NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), "cafe\n").unwrap();
    let mut e = Editor::new();
    e.open(tmp.path()).unwrap();
    e.reopen_with_encoding(encoding_rs::WINDOWS_1252).unwrap();
    e.lines = vec![String::from("café")];
    assert_eq!(e.save_to_disk().unwrap(), SaveOutcome::Saved);
    assert_eq!(std::fs::read(tmp.path()).unwrap(), b"caf\xE9\n");
    assert!(e.unmappable_chars().is_empty());
    // The same text in UTF-8 is always representable. A fresh file: the
    // one above now opens in the encoding picked for it (#1432).
    let fresh = NamedTempFile::new().unwrap();
    let mut u = Editor::new();
    u.open(fresh.path()).unwrap();
    u.lines = vec![String::from("日本語")];
    assert_eq!(u.encoding, encoding_rs::UTF_8);
    assert_eq!(u.save_to_disk().unwrap(), SaveOutcome::Saved);
    assert!(u.unmappable_chars().is_empty());
}

/// Switching the buffer to an encoding that covers it must clear the
/// latch, or auto save would stay wedged on a file that is now fine.
#[test]
fn reopening_with_another_encoding_clears_the_encoding_loss_latch() {
    let tmp = NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), "日本語").unwrap();
    let mut e = Editor::new();
    e.open(tmp.path()).unwrap();
    e.encoding = encoding_rs::WINDOWS_1252;
    assert_eq!(e.save_to_disk().unwrap(), SaveOutcome::EncodingLoss);
    assert!(e.encoding_loss);
    e.reopen_with_encoding(encoding_rs::UTF_8).unwrap();
    assert!(!e.encoding_loss);
    assert_eq!(e.save_to_disk().unwrap(), SaveOutcome::Saved);
}

/// Armed consent covers the buffer the prompt described. An edit changes
/// what the write would destroy, so it must revoke the stale consent (and
/// the auto-save latch) and make the next save prompt afresh — otherwise
/// characters typed after the prompt get mangled under a consent that
/// never named them.
#[test]
fn an_edit_after_a_lossy_refusal_revokes_the_armed_consent() {
    let tmp = NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), "cafe\n").unwrap();
    let mut e = Editor::new();
    e.open(tmp.path()).unwrap();
    e.reopen_with_encoding(encoding_rs::WINDOWS_1252).unwrap();
    e.lines = vec![String::from("日")];
    e.dirty = true;
    assert_eq!(e.save_to_disk().unwrap(), SaveOutcome::EncodingLoss);
    e.lossy_save_armed = true; // the app arms the second Cmd+S...
    e.insert_char('本'); // ...but the user edits instead of pressing it
    assert!(!e.lossy_save_armed, "an edit revokes the stale consent");
    assert!(
        !e.encoding_loss,
        "the latch clears too, so a fixed-up buffer resumes auto save"
    );
    assert_eq!(
        e.save_to_disk().unwrap(),
        SaveOutcome::EncodingLoss,
        "the next save must prompt again for the changed buffer"
    );
    assert_eq!(std::fs::read_to_string(tmp.path()).unwrap(), "cafe\n");
}

#[test]
fn a_failed_external_reload_is_reported_not_swallowed() {
    // #37: `ReloadFailed` was dropped on the floor — the user's file is
    // replaced on disk by something unreadable (here: a directory), the
    // sweep tries and fails to reload, and nothing said so.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f.txt");
    std::fs::write(&path, "one\n").unwrap();
    let mut tabs = EditorTabs::new();
    tabs.open_pinned(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    let report = tabs.reload_externally_changed_tabs(&|_| false);
    assert_eq!(
        report.failed,
        vec![path.clone()],
        "the failure must surface in the report"
    );
    assert!(!report.is_empty(), "a failed reload is not an empty sweep");
    assert_eq!(
        tabs.iter_tabs().next().unwrap().lines[0],
        "one",
        "the last good buffer survives the failed reload"
    );
}

/// #900: the failure re-fired on every FS sweep, pinning its status
/// over every other message. Once per change on disk; a later change is
/// tried afresh.
#[test]
fn a_failed_external_reload_is_reported_once_per_change_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f.txt");
    std::fs::write(&path, "one\n").unwrap();
    let mut e = Editor::new();
    e.open(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert_eq!(e.reload_or_flag_conflict(), ExternalChange::ReloadFailed);
    assert_eq!(e.reload_or_flag_conflict(), ExternalChange::Unchanged);
    std::fs::remove_dir(&path).unwrap();
    std::fs::write(&path, "two, longer\n").unwrap();
    assert_eq!(e.reload_or_flag_conflict(), ExternalChange::Reloaded);
    assert_eq!(e.lines[0], "two, longer");
}

/// #900: a failed reload is announced once, but a clean tab keeps
/// trying it: a read can start working again with nothing on disk
/// moving (a permission put back), and the tab must not stay stale.
#[test]
fn a_failed_reload_is_retried_quietly_until_it_works() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f.txt");
    std::fs::write(&path, "one\n").unwrap();
    let mut e = Editor::new();
    e.open(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert_eq!(e.reload_or_flag_conflict(), ExternalChange::ReloadFailed);
    // Readable again with the same (mtime, len) the failed read saw.
    let meta = std::fs::metadata(&path).unwrap();
    std::fs::remove_dir(&path).unwrap();
    let body = "x".repeat(usize::try_from(meta.len()).unwrap());
    std::fs::write(&path, &body).unwrap();
    let file = std::fs::File::options().write(true).open(&path).unwrap();
    file.set_modified(meta.modified().unwrap()).unwrap();
    drop(file);
    assert_eq!(e.reload_or_flag_conflict(), ExternalChange::Reloaded);
    assert_eq!(e.lines.join("\n"), body);
}

/// #900: only a file that is not there is deleted. One that cannot be
/// stat'd for another reason (here a parent turned into a file) is a
/// failed reload: no `(deleted)` mark, and its save keeps the check.
#[test]
fn a_file_that_cannot_be_statd_is_not_reported_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    let path = sub.join("f.txt");
    std::fs::write(&path, "one\n").unwrap();
    let mut e = Editor::new();
    e.open(&path).unwrap();
    std::fs::remove_dir_all(&sub).unwrap();
    std::fs::write(&sub, "a file now").unwrap();
    assert_eq!(e.reload_or_flag_conflict(), ExternalChange::ReloadFailed);
    assert!(!e.deleted_on_disk());
    assert_eq!(tab_label(&e), "f.txt");
}

/// #900: a preview tab reuses its editor, so what the sweep said about
/// the file it held must not stick to the next one, even one with the
/// same timestamp and length.
#[test]
fn opening_another_file_clears_the_deleted_mark() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.txt");
    let b = dir.path().join("b.txt");
    std::fs::write(&a, "same\n").unwrap();
    std::fs::write(&b, "same\n").unwrap();
    let stamp = std::fs::metadata(&a).unwrap().modified().unwrap();
    let file = std::fs::File::options().write(true).open(&b).unwrap();
    file.set_modified(stamp).unwrap();
    drop(file);
    let mut e = Editor::new();
    e.open(&a).unwrap();
    std::fs::remove_file(&a).unwrap();
    assert_eq!(e.reload_or_flag_conflict(), ExternalChange::Deleted);
    e.open(&b).unwrap();
    assert!(!e.deleted_on_disk());
    assert_eq!(tab_label(&e), "b.txt");
}

/// #900: a file deleted on disk is its own state, not a failed reload:
/// reported once, the buffer kept, the tab marked `(deleted)`.
#[test]
fn a_file_deleted_on_disk_is_reported_once_and_marks_its_tab() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.txt");
    std::fs::write(&path, "alpha\n").unwrap();
    let mut e = Editor::new();
    e.open(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    assert_eq!(e.reload_or_flag_conflict(), ExternalChange::Deleted);
    assert_eq!(e.reload_or_flag_conflict(), ExternalChange::Unchanged);
    assert_eq!(e.lines[0], "alpha", "the buffer is kept");
    assert_eq!(tab_label(&e), "notes.txt (deleted)");
    // Put back (`git stash pop`): the clean tab reloads it as usual.
    std::fs::write(&path, "alpha, restored\n").unwrap();
    assert_eq!(e.reload_or_flag_conflict(), ExternalChange::Reloaded);
    assert_eq!(tab_label(&e), "notes.txt");
}

/// #900 (comment): unsaved edits to a file deleted on disk were treated
/// as a conflict with a disk copy that no longer exists. No conflict:
/// the first explicit save puts the file back, and auto save never does.
#[test]
fn a_dirty_tab_deleted_on_disk_is_no_conflict_and_its_save_recreates_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("b.txt");
    std::fs::write(&path, "beta\n").unwrap();
    let mut e = Editor::new();
    e.open(&path).unwrap();
    e.insert_str("my ");
    std::fs::remove_file(&path).unwrap();
    assert_eq!(e.reload_or_flag_conflict(), ExternalChange::Deleted);
    assert!(!e.disk_conflict);
    assert!(e.auto_save_to_disk().is_err());
    assert!(!path.exists(), "auto save leaves the deletion alone");
    assert_eq!(e.save_to_disk().unwrap(), SaveOutcome::Saved);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "my beta\n");
    assert!(!e.deleted_on_disk());
    assert_eq!(tab_label(&e), "b.txt");
}

#[test]
fn reload_externally_changed_tabs_reloads_background_tab() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.txt");
    let b = dir.path().join("b.txt");
    std::fs::write(&a, "a old\n").unwrap();
    std::fs::write(&b, "b old\n").unwrap();
    let mut tabs = EditorTabs::new();
    tabs.open_pinned(&a).unwrap(); // tab for a
    tabs.open_pinned(&b).unwrap(); // tab for b is now active; a is in the background
    // External edit to the BACKGROUND file. The new content is the same
    // length as the old ("a old\n" / "a NEW\n" are both 6 bytes), so the
    // change is detectable only by mtime — and two back-to-back writes can
    // land in the same mtime tick on a coarse-granularity filesystem,
    // making detection flaky. Force a distinctly-newer mtime so the test is
    // deterministic across filesystems (the production path keys off this
    // same (mtime, len) stamp).
    std::fs::write(&a, "a NEW\n").unwrap();
    let bumped = std::time::SystemTime::now() + std::time::Duration::from_secs(2);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&a)
        .unwrap()
        .set_modified(bumped)
        .unwrap();
    let report = tabs.reload_externally_changed_tabs(&|_| false);
    assert_eq!(report.reloaded, vec![a.clone()]);
    assert!(report.conflicts.is_empty());
    // A skipped path (a collab guest's shared file) is left untouched.
    std::fs::write(&b, "b NEW\n").unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&b)
        .unwrap()
        .set_modified(bumped)
        .unwrap();
    let report = tabs.reload_externally_changed_tabs(&|p| p == b.as_path());
    assert!(report.reloaded.is_empty());
    let b_tab = tabs
        .iter_tabs()
        .find(|e| e.path.as_deref() == Some(b.as_path()))
        .unwrap();
    assert_eq!(b_tab.lines[0], "b old");
    let a_tab = tabs
        .iter_tabs()
        .find(|e| e.path.as_deref() == Some(a.as_path()))
        .unwrap();
    assert_eq!(a_tab.lines[0], "a NEW");
}

/// The field gesture behind the pdftoppm stderr fix: a PDF is open while
/// pdflatex rewrites it. Mid-write the file is truncated garbage - the
/// FS-sync sweep must fail quietly and keep the last good page, then
/// reload once the write completes.
#[test]
fn open_pdf_tab_keeps_last_good_page_through_a_mid_write_sweep() {
    if crate::pdf::detect_backend().is_none() {
        eprintln!("skipping: no PDF rasteriser installed");
        return;
    }
    let one_page_pdf = concat!(
        "%PDF-1.4\n",
        "1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n",
        "2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n",
        "3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 200]/Contents 4 0 R>>endobj\n",
        "4 0 obj<</Length 27>>stream\n0 0 1 rg 10 10 100 100 re f\nendstream endobj\n",
        "trailer<</Root 1 0 R/Size 5>>\n%%EOF\n",
    );
    let bump_mtime = |path: &Path, secs: u64| {
        let newer = std::time::SystemTime::now() + std::time::Duration::from_secs(secs);
        std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(newer)
            .unwrap();
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("presentation.pdf");
    std::fs::write(&path, one_page_pdf).unwrap();
    let mut tabs = EditorTabs::new();
    tabs.open_pinned(&path).unwrap();
    let good_bytes = {
        let tab = tabs.iter_tabs().next().unwrap();
        tab.image
            .as_ref()
            .expect("PDF tab renders a page")
            .bytes
            .clone()
    };
    // pdflatex truncates and starts rewriting the file.
    std::fs::write(&path, b"%PDF-1.7\ntruncated mid-write, no xref").unwrap();
    bump_mtime(&path, 2);
    let report = tabs.reload_externally_changed_tabs(&|_| false);
    assert!(
        report.reloaded.is_empty(),
        "a failed render is not a reload"
    );
    assert!(
        report.conflicts.is_empty(),
        "a clean tab is never a conflict"
    );
    let tab = tabs.iter_tabs().next().unwrap();
    assert_eq!(
        tab.image
            .as_ref()
            .expect("last good page must survive")
            .bytes,
        good_bytes,
        "mid-write failure must keep showing the last good render"
    );
    // The write completes; the next sweep picks it up.
    std::fs::write(&path, one_page_pdf).unwrap();
    bump_mtime(&path, 4);
    let report = tabs.reload_externally_changed_tabs(&|_| false);
    assert_eq!(report.reloaded, vec![path.clone()]);
    let tab = tabs.iter_tabs().next().unwrap();
    assert!(tab.image.is_some(), "completed write renders again");
}

/// Reading page 2 of a deck while pdflatex rebuilds the file: the
/// FS-sync reload must come back on page 2, not snap the reader to
/// page 1 and lose their place.
#[test]
fn pdf_reload_keeps_the_current_page() {
    if crate::pdf::detect_backend().is_none() {
        eprintln!("skipping: no PDF rasteriser installed");
        return;
    }
    let two_page_pdf = concat!(
        "%PDF-1.4\n",
        "1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n",
        "2 0 obj<</Type/Pages/Kids[3 0 R 5 0 R]/Count 2>>endobj\n",
        "3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 200]/Contents 4 0 R>>endobj\n",
        "4 0 obj<</Length 27>>stream\n0 0 1 rg 10 10 100 100 re f\nendstream endobj\n",
        "5 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 200]/Contents 6 0 R>>endobj\n",
        "6 0 obj<</Length 27>>stream\n1 0 0 rg 10 10 100 100 re f\nendstream endobj\n",
        "trailer<</Root 1 0 R/Size 7>>\n%%EOF\n",
    );
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deck.pdf");
    std::fs::write(&path, two_page_pdf).unwrap();
    let mut tabs = EditorTabs::new();
    tabs.open_pinned(&path).unwrap();
    assert!(tabs.change_pdf_page(1), "two pages, so page 2 renders");
    assert_eq!(tabs.pdf_page(), Some(2));
    // pdflatex rewrites the file (same content is enough; only the
    // disk stamp needs to move).
    std::fs::write(&path, two_page_pdf).unwrap();
    let newer = std::time::SystemTime::now() + std::time::Duration::from_secs(2);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(newer)
        .unwrap();
    let report = tabs.reload_externally_changed_tabs(&|_| false);
    assert_eq!(report.reloaded, vec![path.clone()]);
    assert_eq!(
        tabs.pdf_page(),
        Some(2),
        "an external rebuild must not move the reader back to page 1; status: {:?}",
        tabs.iter_tabs().next().unwrap().status
    );
}

/// The transient arm of the rebuild race (#72): the reader is on page 2,
/// the file is rebuilt, and the rasterisation of THEIR page fails once
/// (a spawn refused under load). That failure must flow into the
/// failed-reload path — last good render, place kept — never leave a
/// half-restored tab sitting on page 1. The next sweep recovers.
#[test]
fn a_transient_page_render_failure_during_reload_keeps_the_readers_place() {
    if crate::pdf::detect_backend().is_none() {
        eprintln!("skipping: no PDF rasteriser installed");
        return;
    }
    let two_page_pdf = concat!(
        "%PDF-1.4\n",
        "1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n",
        "2 0 obj<</Type/Pages/Kids[3 0 R 5 0 R]/Count 2>>endobj\n",
        "3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 200]/Contents 4 0 R>>endobj\n",
        "4 0 obj<</Length 27>>stream\n0 0 1 rg 10 10 100 100 re f\nendstream endobj\n",
        "5 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 200]/Contents 6 0 R>>endobj\n",
        "6 0 obj<</Length 27>>stream\n1 0 0 rg 10 10 100 100 re f\nendstream endobj\n",
        "trailer<</Root 1 0 R/Size 7>>\n%%EOF\n",
    );
    let bump_mtime = |path: &Path, secs: u64| {
        let newer = std::time::SystemTime::now() + std::time::Duration::from_secs(secs);
        std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(newer)
            .unwrap();
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deck.pdf");
    std::fs::write(&path, two_page_pdf).unwrap();
    let mut tabs = EditorTabs::new();
    tabs.open_pinned(&path).unwrap();
    assert!(tabs.change_pdf_page(1), "two pages, so page 2 renders");
    let good_bytes = tabs
        .iter_tabs()
        .next()
        .unwrap()
        .image
        .as_ref()
        .unwrap()
        .bytes
        .clone();
    std::fs::write(&path, two_page_pdf).unwrap();
    bump_mtime(&path, 2);
    *crate::pdf::FAIL_RASTERIZE_ONCE_FOR_TEST.lock().unwrap() = Some((path.clone(), 2));
    let report = tabs.reload_externally_changed_tabs(&|_| false);
    assert!(
        report.reloaded.is_empty(),
        "a reload that cannot restore the reader's page is a failed \
             reload, not a successful one that lost their place"
    );
    assert_eq!(
        tabs.pdf_page(),
        Some(2),
        "a transient render failure must not snap the reader to page 1; status: {:?}",
        tabs.iter_tabs().next().unwrap().status
    );
    assert_eq!(
        tabs.iter_tabs()
            .next()
            .unwrap()
            .image
            .as_ref()
            .unwrap()
            .bytes,
        good_bytes,
        "the last good render survives the transient failure"
    );
    // The failure was transient: the next sweep reloads and the reader
    // is still on their page.
    bump_mtime(&path, 4);
    let report = tabs.reload_externally_changed_tabs(&|_| false);
    assert_eq!(report.reloaded, vec![path.clone()]);
    assert_eq!(
        tabs.pdf_page(),
        Some(2),
        "recovery re-renders the reader's page"
    );
}

#[test]
fn insert_str_inserts_newlines() {
    let mut e = editor_with("");
    e.insert_str("a\nb\nc");
    assert_eq!(
        e.lines,
        vec!["a".to_string(), "b".to_string(), "c".to_string()]
    );
    assert_eq!(e.cursor_row, 2);
    assert_eq!(e.cursor_col, 1);
}

#[test]
fn build_line_spans_no_highlights() {
    let spans = build_line_spans("hello", &[], 0);
    assert_eq!(spans.len(), 1);
}

#[test]
fn wrap_segments_short_line_is_one_segment() {
    assert_eq!(wrap_segments("hello", 20), vec![(0, 5)]);
    assert_eq!(wrap_segments("", 20), vec![(0, 0)]);
    // width 0 must not loop forever - one segment covering the line.
    assert_eq!(wrap_segments("abc", 0), vec![(0, 3)]);
}

#[test]
fn wrap_segments_breaks_after_a_space() {
    // "the quick brown" at width 10 breaks after "the quick " (10 chars).
    let segs = wrap_segments("the quick brown", 10);
    assert_eq!(segs, vec![(0, 10), (10, 15)]);
}

#[test]
fn wrap_segments_hard_breaks_a_too_long_word() {
    // No space in the first 6 chars, so the word is split at the column.
    let segs = wrap_segments("abcdefghij", 6);
    assert_eq!(segs, vec![(0, 6), (6, 10)]);
}

#[test]
fn wrap_segments_breaks_after_punctuation_like_vscode() {
    // VS Code breaks after `/` (a break-after char), so a path folds at
    // the slash rather than mid-segment.
    let segs = wrap_segments("abc/defgh", 6);
    assert_eq!(segs, vec![(0, 4), (4, 9)]);
}

#[test]
fn wrap_segments_count_wide_characters_as_two_cells() {
    let line: String = std::iter::repeat_n('漢', 60).collect();
    let segs = wrap_segments(&line, 30);
    assert_eq!(segs.len(), 4, "{segs:?}");
    assert!(segs.iter().all(|&(s, e)| e - s == 15), "{segs:?}");
    // A char wider than the pane still makes progress.
    assert_eq!(wrap_segments("漢漢", 1), vec![(0, 1), (1, 2)]);
}

#[test]
fn down_in_wrap_mode_steps_over_a_collapsed_fold() {
    let mut e = editor_with("fn a() {\n    one\n    two\n}\nafter");
    e.wrap_override = Some(true);
    e.last_inner = Rect::new(0, 0, 40, 10);
    e.toggle_fold(0);
    assert!(e.is_line_hidden(1) && e.is_line_hidden(2));
    e.cursor_row = 0;
    e.cursor_col = 0;
    e.move_down();
    assert!(
        !e.is_line_hidden(e.cursor_row),
        "landed on hidden line {}",
        e.cursor_row
    );
    assert!(e.is_line_hidden(1), "the fold stays collapsed");
}

#[test]
fn a_wrapped_cjk_line_shows_every_character() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("a.md");
    let text: String = (0..60)
        .map(|i| char::from_u32(0x4e00 + i).unwrap())
        .collect();
    std::fs::write(&p, &text).unwrap();
    let mut e = Editor::new();
    e.open(&p).unwrap();
    e.wrap_override = Some(true);
    let area = Rect::new(0, 0, 40, 12);
    let mut buf = Buffer::empty(area);
    e.render(area, &mut buf);
    let shown: String = (0..area.height)
        .flat_map(|y| (0..area.width).map(move |x| (x, y)))
        .map(|(x, y)| buf[(x, y)].symbol().to_string())
        .collect();
    for c in text.chars() {
        assert!(shown.contains(c), "{c} missing from the wrapped render");
    }
}

/// Each wrapped row counts tab stops from its own left edge: `⇥ef` on
/// the second row is 4 + 2 cells, so it fits in 6.
#[test]
fn wrap_segments_count_tab_stops_from_each_rows_left_edge() {
    set_tab_cells(4);
    assert_eq!(wrap_segments("abcd\tef", 6), vec![(0, 4), (4, 7)]);
    assert_eq!(
        wrap_segments("ab\tc", 5),
        vec![(0, 4)],
        "the tab stops at 4"
    );
    assert_eq!(wrap_segments("ab\tc", 4), vec![(0, 3), (3, 4)]);
}

#[test]
fn wrap_segments_tile_the_line_without_gaps() {
    let line = "alpha beta gamma delta epsilon zeta eta theta";
    let segs = wrap_segments(line, 12);
    assert_eq!(segs.first().unwrap().0, 0);
    assert_eq!(segs.last().unwrap().1, line.chars().count());
    for pair in segs.windows(2) {
        assert_eq!(pair[0].1, pair[1].0, "segments must be contiguous");
    }
}

#[test]
fn build_line_spans_full_line_highlighted() {
    let hi = vec![HiSpan {
        start: 0,
        end: 5,
        style: Style::default(),
    }];
    let spans = build_line_spans("hello", &hi, 0);
    assert_eq!(spans.len(), 1);
}

#[test]
fn build_line_spans_partial_highlights() {
    let hi = vec![HiSpan {
        start: 1,
        end: 3,
        style: Style::default(),
    }];
    let spans = build_line_spans("abcde", &hi, 0);
    // Expect: "a", "bc", "de"
    assert_eq!(spans.len(), 3);
}

/// A semantic `comment` token covering a tag must not erase the tag's
/// colour. `merge_overlay` gives the overlay precedence, so a server that
/// reports the whole comment as one token would repaint TODO in ordinary
/// comment grey - the tag highlighting would silently stop working the
/// moment an LSP attached.
#[test]
fn a_semantic_comment_token_does_not_erase_a_tag_colour() {
    use ratatui::style::Color;
    let comment = Style::default().fg(Color::Rgb(0x65, 0x73, 0x7e));
    // BOLD is what the tag pass adds (src/highlight.rs, the
    // `base_style.fg(color).add_modifier(Modifier::BOLD)` push), and it
    // is the marker merge_overlay keys on. A fixture without it would
    // not exercise the real path.
    let tag = Style::default()
        .fg(Color::Rgb(0x4F, 0xC1, 0xFF))
        .add_modifier(Modifier::BOLD);
    // What the tree-sitter pass produces for `// TODO x`: grey, then the
    // tag in its own colour, then grey.
    let base = vec![
        HiSpan {
            start: 0,
            end: 3,
            style: comment,
        },
        HiSpan {
            start: 3,
            end: 7,
            style: tag,
        },
        HiSpan {
            start: 7,
            end: 9,
            style: comment,
        },
    ];
    // What a server reports: one `comment` token over the whole line.
    let over = vec![HiSpan {
        start: 0,
        end: 9,
        style: comment,
    }];
    let merged = merge_overlay(&base, &over);
    assert!(
        merged
            .iter()
            .any(|sp| sp.style.fg == Some(Color::Rgb(0x4F, 0xC1, 0xFF))),
        "the TODO span must survive the semantic overlay: {merged:?}"
    );
}

/// Keywords are BOLD too (`palette_style_for_name`, "keyword"), so the tag
/// carve-out must not key on BOLD alone: a semantic token over a keyword
/// (rust-analyzer's inactive-code dimming, `selfKeyword`) still wins.
#[test]
fn a_semantic_token_still_repaints_a_bold_keyword() {
    use ratatui::style::Color;
    let keyword = Style::default()
        .fg(Color::Rgb(0xc6, 0x78, 0xdd))
        .add_modifier(Modifier::BOLD);
    let inactive = Style::default().fg(Color::Rgb(0x5c, 0x63, 0x70));
    let base = vec![HiSpan {
        start: 0,
        end: 2,
        style: keyword,
    }];
    let over = vec![HiSpan {
        start: 0,
        end: 9,
        style: inactive,
    }];
    let merged = merge_overlay(&base, &over);
    assert_eq!(
        merged.iter().map(|sp| sp.style).collect::<Vec<_>>(),
        vec![inactive],
        "the overlay must repaint the keyword: {merged:?}"
    );
}

#[test]
fn merge_overlay_semantic_wins_and_base_fills_gaps() {
    use ratatui::style::Color;
    let base_style = Style::default().fg(Color::Rgb(1, 1, 1));
    let over_style = Style::default().fg(Color::Rgb(2, 2, 2));
    // Base covers the whole 0..10 line; overlay repaints 3..6.
    let base = vec![HiSpan {
        start: 0,
        end: 10,
        style: base_style,
    }];
    let over = vec![HiSpan {
        start: 3,
        end: 6,
        style: over_style,
    }];
    let merged = merge_overlay(&base, &over);
    // Expect base[0..3], over[3..6], base[6..10], sorted and gapless.
    assert_eq!(merged.len(), 3);
    assert_eq!((merged[0].start, merged[0].end), (0, 3));
    assert_eq!(merged[0].style.fg, Some(Color::Rgb(1, 1, 1)));
    assert_eq!((merged[1].start, merged[1].end), (3, 6));
    assert_eq!(
        merged[1].style.fg,
        Some(Color::Rgb(2, 2, 2)),
        "overlay must win over base where they overlap"
    );
    assert_eq!((merged[2].start, merged[2].end), (6, 10));
    assert_eq!(merged[2].style.fg, Some(Color::Rgb(1, 1, 1)));
}

#[test]
fn merge_overlay_empty_overlay_returns_base() {
    let base = vec![HiSpan {
        start: 0,
        end: 4,
        style: Style::default(),
    }];
    assert_eq!(merge_overlay(&base, &[]).len(), 1);
}

#[test]
fn editor_selection_normalised_handles_anchor_after_head() {
    let s = EditorSelection {
        anchor: (5, 4),
        head: (2, 1),
    };
    assert_eq!(s.normalised(), ((2, 1), (5, 4)));
}

#[test]
fn editor_selection_normalised_handles_same_row() {
    let s = EditorSelection {
        anchor: (3, 9),
        head: (3, 2),
    };
    assert_eq!(s.normalised(), ((3, 2), (3, 9)));
}

#[test]
fn editor_selection_has_area_only_when_endpoints_differ() {
    let s = EditorSelection::new(2, 5);
    assert!(!s.has_area());
    let s2 = EditorSelection {
        anchor: (2, 5),
        head: (2, 6),
    };
    assert!(s2.has_area());
}

#[test]
fn start_selection_at_cursor_creates_zero_area_selection() {
    let mut e = editor_with("hello");
    e.cursor_row = 0;
    e.cursor_col = 2;
    e.start_selection_at_cursor();
    let sel = e.selection.expect("selection should exist");
    assert_eq!(sel.anchor, (0, 2));
    assert_eq!(sel.head, (0, 2));
    assert!(!sel.has_area());
}

#[test]
fn extend_selection_to_cursor_updates_head_only() {
    let mut e = editor_with("abcdef");
    e.cursor_col = 1;
    e.start_selection_at_cursor();
    e.cursor_col = 4;
    e.extend_selection_to_cursor();
    let sel = e.selection.unwrap();
    assert_eq!(sel.anchor, (0, 1));
    assert_eq!(sel.head, (0, 4));
    assert!(sel.has_area());
}

#[test]
fn selection_text_single_line() {
    let mut e = editor_with("hello world");
    e.cursor_col = 6;
    e.start_selection_at_cursor();
    e.cursor_col = 11;
    e.extend_selection_to_cursor();
    assert_eq!(e.selection_text(), "world");
}

#[test]
fn selection_text_handles_reversed_endpoints() {
    let mut e = editor_with("hello world");
    e.cursor_col = 11;
    e.start_selection_at_cursor();
    e.cursor_col = 6;
    e.extend_selection_to_cursor();
    assert_eq!(e.selection_text(), "world");
}

#[test]
fn selection_text_multi_line_includes_newlines() {
    let mut e = editor_with("first line\nsecond line\nthird");
    e.cursor_row = 0;
    e.cursor_col = 6;
    e.start_selection_at_cursor();
    e.cursor_row = 1;
    e.cursor_col = 6;
    e.extend_selection_to_cursor();
    assert_eq!(e.selection_text(), "line\nsecond");
}

#[test]
fn selection_text_multibyte_chars() {
    let mut e = editor_with("héllo");
    e.cursor_col = 1;
    e.start_selection_at_cursor();
    e.cursor_col = 3;
    e.extend_selection_to_cursor();
    assert_eq!(e.selection_text(), "él");
}

#[test]
fn clear_selection_removes_it() {
    let mut e = editor_with("abc");
    e.start_selection_at_cursor();
    assert!(e.selection.is_some());
    e.clear_selection();
    assert!(e.selection.is_none());
}

#[test]
fn delete_selection_removes_range_within_one_line() {
    let mut e = editor_with("hello world");
    e.cursor_col = 5;
    e.start_selection_at_cursor();
    e.cursor_col = 11;
    e.extend_selection_to_cursor();
    assert!(e.delete_selection());
    assert_eq!(e.lines, vec!["hello".to_string()]);
    assert_eq!(e.cursor_row, 0);
    assert_eq!(e.cursor_col, 5);
    assert!(e.selection.is_none());
    assert!(e.dirty);
}

#[test]
fn delete_selection_collapses_multiple_lines() {
    let mut e = editor_with("first\nsecond\nthird");
    e.cursor_row = 0;
    e.cursor_col = 2;
    e.start_selection_at_cursor();
    e.cursor_row = 2;
    e.cursor_col = 2;
    e.extend_selection_to_cursor();
    assert!(e.delete_selection());
    assert_eq!(e.lines, vec!["fiird".to_string()]);
    assert_eq!(e.cursor_row, 0);
    assert_eq!(e.cursor_col, 2);
}

#[test]
fn delete_selection_returns_false_when_no_selection() {
    let mut e = editor_with("abc");
    assert!(!e.delete_selection());
    assert_eq!(e.lines, vec!["abc".to_string()]);
}

#[test]
fn delete_selection_returns_false_when_zero_area() {
    let mut e = editor_with("abc");
    e.cursor_col = 1;
    e.start_selection_at_cursor();
    assert!(!e.delete_selection());
    assert_eq!(e.lines, vec!["abc".to_string()]);
}

#[test]
fn insert_char_replaces_selection_when_active() {
    let mut e = editor_with("hello world");
    e.cursor_col = 6;
    e.start_selection_at_cursor();
    e.cursor_col = 11;
    e.extend_selection_to_cursor();
    e.insert_char('X');
    assert_eq!(e.lines, vec!["hello X".to_string()]);
    assert_eq!(e.cursor_col, 7);
    assert!(e.selection.is_none());
}

#[test]
fn backspace_deletes_selection_when_active() {
    let mut e = editor_with("hello world");
    e.cursor_col = 6;
    e.start_selection_at_cursor();
    e.cursor_col = 11;
    e.extend_selection_to_cursor();
    e.backspace();
    assert_eq!(e.lines, vec!["hello ".to_string()]);
    assert!(e.selection.is_none());
}

#[test]
fn delete_forward_deletes_selection_when_active() {
    let mut e = editor_with("hello world");
    e.cursor_col = 6;
    e.start_selection_at_cursor();
    e.cursor_col = 11;
    e.extend_selection_to_cursor();
    e.delete_forward();
    assert_eq!(e.lines, vec!["hello ".to_string()]);
    assert!(e.selection.is_none());
}

#[test]
fn insert_str_replaces_selection_when_active() {
    let mut e = editor_with("hello world");
    e.cursor_col = 6;
    e.start_selection_at_cursor();
    e.cursor_col = 11;
    e.extend_selection_to_cursor();
    e.insert_str("everyone");
    assert_eq!(e.lines, vec!["hello everyone".to_string()]);
    assert!(e.selection.is_none());
}

#[test]
fn select_all_spans_entire_buffer() {
    let mut e = editor_with("a\nbc\nd");
    e.select_all();
    let sel = e.selection.unwrap();
    let (start, end) = sel.normalised();
    assert_eq!(start, (0, 0));
    assert_eq!(end, (2, 1));
    assert_eq!(e.selection_text(), "a\nbc\nd");
}

#[test]
fn mouse_down_starts_zero_area_selection_at_click() {
    let mut e = editor_with("hello");
    e.last_inner = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 25,
    };
    e.last_gutter_width = 2;
    e.mouse_down(3, 0); // text_x = 0 + 2 + 1 = 3, click col 3 → editor col 0
    assert_eq!(e.cursor_col, 0);
    let sel = e.selection.expect("anchor created on mouse down");
    assert_eq!(sel.anchor, (0, 0));
    assert!(!sel.has_area());
}

#[test]
fn render_never_replaces_character_at_cursor() {
    // The hardware caret (DECSCUSR BlinkingBar) overlays the cell at the
    // cursor position; the editor's own render must NEVER change the
    // symbol there or the blink would visibly swallow the letter.
    use ratatui::buffer::Buffer;
    let mut e = editor_with("hello");
    e.cursor_col = 2;
    e.focused = true;

    let area = Rect {
        x: 0,
        y: 0,
        width: 30,
        height: 5,
    };
    let mut buf = Buffer::empty(area);
    (&mut e).render(area, &mut buf);

    let text_x = e.last_inner.x + e.last_gutter_width + 1;
    let cell = &buf[(text_x + 2, e.last_inner.y)];
    assert_eq!(
        cell.symbol(),
        "l",
        "editor render must leave the underlying glyph alone"
    );
}

/// #891: a tab takes the cell the caret, selection and clicks count for
/// it. ratatui drops a raw `'\t'`, so `x\ty\tz` was drawn `xyz` while
/// the caret before `z` sat two cells right of it, on blank space.
#[test]
fn a_tab_takes_its_cell_so_text_caret_and_clicks_agree() {
    for wrap in [false, true] {
        let mut e = editor_with("x\ty\tz");
        e.wrap_override = Some(wrap);
        e.cursor_col = 4;
        let buf = guide_buf(&mut e, 30, 5);
        let text_x = e.last_inner.x + e.last_gutter_width + 1;
        let y = e.last_inner.y;
        assert_eq!(
            row_text(&buf, y, text_x, text_x + 9),
            "x   y   z",
            "wrap {wrap}"
        );
        let (cx, cy) = e.cursor_screen_pos().expect("the caret is on screen");
        assert_eq!(
            buf[(cx, cy)].symbol(),
            "z",
            "the caret sits on z, wrap {wrap}"
        );
        assert_eq!(
            e.buffer_pos_at(text_x + 4, y),
            Some((0, 2)),
            "a click on y lands on y, wrap {wrap}"
        );
    }
}

/// A tab is drawn as the buffer's Tab Size, and the caret, clicks and
/// horizontal scroll follow it.
#[test]
fn a_tab_is_as_wide_as_the_tab_size() {
    for width in [2u32, 4, 8] {
        let mut e = editor_with("\t\tz");
        e.set_indent_style(IndentStyle {
            width,
            use_spaces: false,
        });
        e.wrap_override = Some(false);
        e.cursor_col = 2;
        let buf = guide_buf(&mut e, 40, 5);
        let text_x = e.last_inner.x + e.last_gutter_width + 1;
        let y = e.last_inner.y;
        let z_cell = text_x + 2 * width as u16;
        assert_eq!(buf[(z_cell, y)].symbol(), "z", "tab size {width}");
        assert_eq!(e.cursor_screen_pos(), Some((z_cell, y)), "tab size {width}");
        assert_eq!(e.buffer_pos_at(z_cell, y), Some((0, 2)), "tab size {width}");
    }
}

/// A tab runs to the next tab stop, so text after tabs lines up in columns
/// whatever sits before them (vim, VS Code), and Col counts those cells.
#[test]
fn a_tab_runs_to_the_next_tab_stop() {
    let mut e = editor_with(" *   bits\t\tmodifier\n *   is_text\tthe key\n\tx\nab\tc");
    e.set_indent_style(IndentStyle {
        width: 8,
        use_spaces: false,
    });
    e.wrap_override = Some(false);
    let buf = guide_buf(&mut e, 60, 8);
    let text_x = e.last_inner.x + e.last_gutter_width + 1;
    let y0 = e.last_inner.y;
    let row = |r: u16| -> Vec<String> {
        (0..40)
            .map(|x| buf[(text_x + x, y0 + r)].symbol().to_string())
            .collect()
    };
    let find = |r: u16, s: &str| row(r).iter().position(|c| c == s);
    // "bits" ends at cell 9, "is_text" at 12: both second words start at
    // the stops 16 and 24 resp. 16, so "modifier" and "the key" line up
    // only where the stops do.
    assert_eq!(find(0, "m"), Some(24), "{:?}", row(0));
    assert_eq!(
        find(1, "k"),
        Some(20),
        "the key starts at the stop 16: {:?}",
        row(1)
    );
    assert_eq!(find(2, "x"), Some(8));
    assert_eq!(find(3, "c"), Some(8), "ab then a tab to the stop");
    // The caret, a click and Col agree with the drawing.
    e.cursor_row = 3;
    e.cursor_col = 3;
    assert_eq!(e.cursor_screen_pos(), Some((text_x + 8, y0 + 3)));
    assert_eq!(e.buffer_pos_at(text_x + 8, y0 + 3), Some((3, 3)));
    assert_eq!(
        e.buffer_pos_at(text_x + 2, y0 + 3),
        Some((3, 2)),
        "the tab starts at cell 2"
    );
    assert_eq!(e.cursor_visual_col(), 9, "ab, the tab's 6 cells, then c");
    e.cursor_row = 2;
    e.cursor_col = 1;
    assert_eq!(e.cursor_visual_col(), 9, "after one tab at Tab Size 8");
    e.cursor_col = 0;
    assert_eq!(e.cursor_visual_col(), 1);
}

/// A tab after a grapheme cluster runs from where the cluster ends as
/// drawn: `👍🏽` is two cells, not the four its two characters measure one
/// by one, so the text after the tab, the caret and a click agree.
#[test]
fn a_tab_after_an_emoji_cluster_counts_from_its_drawn_cells() {
    for wrap in [false, true] {
        let mut e = editor_with("\u{1f44d}\u{1f3fd}\tX");
        e.set_indent_style(IndentStyle {
            width: 4,
            use_spaces: false,
        });
        e.wrap_override = Some(wrap);
        let buf = guide_buf(&mut e, 40, 4);
        let text_x = e.last_inner.x + e.last_gutter_width + 1;
        let y = e.last_inner.y;
        assert_eq!(buf[(text_x + 4, y)].symbol(), "X", "wrap {wrap}");
        e.cursor_col = 3;
        assert_eq!(e.cursor_screen_pos(), Some((text_x + 4, y)), "wrap {wrap}");
        assert_eq!(
            e.buffer_pos_at(text_x + 4, y),
            Some((0, 3)),
            "a click on X lands before X, wrap {wrap}"
        );
        assert_eq!(e.cursor_visual_col(), 5, "the cluster counts one column");
    }
}

#[test]
fn the_diff_maps_clicks_and_the_caret_through_the_cells_a_tab_takes() {
    use crate::widgets::diff::DiffSide;
    let f1 = NamedTempFile::new().unwrap();
    let f2 = NamedTempFile::new().unwrap();
    std::fs::write(f1.path(), "\tx\n").unwrap();
    std::fs::write(f2.path(), "\ty\n").unwrap();
    let mut t = EditorTabs::new();
    t.open_diff(f1.path(), f2.path()).unwrap();
    let i = t.active_index();
    t.editors[i].focused = true;
    t.editors[i].set_indent_style(IndentStyle {
        width: 4,
        use_spaces: false,
    });
    let area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 10,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut t.editors[i], area, &mut buf);
    let ed = &t.editors[i];
    let diff = ed.diff.as_ref().unwrap();
    let body_top = ed.last_inner.y + 1;
    let l_gutter = (diff.left_lines.len() + 1).to_string().len() as u16 + 1;
    let l_text_x = ed.last_inner.x + l_gutter + 2;
    // Cells 0-3 are the tab, cell 4 the "x": a click on it is char 1, and
    // one on the tab is char 0.
    let hit = |dx: u16| {
        crate::widgets::editor::diff_hit_test(diff, ed.last_inner, l_text_x + dx, body_top)
    };
    assert_eq!(hit(4), Some((DiffSide::Left, 0, 1)));
    assert_eq!(hit(2), Some((DiffSide::Left, 0, 0)));
    // And the caret on char 1 is drawn on cell 4.
    t.editors[i]
        .diff
        .as_mut()
        .unwrap()
        .start_selection(DiffSide::Left, 0, 1);
    assert_eq!(
        t.editors[i].diff_caret_screen_pos(),
        Some((l_text_x + 4, body_top))
    );
}

#[test]
fn a_line_of_tabs_wider_than_the_pane_can_scroll_to_its_tail() {
    let mut e = editor_with(&"\t".repeat(10));
    e.set_indent_style(IndentStyle {
        width: 8,
        use_spaces: false,
    });
    e.wrap_override = Some(false);
    // Ten tabs are 80 cells: in a 30-cell column the bar must appear and
    // reach the scroll that shows the last of them, in char columns.
    let cols = e.content_cols(30);
    assert!(cols > 30, "the line overflows, got extent {cols}");
    let max_scroll = cols - 30;
    let tail: usize = 10 - max_scroll;
    assert!(
        tail * 8 <= 30 && (tail + 1) * 8 > 30,
        "{max_scroll} chars scroll off"
    );
    // A different Tab Size is a different extent, not a stale cache hit.
    e.set_indent_style(IndentStyle {
        width: 2,
        use_spaces: false,
    });
    assert_eq!(e.content_cols(30), 10, "20 cells fit in 30");
    // `ab⇥c` at Tab Size 8 is 9 cells, the tab running from cell 2 to 8:
    // in 5 cells only `c` fits, so 3 chars scroll off.
    e.lines = vec!["ab\tc".to_string()];
    e.set_indent_style(IndentStyle {
        width: 8,
        use_spaces: false,
    });
    assert_eq!(e.content_cols(5), 3 + 5);
    // `⇥👍🏽123456789` at Tab Size 4 is 15 cells. The scroll that shows its
    // tail in 10 passes the whole emoji, not just its first char, which
    // would leave the skin tone drawn alone and the last digit cut off.
    e.lines = vec!["\t\u{1f44d}\u{1f3fd}123456789".to_string()];
    e.set_indent_style(IndentStyle {
        width: 4,
        use_spaces: false,
    });
    assert_eq!(e.content_cols(10), 3 + 10);
}

#[test]
fn a_crowded_tab_strip_scrolls_and_stops_short_of_the_layout_icons() {
    let tmp = tempfile::tempdir().unwrap();
    let mut tabs = EditorTabs::new();
    for i in 0..12 {
        let f = tmp.path().join(format!("file_{i:02}.txt"));
        std::fs::write(&f, "x\n").unwrap();
        tabs.open_pinned(&f).unwrap();
    }
    let area = Rect::new(0, 0, 60, 10);
    let strip_right = 60 - 9;
    let draw = |tabs: &mut EditorTabs| {
        tabs.set_tab_reserved_right(9);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        ratatui::widgets::Widget::render(&mut *tabs, area, &mut buf);
    };
    let visible = |tabs: &EditorTabs| -> Vec<usize> {
        (0..12)
            .filter(|&i| tabs.tab_screen_x(i).is_some_and(|(_, w)| w > 0))
            .collect()
    };
    draw(&mut tabs);
    // The last tab is active, so the strip has scrolled to show it whole,
    // and nothing reaches the cells the layout icons own.
    let vis = visible(&tabs);
    assert!(vis.contains(&11), "the active tab is on screen: {vis:?}");
    assert!(vis.len() < 12, "not every tab fits");
    for &i in &vis {
        let (x, w) = tabs.tab_screen_x(i).unwrap();
        assert!(x + w <= strip_right, "tab {i} runs under the icons");
    }
    // The wheel brings earlier tabs back without activating anything.
    for _ in 0..12 {
        tabs.scroll_tabs(-1);
    }
    draw(&mut tabs);
    let vis = visible(&tabs);
    assert_eq!(vis.first(), Some(&0), "scrolled back to the first tab");
    assert!(
        !vis.contains(&11),
        "the scroll is not snapped back: {vis:?}"
    );
    assert_eq!(tabs.active_index(), 11);
    // Picking another tab reveals it again.
    tabs.select(0);
    draw(&mut tabs);
    assert!(visible(&tabs).contains(&0));
    tabs.select(11);
    draw(&mut tabs);
    assert!(visible(&tabs).contains(&11));
}

#[test]
fn cursor_screen_pos_inside_viewport() {
    let mut e = editor_with("hello\nworld");
    e.last_inner = Rect {
        x: 5,
        y: 7,
        width: 80,
        height: 25,
    };
    e.last_gutter_width = 2;
    e.cursor_row = 1;
    e.cursor_col = 3;
    // text_x = inner.x + gutter + 1 = 5 + 2 + 1 = 8
    // cy = inner.y + (cursor_row - scroll) = 7 + 1 = 8
    assert_eq!(e.cursor_screen_pos(), Some((8 + 3, 8)));
}

#[test]
fn cursor_screen_pos_wrap_shows_caret_on_blank_line() {
    // Regression: in soft-wrap mode (the Markdown default) the caret
    // vanished on an empty line — the row's segment is (start 0, end 0)
    // and the old guard bailed on `end == start`, so the user typed
    // blind on every blank line.
    let mut e = editor_with("hello\n\nworld");
    e.wrap_override = Some(true);
    e.focused = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 30,
        height: 6,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut e).render(area, &mut buf);
    e.cursor_row = 1;
    e.cursor_col = 0;
    let text_x = e.last_inner.x + e.last_gutter_width + 1;
    assert_eq!(
        e.cursor_screen_pos(),
        Some((text_x, e.last_inner.y + 1)),
        "the caret must be visible on a blank line in wrap mode"
    );
}

#[test]
fn cursor_screen_pos_wrap_shows_caret_at_end_of_line() {
    // Regression: typing at the end of a line in wrap mode hid the
    // caret — `cursor_col == end == line length` is accepted by the
    // row matcher but was then rejected by the old
    // `visible_col >= end - start` guard.
    let mut e = editor_with("hello\nworld");
    e.wrap_override = Some(true);
    e.focused = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 30,
        height: 6,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut e).render(area, &mut buf);
    e.cursor_row = 0;
    e.cursor_col = 5;
    let text_x = e.last_inner.x + e.last_gutter_width + 1;
    assert_eq!(
        e.cursor_screen_pos(),
        Some((text_x + 5, e.last_inner.y)),
        "the caret must be visible one past the last character in wrap mode"
    );
}

#[test]
fn cursor_screen_pos_wrap_boundary_still_maps_to_next_row_start() {
    // A caret on a wrap boundary belongs to the NEXT visual row's first
    // cell, never one past the previous row's last cell.
    let mut e = editor_with("aaaa bbbb cccc dddd eeee ffff gggg hhhh");
    e.wrap_override = Some(true);
    e.focused = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 20,
        height: 6,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut e).render(area, &mut buf);
    // Find the second visual row's start and put the caret exactly there
    // (the first row's `end`).
    let Some((_, start, _)) = e.text_row(1) else {
        panic!("expected a wrapped second row");
    };
    e.cursor_row = 0;
    e.cursor_col = start;
    let text_x = e.last_inner.x + e.last_gutter_width + 1;
    assert_eq!(
        e.cursor_screen_pos(),
        Some((text_x, e.last_inner.y + 1)),
        "a wrap-boundary caret must land on the next row's first cell"
    );
}

#[test]
fn cursor_screen_pos_wrap_zero_width_column_rejects_out_of_pane_cells() {
    // A pane narrow enough leaves a zero-width wrap column, where
    // wrap_segments degenerates to one whole-line segment: a caret deep
    // in the line would compute a cell far past the pane's right edge
    // (across the scrollbar column and beyond). The right-edge clamp
    // must reject it, not paint it. With any nonzero column width the
    // segments are capped at that width, so this degenerate layout is
    // the only way an accepted caret can reach the clamp.
    let mut e = editor_with("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    e.wrap_override = Some(true);
    e.focused = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 8,
        height: 5,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut e).render(area, &mut buf);
    let (_, start, end) = e.text_row(0).expect("a text row");
    assert_eq!(
        (start, end),
        (0, 40),
        "expected the zero-width degenerate one-segment layout"
    );
    e.cursor_row = 0;
    e.cursor_col = 20;
    assert_eq!(
        e.cursor_screen_pos(),
        None,
        "a caret cell past the pane edge must not be painted"
    );
}

#[test]
fn cursor_screen_pos_returns_none_when_scrolled_off() {
    let mut e = editor_with_lines(50);
    e.last_inner = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 10,
    };
    e.last_gutter_width = 3;
    e.scroll = 30;
    e.cursor_row = 5; // above viewport
    assert_eq!(e.cursor_screen_pos(), None);
}

#[test]
fn undo_restores_previous_buffer_and_cursor() {
    let mut e = editor_with("abc");
    e.cursor_col = 3;
    e.insert_char('d');
    assert_eq!(e.lines[0], "abcd");
    assert!(e.undo());
    assert_eq!(e.lines[0], "abc");
    assert_eq!(e.cursor_col, 3);
}

#[test]
fn undo_on_empty_stack_returns_false() {
    let mut e = editor_with("abc");
    assert!(!e.undo());
    assert_eq!(e.lines[0], "abc");
}

#[test]
fn undo_coalesces_consecutive_typed_chars_into_one_step() {
    let mut e = editor_with("");
    e.insert_char('h');
    e.insert_char('i');
    e.insert_char('!');
    // One undo undoes the whole typed run "hi!".
    assert!(e.undo());
    assert_eq!(e.lines[0], "");
}

#[test]
fn undo_does_not_coalesce_across_movement() {
    let mut e = editor_with("abc");
    e.cursor_col = 3;
    e.insert_char('d');
    e.move_left();
    e.insert_char('z');
    // First undo removes 'z', second undo removes 'd'.
    assert!(e.undo());
    assert_eq!(e.lines[0], "abcd");
    assert!(e.undo());
    assert_eq!(e.lines[0], "abc");
}

#[test]
fn undo_does_not_coalesce_across_backspace() {
    let mut e = editor_with("abc");
    e.cursor_col = 3;
    e.insert_char('d');
    e.backspace();
    e.insert_char('e');
    assert!(e.undo());
    assert_eq!(e.lines[0], "abc");
    assert!(e.undo());
    assert_eq!(e.lines[0], "abcd");
    assert!(e.undo());
    assert_eq!(e.lines[0], "abc");
}

#[test]
fn undo_paste_is_one_step() {
    let mut e = editor_with("abc");
    e.cursor_col = 3;
    e.insert_str("XYZ");
    assert_eq!(e.lines[0], "abcXYZ");
    assert!(e.undo());
    assert_eq!(e.lines[0], "abc");
}

#[test]
fn undo_after_replace_selection_restores_original() {
    let mut e = editor_with("hello world");
    e.cursor_col = 6;
    e.start_selection_at_cursor();
    e.cursor_col = 11;
    e.extend_selection_to_cursor();
    e.insert_char('X');
    assert_eq!(e.lines[0], "hello X");
    assert!(e.undo());
    assert_eq!(e.lines[0], "hello world");
}

#[test]
fn undo_restores_dirty_flag() {
    let mut e = editor_with("abc");
    assert!(!e.dirty);
    e.cursor_col = 3;
    e.insert_char('d');
    assert!(e.dirty);
    e.undo();
    assert!(!e.dirty, "undoing the only edit restores the clean state");
}

#[test]
fn editor_render_paints_search_match_cells_with_yellow_bg() {
    use ratatui::buffer::Buffer;
    let mut e = editor_with("alpha needle bravo needle zulu");
    // The boxed themes' solid find block; Croft Loch tints matches
    // amber (`croft_loch_tints_find_matches_amber_and_the_current_one_stronger`).
    e.theme = crate::theme::Theme::BLACK;
    e.set_search_highlight(Some(String::from("needle")));
    e.focused = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 3,
    };
    let mut buf = Buffer::empty(area);
    (&mut e).render(area, &mut buf);
    let text_x = e.last_inner.x + e.last_gutter_width + 1;
    let y = e.last_inner.y;
    let yellow = Color::Rgb(0xff, 0xd7, 0x4a);
    // First "needle" starts at char index 6 ("alpha "), 6 chars long.
    for col in 6..12u16 {
        assert_eq!(
            buf[(text_x + col, y)].bg,
            yellow,
            "first match cell {col} must have yellow bg"
        );
    }
    // Cells just outside the match must NOT be yellow.
    assert_ne!(buf[(text_x + 5, y)].bg, yellow, "cell before match");
    assert_ne!(buf[(text_x + 12, y)].bg, yellow, "cell after match");
    // Second "needle" starts at char index 19 ("alpha needle bravo "), 6 chars.
    for col in 19..25u16 {
        assert_eq!(
            buf[(text_x + col, y)].bg,
            yellow,
            "second match cell {col} must have yellow bg"
        );
    }
}

#[test]
fn editor_render_lights_other_occurrences_of_selection_in_blue() {
    use ratatui::buffer::Buffer;
    let mut e = editor_with("alpha needle bravo needle zulu");
    // Select the first "needle" (chars 6..12).
    e.selection = Some(EditorSelection {
        anchor: (0, 6),
        head: (0, 12),
    });
    e.focused = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 3,
    };
    let mut buf = Buffer::empty(area);
    (&mut e).render(area, &mut buf);
    let text_x = e.last_inner.x + e.last_gutter_width + 1;
    let y = e.last_inner.y;
    let occurrence = e.theme.ui(Color::Rgb(0x37, 0x61, 0x8e));
    let selection = e.theme.ui(Color::Rgb(0x26, 0x4f, 0x78));
    // The selected occurrence wears the brighter selection band, not the
    // dimmer occurrence blue.
    for col in 6..12u16 {
        assert_eq!(
            buf[(text_x + col, y)].bg,
            selection,
            "selected occurrence cell {col} must show the selection band"
        );
    }
    // The OTHER "needle" (chars 19..25) wears the occurrence blue.
    for col in 19..25u16 {
        assert_eq!(
            buf[(text_x + col, y)].bg,
            occurrence,
            "other occurrence cell {col} must show the occurrence blue"
        );
    }
    // A non-matching cell stays unpainted.
    assert_ne!(buf[(text_x + 18, y)].bg, occurrence, "space before match");
}

#[test]
fn editor_render_skips_occurrence_highlight_for_whitespace_selection() {
    use ratatui::buffer::Buffer;
    let mut e = editor_with("a  b  c  d");
    // Select two spaces (chars 1..3) — whitespace only.
    e.selection = Some(EditorSelection {
        anchor: (0, 1),
        head: (0, 3),
    });
    e.focused = true;
    assert_eq!(e.selection_occurrence_needle(), None);
    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 3,
    };
    let mut buf = Buffer::empty(area);
    (&mut e).render(area, &mut buf);
    let text_x = e.last_inner.x + e.last_gutter_width + 1;
    let y = e.last_inner.y;
    let occurrence = Color::Rgb(0x37, 0x61, 0x8e);
    for col in 0..10u16 {
        assert_ne!(
            buf[(text_x + col, y)].bg,
            occurrence,
            "whitespace-only selection must not light occurrences"
        );
    }
}

#[test]
fn editor_render_skips_occurrence_highlight_for_multiline_selection() {
    let mut e = editor_with("needle\nneedle");
    e.selection = Some(EditorSelection {
        anchor: (0, 0),
        head: (1, 6),
    });
    assert_eq!(
        e.selection_occurrence_needle(),
        None,
        "multi-line selections do not drive occurrence highlighting"
    );
}

#[test]
fn editor_render_does_not_paint_search_highlight_when_unset() {
    use ratatui::buffer::Buffer;
    let mut e = editor_with("alpha needle bravo");
    // Pins the boxed themes' yellow; the Croft Loch twin is
    // `croft_loch_paints_no_find_tint_when_the_highlight_is_unset`.
    e.theme = crate::theme::Theme::BLACK;
    e.set_search_highlight(None);
    e.focused = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 3,
    };
    let mut buf = Buffer::empty(area);
    (&mut e).render(area, &mut buf);
    let text_x = e.last_inner.x + e.last_gutter_width + 1;
    let y = e.last_inner.y;
    let yellow = Color::Rgb(0xff, 0xd7, 0x4a);
    for col in 0..18u16 {
        assert_ne!(
            buf[(text_x + col, y)].bg,
            yellow,
            "no cell should be highlighted when search_highlight is None"
        );
    }
}

#[test]
fn editor_render_search_highlight_is_case_insensitive() {
    use ratatui::buffer::Buffer;
    let mut e = editor_with("Foo bar FOO baz");
    // Read through the boxed themes' solid find block.
    e.theme = crate::theme::Theme::BLACK;
    e.set_search_highlight(Some(String::from("foo")));
    e.focused = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 3,
    };
    let mut buf = Buffer::empty(area);
    (&mut e).render(area, &mut buf);
    let text_x = e.last_inner.x + e.last_gutter_width + 1;
    let y = e.last_inner.y;
    let yellow = Color::Rgb(0xff, 0xd7, 0x4a);
    for col in 0..3u16 {
        assert_eq!(buf[(text_x + col, y)].bg, yellow, "cell {col} of 'Foo'");
    }
    for col in 8..11u16 {
        assert_eq!(buf[(text_x + col, y)].bg, yellow, "cell {col} of 'FOO'");
    }
}

/// Croft Loch: a find match is an amber tint of the row it sits on, not
/// the solid yellow block, with the match's own ink kept (or lifted)
/// so it reads at 4.5:1 or better and no bold added; the current match
/// is a clearly stronger tint of the same amber.
#[test]
fn croft_loch_tints_find_matches_amber_and_the_current_one_stronger() {
    use crate::widgets::surface;
    use ratatui::buffer::Buffer;
    let mut e = editor_with("alpha needle bravo needle zulu");
    let t = surface::tones(e.theme).expect("the default theme is Croft Loch");
    e.set_search_highlight(Some(String::from("needle")));
    e.active_search_match = Some((0, 19, 6));
    e.focused = true;
    let area = Rect::new(0, 0, 60, 3);
    let mut buf = Buffer::empty(area);
    (&mut e).render(area, &mut buf);
    let text_x = e.last_inner.x + e.last_gutter_width + 1;
    let y = e.last_inner.y;
    // The space before the first match carries the row's own fill (the
    // current line's, as the caret is on this row).
    let row = buf[(text_x + 5, y)].bg;
    let tint = |active: bool| {
        surface::match_style(Style::default().bg(row), t, e.theme, active)
            .bg
            .unwrap()
    };
    assert_ne!(tint(false), Color::Rgb(0xff, 0xd7, 0x4a));
    assert_ne!(tint(false), tint(true));
    let lum = |c: Color| match c {
        Color::Rgb(r, g, b) => crate::theme::contrast_ratio((r, g, b), (0, 0, 0)),
        _ => 0.0,
    };
    assert!(
        lum(tint(true)) > lum(tint(false)) + 0.5,
        "the current match is a clearly stronger tint"
    );
    for (cols, active) in [(6..12u16, false), (19..25u16, true)] {
        for col in cols {
            let cell = &buf[(text_x + col, y)];
            assert_eq!(cell.bg, tint(active), "match cell {col}");
            let (Color::Rgb(fr, fg, fb), Color::Rgb(br, bg, bb)) = (cell.fg, cell.bg) else {
                panic!("cell {col} has RGB inks: {cell:?}");
            };
            assert!(
                crate::theme::contrast_ratio((fr, fg, fb), (br, bg, bb)) >= 4.5,
                "cell {col} reads on its tint"
            );
            assert!(!cell.modifier.contains(Modifier::BOLD), "no bold on {col}");
        }
    }
    assert_eq!(buf[(text_x + 12, y)].bg, row, "the tint stops at the match");
}

/// Croft Loch negative: with no find highlight set, no cell of the row
/// wears the amber tint.
#[test]
fn croft_loch_paints_no_find_tint_when_the_highlight_is_unset() {
    let mut e = editor_with("alpha needle bravo");
    e.set_search_highlight(None);
    e.focused = true;
    let buf = guide_buf(&mut e, 40, 3);
    assert_eq!(glyphs_where(&e, &buf, loch_find_tint), "");
}

/// Croft Loch: Cmd+F then Enter parks the caret on a match whose
/// occurrences the idle documentHighlight then greys; the find tint is
/// laid over the occurrence grey (so it still reads as a match) and its
/// ink stays legible on it.
#[test]
fn croft_loch_find_tints_stay_on_top_of_occurrence_tints() {
    let mut e = editor_with("config here");
    e.path = Some(PathBuf::from("/x/a.rs"));
    e.set_search_highlight(Some(String::from("config")));
    e.occurrences = vec![(0, 0, 6, false)];
    let buf = guide_buf(&mut e, 40, 5);
    assert_eq!(glyphs_where(&e, &buf, loch_find_tint), "config");
    let occ = e.theme.occurrence_bg();
    assert_eq!(glyphs_where(&e, &buf, |c| c.bg == occ), "");
    let y = e.last_inner.y;
    let x = e.last_inner.x + e.last_gutter_width + 1;
    let (Color::Rgb(fr, fg, fb), Color::Rgb(br, bg, bb)) = (buf[(x, y)].fg, buf[(x, y)].bg) else {
        panic!("RGB inks");
    };
    assert!(crate::theme::contrast_ratio((fr, fg, fb), (br, bg, bb)) >= 4.5);
}

#[test]
fn open_diff_replaces_blank_initial_tab_with_diff_view() {
    let f1 = NamedTempFile::new().unwrap();
    let f2 = NamedTempFile::new().unwrap();
    std::fs::write(f1.path(), "alpha\nbravo\ncharlie\n").unwrap();
    std::fs::write(f2.path(), "alpha\nBRAVO\ncharlie\n").unwrap();
    let mut t = EditorTabs::new();
    t.open_diff(f1.path(), f2.path()).unwrap();
    assert_eq!(t.tab_count(), 1, "blank initial slot should be reused");
    let active = &t.editors[t.active_index()];
    assert!(active.diff.is_some(), "active tab must hold diff data");
    let diff = active.diff.as_ref().unwrap();
    assert_eq!(diff.left_lines, vec!["alpha", "bravo", "charlie"]);
    assert_eq!(diff.right_lines, vec!["alpha", "BRAVO", "charlie"]);
    assert_eq!(diff.rows.len(), 3);
}

#[test]
fn tab_full_path_reports_the_whole_path_for_disambiguation() {
    // The tab label only shows `file_name()`, so two `app.ts` tabs look
    // identical. The hover tooltip must expose the full path to tell
    // them apart.
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("app.ts");
    std::fs::write(&p, "export const x = 1;\n").unwrap();
    let mut t = EditorTabs::new();
    t.open_pinned(&p).unwrap();
    let idx = t.active_index();
    assert_eq!(
        t.tab_full_path(idx).as_deref(),
        Some(p.to_string_lossy().as_ref()),
        "tooltip must expose the full path, not just the file name"
    );
}

#[test]
fn tab_full_path_is_none_for_an_unsaved_buffer() {
    let t = EditorTabs::new();
    assert_eq!(
        t.tab_full_path(t.active_index()),
        None,
        "an untitled buffer has no path to disambiguate"
    );
}

#[test]
fn tab_full_path_shows_both_sides_of_a_diff() {
    let f1 = NamedTempFile::new().unwrap();
    let f2 = NamedTempFile::new().unwrap();
    std::fs::write(f1.path(), "a\n").unwrap();
    std::fs::write(f2.path(), "b\n").unwrap();
    let mut t = EditorTabs::new();
    t.open_diff(f1.path(), f2.path()).unwrap();
    let label = t.tab_full_path(t.active_index()).unwrap();
    assert!(label.contains(&f1.path().to_string_lossy().into_owned()));
    assert!(label.contains(&f2.path().to_string_lossy().into_owned()));
    assert!(label.contains('\u{2194}'), "diff tooltip shows both sides");
}

#[test]
fn diff_header_paints_prev_next_arrows_at_the_right_edge() {
    // Regression: the user asked for clickable arrows in the diff
    // header so they can hop between change hunks without
    // hand-scrolling. The arrows must paint at the rightmost cells
    // of the header row AND must populate the editor's hit-test
    // fields so the click handler can route mouse events to
    // prev/next-change navigation.
    let f1 = NamedTempFile::new().unwrap();
    let f2 = NamedTempFile::new().unwrap();
    std::fs::write(f1.path(), "alpha\nbravo\ncharlie\n").unwrap();
    std::fs::write(f2.path(), "alpha\nBRAVO\ncharlie\n").unwrap();
    let mut t = EditorTabs::new();
    t.open_diff(f1.path(), f2.path()).unwrap();
    let active_idx = t.active_index();
    t.editors[active_idx].focused = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 20,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut t.editors[active_idx], area, &mut buf);
    let ed = &t.editors[active_idx];
    assert!(
        ed.diff_prev_arrow.width > 0,
        "prev arrow rect must be tracked so a click can navigate to the previous hunk"
    );
    assert!(
        ed.diff_next_arrow.width > 0,
        "next arrow rect must be tracked so a click can navigate to the next hunk"
    );
    // Glyphs land where the rects say they do.
    assert_eq!(
        buf[(ed.diff_prev_arrow.x, ed.diff_prev_arrow.y)].symbol(),
        "\u{2039}",
        "prev arrow cell must paint ‹"
    );
    assert_eq!(
        buf[(ed.diff_next_arrow.x, ed.diff_next_arrow.y)].symbol(),
        "\u{203a}",
        "next arrow cell must paint ›"
    );
    // Next sits to the right of prev on the same header row.
    assert_eq!(ed.diff_prev_arrow.y, ed.diff_next_arrow.y);
    assert!(ed.diff_prev_arrow.x < ed.diff_next_arrow.x);
    // Both arrows sit inside the editor's inner rect's right band.
    let right_edge = ed.last_inner.x + ed.last_inner.width;
    assert!(ed.diff_next_arrow.x < right_edge);
}

#[test]
fn non_diff_tab_clears_diff_arrow_hit_rects_on_render() {
    // Regression guard: switching from a diff tab to a regular file
    // tab must NOT leave the stale arrow rects in place — otherwise
    // a click at those cells on the new tab would mis-route as a
    // change-navigation event.
    let f = NamedTempFile::new().unwrap();
    std::fs::write(f.path(), "alpha\nbravo\ncharlie\n").unwrap();
    let mut ed = Editor::new();
    ed.open(f.path()).unwrap();
    ed.diff_prev_arrow = Rect {
        x: 30,
        y: 0,
        width: 1,
        height: 1,
    };
    ed.diff_next_arrow = Rect {
        x: 32,
        y: 0,
        width: 1,
        height: 1,
    };
    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 10,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut ed, area, &mut buf);
    assert_eq!(ed.diff_prev_arrow, Rect::default());
    assert_eq!(ed.diff_next_arrow, Rect::default());
}

#[test]
fn diff_hit_test_maps_click_in_left_text_column_to_left_side_and_char_col() {
    use crate::widgets::diff::DiffSide;
    let f1 = NamedTempFile::new().unwrap();
    let f2 = NamedTempFile::new().unwrap();
    std::fs::write(f1.path(), "alpha\nbravo\ncharlie\n").unwrap();
    std::fs::write(f2.path(), "alpha\nBRAVO\ncharlie\n").unwrap();
    let mut t = EditorTabs::new();
    t.open_diff(f1.path(), f2.path()).unwrap();
    let active_idx = t.active_index();
    t.editors[active_idx].focused = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 10,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut t.editors[active_idx], area, &mut buf);
    let ed = &t.editors[active_idx];
    let diff = ed.diff.as_ref().unwrap();
    // Header is at ed.last_inner.y; body starts at last_inner.y + 1.
    let body_top = ed.last_inner.y + 1;
    // Left text column begins at l_text_x = inner.x + l_gutter + 2.
    let l_gutter = (diff.left_lines.len() + 1).to_string().len() as u16 + 1;
    let l_text_x = ed.last_inner.x + l_gutter + 2;
    let hit =
        crate::widgets::editor::diff_hit_test(diff, ed.last_inner, l_text_x + 2, body_top + 1);
    assert_eq!(
        hit,
        Some((DiffSide::Left, 1, 2)),
        "a click two cells into the left text column of the second body row must map to Left, row 1, char col 2"
    );
}

#[test]
fn diff_hit_test_maps_click_in_right_text_column_to_right_side() {
    use crate::widgets::diff::DiffSide;
    let f1 = NamedTempFile::new().unwrap();
    let f2 = NamedTempFile::new().unwrap();
    std::fs::write(f1.path(), "alpha\nbravo\n").unwrap();
    std::fs::write(f2.path(), "alpha\nBRAVO\n").unwrap();
    let mut t = EditorTabs::new();
    t.open_diff(f1.path(), f2.path()).unwrap();
    let active_idx = t.active_index();
    t.editors[active_idx].focused = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 10,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut t.editors[active_idx], area, &mut buf);
    let ed = &t.editors[active_idx];
    let diff = ed.diff.as_ref().unwrap();
    let body_top = ed.last_inner.y + 1;
    let half = ed.last_inner.width / 2;
    let r_gutter = (diff.right_lines.len() + 1).to_string().len() as u16 + 1;
    let r_text_x = ed.last_inner.x + half + 1 + r_gutter + 2;
    let hit =
        crate::widgets::editor::diff_hit_test(diff, ed.last_inner, r_text_x + 3, body_top + 1);
    assert!(
        matches!(hit, Some((DiffSide::Right, 1, 3))),
        "a click three cells into the right text column of the second body row must map to Right, row 1, char col 3; got {hit:?}"
    );
}

#[test]
fn diff_caret_screen_pos_inverts_the_right_column_hit_test() {
    use crate::widgets::diff::DiffSide;
    let f1 = NamedTempFile::new().unwrap();
    let f2 = NamedTempFile::new().unwrap();
    std::fs::write(f1.path(), "alpha\nbravo\n").unwrap();
    std::fs::write(f2.path(), "alpha\nBRAVO\n").unwrap();
    let mut t = EditorTabs::new();
    t.open_diff(f1.path(), f2.path()).unwrap();
    let active_idx = t.active_index();
    t.editors[active_idx].focused = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 10,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut t.editors[active_idx], area, &mut buf);
    let body_top = t.editors[active_idx].last_inner.y + 1;
    let half = t.editors[active_idx].last_inner.width / 2;
    let diff = t.editors[active_idx].diff.as_ref().unwrap();
    let r_gutter = (diff.right_lines.len() + 1).to_string().len() as u16 + 1;
    let r_text_x = t.editors[active_idx].last_inner.x + half + 1 + r_gutter + 2;
    t.editors[active_idx]
        .diff
        .as_mut()
        .unwrap()
        .start_selection(DiffSide::Right, 1, 3);
    let pos = t.editors[active_idx].diff_caret_screen_pos();
    assert_eq!(
        pos,
        Some((r_text_x + 3, body_top + 1)),
        "caret at right row 1 col 3 must land on the cell diff_hit_test maps that screen point back to"
    );
}

/// Render a side-by-side diff of `left` vs `right` into an 80x10 buffer.
fn render_wide_diff(left: &str, right: &str) -> (ratatui::buffer::Buffer, Rect, u16) {
    let f1 = NamedTempFile::new().unwrap();
    let f2 = NamedTempFile::new().unwrap();
    std::fs::write(f1.path(), left).unwrap();
    std::fs::write(f2.path(), right).unwrap();
    let mut t = EditorTabs::new();
    t.open_diff(f1.path(), f2.path()).unwrap();
    let idx = t.active_index();
    let area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 10,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut t.editors[idx], area, &mut buf);
    let inner = t.editors[idx].last_inner;
    let r_gutter = (t.editors[idx].diff.as_ref().unwrap().right_lines.len() + 1)
        .to_string()
        .len() as u16
        + 1;
    let r_text_x = inner.x + inner.width / 2 + 1 + r_gutter + 2;
    (buf, inner, r_text_x)
}

fn row_text(buf: &ratatui::buffer::Buffer, y: u16, from: u16, to: u16) -> String {
    (from..to).map(|x| buf[(x, y)].symbol()).collect()
}

/// The x of the left text column and the row showing `left_text` in a
/// diff rendered by [`render_wide_diff`].
fn left_text_at(
    buf: &ratatui::buffer::Buffer,
    inner: Rect,
    lines: usize,
    left_text: &str,
) -> (u16, u16) {
    let l_text_x = inner.x + (lines + 1).to_string().len() as u16 + 1 + 2;
    let y = (inner.y..inner.bottom())
        .find(|&y| row_text(buf, y, l_text_x, inner.x + inner.width / 2).starts_with(left_text))
        .expect("the changed row is drawn");
    (l_text_x, y)
}

/// The inks the default theme's diff view paints with, for the
/// tests that check where a mark lands rather than what it looks like.
fn default_inks() -> DiffInks {
    diff_inks(crate::theme::Theme::default())
}

/// #1351: a one-character edit marks just that character on each side,
/// in a stronger tint than the rest of the changed line.
#[test]
fn a_one_character_change_is_marked_within_the_line() {
    let (buf, inner, r_text_x) = render_wide_diff("x\nbackoff=1.5\n", "x\nbackoff=2.5\n");
    let (l_text_x, y) = left_text_at(&buf, inner, 2, "backoff=1.5");
    assert_eq!(buf[(l_text_x + 8, y)].symbol(), "1");
    assert_eq!(buf[(l_text_x + 8, y)].bg, default_inks().intraline_removed);
    assert_eq!(buf[(r_text_x + 8, y)].symbol(), "2");
    assert_eq!(buf[(r_text_x + 8, y)].bg, default_inks().intraline_added);
    for col in [0u16, 7, 9, 10] {
        assert_ne!(
            buf[(l_text_x + col, y)].bg,
            default_inks().intraline_removed,
            "col {col}"
        );
        assert_ne!(
            buf[(r_text_x + col, y)].bg,
            default_inks().intraline_added,
            "col {col}"
        );
    }
}

/// #1351 negative: a line rewritten from scratch gets no character
/// marks, so it does not turn into confetti.
#[test]
fn a_rewritten_line_gets_no_character_marks() {
    let (buf, inner, _) = render_wide_diff("x\nalpha beta gamma\n", "x\nquite other words\n");
    let marked = (inner.y..inner.bottom())
        .flat_map(|y| (inner.x..inner.right()).map(move |x| (x, y)))
        .filter(|&p| {
            let bg = buf[p].bg;
            bg == default_inks().intraline_removed || bg == default_inks().intraline_added
        })
        .count();
    assert_eq!(marked, 0);
}

/// #1351: a change past the right edge of a side is flagged with `›` at
/// that edge, so two lines that look the same on screen say they differ.
#[test]
fn a_change_past_the_right_edge_is_flagged_at_the_edge() {
    let line = |v: &str| {
        format!("TIMEOUT_SECONDS = compute_timeout(base=30, retries=3, backoff={v}, jitter=True)")
    };
    let (buf, inner, r_text_x) = render_wide_diff(
        &format!("x\n{}\n", line("1.5")),
        &format!("x\n{}\n", line("2.5")),
    );
    let (_, y) = left_text_at(&buf, inner, 2, "TIMEOUT_SECONDS");
    let edge = inner.right() - 1;
    assert!(
        r_text_x + 64 > edge,
        "precondition: the change is off-screen"
    );
    assert_eq!(buf[(edge, y)].symbol(), "\u{203a}");
    assert_eq!(buf[(edge, y)].bg, default_inks().intraline_added);
}

/// #1351: wide characters before the change keep the mark on the right
/// cells (#1248): `語` takes two cells, so `b` is at cell 6.
#[test]
fn the_mark_stays_aligned_after_wide_characters() {
    let (buf, inner, r_text_x) = render_wide_diff("x\n日本語a\n", "x\n日本語b\n");
    let (_, y) = left_text_at(&buf, inner, 2, "日");
    assert_eq!(buf[(r_text_x + 6, y)].symbol(), "b");
    assert_eq!(buf[(r_text_x + 6, y)].bg, default_inks().intraline_added);
    for col in 0..6u16 {
        assert_ne!(
            buf[(r_text_x + col, y)].bg,
            default_inks().intraline_added,
            "col {col}"
        );
    }
}

/// Wide (CJK) characters take two cells: each side is clipped to its
/// half by display width, so the seam survives on every row and the
/// right side starts where it should (#1248).
#[test]
fn a_diff_of_wide_characters_keeps_each_side_in_its_half() {
    let left = "# 项目\n这是一个轻量级的终端编辑器，支持多种编程语言和版本控制功能。\nend\n";
    let right = "# 项目\n这是一个高性能的终端编辑器，支持多种编程语言和版本控制功能。\nend\n";
    let (buf, inner, r_text_x) = render_wide_diff(left, right);
    let seam_x = inner.x + inner.width / 2;
    for y in inner.y + 1..inner.y + 4 {
        assert_eq!(
            buf[(seam_x, y)].symbol(),
            default_inks().seam.0,
            "row {y}: {:?}",
            row_text(&buf, y, inner.x, inner.right())
        );
    }
    let changed = (inner.y..inner.bottom())
        .find(|&y| row_text(&buf, y, r_text_x, inner.right()).contains('高'))
        .expect("the changed right line is drawn");
    assert_eq!(
        buf[(r_text_x, changed)].symbol(),
        "这",
        "starts at its column"
    );
    assert!(
        !row_text(&buf, changed, inner.x, seam_x).contains('高'),
        "no right text left of the seam"
    );
}

/// A wide character that would straddle a column's edge is left out and
/// its cell padded, rather than drawn across the seam.
#[test]
fn clip_pad_cells_never_splits_a_wide_character() {
    assert_eq!(clip_pad_cells("日本語", 0, 5), "日本 ");
    assert_eq!(clip_pad_cells("日本語", 1, 6), "本語  ");
    assert_eq!(clip_pad_cells("ab日", 0, 3), "ab ");
}

/// The diff measures a tab after a grapheme cluster from the cells the
/// cluster is drawn in, so its text, marks and clicks stay together.
#[test]
fn the_diff_counts_a_tab_after_an_emoji_cluster_from_its_drawn_cells() {
    set_tab_cells(4);
    let line = "\u{1f44d}\u{1f3fd}\tX";
    assert_eq!(clip_pad_cells(line, 0, 6), "\u{1f44d}\u{1f3fd}  X ");
    assert_eq!(char_at_cell_from(line, 0, 4), 3, "a click on X lands on X");
}

/// A change to a character inside a grapheme cluster, such as only the
/// skin tone of `👍🏽`, marks the cells the whole cluster is drawn in.
#[test]
fn a_change_inside_an_emoji_cluster_marks_the_whole_cluster() {
    let mut buf = Buffer::empty(Rect::new(0, 0, 6, 1));
    let bg = Color::Red;
    let line = "a\u{1f44d}\u{1f3fd}b";
    paint_intraline_range(&mut buf, 0, 0, 6, line, 0, (2, 3), bg, Color::White, None);
    let marked: Vec<bool> = (0..6).map(|x| buf[(x, 0)].bg == bg).collect();
    assert_eq!(marked, [false, true, true, false, false, false]);
    assert_ne!(buf[(5, 0)].symbol(), "\u{203a}", "no off-screen marker");
}

/// The diff's syntax colours land on the cells its text is drawn in, so a
/// token after a tab is coloured at the tab stop, not Tab Size cells on.
#[test]
fn the_diffs_syntax_colours_follow_the_tab_stops() {
    set_tab_cells(4);
    let mut buf = Buffer::empty(Rect::new(0, 0, 8, 1));
    let red = Style::default().fg(Color::Red);
    paint_diff_syntax(
        &mut buf,
        0,
        0,
        8,
        "a\tb",
        0,
        &[HiSpan::new(2, 3, red)],
        DiffInkLift::new(None, Color::Reset),
    );
    assert_eq!(clip_pad_cells("a\tb", 0, 8), "a   b   ");
    assert_eq!(buf[(4, 0)].fg, Color::Red, "b is drawn at the stop, cell 4");
    assert_eq!(buf[(5, 0)].fg, Color::Reset);
}

/// #891: a tab takes one cell in the diff, as its selection, find
/// highlight and clicks count it, so a tab-indented line is not drawn
/// flush-left and its changed character is marked where it is drawn.
#[test]
fn a_tab_takes_its_cell_in_the_side_by_side_diff() {
    set_tab_cells(1);
    assert_eq!(clip_pad_cells("\tx", 0, 4), " x  ");
    // The diff draws a tab as the Tab Size of the buffer (4 by default).
    let (buf, inner, _) = render_wide_diff("x\n\tn = 1\n", "x\n\tn = 2\n");
    let (l_text_x, y) = left_text_at(&buf, inner, 2, "    n = 1");
    assert_eq!(buf[(l_text_x + 8, y)].symbol(), "1");
    assert_eq!(buf[(l_text_x + 8, y)].bg, default_inks().intraline_removed);
}

/// Narrow text clips and pads exactly as before: by character, to the
/// column's width.
#[test]
fn clip_pad_cells_matches_char_clipping_for_narrow_text() {
    assert_eq!(clip_pad_cells("abcdef", 2, 3), "cde");
    assert_eq!(clip_pad_cells("ab", 0, 4), "ab  ");
    assert_eq!(clip_pad_cells("abc", 5, 2), "  ");
    let (buf, inner, r_text_x) = render_wide_diff("x\nold line\n", "x\nnew line\n");
    let changed = (inner.y..inner.bottom())
        .find(|&y| row_text(&buf, y, r_text_x, inner.right()).starts_with("new line"))
        .expect("ascii right line drawn at its column");
    assert_eq!(
        buf[(inner.x + inner.width / 2, changed)].symbol(),
        default_inks().seam.0
    );
}

/// #2020: the diff view paints code in its language's colours, on the
/// rows' red, green and plain backgrounds, as the editor does.
#[test]
fn the_diff_view_paints_code_in_its_syntax_colours() {
    let file = |text: &str| {
        let f = tempfile::Builder::new().suffix(".rs").tempfile().unwrap();
        std::fs::write(f.path(), text).unwrap();
        f
    };
    let f1 = file("fn main() {\n    let x = 1;\n}\n");
    let f2 = file("fn main() {\n    let x = 2;\n}\n");
    let mut t = EditorTabs::new();
    t.open_diff(f1.path(), f2.path()).unwrap();
    let idx = t.active_index();
    t.editors[idx].diff.as_mut().unwrap().colour_syntax(None);
    let area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 10,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut t.editors[idx], area, &mut buf);
    let ed = &t.editors[idx];
    let inner = ed.last_inner;
    let (l_text_x, unchanged) = left_text_at(&buf, inner, 3, "fn main() {");
    assert_ne!(
        buf[(l_text_x, unchanged)].fg,
        buf[(l_text_x + 3, unchanged)].fg,
        "the keyword `fn` and the name `main` read in different colours"
    );
    let (_, changed) = left_text_at(&buf, inner, 3, "    let x = 1;");
    let r_text_x = inner.x + inner.width / 2 + 1 + 2 + 2;
    assert_eq!(
        row_text(&buf, changed, r_text_x, r_text_x + 14),
        "    let x = 2;"
    );
    let DiffInks {
        removed_bg,
        added_bg,
        ..
    } = diff_inks(ed.theme);
    for (x, bg, side) in [
        (l_text_x, removed_bg, "removed"),
        (r_text_x, added_bg, "added"),
    ] {
        let keyword = &buf[(x + 4, changed)];
        let name = &buf[(x + 8, changed)];
        assert_eq!(keyword.symbol(), "l");
        assert_ne!(
            keyword.fg, name.fg,
            "`let` and `x` differ on the {side} side"
        );
        assert_eq!(keyword.bg, bg, "the {side} row keeps its background");
        assert_eq!(name.bg, bg);
    }
}

/// A file with no grammar keeps the diff's own tints: red text on the
/// removed side, green on the added (Croft Loch's are the body ink
/// leaning toward each hue, and on the changed characters' brighter
/// tint that ink is lifted to read but still leans the same way).
#[test]
fn a_diff_with_no_grammar_keeps_its_red_and_green_text() {
    let (buf, inner, r_text_x) = render_wide_diff("x\nold line\n", "x\nnew line\n");
    let (l_text_x, y) = left_text_at(&buf, inner, 2, "old line");
    let inks = default_inks();
    // " line" is the unchanged tail; "old" / "new" are the change.
    assert_eq!(buf[(l_text_x + 4, y)].fg, inks.removed_fg);
    assert_eq!(buf[(r_text_x + 4, y)].fg, inks.added_fg);
    for (ink, changed) in [
        (inks.removed_fg, buf[(l_text_x, y)].fg),
        (inks.added_fg, buf[(r_text_x, y)].fg),
    ] {
        for c in [ink, changed] {
            let Color::Rgb(r, g, _) = c else {
                panic!("an RGB ink");
            };
            if ink == inks.removed_fg {
                assert!(r > g, "removed text leans red ({c:?})");
            } else {
                assert!(g > r, "added text leans green ({c:?})");
            }
        }
    }
}

/// A two-file diff of a one-line change plus an inserted line, drawn
/// 80 by 10 under `theme` with the pointer at `pointer`; returns the
/// buffer, the inner rect and the arrows' hit rects.
fn render_diff_in(
    theme: crate::theme::Theme,
    pointer: Option<(u16, u16)>,
) -> (ratatui::buffer::Buffer, Rect, (Rect, Rect)) {
    let f1 = tempfile::Builder::new().suffix(".txt").tempfile().unwrap();
    let f2 = tempfile::Builder::new().suffix(".txt").tempfile().unwrap();
    std::fs::write(f1.path(), "keep\nbackoff=1.5\ntail\n").unwrap();
    std::fs::write(f2.path(), "keep\nbackoff=2.5\nnew line\ntail\n").unwrap();
    let mut t = EditorTabs::new();
    t.open_diff(f1.path(), f2.path()).unwrap();
    let idx = t.active_index();
    t.editors[idx].theme = theme;
    t.editors[idx].pointer = pointer;
    let area = Rect::new(0, 0, 80, 10);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut t.editors[idx], area, &mut buf);
    let ed = &t.editors[idx];
    (buf, ed.last_inner, (ed.diff_prev_arrow, ed.diff_next_arrow))
}

/// Croft Loch: the diff header sits on the chrome tone (not the teal
/// band), each file's name over its own column with the current side's
/// in the strong ink and `↔` faint on the seam; the `‹ ›` arrows are
/// muted at rest and strong under the pointer, on the same hit cells a
/// boxed theme gives them.
#[test]
fn croft_loch_diff_header_sits_on_chrome() {
    let theme = crate::theme::Theme::LOCH;
    let t = crate::widgets::surface::tones(theme).unwrap();
    let (buf, inner, (prev, next)) = render_diff_in(theme, None);
    for x in inner.x..inner.right() {
        assert_eq!(buf[(x, inner.y)].bg, t.chrome(), "({x}) header on chrome");
        assert!(!buf[(x, inner.y)].modifier.contains(Modifier::BOLD));
    }
    let row = row_text(&buf, inner.y, inner.x, inner.right());
    assert!(!row.contains("diff:"), "{row:?}");
    let seam_x = inner.x + inner.width / 2;
    assert_eq!(buf[(seam_x, inner.y)].symbol(), "\u{2194}");
    assert_eq!(buf[(seam_x, inner.y)].fg, t.faint());
    // Two digits of gutter (3 lines + 1), a space, the sign and its pad.
    let l_text_x = inner.x + 2 + 2;
    let r_text_x = seam_x + 1 + 2 + 2;
    assert_eq!(buf[(l_text_x, inner.y)].fg, t.text(), "the old side");
    assert_eq!(buf[(r_text_x, inner.y)].fg, t.strong(), "the current side");
    assert_ne!(buf[(r_text_x, inner.y)].symbol(), " ");
    assert_eq!(buf[(prev.x, prev.y)].symbol(), "\u{2039}");
    assert_eq!(buf[(prev.x, prev.y)].fg, t.muted());
    assert_eq!(buf[(next.x, next.y)].fg, t.muted());
    let (_, _, boxed) = render_diff_in(crate::theme::Theme::BLACK, None);
    assert_eq!((prev, next), boxed, "the arrows keep their hit cells");
    let (hovered, _, _) = render_diff_in(theme, Some((next.x, next.y)));
    assert_eq!(hovered[(next.x, next.y)].fg, t.strong());
    assert_eq!(hovered[(next.x, next.y)].bg, t.hover_on(t.chrome));
    assert_eq!(hovered[(prev.x, prev.y)].fg, t.muted());
}

/// Croft Loch: a changed row is a tint of its hue over the canvas, its
/// changed characters a stronger tint, its sign in the hue and its line
/// number muted (an unchanged row's stays faint); the blank side
/// opposite an inserted line is a faint hatch, and the footer is muted
/// text with keycaps on chrome.
#[test]
fn croft_loch_diff_rows_are_soft_tints() {
    let theme = crate::theme::Theme::LOCH;
    let t = crate::widgets::surface::tones(theme).unwrap();
    let inks = diff_inks(theme);
    let tint = |c: Color, k: f32| {
        let Color::Rgb(r, g, b) = c else {
            panic!("an RGB colour");
        };
        diff_tint(t, (r, g, b), k)
    };
    assert_eq!(inks.removed_bg, tint(theme.git_deleted(), DIFF_ROW_TINT.0));
    assert_eq!(inks.added_bg, tint(theme.git_added(), DIFF_ROW_TINT.1));
    let Color::Rgb(r, g, b) = inks.removed_bg else {
        panic!("an RGB tint");
    };
    assert!(r > g && r > b, "the removed tint is still plainly red");
    let Color::Rgb(r, g, b) = inks.added_bg else {
        panic!("an RGB tint");
    };
    assert!(g > r && g > b, "the added tint is still plainly green");
    let (buf, inner, _) = render_diff_in(theme, None);
    let (l_text_x, y) = left_text_at(&buf, inner, 3, "backoff=1.5");
    let seam_x = inner.x + inner.width / 2;
    let r_text_x = seam_x + 1 + 2 + 2;
    assert_eq!(buf[(l_text_x, y)].bg, inks.removed_bg);
    assert_eq!(buf[(r_text_x, y)].bg, inks.added_bg);
    assert_eq!(buf[(l_text_x + 8, y)].bg, inks.intraline_removed, "the 1");
    assert_eq!(buf[(r_text_x + 8, y)].bg, inks.intraline_added, "the 2");
    assert_eq!(buf[(l_text_x - 2, y)].symbol(), "-");
    assert_eq!(buf[(l_text_x - 2, y)].fg, theme.git_deleted());
    assert_eq!(buf[(r_text_x - 2, y)].fg, theme.git_added());
    assert_eq!(buf[(inner.x, y)].symbol(), "2");
    assert_eq!(buf[(inner.x, y)].fg, t.muted(), "a changed line number");
    assert_eq!(buf[(inner.x, y - 1)].symbol(), "1");
    assert_eq!(buf[(inner.x, y - 1)].fg, t.faint(), "an unchanged one");
    assert_eq!(buf[(seam_x - 1, y)].bg, inks.removed_bg, "tint to the seam");
    assert_eq!(buf[(seam_x, y)].symbol(), "\u{258f}");
    // The inserted line: hatched on the left, tinted on the right.
    let ins = y + 1;
    assert_eq!(row_text(&buf, ins, r_text_x, r_text_x + 8), "new line");
    for x in inner.x..seam_x {
        assert_eq!(buf[(x, ins)].symbol(), "\u{2571}", "({x}) hatched");
        assert_eq!(buf[(x, ins)].bg, t.canvas());
    }
    assert_eq!(buf[(r_text_x, ins)].bg, inks.added_bg);
    // The footer: muted on chrome, the keys as keycaps.
    let foot = inner.bottom() - 1;
    let text = row_text(&buf, foot, inner.x, inner.right());
    assert!(text.contains(" F7 "), "{text:?}");
    assert_eq!(buf[(inner.x + 1, foot)].fg, t.muted());
    assert_eq!(buf[(inner.right() - 1, foot)].bg, t.chrome());
    let cells: Vec<&str> = (inner.x..inner.right())
        .map(|x| buf[(x, foot)].symbol())
        .collect();
    let f7 = cells.windows(2).position(|w| w == ["F", "7"]).unwrap() as u16;
    assert_eq!(buf[(inner.x + f7, foot)].bg, t.keycap_on(t.chrome()));
}

/// Croft Loch: a changed row's tint stands 1.5 to 1.7:1 off the black
/// canvas, so a change is seen at a glance and still sits calmly, its
/// changed characters a further 1.5:1 or more off the row, and its line
/// number reads at 3:1 on the tint; the hatch opposite an insertion
/// stays a whisper. A boxed theme lifts nothing and keeps one gutter
/// ink.
#[test]
fn croft_loch_diff_tints_are_plain_at_a_glance() {
    use crate::theme::contrast_ratio;
    let rgb = |c: Color| match c {
        Color::Rgb(r, g, b) => (r, g, b),
        other => panic!("an RGB colour, not {other:?}"),
    };
    let theme = crate::theme::Theme::LOCH;
    let t = crate::widgets::surface::tones(theme).unwrap();
    let inks = diff_inks(theme);
    for (side, row, change) in [
        ("removed", inks.removed_bg, inks.intraline_removed),
        ("added", inks.added_bg, inks.intraline_added),
    ] {
        let tint = contrast_ratio(rgb(row), t.canvas);
        assert!((1.5..=1.7).contains(&tint), "{side} row {tint:.2}:1");
        let mark = contrast_ratio(rgb(change), rgb(row));
        assert!(mark >= 1.5, "{side} change {mark:.2}:1 on its row");
        let number = contrast_ratio(rgb(inks.changed_gutter_fg), rgb(row));
        assert!(number >= 3.0, "{side} line number {number:.2}:1");
        let faint = contrast_ratio(t.faint, rgb(row));
        assert!(
            faint < 3.0,
            "the faint ink would not have read ({faint:.2}:1)"
        );
    }
    let (_, hatch) = inks.hatch.expect("Loch hatches its filler");
    assert!(
        contrast_ratio(rgb(hatch), t.canvas) < 1.3,
        "the hatch is subtle"
    );

    let boxed = diff_inks(crate::theme::Theme::BLACK);
    assert!(boxed.legible.is_none(), "a boxed theme lifts no ink");
    assert_eq!(boxed.changed_gutter_fg, boxed.gutter_fg);
}

/// Croft Loch: code on a changed row reads at 4.5:1 or better on the
/// row's tint and on its changed characters' stronger one. A colour
/// that already does (a keyword on the row) is left as the editor
/// draws it; one that would not (a comment) is lifted only as far as
/// it must, keeping its hue rather than turning into the strong ink.
#[test]
fn croft_loch_diff_lifts_code_that_would_not_read_on_a_tint() {
    use crate::theme::contrast_ratio;
    let rgb = |c: Color| match c {
        Color::Rgb(r, g, b) => (r, g, b),
        other => panic!("an RGB colour, not {other:?}"),
    };
    let file = |text: &str| {
        let f = tempfile::Builder::new().suffix(".rs").tempfile().unwrap();
        std::fs::write(f.path(), text).unwrap();
        f
    };
    let head = "fn main() {\n    let a = 1;\n    // keep\n";
    let f1 = file(&format!("{head}    // read the text\n    let n = 8;\n}}\n"));
    let f2 = file(&format!(
        "{head}    // read the file\n    let n = 12;\n}}\n"
    ));
    let mut t = EditorTabs::new();
    t.open_diff(f1.path(), f2.path()).unwrap();
    let idx = t.active_index();
    t.editors[idx].theme = crate::theme::Theme::LOCH;
    t.editors[idx].diff.as_mut().unwrap().colour_syntax(None);
    let area = Rect::new(0, 0, 80, 14);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut t.editors[idx], area, &mut buf);
    let ed = &t.editors[idx];
    let tones = crate::widgets::surface::tones(ed.theme).unwrap();
    let inner = ed.last_inner;
    let seam_x = inner.x + inner.width / 2;
    let r_text_x = seam_x + 1 + 2 + 2;
    let (l_text_x, kept_let) = left_text_at(&buf, inner, 6, "    let a = 1;");
    let (_, kept_comment) = left_text_at(&buf, inner, 6, "    // keep");
    let (_, comment) = left_text_at(&buf, inner, 6, "    // read the text");
    let (_, number) = left_text_at(&buf, inner, 6, "    let n = 8;");
    let inks = diff_inks(ed.theme);
    assert_eq!(buf[(l_text_x + 4, comment)].bg, inks.removed_bg);
    assert_eq!(buf[(l_text_x + 16, comment)].bg, inks.intraline_removed);
    for y in [comment, number] {
        for x in (l_text_x..seam_x - 1).chain(r_text_x..inner.right()) {
            let cell = &buf[(x, y)];
            if cell.symbol() == " " {
                continue;
            }
            let ratio = contrast_ratio(rgb(cell.fg), rgb(cell.bg));
            assert!(
                ratio >= 4.5,
                "{:?} at ({x},{y}): {ratio:.2}:1",
                cell.symbol()
            );
        }
    }
    assert_eq!(
        buf[(l_text_x + 4, number)].fg,
        buf[(l_text_x + 4, kept_let)].fg,
        "`let` reads on the tint as it is"
    );
    let lifted = buf[(l_text_x + 4, comment)].fg;
    assert_ne!(lifted, buf[(l_text_x + 4, kept_comment)].fg, "lifted");
    assert_ne!(lifted, tones.strong(), "only as far as it must");
}

/// A boxed theme's diff is the one it always drew: the teal header
/// band in bold white, the saturated red and green rows and the solid
/// filler, the `│` seam and the grey footer band.
#[test]
fn a_boxed_diff_keeps_its_bands_and_saturated_rows() {
    let theme = crate::theme::Theme::BLACK;
    let (buf, inner, _) = render_diff_in(theme, None);
    assert_eq!(
        buf[(inner.x, inner.y)].bg,
        theme.ui(Color::Rgb(0x09, 0x4d, 0x77))
    );
    assert!(row_text(&buf, inner.y, inner.x, inner.right()).starts_with(" diff: "));
    let (l_text_x, y) = left_text_at(&buf, inner, 3, "backoff=1.5");
    assert_eq!(
        buf[(l_text_x, y)].bg,
        theme.ui(Color::Rgb(0x4a, 0x1f, 0x1f))
    );
    let (r, g, b) = crate::widgets::diff::INTRALINE_REMOVED_BG;
    assert_eq!(buf[(l_text_x + 8, y)].bg, Color::Rgb(r, g, b));
    assert_eq!(
        buf[(inner.x, y + 1)].bg,
        theme.ui(Color::Rgb(0x1f, 0x42, 0x2a)),
        "the filler is the solid green"
    );
    assert_eq!(buf[(inner.x, y + 1)].symbol(), " ");
    assert_eq!(buf[(inner.x + inner.width / 2, y)].symbol(), "\u{2502}");
    assert_eq!(
        buf[(inner.x, inner.bottom() - 1)].bg,
        theme.ui(Color::Rgb(0x14, 0x18, 0x22))
    );
}

/// Croft Loch's deleted-file view: the same chrome header (the name in
/// the strong ink, the note muted), soft red rows and a chrome footer.
#[test]
fn croft_loch_deleted_file_diff_is_calm() {
    let theme = crate::theme::Theme::LOCH;
    let t = crate::widgets::surface::tones(theme).unwrap();
    let inks = diff_inks(theme);
    let mut diff = crate::widgets::diff::DiffData::build_unified_deletion(
        std::path::PathBuf::from("gone.txt"),
        "one\ntwo\n",
    );
    let area = Rect::new(0, 0, 60, 8);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    render_diff(&mut diff, area, &mut buf, theme.editor_bg(), theme, None);
    assert_eq!(buf[(0, 0)].bg, t.chrome());
    let head = row_text(&buf, 0, 0, 60);
    assert!(head.starts_with(" gone.txt  \u{b7}  deleted"), "{head:?}");
    assert_eq!(buf[(1, 0)].fg, t.strong());
    assert_eq!(buf[(14, 0)].symbol(), "d");
    assert_eq!(buf[(14, 0)].fg, t.muted());
    let text_x = 2 + 2;
    assert_eq!(row_text(&buf, 1, text_x, text_x + 3), "one");
    assert_eq!(buf[(text_x, 1)].bg, inks.removed_bg);
    assert_eq!(buf[(text_x - 2, 1)].fg, theme.git_deleted());
    assert_eq!(buf[(59, 7)].bg, t.chrome(), "footer on chrome");
}

#[test]
fn render_diff_paints_selection_band_over_selected_cells() {
    use crate::widgets::diff::DiffSide;
    let f1 = NamedTempFile::new().unwrap();
    let f2 = NamedTempFile::new().unwrap();
    std::fs::write(f1.path(), "alpha\nbravo\n").unwrap();
    std::fs::write(f2.path(), "alpha\nBRAVO\n").unwrap();
    let mut t = EditorTabs::new();
    t.open_diff(f1.path(), f2.path()).unwrap();
    let active_idx = t.active_index();
    t.editors[active_idx].focused = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 10,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut t.editors[active_idx], area, &mut buf);
    let diff_mut = t.editors[active_idx].diff.as_mut().unwrap();
    diff_mut.start_selection(DiffSide::Left, 1, 0);
    diff_mut.extend_selection_to(1, 5);
    ratatui::widgets::Widget::render(&mut t.editors[active_idx], area, &mut buf);
    let ed = &t.editors[active_idx];
    let diff = ed.diff.as_ref().unwrap();
    let l_gutter = (diff.left_lines.len() + 1).to_string().len() as u16 + 1;
    let l_text_x = ed.last_inner.x + l_gutter + 2;
    let y = ed.last_inner.y + 2;
    let band_bg = ed.theme.ui(Color::Rgb(0x26, 0x4f, 0x78));
    for col in 0..5u16 {
        assert_eq!(
            buf[(l_text_x + col, y)].bg,
            band_bg,
            "cell {col} of the second body row must paint the selection band on the left column"
        );
    }
}

#[test]
fn open_diff_inserts_a_new_tab_when_an_open_file_already_exists() {
    let existing = NamedTempFile::new().unwrap();
    std::fs::write(existing.path(), "x\n").unwrap();
    let f1 = NamedTempFile::new().unwrap();
    let f2 = NamedTempFile::new().unwrap();
    std::fs::write(f1.path(), "1\n").unwrap();
    std::fs::write(f2.path(), "2\n").unwrap();
    let mut t = EditorTabs::new();
    t.open_pinned(existing.path()).unwrap();
    let before = t.tab_count();
    t.open_diff(f1.path(), f2.path()).unwrap();
    assert_eq!(t.tab_count(), before + 1, "must insert a new diff tab");
    assert!(t.editors[t.active_index()].diff.is_some());
}

#[test]
fn diff_tab_label_uses_arrow_between_filenames() {
    let f1 = NamedTempFile::new().unwrap();
    let f2 = NamedTempFile::new().unwrap();
    std::fs::write(f1.path(), "a\n").unwrap();
    std::fs::write(f2.path(), "b\n").unwrap();
    let mut t = EditorTabs::new();
    t.open_diff(f1.path(), f2.path()).unwrap();
    let label = tab_label(&t.editors[t.active_index()]);
    let l = f1
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let r = f2
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert_eq!(label, format!("{l} \u{2194} {r}"));
}

#[test]
fn git_diff_text_tab_label_omits_arrow_when_right_side_is_synthetic() {
    // Regression for "git diff master ↔ null" in the tab strip:
    // synthetic single-sided diffs (raw `git diff` text view, full-
    // file deletion view) leave `right_path` empty; the tab label
    // must collapse to just the left label rather than trailing the
    // misleading "↔ null".
    let mut t = EditorTabs::new();
    t.open_git_diff_side_by_side(
        std::path::Path::new("git diff master"),
        "diff --git a/x b/x\n@@ -1 +1 @@\n-old\n+new\n",
    )
    .unwrap();
    let label = tab_label(&t.editors[t.active_index()]);
    assert_eq!(
        label, "git diff master",
        "synthetic git-diff tab must not show '↔ null' — the right side is virtual"
    );
}

#[test]
fn editor_tabs_search_highlight_survives_close_then_open_via_preview() {
    // Reported bug: after closing the first tab with X, the next file
    // opened from search lost the yellow highlights. The new editor
    // was created without the EditorTabs' current term applied.
    let f1 = NamedTempFile::new().unwrap();
    let f2 = NamedTempFile::new().unwrap();
    std::fs::write(f1.path(), "alpha needle bravo\n").unwrap();
    std::fs::write(f2.path(), "second needle line\n").unwrap();
    let mut t = EditorTabs::new();
    t.open_pinned(f1.path()).unwrap();
    t.set_search_highlight(
        Some(String::from("needle")),
        crate::widgets::search::SearchOpts::default(),
    );
    // Close the first (and only non-blank) tab — leaves the blank
    // initial tab.
    let close_idx = t
        .editors
        .iter()
        .position(|e| e.path.as_deref() == Some(f1.path()))
        .unwrap();
    assert!(t.close_tab(close_idx));
    // Now open a different file via preview, mirroring "click on a
    // search hit" in the running app.
    t.open_preview(f2.path()).unwrap();
    let active = t
        .editors
        .iter()
        .find(|e| e.path.as_deref() == Some(f2.path()))
        .unwrap();
    assert_eq!(
        active.search_highlight.as_deref(),
        Some("needle"),
        "newly opened tab must inherit the EditorTabs' current search term"
    );
}

#[test]
fn editor_tabs_search_highlight_survives_open_pinned_after_close() {
    // Same scenario as above but via open_pinned (double-click /
    // Ctrl+Enter). The new editor must also inherit the term.
    let f1 = NamedTempFile::new().unwrap();
    let f2 = NamedTempFile::new().unwrap();
    std::fs::write(f1.path(), "first\n").unwrap();
    std::fs::write(f2.path(), "second needle line\n").unwrap();
    let mut t = EditorTabs::new();
    t.open_pinned(f1.path()).unwrap();
    t.open_pinned(f2.path()).unwrap();
    t.set_search_highlight(
        Some(String::from("needle")),
        crate::widgets::search::SearchOpts::default(),
    );
    // Close f1; both should still be highlighted, and now a fresh open
    // of a third file should inherit too.
    let f3 = NamedTempFile::new().unwrap();
    std::fs::write(f3.path(), "third needle row\n").unwrap();
    let close_idx = t
        .editors
        .iter()
        .position(|e| e.path.as_deref() == Some(f1.path()))
        .unwrap();
    assert!(t.close_tab(close_idx));
    t.open_pinned(f3.path()).unwrap();
    let third = t
        .editors
        .iter()
        .find(|e| e.path.as_deref() == Some(f3.path()))
        .unwrap();
    assert_eq!(third.search_highlight.as_deref(), Some("needle"));
}

#[test]
fn editor_tabs_set_search_highlight_propagates_to_every_tab() {
    let mut t = EditorTabs::new();
    let f1 = NamedTempFile::new().unwrap();
    let f2 = NamedTempFile::new().unwrap();
    std::fs::write(f1.path(), "hello").unwrap();
    std::fs::write(f2.path(), "world").unwrap();
    t.open_pinned(f1.path()).unwrap();
    t.open_pinned(f2.path()).unwrap();
    t.set_search_highlight(
        Some(String::from("term")),
        crate::widgets::search::SearchOpts::default(),
    );
    for ed in &t.editors {
        assert_eq!(ed.search_highlight.as_deref(), Some("term"));
    }
    t.set_search_highlight(None, crate::widgets::search::SearchOpts::default());
    for ed in &t.editors {
        assert!(ed.search_highlight.is_none());
    }
}

#[test]
fn editor_starts_with_zero_horizontal_scroll() {
    let e = editor_with("abcdef");
    assert_eq!(e.scroll_col, 0);
}

#[test]
fn scroll_right_by_advances_horizontal_offset() {
    let mut e = editor_with("a".repeat(2000).as_str());
    e.scroll_right_by(40);
    assert_eq!(e.scroll_col, 40);
    e.scroll_right_by(60);
    assert_eq!(e.scroll_col, 100);
    e.scroll_left_by(30);
    assert_eq!(e.scroll_col, 70);
    e.scroll_left_by(1000); // saturating
    assert_eq!(e.scroll_col, 0);
}

#[test]
fn move_right_past_visible_edge_advances_horizontal_scroll() {
    // User-reported bug: long-line files (HTML blob with base64 inline)
    // had no horizontal scroll, so moving the cursor right past the
    // viewport edge just clamped the visible cursor at the right
    // border without actually advancing scroll_col.
    let mut e = editor_with("a".repeat(500).as_str());
    e.focused = true;
    // Render once so last_inner / last_gutter_width are populated.
    let area = Rect {
        x: 0,
        y: 0,
        width: 30,
        height: 5,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut e).render(area, &mut buf);
    let text_width = e
        .last_inner
        .width
        .saturating_sub(e.last_gutter_width + 2 + u16::from(e.last_scrollbar.width > 0))
        as usize;
    assert!(text_width > 0 && text_width < 500);
    // Move cursor to the very first off-screen column.
    e.cursor_col = text_width;
    e.move_right();
    assert!(
        e.scroll_col > 0,
        "moving the cursor past the visible edge must advance horizontal scroll"
    );
}

#[test]
fn render_starts_line_from_scroll_col() {
    // With scroll_col = 3, a long line should display starting from its
    // 4th character ('D') at the text origin, not from 'A'. The line must
    // overflow the viewport, otherwise render clamps scroll_col to 0 (a
    // line that fits has nothing to scroll past).
    let mut e = editor_with("ABCDEFGHIJ".repeat(10).as_str());
    e.scroll_col = 3;
    e.focused = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 30,
        height: 5,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut e).render(area, &mut buf);
    let text_x = e.last_inner.x + e.last_gutter_width + 1;
    assert_eq!(
        buf[(text_x, e.last_inner.y)].symbol(),
        "D",
        "first visible column must be the (scroll_col)th character"
    );
}

#[test]
fn render_clamps_scroll_col_to_content_width() {
    // A trackpad swipe can push scroll_col arbitrarily far right via
    // scroll_right_by (no clamp of its own). Render must pull it back so
    // the buffer can never be stranded entirely off-screen.
    let mut e = editor_with("a".repeat(40).as_str());
    e.focused = true;
    e.scroll_right_by(10_000);
    let area = Rect {
        x: 0,
        y: 0,
        width: 30,
        height: 5,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut e).render(area, &mut buf);
    let text_width = e
        .last_inner
        .width
        .saturating_sub(e.last_gutter_width + 2 + u16::from(e.last_scrollbar.width > 0))
        as usize;
    assert!(text_width > 0);
    assert_eq!(
        e.scroll_col,
        40usize.saturating_sub(text_width),
        "scroll_col must clamp to (content cols - text width)"
    );
}

#[test]
fn render_shows_horizontal_scrollbar_only_on_overflow() {
    let area = Rect {
        x: 0,
        y: 0,
        width: 30,
        height: 6,
    };

    // Short line: no horizontal overflow, no bar.
    let mut short = editor_with("abc");
    short.focused = true;
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut short).render(area, &mut buf);
    assert_eq!(
        short.last_hscrollbar,
        Rect::default(),
        "a line that fits must not reserve a horizontal scrollbar row"
    );

    // Long line: overflow, bar painted on the bottom inner row.
    let mut long = editor_with("a".repeat(500).as_str());
    long.focused = true;
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut long).render(area, &mut buf);
    assert!(
        long.last_hscrollbar.width > 0 && long.last_hscrollbar.height == 1,
        "an overflowing line must paint a one-row horizontal scrollbar"
    );
    assert_eq!(
        long.last_hscrollbar.y,
        long.last_inner.y + long.last_inner.height - 1,
        "the horizontal scrollbar sits on the bottom inner row"
    );
}

/// #1563: the edge arrows' line-end and line-start landings scroll the
/// view sideways to keep the caret on screen in a narrow pane.
#[test]
fn edge_arrows_keep_the_caret_horizontally_visible() {
    let mut e = editor_with(&format!("short\n{}", "b".repeat(200)));
    e.focused = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 30,
        height: 6,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut e).render(area, &mut buf);
    e.cursor_row = 1;
    e.cursor_col = 0;
    e.arrow_down();
    assert_eq!(e.cursor_col, 200);
    assert!(
        e.scroll_col > 0,
        "the view follows the caret to the line end"
    );
    let width = e.visible_text_width();
    assert!(
        e.cursor_col < e.scroll_col + width,
        "the caret is on screen"
    );
    e.cursor_row = 0;
    e.cursor_col = 3;
    e.scroll_col = 100;
    e.arrow_up();
    assert_eq!((e.cursor_row, e.cursor_col), (0, 0));
    assert_eq!(e.scroll_col, 0, "the view comes back to the line start");
}

#[test]
fn scroll_to_bar_x_jumps_horizontal_offset() {
    let mut e = editor_with("a".repeat(500).as_str());
    e.focused = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 30,
        height: 6,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut e).render(area, &mut buf);
    assert!(e.last_hscrollbar.width > 0);
    // Click the far-right end of the track → scroll to (near) max.
    let right_x = e.last_hscrollbar.x + e.last_hscrollbar.width - 1;
    assert!(e.scroll_to_bar_x(right_x));
    assert!(
        e.scroll_col > 0,
        "dragging the thumb to the right edge must advance scroll_col"
    );
    // Click the far-left end → scroll back to 0.
    assert!(e.scroll_to_bar_x(e.last_hscrollbar.x));
    assert_eq!(e.scroll_col, 0);
}

fn md_editor(text: &str) -> Editor {
    let mut e = editor_with(text);
    e.lang = Some(LangKind::Markdown);
    e
}

fn render_at(e: &mut Editor, w: u16, h: u16) {
    e.focused = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: w,
        height: h,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (e as &mut Editor).render(area, &mut buf);
}

/// On hosts that ignore iTerm2's `SetColors` (Ghostty, Kitty, sixel) the
/// image/CSV/diff canvases must paint the theme background explicitly:
/// their `Reset` fill overrides the non-iTerm2 frame prefill (ratatui
/// applies any `Some` bg), so with the chrome themed they were the only
/// host-black islands left in the frame.
#[test]
fn non_iterm2_canvases_paint_the_theme_background_not_reset() {
    let themed = crate::theme::Theme::from_id("solarized-dark");
    assert_eq!(themed.id(), "solarized-dark", "bundled theme resolves");
    assert_eq!(
        canvas_bg(false, themed),
        themed.editor_bg(),
        "a non-iTerm2 host fills the canvas with the theme bg"
    );
    assert_eq!(
        canvas_bg(true, themed),
        Color::Reset,
        "iTerm2 keeps Reset so the SetColors session bg shows through"
    );
    // And the fill actually lands on every cell of the canvas.
    let mut e = editor_with("");
    e.theme = themed;
    e.image = Some(ImageView {
        bytes: Vec::new(),
        format_label: String::from("png"),
        pixel_w: 4,
        pixel_h: 4,
        natural_size: None,
        byte_size: 1,
        generation: 0,
        pdf: None,
    });
    let area = Rect {
        x: 0,
        y: 0,
        width: 20,
        height: 6,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    render_image_placeholder(
        e.image.as_ref().unwrap(),
        None,
        area,
        &mut buf,
        canvas_bg(false, themed),
        crate::theme::Theme::default(),
    );
    assert_eq!(
        buf[(1, area.height - 1)].bg,
        themed.editor_bg(),
        "the canvas body must wear the theme bg, not the host default"
    );
}

/// The editor picture sits at `KITTY_Z_BELOW_TEXT_AND_BG`, and Kitty
/// draws any cell with a non-default background OVER an image that deep.
/// Painting the canvas with the explicit theme bg (the CSV/diff island
/// fix) therefore hid the preview behind its own canvas on Ghostty/Kitty
/// — transmitted every frame, never visible. The image canvas must keep
/// the DEFAULT background on the protocols that layer the picture
/// against the cells (Kitty) or blit it over them post-frame (iTerm2);
/// the themed look survives because the bake letterboxes with the theme
/// bg pixel. Sixel and no-graphics hosts keep the themed fill.
#[test]
fn the_kitty_image_canvas_keeps_the_default_bg_so_the_deep_z_preview_shows() {
    use crate::iterm2_inline::InlineImageProtocol;
    let themed = crate::theme::Theme::from_id("solarized-dark");
    assert_eq!(themed.id(), "solarized-dark", "bundled theme resolves");
    assert_eq!(
        image_canvas_bg(InlineImageProtocol::Kitty, themed),
        Color::Reset,
        "Kitty layers the deep-z image below non-default-bg cells, so the picture canvas must stay default-bg"
    );
    assert_eq!(
        image_canvas_bg(InlineImageProtocol::ITerm2, themed),
        Color::Reset,
        "iTerm2 keeps Reset so the SetColors session bg shows through"
    );
    assert_eq!(
        image_canvas_bg(InlineImageProtocol::Sixel, themed),
        themed.editor_bg(),
        "sixel blits into the cell buffer over the canvas, so the themed fill stays"
    );
    assert_eq!(
        image_canvas_bg(InlineImageProtocol::None, themed),
        themed.editor_bg(),
        "with no graphics the placeholder is all the user sees; it stays themed"
    );
    // And the Kitty choice actually lands on every canvas cell.
    let image = ImageView {
        bytes: Vec::new(),
        format_label: String::from("png"),
        pixel_w: 4,
        pixel_h: 4,
        natural_size: None,
        byte_size: 1,
        generation: 0,
        pdf: None,
    };
    let area = Rect {
        x: 0,
        y: 0,
        width: 20,
        height: 6,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    render_image_placeholder(
        &image,
        None,
        area,
        &mut buf,
        image_canvas_bg(InlineImageProtocol::Kitty, themed),
        crate::theme::Theme::default(),
    );
    // The metadata pill on the first row keeps its explicit bg by design:
    // the overlay anchors the picture one row BELOW it
    // (`update_editor_image_overlay` carves off the header row), so the
    // pill never overlaps the image. Every cell in the picture band under
    // it must stay default-bg.
    assert_ne!(
        buf[(1, area.y)].bg,
        Color::Reset,
        "the metadata pill row keeps its own background"
    );
    for y in area.y + 1..area.bottom() {
        for x in area.x..area.right() {
            assert_eq!(
                buf[(x, y)].bg,
                Color::Reset,
                "canvas cell ({x}, {y}) must keep the default background or Kitty paints it over the image"
            );
        }
    }
}

/// Cmd+K Cmd+T while a preview is open: the preview bakes its colors at
/// build time and was rebuilt only when the buffer edited, so it kept the
/// previous theme's colors (chrome and code alike) until the user touched
/// the file or toggled the preview off and on.
#[test]
fn a_theme_switch_rebuilds_an_open_markdown_preview() {
    let mut e = md_editor("```notalanguage\nplain code line\n```");
    assert!(e.toggle_markdown_preview());
    let themed = *crate::theme::Theme::all()
        .iter()
        .find(|t| t.syntax().fg != crate::theme::SyntaxPalette::BASE16.fg)
        .expect("a bundled theme with a non-Base16 code palette");
    e.theme = themed;
    e.rehighlight_for_theme();
    let (r, g, b) = themed.syntax().fg;
    let md = e.markdown_preview.as_ref().expect("preview stays open");
    let code = md
        .lines
        .iter()
        .flat_map(|l| l.spans.iter())
        .find(|s| s.content.as_ref() == "plain code line")
        .expect("the code line must render");
    assert_eq!(
        code.style.fg,
        Some(Color::Rgb(r, g, b)),
        "the preview must recolor on a theme switch, not wait for an edit"
    );
}

#[test]
fn markdown_wraps_long_line_without_horizontal_scrollbar() {
    let mut e = md_editor(&"word ".repeat(40)); // 200 chars, one logical line
    render_at(&mut e, 30, 10);
    assert_eq!(
        e.last_hscrollbar,
        Rect::default(),
        "wrapped markdown must not show a horizontal scrollbar"
    );
    assert_eq!(e.scroll_col, 0, "wrap mode never scrolls horizontally");
    let rows_for_line0 = e
        .last_wrap_rows
        .iter()
        .filter(|r| matches!(r, VisRow::Text { line: 0, .. }))
        .count();
    assert!(
        rows_for_line0 > 1,
        "a 200-char paragraph must fold onto multiple visual rows"
    );
}

#[test]
fn non_markdown_long_line_still_scrolls_horizontally() {
    let mut e = editor_with(&"a".repeat(200));
    e.lang = Some(LangKind::Rust);
    render_at(&mut e, 30, 10);
    assert!(
        e.last_hscrollbar.width > 0,
        "code keeps the horizontal scrollbar instead of wrapping"
    );
}

#[test]
fn markdown_cursor_screen_pos_lands_on_the_wrapped_row() {
    let mut e = md_editor(&"a".repeat(100));
    render_at(&mut e, 30, 10);
    let (_, seg0_start, seg0_end) = e.text_row(0).unwrap();
    assert_eq!(seg0_start, 0);
    // A column just past the first visual row's content sits on row 2.
    e.cursor_row = 0;
    e.cursor_col = seg0_end + 1;
    let (_, cy) = e.cursor_screen_pos().expect("cursor must be visible");
    assert_eq!(
        cy,
        e.last_inner.y + 1,
        "a column past the first segment renders on the second visual row"
    );
}

#[test]
fn markdown_click_on_second_visual_row_maps_into_the_line() {
    let mut e = md_editor(&"a".repeat(100));
    render_at(&mut e, 30, 10);
    let seg0_end = e.text_row(0).unwrap().2;
    let text_x = e.last_inner.x + e.last_gutter_width + 1;
    let (line, col) = e
        .buffer_pos_at(text_x, e.last_inner.y + 1)
        .expect("a click on the second visual row is a text hit");
    assert_eq!(line, 0, "still the same logical line");
    assert_eq!(
        col, seg0_end,
        "the first column of the second visual row is where segment one ended"
    );
}

#[test]
fn markdown_mouse_down_below_wrapped_lines_lands_on_the_row_under_the_pointer() {
    // Line 0 wraps onto several visual rows; pressing on the row that
    // SHOWS line 1 must anchor the selection on line 1, not on the
    // linear `scroll + visual_row` (which counts every wrapped
    // continuation row as its own line and drifts downward).
    let mut e = md_editor(&format!("{}\nsecond\nthird\nfourth", "a".repeat(100)));
    render_at(&mut e, 30, 10);
    let vis_row = e
        .last_wrap_rows
        .iter()
        .position(|r| matches!(r, VisRow::Text { line: 1, .. }))
        .expect("line 1 is on screen");
    assert!(vis_row > 1, "line 0 must wrap for the rows to be displaced");
    let text_x = e.last_inner.x + e.last_gutter_width + 1;
    e.mouse_down(text_x, e.last_inner.y + vis_row as u16);
    assert_eq!(
        e.cursor_row, 1,
        "the press lands on the line under the pointer"
    );
    assert_eq!(e.cursor_col, 0);
}

#[test]
fn markdown_move_down_steps_within_a_wrapped_paragraph() {
    let mut e = md_editor(&"a".repeat(100));
    render_at(&mut e, 30, 10);
    e.cursor_row = 0;
    e.cursor_col = 0;
    e.move_down();
    assert_eq!(
        e.cursor_row, 0,
        "move_down stays on the same logical line - it steps to the next wrapped segment"
    );
    assert!(e.cursor_col > 0, "and advances into that next segment");
}

#[test]
fn markdown_scrolls_to_reveal_tail_of_a_tall_paragraph() {
    // One logical line taller than the viewport must still be scrollable
    // via sub-line (segment) scroll.
    let mut e = md_editor(&"a".repeat(400));
    render_at(&mut e, 30, 6);
    let before = e.text_row(0).unwrap();
    e.scroll_down(3);
    render_at(&mut e, 30, 6);
    let after = e.text_row(0).unwrap();
    assert_eq!(after.0, 0, "still the single logical line");
    assert!(
        after.1 > before.1,
        "scrolling down advances into the paragraph (sub-line scroll)"
    );
}

#[test]
fn render_paints_selection_band_on_selected_cells() {
    use ratatui::buffer::Buffer;
    let mut e = editor_with("hello world");
    e.cursor_col = 0;
    e.start_selection_at_cursor();
    e.cursor_col = 5;
    e.extend_selection_to_cursor();
    e.focused = true;

    let area = Rect {
        x: 0,
        y: 0,
        width: 30,
        height: 5,
    };
    let mut buf = Buffer::empty(area);
    (&mut e).render(area, &mut buf);

    // gutter for 1 line: "1 ".len() = 2 → text_x = 0+1+2+1 = 4
    let text_x = e.last_inner.x + e.last_gutter_width + 1;
    let selected_bg = e.theme.ui(Color::Rgb(0x26, 0x4f, 0x78));
    // chars 0..5 should be highlighted
    for col in 0..5u16 {
        let bg = buf[(text_x + col, e.last_inner.y)].bg;
        assert_eq!(
            bg, selected_bg,
            "cell at col {col} should have selection bg, got {bg:?}"
        );
    }
    // char 5 (the space) is OUTSIDE the selection (head exclusive end)
    let bg5 = buf[(text_x + 5, e.last_inner.y)].bg;
    assert_ne!(bg5, selected_bg, "cell at col 5 should NOT be highlighted");
}

#[test]
fn render_paints_selection_band_across_multiple_lines() {
    use ratatui::buffer::Buffer;
    let mut e = editor_with("first\nsecond\nthird");
    e.cursor_row = 0;
    e.cursor_col = 2;
    e.start_selection_at_cursor();
    e.cursor_row = 2;
    e.cursor_col = 2;
    e.extend_selection_to_cursor();
    e.focused = true;

    let area = Rect {
        x: 0,
        y: 0,
        width: 30,
        height: 6,
    };
    let mut buf = Buffer::empty(area);
    (&mut e).render(area, &mut buf);

    let text_x = e.last_inner.x + e.last_gutter_width + 1;
    let selected_bg = e.theme.ui(Color::Rgb(0x26, 0x4f, 0x78));

    // Row 0: cols 2..end (all the way past the end of "first")
    let row0_y = e.last_inner.y;
    assert_eq!(buf[(text_x + 2, row0_y)].bg, selected_bg, "row 0 col 2");
    assert_eq!(buf[(text_x + 4, row0_y)].bg, selected_bg, "row 0 col 4");
    assert_ne!(
        buf[(text_x, row0_y)].bg,
        selected_bg,
        "row 0 col 0 not selected"
    );

    // Row 1 (full line "second"): all cells in selection
    let row1_y = e.last_inner.y + 1;
    assert_eq!(buf[(text_x, row1_y)].bg, selected_bg, "row 1 col 0");
    assert_eq!(buf[(text_x + 5, row1_y)].bg, selected_bg, "row 1 col 5");

    // Row 2 (last line "third"): cols 0..2 in selection
    let row2_y = e.last_inner.y + 2;
    assert_eq!(buf[(text_x, row2_y)].bg, selected_bg, "row 2 col 0");
    assert_eq!(buf[(text_x + 1, row2_y)].bg, selected_bg, "row 2 col 1");
    assert_ne!(
        buf[(text_x + 2, row2_y)].bg,
        selected_bg,
        "row 2 col 2 not selected"
    );
}

#[test]
fn double_click_selects_word_and_moves_cursor_to_end() {
    let mut e = editor_with("hello world");
    e.last_inner = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 25,
    };
    e.last_gutter_width = 2;
    let text_x: u16 = 3;
    e.select_word_at(text_x + 7, 0);
    let sel = e.selection.expect("word selection created");
    assert_eq!(sel.normalised(), ((0, 6), (0, 11)));
    assert_eq!(e.cursor_col, 11);
}

#[test]
fn double_click_on_first_word_selects_it_from_column_zero() {
    let mut e = editor_with("foo bar");
    e.last_inner = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 25,
    };
    e.last_gutter_width = 2;
    let text_x: u16 = 3;
    e.select_word_at(text_x + 1, 0);
    let sel = e.selection.unwrap();
    assert_eq!(sel.normalised(), ((0, 0), (0, 3)));
    assert_eq!(e.cursor_col, 3);
}

#[test]
fn double_click_on_whitespace_does_not_create_a_selection() {
    let mut e = editor_with("hello world");
    e.last_inner = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 25,
    };
    e.last_gutter_width = 2;
    let text_x: u16 = 3;
    e.select_word_at(text_x + 5, 0);
    assert!(
        e.selection.map(|s| !s.has_area()).unwrap_or(true),
        "whitespace double-click must not start a non-empty selection"
    );
}

#[test]
fn double_click_past_end_of_line_extends_last_word() {
    let mut e = editor_with("foo");
    e.last_inner = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 25,
    };
    e.last_gutter_width = 2;
    let text_x: u16 = 3;
    e.select_word_at(text_x + 12, 0);
    let sel = e.selection.unwrap();
    assert_eq!(sel.normalised(), ((0, 0), (0, 3)));
    assert_eq!(e.cursor_col, 3);
}

#[test]
fn editor_tabs_starts_with_one_empty_editor() {
    let t = EditorTabs::new();
    assert_eq!(t.tab_count(), 1);
    assert_eq!(t.active_index(), 0);
    assert!(t.path.is_none());
}

#[test]
fn editor_tabs_open_in_new_tab_appends_and_activates() {
    let mut t = EditorTabs::new();
    t.editors[0].path = Some(std::path::PathBuf::from("/tmp/a.rs"));
    t.add_tab_with_path(std::path::PathBuf::from("/tmp/b.rs"));
    assert_eq!(t.tab_count(), 2);
    assert_eq!(t.active_index(), 1);
    assert_eq!(t.path.as_deref(), Some(std::path::Path::new("/tmp/b.rs")));
}

#[test]
fn editor_tabs_new_tab_lands_immediately_after_active() {
    let mut t = EditorTabs::new();
    t.editors[0].path = Some(std::path::PathBuf::from("/a"));
    t.add_tab_with_path(std::path::PathBuf::from("/b"));
    t.add_tab_with_path(std::path::PathBuf::from("/c"));
    // active is now 2 (c). Switch back to a (idx 0), open d → should be at idx 1.
    t.select(0);
    t.add_tab_with_path(std::path::PathBuf::from("/d"));
    let labels: Vec<_> = t
        .iter_tabs()
        .map(|e| e.path.as_ref().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(labels, vec!["/a", "/d", "/b", "/c"]);
    assert_eq!(t.active_index(), 1);
}

#[test]
fn editor_tabs_close_active_drops_tab_and_reselects() {
    let mut t = EditorTabs::new();
    t.editors[0].path = Some(std::path::PathBuf::from("/a"));
    t.add_tab_with_path(std::path::PathBuf::from("/b"));
    t.add_tab_with_path(std::path::PathBuf::from("/c"));
    t.select(1); // active = b
    assert!(t.close_active());
    assert_eq!(t.tab_count(), 2);
    // After closing the middle tab, the next tab takes its slot, which is c.
    assert_eq!(t.path.as_deref(), Some(std::path::Path::new("/c")));
}

#[test]
fn editor_tabs_close_last_tab_resets_buffer_to_blank() {
    let mut t = EditorTabs::new();
    t.editors[0].path = Some(std::path::PathBuf::from("/a"));
    assert!(
        t.close_active(),
        "closing the only tab resets it instead of refusing"
    );
    assert_eq!(t.tab_count(), 1);
    assert!(t.path.is_none());
}

#[test]
fn close_at_maps_a_click_on_the_x_glyph_to_its_tab_index() {
    use ratatui::buffer::Buffer;
    let mut t = EditorTabs::new();
    t.editors[0].path = Some(std::path::PathBuf::from("/foo.rs"));
    t.add_tab_with_path(std::path::PathBuf::from("/bar.rs"));
    let area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 10,
    };
    let mut buf = Buffer::empty(area);
    (&mut t).render(area, &mut buf);
    let cx0 = t
        .close_screen_x(0)
        .expect("tab 0 has a close button when count > 1");
    let cx1 = t.close_screen_x(1).expect("tab 1 has a close button");
    assert_eq!(t.close_at(cx0, area.y), Some(0));
    assert_eq!(t.close_at(cx1, area.y), Some(1));
    // A click on a non-close cell of the tab still routes to `tab_at`,
    // not `close_at`.
    let (tab0_x, _) = t.tab_screen_x(0).unwrap();
    assert_ne!(tab0_x, cx0);
    assert_eq!(t.close_at(tab0_x, area.y), None);
    // Clicks outside the strip row are not close clicks.
    assert_eq!(t.close_at(cx0, area.y + 2), None);
}

#[test]
fn hovering_the_close_cross_paints_a_pill_on_that_cell() {
    use ratatui::buffer::Buffer;
    let mut t = EditorTabs::new();
    t.editors[0].path = Some(std::path::PathBuf::from("/foo.rs"));
    t.add_tab_with_path(std::path::PathBuf::from("/bar.rs"));
    let area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 10,
    };
    // First render to learn where the close crosses landed.
    let mut buf = Buffer::empty(area);
    (&mut t).render(area, &mut buf);
    let cx = t.close_screen_x(0).expect("two tabs -> tab 0 has a cross");
    let other = t.close_screen_x(1).expect("tab 1 has a cross");
    // Hover the first tab's cross and re-render.
    t.hover_pointer = Some((cx, area.y));
    let mut buf = Buffer::empty(area);
    (&mut t).render(area, &mut buf);
    let theme = crate::theme::Theme::default();
    let pill = theme.tab_close_pill_bg();
    assert_eq!(
        buf[(cx, area.y)].bg,
        pill,
        "the hovered cross cell wears the pill"
    );
    // Croft Loch inks the glyph on its neutral pill in the strong grey,
    // not bold white.
    let t = crate::widgets::surface::tones(theme).expect("Loch is the default");
    assert_eq!(buf[(cx, area.y)].fg, t.strong());
    assert!(!buf[(cx, area.y)].modifier.contains(Modifier::BOLD));
    assert_eq!(
        buf[(cx, area.y)].symbol(),
        "\u{2715}",
        "the pill sits behind the cross glyph, not a blank cell"
    );
    assert_ne!(
        buf[(other, area.y)].bg,
        pill,
        "an un-hovered cross gets no pill"
    );
}

/// #1548: on High Contrast Light the tab labels are black, not the
/// dark themes' white and grey, so the active tab reads on its pale
/// fill; High Contrast keeps white on its dark blue.
#[test]
fn high_contrast_tab_labels_use_the_themes_foreground() {
    use ratatui::buffer::Buffer;
    for (id, want) in [
        ("hc-light", Color::Rgb(0, 0, 0)),
        ("hc-black", Color::Rgb(0xff, 0xff, 0xff)),
    ] {
        let theme = crate::theme::Theme::from_id(id);
        let mut t = EditorTabs::new();
        t.editors[0].path = Some(std::path::PathBuf::from("/foo.rs"));
        t.add_tab_with_path(std::path::PathBuf::from("/bar.rs"));
        for ed in t.editors.iter_mut() {
            ed.theme = theme;
        }
        let area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 10,
        };
        let mut buf = Buffer::empty(area);
        (&mut t).render(area, &mut buf);
        let label_fg = |name: &str| buf[(cell_of(&buf, area.width, name), 0)].fg;
        assert_eq!(label_fg("bar.rs"), want, "{id}: the active tab's label");
        assert_eq!(label_fg("foo.rs"), want, "{id}: an inactive tab's label");
    }
    // Negative: the ordinary light theme keeps its own tab colours.
    let mut t = EditorTabs::new();
    t.editors[0].path = Some(std::path::PathBuf::from("/foo.rs"));
    t.editors[0].theme = crate::theme::Theme::from_id("light");
    let area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 10,
    };
    let mut buf = Buffer::empty(area);
    (&mut t).render(area, &mut buf);
    assert_eq!(
        buf[(cell_of(&buf, area.width, "foo.rs"), 0)].fg,
        TAB_ACTIVE_FG
    );
}

/// The column where `text` starts on row 0, cell by cell (a tab's icon
/// may be several bytes wide in one cell).
fn cell_of(buf: &ratatui::buffer::Buffer, width: u16, text: &str) -> u16 {
    let cells: Vec<String> = (0..width)
        .map(|x| buf[(x, 0)].symbol().to_string())
        .collect();
    (0..cells.len())
        .find(|&i| cells[i..].concat().starts_with(text))
        .unwrap_or_else(|| panic!("{text} is on the tab row: {cells:?}")) as u16
}

/// #1548: the close glyph on High Contrast's orange hover pill is
/// black (white reads at about 2.6:1 there); High Contrast Light's dark
/// blue pill and the ordinary themes keep white.
#[test]
fn high_contrast_close_pill_glyph_contrasts_with_the_pill() {
    use ratatui::buffer::Buffer;
    for (id, want) in [
        ("hc-black", Color::Black),
        ("hc-light", Color::White),
        ("dark-blue", Color::White),
    ] {
        let theme = crate::theme::Theme::from_id(id);
        let mut t = EditorTabs::new();
        t.editors[0].path = Some(std::path::PathBuf::from("/foo.rs"));
        t.editors[0].theme = theme;
        let area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 10,
        };
        let mut buf = Buffer::empty(area);
        (&mut t).render(area, &mut buf);
        let cx = t.close_screen_x(t.active).expect("a close cross");
        t.hover_pointer = Some((cx, area.y));
        let mut buf = Buffer::empty(area);
        (&mut t).render(area, &mut buf);
        let cell = &buf[(cx, area.y)];
        assert_eq!(cell.bg, theme.tab_close_pill_bg(), "{id}: the pill shows");
        assert_eq!(cell.fg, want, "{id}: the glyph's ink");
    }
}

#[test]
fn black_theme_close_pill_is_distinct_from_the_active_tab_bg() {
    use ratatui::buffer::Buffer;
    let mut t = EditorTabs::new();
    t.editors[0].path = Some(std::path::PathBuf::from("/foo.rs"));
    t.editors[0].theme = crate::theme::Theme::BLACK;
    t.editors[0].focus_gradient = true; // turn on the Black theme
    t.add_tab_with_path(std::path::PathBuf::from("/bar.rs"));
    let area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 10,
    };
    let mut buf = Buffer::empty(area);
    (&mut t).render(area, &mut buf);
    let active = t.active;
    let cx = t
        .close_screen_x(active)
        .expect("the active tab has a close cross");
    t.hover_pointer = Some((cx, area.y));
    let mut buf = Buffer::empty(area);
    (&mut t).render(area, &mut buf);
    let black = crate::theme::Theme::from_id("black");
    assert_eq!(
        black.tab_active_bg(),
        crate::gradient::rgb_color(crate::gradient::POPUP_SEL_BG),
        "the Black theme's active chip is still the brand selection teal"
    );
    assert_eq!(
        buf[(cx, area.y)].bg,
        black.tab_close_pill_bg(),
        "the Black-theme close pill uses the brighter teal"
    );
    assert_ne!(
        buf[(cx, area.y)].bg,
        black.tab_active_bg(),
        "the pill must not equal the active tab bg, or the cross shows no hover at all"
    );
}

#[test]
fn hovering_an_inactive_tab_lifts_its_body_not_the_active_one() {
    use ratatui::buffer::Buffer;
    let mut t = EditorTabs::new();
    t.editors[0].path = Some(std::path::PathBuf::from("/foo.rs"));
    // Adding a tab makes the new one (index 1) active; index 0 is inactive.
    t.add_tab_with_path(std::path::PathBuf::from("/bar.rs"));
    let area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 10,
    };
    let mut buf = Buffer::empty(area);
    (&mut t).render(area, &mut buf);
    let (x0, _) = t.tab_screen_x(0).unwrap();
    let (x1, _) = t.tab_screen_x(1).unwrap();
    // Hover the inactive tab's label area (one cell past its left pad).
    t.hover_pointer = Some((x0 + 1, area.y));
    let mut buf = Buffer::empty(area);
    (&mut t).render(area, &mut buf);
    let hover = crate::theme::Theme::default().tab_hover_bg();
    assert_eq!(
        buf[(x0 + 1, area.y)].bg,
        hover,
        "an inactive tab body lifts under the pointer"
    );
    assert_ne!(
        buf[(x1 + 1, area.y)].bg,
        hover,
        "the active tab keeps its own bg when a sibling is hovered"
    );
}

/// Two tabs on Croft Loch, `foo.rs` inactive and `bar.rs` active, the
/// group focused or not, rendered on an 80x10 area.
fn loch_tab_pair(focused: bool) -> (EditorTabs, Buffer) {
    let mut t = EditorTabs::new();
    t.editors[0].path = Some(PathBuf::from("/foo.rs"));
    t.add_tab_with_path(PathBuf::from("/bar.rs"));
    t.editors[1].focused = focused;
    let mut buf = Buffer::empty(Rect::new(0, 0, 80, 10));
    let area = buf.area;
    (&mut t).render(area, &mut buf);
    (t, buf)
}

/// Croft Loch lays a tab out as ` {icon} {label} {close} `: the
/// Explorer's file icon leads, an inactive label is muted with a faint
/// cross, and the active tab of the focused group is the strong ink,
/// never bold, with the accent only as an underline under its label.
#[test]
fn a_loch_tab_leads_with_its_icon_and_marks_the_focused_active_tab() {
    let (t, buf) = loch_tab_pair(true);
    let s = crate::widgets::surface::tones(crate::theme::Theme::default()).unwrap();
    let (x0, w0) = t.tab_screen_x(0).unwrap();
    let (x1, _) = t.tab_screen_x(1).unwrap();
    assert_eq!(w0, 6 + 6, "pad, icon, gap, label, gap, close, pad");
    let icon = crate::icons::for_path("foo.rs", ".rs");
    assert_eq!(buf[(x0 + 1, 0)].symbol(), icon.glyph.to_string());
    let (inactive, active) = (&buf[(x0 + 3, 0)], &buf[(x1 + 3, 0)]);
    assert_eq!((inactive.symbol(), active.symbol()), ("f", "b"));
    assert_eq!(inactive.fg, s.muted());
    assert_eq!(active.fg, s.strong());
    assert!(!active.modifier.contains(Modifier::BOLD), "no bold tabs");
    assert!(active.modifier.contains(Modifier::UNDERLINED));
    assert_eq!(active.underline_color, s.accent());
    assert!(!inactive.modifier.contains(Modifier::UNDERLINED));
    assert!(
        !buf[(x1 + 1, 0)].modifier.contains(Modifier::UNDERLINED),
        "the underline sits under the label, not the icon"
    );
    let (c0, c1) = (t.close_screen_x(0).unwrap(), t.close_screen_x(1).unwrap());
    assert_eq!(buf[(c0, 0)].symbol(), "\u{2715}");
    assert_eq!(buf[(c0, 0)].fg, s.faint(), "an inactive cross recedes");
    assert_eq!(buf[(c1, 0)].fg, s.muted());
}

/// The accent means "has the keyboard": an unfocused group's active
/// tab is body text without the underline, and a modal over a focused
/// group takes the underline but leaves the tab its strong ink.
#[test]
fn a_loch_tab_wears_the_accent_only_while_its_group_has_the_keyboard() {
    let s = crate::widgets::surface::tones(crate::theme::Theme::default()).unwrap();
    let (t, buf) = loch_tab_pair(false);
    let label = &buf[(t.tab_screen_x(1).unwrap().0 + 3, 0)];
    assert_eq!(label.fg, s.text());
    assert!(!label.modifier.contains(Modifier::UNDERLINED));
    let (mut t, _) = loch_tab_pair(true);
    t.editors[1].overlay_has_keyboard = true;
    let mut buf = Buffer::empty(Rect::new(0, 0, 80, 10));
    let area = buf.area;
    (&mut t).render(area, &mut buf);
    let label = &buf[(t.tab_screen_x(1).unwrap().0 + 3, 0)];
    assert_eq!(label.fg, s.strong());
    assert!(!label.modifier.contains(Modifier::UNDERLINED));
}

/// A dirty file's tab keeps its width on Croft Loch: the dot moves from
/// the label into the close slot, turns back into the cross under the
/// pointer, and the label the menus read still carries it.
#[test]
fn a_loch_dirty_tab_shows_its_dot_in_the_close_slot_without_moving() {
    let (mut t, _) = loch_tab_pair(true);
    let clean = t.tab_screen_x(0).unwrap();
    t.editors[0].dirty = true;
    let mut buf = Buffer::empty(Rect::new(0, 0, 80, 10));
    let area = buf.area;
    (&mut t).render(area, &mut buf);
    assert_eq!(t.tab_screen_x(0).unwrap(), clean, "no width jump");
    let c0 = t.close_screen_x(0).unwrap();
    assert_eq!(buf[(c0, 0)].symbol(), "\u{25cf}");
    assert_eq!(
        buf[(clean.0 + 3, 0)].symbol(),
        "f",
        "no dot before the label"
    );
    assert!(t.tab_display_label(0).starts_with("\u{25cf} "));
    t.hover_pointer = Some((clean.0 + 3, 0));
    let mut buf = Buffer::empty(area);
    (&mut t).render(area, &mut buf);
    assert_eq!(buf[(c0, 0)].symbol(), "\u{2715}", "the cross on hover");
    // A boxed theme keeps the dot in front of the label.
    for ed in t.editors.iter_mut() {
        ed.theme = crate::theme::Theme::BLACK;
    }
    t.hover_pointer = None;
    let mut buf = Buffer::empty(area);
    (&mut t).render(area, &mut buf);
    assert_eq!(
        buf[(t.tab_screen_x(0).unwrap().0 + 1, 0)].symbol(),
        "\u{25cf}"
    );
}

/// A pinned tab's close slot holds its pin, so on Croft Loch a dirty
/// pinned tab keeps the dot in front of its label: editing it never
/// makes it look saved.
#[test]
fn a_loch_dirty_pinned_tab_keeps_its_dot_beside_the_pin() {
    let (mut t, _) = loch_tab_pair(true);
    t.editors[0].pinned = true;
    t.editors[0].dirty = true;
    let mut buf = Buffer::empty(Rect::new(0, 0, 80, 10));
    let area = buf.area;
    (&mut t).render(area, &mut buf);
    let (x0, _) = t.tab_screen_x(0).unwrap();
    assert_eq!(
        buf[(t.close_screen_x(0).unwrap(), 0)].symbol(),
        "\u{f08d}",
        "the pin keeps the close slot"
    );
    assert_eq!(
        buf[(x0 + 3, 0)].symbol(),
        "\u{25cf}",
        "the dot leads the label"
    );
}

/// Hover on Croft Loch is a neutral lift: the hovered tab's label rises
/// to body text on the chrome's hover tone, and the close pill is a
/// neutral step above it, never the teal.
#[test]
fn a_loch_hover_is_a_neutral_lift() {
    let theme = crate::theme::Theme::default();
    let s = crate::widgets::surface::tones(theme).unwrap();
    assert_eq!(theme.tab_hover_bg(), s.hover_on(s.chrome));
    assert_ne!(theme.tab_close_pill_bg(), theme.tab_hover_bg());
    assert_ne!(theme.tab_close_pill_bg(), theme.tab_active_bg());
    let (mut t, _) = loch_tab_pair(true);
    let (x0, _) = t.tab_screen_x(0).unwrap();
    t.hover_pointer = Some((x0 + 3, 0));
    let mut buf = Buffer::empty(Rect::new(0, 0, 80, 10));
    let area = buf.area;
    (&mut t).render(area, &mut buf);
    assert_eq!(buf[(x0 + 3, 0)].fg, s.text());
    assert_eq!(buf[(x0 + 1, 0)].bg, theme.tab_hover_bg());
    assert_eq!(
        buf[(t.close_screen_x(0).unwrap(), 0)].fg,
        s.muted(),
        "a hovered tab's cross comes forward"
    );
}

/// Croft Loch's breadcrumbs are in its inks: faint separators, muted
/// crumbs, the last one in body text, and a crumb's icon drawn before
/// its label without joining the label's hit range.
#[test]
fn loch_breadcrumbs_lead_to_the_last_crumb_with_icons_outside_the_hit_range() {
    let s = crate::widgets::surface::tones(crate::theme::Theme::default()).unwrap();
    let mut tabs = EditorTabs::new();
    tabs.breadcrumbs = vec![
        Crumb {
            label: "src".into(),
            target: None,
            icon: None,
        },
        Crumb {
            label: "m.rs".into(),
            target: None,
            icon: Some(('R', Color::Rgb(0xde, 0xa5, 0x84))),
        },
        Crumb {
            label: "build".into(),
            target: Some((5, 2)),
            icon: Some(('F', Color::Rgb(0xb1, 0x80, 0xd7))),
        },
    ];
    let area = Rect::new(0, 0, 60, 10);
    let mut buf = Buffer::empty(area);
    (&mut tabs).render(area, &mut buf);
    let row: String = (0..area.width).map(|x| buf[(x, 1)].symbol()).collect();
    assert!(
        row.starts_with(" src \u{203a} R m.rs \u{203a} F build"),
        "{row:?}"
    );
    let sep = row.find('\u{203a}').unwrap();
    let sep_x = row[..sep].chars().count() as u16;
    assert_eq!(buf[(sep_x, 1)].fg, s.faint());
    assert_eq!(buf[(1, 1)].fg, s.muted(), "a path crumb is muted");
    let (fx, fw, _) = tabs.breadcrumb_ranges[1];
    assert_eq!(buf[(fx, 1)].symbol(), "m");
    assert_eq!(fw, 4, "the hit range is the label alone");
    assert_eq!(buf[(fx - 2, 1)].symbol(), "R");
    let theme = crate::theme::Theme::default();
    assert_eq!(buf[(fx - 2, 1)].fg, theme.ui(Color::Rgb(0xde, 0xa5, 0x84)));
    let (lx, _, target) = tabs.breadcrumb_ranges[2];
    assert_eq!(target, Some((5, 2)));
    assert_eq!(
        buf[(lx, 1)].fg,
        s.text(),
        "the crumb you are in reads first"
    );
    // A boxed theme draws no icons.
    for ed in tabs.editors.iter_mut() {
        ed.theme = crate::theme::Theme::BLACK;
    }
    let mut buf = Buffer::empty(area);
    (&mut tabs).render(area, &mut buf);
    let row: String = (0..area.width).map(|x| buf[(x, 1)].symbol()).collect();
    assert!(
        row.starts_with(" src \u{203a} m.rs \u{203a} build"),
        "{row:?}"
    );
}

/// The seam under Croft Loch's header is always drawn: the plain
/// hairline while the editor is unfocused or a modal quick input has the
/// keyboard, the accent lip while it has the keyboard itself.
#[test]
fn a_loch_editor_always_draws_its_top_seam_and_lights_it_with_the_keyboard() {
    let theme = crate::theme::Theme::default();
    let s = crate::widgets::surface::tones(theme).unwrap();
    let area = Rect::new(0, 0, 30, 6);
    let top = |e: &mut Editor| {
        let mut buf = Buffer::empty(area);
        (&mut *e).render(area, &mut buf);
        (buf[(0, 0)].symbol().to_string(), buf[(0, 0)].fg)
    };
    let mut e = editor_with("a\nb");
    e.focused = false;
    assert_eq!(top(&mut e), ("\u{2594}".to_string(), s.hairline()));
    e.focused = true;
    assert_eq!(top(&mut e), ("\u{2594}".to_string(), s.accent()));
    e.overlay_has_keyboard = true;
    assert_eq!(top(&mut e), ("\u{2594}".to_string(), s.hairline()));
    assert!(e.focused, "the modal leaves focus where it was");
}

/// Croft Loch keeps an expanded fold's chevron out of the gutter until
/// the caret is on its line or the pointer is over the gutter; a
/// collapsed fold's chevron always shows, in the muted ink.
#[test]
fn loch_fold_chevrons_wait_for_the_caret_or_the_pointer() {
    let s = crate::widgets::surface::tones(crate::theme::Theme::default()).unwrap();
    let mut e = editor_with("fn a() {\n    x\n}\nfn b() {\n    y\n}");
    e.cursor_row = 1;
    let area = Rect::new(0, 0, 30, 10);
    let chevrons = |e: &mut Editor| -> Vec<String> {
        let mut buf = Buffer::empty(area);
        (&mut *e).render(area, &mut buf);
        let x = e.last_inner.x + 1;
        (0..6)
            .map(|r| buf[(x, e.last_inner.y + r)].symbol().to_string())
            .collect()
    };
    assert!(e.is_foldable(0) && e.is_foldable(3));
    let quiet = chevrons(&mut e);
    assert_eq!(quiet[0], " ", "no chevron off the caret's line");
    assert_eq!(quiet[3], " ");
    e.cursor_row = 0;
    assert_eq!(chevrons(&mut e)[0], "\u{25be}", "the caret's line shows it");
    assert_eq!(chevrons(&mut e)[3], " ");
    e.cursor_row = 1;
    e.pointer = Some((e.last_inner.x, e.last_inner.y + 4));
    let hovered = chevrons(&mut e);
    assert_eq!(
        (hovered[0].as_str(), hovered[3].as_str()),
        ("\u{25be}", "\u{25be}")
    );
    e.pointer = None;
    e.toggle_fold(3);
    let mut buf = Buffer::empty(area);
    (&mut e).render(area, &mut buf);
    let cell = &buf[(e.last_inner.x + 1, e.last_inner.y + 3)];
    assert_eq!(cell.symbol(), "\u{25b8}", "a collapsed fold always shows");
    assert_eq!(cell.fg, s.muted());
}

/// The ten manifest themes recolor the editor body; the tab strip must
/// follow, not stay the hardcoded VS-Code navy of the two built-ins. A
/// theme without explicit tab colors derives them: the strip wears the
/// theme's secondary panel fill and the active tab its selection fill.
#[test]
fn tab_strip_follows_the_selected_theme() {
    use ratatui::buffer::Buffer;
    let mut t = EditorTabs::new();
    t.editors[0].path = Some(std::path::PathBuf::from("/foo.rs"));
    let solarized = crate::theme::Theme::from_id("solarized-dark");
    assert_eq!(solarized.id(), "solarized-dark", "bundled theme resolves");
    t.editors[0].theme = solarized;
    let area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 10,
    };
    let mut buf = Buffer::empty(area);
    (&mut t).render(area, &mut buf);
    assert_eq!(
        buf[(area.width - 1, 0)].bg,
        solarized.search_bg(),
        "the gap right of the last tab wears the theme's strip color"
    );
    let (x0, _) = t.tab_screen_x(0).unwrap();
    assert_eq!(
        buf[(x0 + 1, 0)].bg,
        solarized.selection(),
        "the active tab wears the theme's selection fill, not the navy chip"
    );
}

#[test]
fn close_tab_removes_specific_index_and_keeps_active_pointing_correctly() {
    let mut t = EditorTabs::new();
    t.editors[0].path = Some(std::path::PathBuf::from("/a"));
    t.add_tab_with_path(std::path::PathBuf::from("/b"));
    t.add_tab_with_path(std::path::PathBuf::from("/c"));
    // active = 2 (c). Closing tab 1 (b) shifts c to index 1; active follows.
    assert!(t.close_tab(1));
    assert_eq!(t.tab_count(), 2);
    assert_eq!(t.active_index(), 1);
    assert_eq!(t.path.as_deref(), Some(std::path::Path::new("/c")));
}

#[test]
fn close_others_keeps_only_the_chosen_tab_and_makes_it_active() {
    let mut t = EditorTabs::new();
    t.editors[0].path = Some(std::path::PathBuf::from("/a"));
    t.add_tab_with_path(std::path::PathBuf::from("/b"));
    t.add_tab_with_path(std::path::PathBuf::from("/c"));
    // Keep "b" (originally index 1). The other two must go.
    let removed = t.close_others(1);
    assert_eq!(removed, 2, "close_others must report how many were dropped");
    assert_eq!(t.tab_count(), 1);
    assert_eq!(t.active_index(), 0);
    assert_eq!(t.path.as_deref(), Some(std::path::Path::new("/b")));
    assert!(t.focused, "the surviving tab takes focus");
}

#[test]
fn toggle_pin_keeps_pinned_tabs_leftmost_and_follows_the_active_tab() {
    let mut t = EditorTabs::new();
    t.editors[0].path = Some(std::path::PathBuf::from("/a"));
    t.add_tab_with_path(std::path::PathBuf::from("/b"));
    t.add_tab_with_path(std::path::PathBuf::from("/c")); // active = /c (idx 2)
    // Pin /c: it joins the (empty) pinned block at the front and stays
    // active even though its index changed from 2 to 0.
    assert!(t.toggle_pin(2));
    assert!(t.is_pinned(0));
    assert_eq!(t.tab_path(0).as_deref(), Some(std::path::Path::new("/c")));
    assert_eq!(t.active_index(), 0, "the active tab follows its editor");
    assert_eq!(t.tab_path(1).as_deref(), Some(std::path::Path::new("/a")));
    assert_eq!(t.tab_path(2).as_deref(), Some(std::path::Path::new("/b")));
    // Pinning /b too lands it at the END of the pinned block (after /c).
    let b_idx = 2;
    assert!(t.toggle_pin(b_idx));
    assert_eq!(t.tab_path(0).as_deref(), Some(std::path::Path::new("/c")));
    assert_eq!(t.tab_path(1).as_deref(), Some(std::path::Path::new("/b")));
    assert!(t.is_pinned(0) && t.is_pinned(1) && !t.is_pinned(2));
    // Unpinning /c returns it to the front of the unpinned block.
    assert!(!t.toggle_pin(0));
    assert!(t.is_pinned(0), "/b is still pinned and stays leftmost");
    assert_eq!(t.tab_path(0).as_deref(), Some(std::path::Path::new("/b")));
    assert_eq!(t.tab_path(1).as_deref(), Some(std::path::Path::new("/c")));
}

#[test]
fn close_others_keeps_pinned_tabs_alongside_the_kept_tab() {
    let mut t = EditorTabs::new();
    t.editors[0].path = Some(std::path::PathBuf::from("/a"));
    t.add_tab_with_path(std::path::PathBuf::from("/b"));
    t.add_tab_with_path(std::path::PathBuf::from("/c"));
    t.add_tab_with_path(std::path::PathBuf::from("/d"));
    t.toggle_pin(0); // pin /a (stays leftmost); order [/a*, /b, /c, /d]
    let removed = t.close_others(2); // keep /c
    assert_eq!(removed, 2, "only the unpinned non-kept tabs (/b, /d) close");
    assert_eq!(t.tab_count(), 2);
    assert!(t.is_pinned(0));
    assert_eq!(t.tab_path(0).as_deref(), Some(std::path::Path::new("/a")));
    assert_eq!(t.tab_path(1).as_deref(), Some(std::path::Path::new("/c")));
    assert_eq!(t.active_index(), 1, "the kept tab is active");
}

#[test]
fn close_to_right_keeps_pinned_tabs_to_the_right_of_the_pivot() {
    let mut t = EditorTabs::new();
    t.editors[0].path = Some(std::path::PathBuf::from("/a"));
    t.add_tab_with_path(std::path::PathBuf::from("/b"));
    t.add_tab_with_path(std::path::PathBuf::from("/c"));
    t.add_tab_with_path(std::path::PathBuf::from("/d"));
    t.toggle_pin(0); // pin /a
    t.toggle_pin(1); // pin /b -> end of pinned block; order [/a*, /b*, /c, /d]
    let removed = t.close_to_right(0); // close to the right of /a
    assert_eq!(
        removed, 2,
        "/c and /d close; the pinned /b to the right survives"
    );
    assert_eq!(t.tab_count(), 2);
    assert!(t.is_pinned(0) && t.is_pinned(1));
    assert_eq!(t.tab_path(0).as_deref(), Some(std::path::Path::new("/a")));
    assert_eq!(t.tab_path(1).as_deref(), Some(std::path::Path::new("/b")));
    assert_eq!(t.active_index(), 0, "the pivot tab is active");
}

#[test]
fn keep_open_promotes_a_preview_tab_and_returns_true() {
    let mut t = EditorTabs::new();
    t.editors[0].path = Some(std::path::PathBuf::from("/a"));
    t.editors[0].preview = true;
    assert!(t.is_preview(0));
    assert!(t.keep_open(0), "promoting a preview tab returns true");
    assert!(!t.is_preview(0), "the tab is no longer the preview slot");
}

#[test]
fn keep_open_is_a_noop_on_an_already_permanent_tab() {
    let mut t = EditorTabs::new();
    t.editors[0].path = Some(std::path::PathBuf::from("/a"));
    t.editors[0].preview = false;
    assert!(
        !t.keep_open(0),
        "a permanent tab returns false (nothing to promote)"
    );
}

#[test]
fn keep_open_out_of_range_is_false() {
    let mut t = EditorTabs::new();
    assert!(!t.keep_open(99));
}

#[test]
fn keep_open_only_affects_the_target_tab() {
    let mut t = EditorTabs::new();
    t.editors[0].path = Some(std::path::PathBuf::from("/a"));
    t.editors[0].preview = true;
    t.add_tab_with_path(std::path::PathBuf::from("/b"));
    t.editors[1].preview = true;
    t.keep_open(0);
    assert!(!t.is_preview(0));
    assert!(t.is_preview(1), "the sibling's preview flag is untouched");
}

#[test]
fn is_preview_and_is_pinned_are_false_out_of_range() {
    let t = EditorTabs::new();
    assert!(!t.is_preview(99));
    assert!(!t.is_pinned(99));
}

#[test]
fn toggle_pin_clears_the_preview_flag_when_pinning() {
    let mut t = EditorTabs::new();
    t.editors[0].path = Some(std::path::PathBuf::from("/a"));
    t.editors[0].preview = true;
    assert!(t.toggle_pin(0), "pinning returns the new state = true");
    assert!(t.is_pinned(0));
    assert!(!t.is_preview(0), "a pinned tab is never the preview slot");
}

#[test]
fn toggle_pin_returns_new_state_and_round_trips() {
    let mut t = EditorTabs::new();
    t.editors[0].path = Some(std::path::PathBuf::from("/a"));
    assert!(t.toggle_pin(0), "first toggle pins");
    assert!(!t.toggle_pin(0), "second toggle unpins");
    assert!(!t.is_pinned(0));
}

#[test]
fn toggle_pin_out_of_range_is_a_noop_and_false() {
    let mut t = EditorTabs::new();
    let before = t.tab_count();
    assert!(!t.toggle_pin(99));
    assert_eq!(t.tab_count(), before, "no panic, no mutation");
}

/// #941: Close All leaves pinned tabs open, as Close Others and Close
/// to the Right do (and VS Code's Close All), and the active pinned tab
/// stays active.
#[test]
fn close_all_keeps_pinned_tabs() {
    let mut t = EditorTabs::new();
    t.editors[0].path = Some(std::path::PathBuf::from("/a"));
    t.add_tab_with_path(std::path::PathBuf::from("/b"));
    t.add_tab_with_path(std::path::PathBuf::from("/c"));
    t.toggle_pin(0); // pin /a
    t.toggle_pin(1); // pin /b; order [/a*, /b*, /c]
    t.select(1);
    let removed = t.close_all();
    assert_eq!(removed, 1, "only the unpinned /c closes");
    assert_eq!(t.tab_path(0).as_deref(), Some(std::path::Path::new("/a")));
    assert_eq!(t.tab_path(1).as_deref(), Some(std::path::Path::new("/b")));
    assert!(t.is_pinned(0) && t.is_pinned(1));
    assert_eq!(t.active_index(), 1, "the active /b stays active");
    assert!(t.editors[1].focused && !t.editors[0].focused);
}

/// #941: Close Saved leaves a clean pinned tab open beside the dirty
/// one; only the clean unpinned tab closes.
#[test]
fn close_saved_keeps_clean_pinned_tabs() {
    let mut t = EditorTabs::new();
    t.editors[0].path = Some(std::path::PathBuf::from("/a"));
    t.add_tab_with_path(std::path::PathBuf::from("/b"));
    t.add_tab_with_path(std::path::PathBuf::from("/c"));
    t.editors[1].dirty = true;
    t.toggle_pin(0); // pin the clean /a
    let removed = t.close_saved();
    assert_eq!(removed, 1, "only the clean unpinned /c closes");
    assert_eq!(t.tab_path(0).as_deref(), Some(std::path::Path::new("/a")));
    assert_eq!(t.tab_path(1).as_deref(), Some(std::path::Path::new("/b")));
    assert_eq!(
        t.active_index(),
        0,
        "the closed /c hands active to the first"
    );
}

#[test]
fn pinned_tab_paints_the_thumb_tack_instead_of_the_close_cross() {
    let f1 = NamedTempFile::new().unwrap();
    let f2 = NamedTempFile::new().unwrap();
    std::fs::write(f1.path(), "1\n").unwrap();
    std::fs::write(f2.path(), "2\n").unwrap();
    let mut t = EditorTabs::new();
    t.open_pinned(f1.path()).unwrap();
    t.open_pinned(f2.path()).unwrap();
    t.toggle_pin(0); // pin the first tab (stays leftmost)
    let active_idx = t.active_index();
    t.editors[active_idx].focused = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 10,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut t).render(area, &mut buf);
    let strip: String = (0..area.width).map(|x| buf[(x, 0)].symbol()).collect();
    assert!(
        strip.contains('\u{f08d}'),
        "the pinned tab shows the thumb-tack glyph in its close cell; strip was {strip:?}"
    );
    assert!(
        strip.contains('\u{2715}'),
        "the unpinned tab still shows the close cross; strip was {strip:?}"
    );
}

#[test]
fn take_active_editor_removes_and_returns_the_active_tab() {
    let mut t = EditorTabs::new();
    t.editors[0].path = Some(std::path::PathBuf::from("/a"));
    t.add_tab_with_path(std::path::PathBuf::from("/b")); // active = /b
    let ed = t.take_active_editor();
    assert_eq!(ed.path.as_deref(), Some(std::path::Path::new("/b")));
    assert_eq!(t.tab_count(), 1);
    assert_eq!(t.tab_path(0).as_deref(), Some(std::path::Path::new("/a")));
}

#[test]
fn take_active_editor_on_the_last_tab_leaves_a_blank_group() {
    let mut t = EditorTabs::new();
    t.editors[0].path = Some(std::path::PathBuf::from("/a"));
    let ed = t.take_active_editor();
    assert_eq!(ed.path.as_deref(), Some(std::path::Path::new("/a")));
    assert_eq!(t.tab_count(), 1);
    assert!(t.is_blank_initial(), "the group falls back to a blank tab");
}

#[test]
fn push_editor_replaces_a_blank_group_then_appends() {
    let mut src = EditorTabs::new();
    src.editors[0].path = Some(std::path::PathBuf::from("/a"));
    let ed = src.take_active_editor();
    let mut dst = EditorTabs::new(); // blank-initial
    dst.push_editor(ed);
    assert_eq!(dst.tab_count(), 1, "pushing into a blank group replaces it");
    assert_eq!(dst.tab_path(0).as_deref(), Some(std::path::Path::new("/a")));
    assert_eq!(dst.active_index(), 0);
    let mut src2 = EditorTabs::new();
    src2.editors[0].path = Some(std::path::PathBuf::from("/b"));
    let ed2 = src2.take_active_editor();
    dst.push_editor(ed2);
    assert_eq!(dst.tab_count(), 2, "a second push appends and activates it");
    assert_eq!(dst.active_index(), 1);
    assert_eq!(dst.tab_path(1).as_deref(), Some(std::path::Path::new("/b")));
}

#[test]
fn push_editor_puts_a_pinned_editor_at_the_end_of_the_pinned_block() {
    let mut dst = EditorTabs::new();
    dst.editors[0].path = Some(std::path::PathBuf::from("/p"));
    dst.add_tab_with_path(std::path::PathBuf::from("/u"));
    dst.toggle_pin(0);
    let mut src = EditorTabs::new();
    src.editors[0].path = Some(std::path::PathBuf::from("/moved"));
    src.toggle_pin(0);
    dst.push_editor(src.take_active_editor());
    assert_eq!(dst.tab_count(), 3);
    assert_eq!(
        dst.tab_path(1).as_deref(),
        Some(std::path::Path::new("/moved")),
        "the pinned editor joins the pinned block, ahead of unpinned tabs"
    );
    assert!(dst.editors[1].pinned);
    assert_eq!(dst.tab_path(2).as_deref(), Some(std::path::Path::new("/u")));
    assert_eq!(dst.active_index(), 1, "the moved editor is active");
}

#[test]
fn close_others_is_a_noop_when_only_one_tab_is_open() {
    let mut t = EditorTabs::new();
    t.editors[0].path = Some(std::path::PathBuf::from("/only"));
    let removed = t.close_others(0);
    assert_eq!(removed, 0, "nothing to drop");
    assert_eq!(t.tab_count(), 1);
    assert_eq!(t.path.as_deref(), Some(std::path::Path::new("/only")));
}

#[test]
fn close_to_right_drops_only_tabs_past_the_pivot_and_keeps_left_side() {
    let mut t = EditorTabs::new();
    t.editors[0].path = Some(std::path::PathBuf::from("/a"));
    t.add_tab_with_path(std::path::PathBuf::from("/b"));
    t.add_tab_with_path(std::path::PathBuf::from("/c"));
    t.add_tab_with_path(std::path::PathBuf::from("/d"));
    // Pivot on "b" (index 1). c and d disappear; a and b stay.
    let removed = t.close_to_right(1);
    assert_eq!(removed, 2);
    assert_eq!(t.tab_count(), 2);
    assert_eq!(
        t.editors[0].path.as_deref(),
        Some(std::path::Path::new("/a")),
        "left-side tabs are untouched"
    );
    assert_eq!(
        t.editors[1].path.as_deref(),
        Some(std::path::Path::new("/b"))
    );
    assert!(
        t.active_index() < t.tab_count(),
        "active must still point at a valid tab"
    );
}

#[test]
fn close_to_right_at_last_tab_is_a_noop() {
    let mut t = EditorTabs::new();
    t.editors[0].path = Some(std::path::PathBuf::from("/a"));
    t.add_tab_with_path(std::path::PathBuf::from("/b"));
    let before = t.tab_count();
    let removed = t.close_to_right(1);
    assert_eq!(removed, 0);
    assert_eq!(t.tab_count(), before);
}

#[test]
fn close_all_collapses_to_a_single_blank_tab() {
    let mut t = EditorTabs::new();
    t.editors[0].path = Some(std::path::PathBuf::from("/a"));
    t.add_tab_with_path(std::path::PathBuf::from("/b"));
    t.add_tab_with_path(std::path::PathBuf::from("/c"));
    let removed = t.close_all();
    assert_eq!(removed, 3, "report how many tabs were collapsed");
    assert_eq!(t.tab_count(), 1, "always at least one tab survives");
    assert!(t.path.is_none(), "the surviving tab is a fresh blank slot");
}

#[test]
fn close_saved_drops_clean_tabs_and_keeps_dirty_ones() {
    let mut t = EditorTabs::new();
    t.editors[0].path = Some(std::path::PathBuf::from("/a"));
    t.add_tab_with_path(std::path::PathBuf::from("/b"));
    t.add_tab_with_path(std::path::PathBuf::from("/c"));
    // Mark "b" (index 1) as having unsaved changes; a and c are saved.
    t.editors[1].dirty = true;
    let removed = t.close_saved();
    assert_eq!(removed, 2, "the two saved tabs (a, c) are closed");
    assert_eq!(t.tab_count(), 1, "only the dirty tab survives");
    assert_eq!(
        t.editors[0].path.as_deref(),
        Some(std::path::Path::new("/b")),
        "the kept tab is the dirty one"
    );
    assert_eq!(t.active_index(), 0, "the kept tab becomes active");
    assert!(t.editors[0].focused, "the kept tab is focused");
}

#[test]
fn close_saved_collapses_to_blank_when_no_tab_is_dirty() {
    let mut t = EditorTabs::new();
    t.editors[0].path = Some(std::path::PathBuf::from("/a"));
    t.add_tab_with_path(std::path::PathBuf::from("/b"));
    let removed = t.close_saved();
    assert_eq!(removed, 2, "both saved tabs are closed");
    assert_eq!(t.tab_count(), 1, "always at least one tab survives");
    assert!(t.path.is_none(), "the survivor is a fresh blank slot");
}

#[test]
fn close_saved_is_a_noop_when_every_tab_is_dirty() {
    let mut t = EditorTabs::new();
    t.editors[0].path = Some(std::path::PathBuf::from("/a"));
    t.editors[0].dirty = true;
    t.add_tab_with_path(std::path::PathBuf::from("/b"));
    t.editors[1].dirty = true;
    let removed = t.close_saved();
    assert_eq!(removed, 0, "nothing saved means nothing to close");
    assert_eq!(t.tab_count(), 2, "both dirty tabs stay open");
}

/// #862: the bulk closes never drop unsaved edits: Close Others, Close
/// to the Right and Close All each leave a dirty tab open.
#[test]
fn bulk_closes_keep_tabs_with_unsaved_changes() {
    let tabs = || {
        let mut t = EditorTabs::new();
        t.editors[0].path = Some(std::path::PathBuf::from("/a"));
        t.add_tab_with_path(std::path::PathBuf::from("/b"));
        t.add_tab_with_path(std::path::PathBuf::from("/c"));
        t.editors[2].dirty = true;
        t
    };
    let paths = |t: &EditorTabs| -> Vec<Option<std::path::PathBuf>> {
        t.editors.iter().map(|e| e.path.clone()).collect()
    };
    let path = |p: &str| Some(std::path::PathBuf::from(p));

    let mut t = tabs();
    assert_eq!(t.close_others(0), 1, "only the clean /b goes");
    assert_eq!(paths(&t), vec![path("/a"), path("/c")]);
    assert_eq!(t.active_index(), 0, "the kept tab stays active");

    let mut t = tabs();
    assert_eq!(t.close_to_right(0), 1, "only the clean /b goes");
    assert_eq!(paths(&t), vec![path("/a"), path("/c")]);

    let mut t = tabs();
    assert_eq!(t.close_all(), 2);
    assert_eq!(paths(&t), vec![path("/c")], "the dirty tab survives");
    assert!(t.editors[0].dirty);
}

#[test]
fn close_tab_on_last_remaining_tab_resets_to_blank_buffer() {
    let mut t = EditorTabs::new();
    t.editors[0].path = Some(std::path::PathBuf::from("/only.rs"));
    t.editors[0].lines = vec!["something".to_string()];
    t.editors[0].dirty = true;
    assert!(t.close_tab(0));
    assert_eq!(t.tab_count(), 1);
    assert!(t.path.is_none());
    assert!(t.lines.is_empty() || t.lines == vec![String::new()]);
    assert!(!t.dirty);
}

#[test]
fn close_screen_x_is_present_even_for_a_single_open_tab() {
    use ratatui::buffer::Buffer;
    let mut t = EditorTabs::new();
    t.editors[0].path = Some(std::path::PathBuf::from("/only.rs"));
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 5,
    };
    let mut buf = Buffer::empty(area);
    (&mut t).render(area, &mut buf);
    let close_x = t.close_screen_x(0).expect("single tab still shows X");
    assert_eq!(t.close_at(close_x, area.y), Some(0));
}

#[test]
fn editor_tabs_tab_at_x_returns_index_of_clicked_tab() {
    use ratatui::buffer::Buffer;
    let mut t = EditorTabs::new();
    t.editors[0].path = Some(std::path::PathBuf::from("/long_name.rs"));
    t.add_tab_with_path(std::path::PathBuf::from("/b.rs"));
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 10,
    };
    let mut buf = Buffer::empty(area);
    (&mut t).render(area, &mut buf);
    let first = t.tab_screen_x(0).expect("tab 0 laid out");
    let second = t.tab_screen_x(1).expect("tab 1 laid out");
    assert_eq!(t.tab_at(first.0, area.y), Some(0));
    assert_eq!(t.tab_at(second.0, area.y), Some(1));
    assert_eq!(t.tab_at(area.x + area.width - 1, area.y + 5), None);
}

#[test]
fn open_preview_creates_a_preview_tab_when_none_exists() {
    let mut t = EditorTabs::new();
    t.editors[0].path = Some(std::path::PathBuf::from("/a"));
    t.editors[0].preview = false;
    // Stand in for a real disk open: simulate the side-effects directly.
    t.add_preview_tab_with_path(std::path::PathBuf::from("/b"));
    assert_eq!(t.tab_count(), 2);
    assert_eq!(t.preview_index(), Some(1));
    assert_eq!(t.active_index(), 1);
}

#[test]
fn open_preview_reuses_existing_preview_slot_for_a_new_path() {
    let mut t = EditorTabs::new();
    t.editors[0].path = Some(std::path::PathBuf::from("/a"));
    t.editors[0].preview = false;
    t.add_preview_tab_with_path(std::path::PathBuf::from("/b"));
    // Now repoint the preview tab at /c — count must NOT grow.
    t.repoint_preview_to(std::path::PathBuf::from("/c"));
    assert_eq!(t.tab_count(), 2);
    assert_eq!(t.preview_index(), Some(1));
    assert_eq!(
        t.editors[1].path.as_deref(),
        Some(std::path::Path::new("/c"))
    );
}

#[test]
fn open_preview_reuses_blank_initial_tab_instead_of_stacking() {
    let mut t = EditorTabs::new();
    let mut f = NamedTempFile::new().unwrap();
    write!(f, "hello").unwrap();
    t.open_preview(f.path()).unwrap();
    assert_eq!(t.tab_count(), 1, "blank initial tab must be reused");
    assert_eq!(t.preview_index(), Some(0));
    assert_eq!(t.path.as_deref(), Some(f.path()));
}

#[test]
fn tab_labels_disambiguate_colliding_names_with_the_shortest_dir_context() {
    // #167, VS Code labelFormat: colliding basenames gain the shortest
    // distinguishing trailing directory; unique names stay bare.
    let tmp = tempfile::tempdir().unwrap();
    for d in ["alpha", "beta", "a/x", "b/x"] {
        std::fs::create_dir_all(tmp.path().join(d)).unwrap();
    }
    for f in [
        "alpha/main.rs",
        "beta/main.rs",
        "solo.rs",
        "a/x/mod.rs",
        "b/x/mod.rs",
    ] {
        std::fs::write(tmp.path().join(f), "x\n").unwrap();
    }
    let mut tabs = EditorTabs::new();
    for f in [
        "alpha/main.rs",
        "beta/main.rs",
        "solo.rs",
        "a/x/mod.rs",
        "b/x/mod.rs",
    ] {
        tabs.open_pinned(&tmp.path().join(f)).unwrap();
    }
    let labels = tabs.tab_display_labels();
    assert_eq!(labels[0], "main.rs — alpha");
    assert_eq!(labels[1], "main.rs — beta");
    assert_eq!(labels[2], "solo.rs", "a unique name stays bare");
    // Parents collide (`x` vs `x`), so the suffix walks one level up.
    assert_eq!(labels[3], "mod.rs — a/x");
    assert_eq!(labels[4], "mod.rs — b/x");
}

#[test]
fn tab_labels_disambiguate_deep_shared_tails_and_measure_wide_names_in_cells() {
    // #168 review: a fixed depth cap left paths sharing a deep tail
    // identical, and char-count width shifted hit ranges for wide
    // (CJK) names.
    let tmp = tempfile::tempdir().unwrap();
    let deep = "d01/d02/d03/d04/d05/d06/d07/d08/d09/d10/d11/d12/d13/d14/d15/d16/d17";
    let a = tmp.path().join("alpha").join(deep);
    let b = tmp.path().join("beta").join(deep);
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    std::fs::write(a.join("main.rs"), "a\n").unwrap();
    std::fs::write(b.join("main.rs"), "b\n").unwrap();
    let wide = tmp.path().join("日本語");
    std::fs::create_dir_all(&wide).unwrap();
    std::fs::write(wide.join("main.rs"), "w\n").unwrap();

    let mut tabs = EditorTabs::new();
    tabs.open_pinned(&a.join("main.rs")).unwrap();
    tabs.open_pinned(&b.join("main.rs")).unwrap();
    tabs.open_pinned(&wide.join("main.rs")).unwrap();
    let labels = tabs.tab_display_labels();
    let mut seen = std::collections::HashSet::new();
    assert!(
        labels.iter().all(|l| seen.insert(l.clone())),
        "every colliding tab gets a distinct title, however deep the shared tail: {labels:?}"
    );
    assert!(
        labels[2].contains("日本語"),
        "the wide dir name suffixes too: {labels:?}"
    );
    // The strip measures in display cells: a render must place the
    // close glyph inside each tab's recorded range (frame truth).
    let area = ratatui::layout::Rect {
        x: 0,
        y: 0,
        width: 600,
        height: 20,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    use ratatui::widgets::Widget;
    (&mut tabs).render(area, &mut buf);
    let ranges: Vec<(u16, u16)> = tabs.tab_screen_ranges.clone();
    for (i, (x, w)) in ranges.iter().enumerate() {
        if *w == 0 {
            continue;
        }
        let row: String = (*x..*x + *w)
            .map(|cx| buf[(cx, tabs.tab_strip_y_for_test())].symbol().to_string())
            .collect();
        assert!(
            row.contains('✕'),
            "tab {i}'s close glyph must sit inside its hit range; range painted: {row:?}"
        );
    }
}

#[test]
fn open_pinned_reuses_blank_initial_tab_instead_of_stacking() {
    let mut t = EditorTabs::new();
    let mut f = NamedTempFile::new().unwrap();
    write!(f, "hello").unwrap();
    t.open_pinned(f.path()).unwrap();
    assert_eq!(t.tab_count(), 1, "blank initial tab must be reused");
    assert!(
        t.preview_index().is_none(),
        "pinned open must not leave preview state"
    );
}

#[test]
fn open_preview_drops_stale_preview_when_switching_to_pinned_tab() {
    let mut t = EditorTabs::new();
    let mut f1 = NamedTempFile::new().unwrap();
    write!(f1, "a").unwrap();
    let mut f2 = NamedTempFile::new().unwrap();
    write!(f2, "b").unwrap();
    t.open_preview(f1.path()).unwrap(); // preview slot = f1, only tab
    t.pin_active(); // f1 pinned
    t.open_preview(f2.path()).unwrap(); // preview tab for f2 alongside pinned f1
    assert_eq!(t.tab_count(), 2);
    t.open_preview(f1.path()).unwrap(); // back to f1 → stale f2 preview must vanish
    assert_eq!(t.tab_count(), 1);
    assert!(t.preview_index().is_none());
    assert_eq!(t.path.as_deref(), Some(f1.path()));
}

#[test]
fn pin_active_clears_preview_flag() {
    let mut t = EditorTabs::new();
    t.editors[0].path = Some(std::path::PathBuf::from("/a"));
    t.add_preview_tab_with_path(std::path::PathBuf::from("/b"));
    assert_eq!(t.preview_index(), Some(1));
    t.pin_active();
    assert!(
        t.preview_index().is_none(),
        "no tab should be in preview state"
    );
    assert!(!t.editors[1].preview);
}

#[test]
fn editor_tabs_find_tab_with_path_returns_index_when_open() {
    let mut t = EditorTabs::new();
    t.editors[0].path = Some(std::path::PathBuf::from("/a"));
    t.add_tab_with_path(std::path::PathBuf::from("/b"));
    t.add_tab_with_path(std::path::PathBuf::from("/c"));
    assert_eq!(t.find_tab_with_path(std::path::Path::new("/a")), Some(0));
    assert_eq!(t.find_tab_with_path(std::path::Path::new("/b")), Some(1));
    assert_eq!(t.find_tab_with_path(std::path::Path::new("/c")), Some(2));
    assert_eq!(t.find_tab_with_path(std::path::Path::new("/missing")), None);
}

/// #1113: the buffer for a path is its own tab, never a diff tab
/// carrying the same path.
#[test]
fn the_buffer_tab_of_a_file_is_never_its_diff() {
    let mut t = EditorTabs::new();
    let file = std::path::PathBuf::from("/a");
    t.editors[0].path = Some(file.clone());
    t.editors[0].diff = Some(crate::widgets::diff::DiffData::build_unified_deletion(
        file.clone(),
        "x\n",
    ));
    assert_eq!(t.find_buffer_tab(&file), None);
    assert_eq!(t.find_tab_with_path(&file), Some(0), "still found as a tab");
    t.add_tab_with_path(file.clone());
    assert_eq!(t.find_buffer_tab(&file), Some(1), "its own tab");
    assert_eq!(t.find_buffer_tab(std::path::Path::new("/b")), None);
}

#[test]
fn editor_tabs_deref_exposes_active_editor_state() {
    let mut t = EditorTabs::new();
    t.lines = vec!["abc".to_string()];
    t.cursor_col = 3;
    // Field access reaches active editor via DerefMut.
    assert_eq!(t.lines, vec!["abc".to_string()]);
    assert_eq!(t.cursor_col, 3);
}

#[test]
fn mouse_drag_extends_selection() {
    let mut e = editor_with("hello world");
    e.last_inner = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 25,
    };
    e.last_gutter_width = 2;
    let text_x: u16 = 3;
    e.mouse_down(text_x, 0);
    e.mouse_drag(text_x + 5, 0);
    let sel = e.selection.unwrap();
    assert_eq!(sel.anchor, (0, 0));
    assert_eq!(sel.head, (0, 5));
    assert_eq!(e.cursor_col, 5);
}

#[test]
fn editor_tabs_rename_open_path_updates_matching_tab_only() {
    // Renaming a file on disk must update the matching tab so the
    // editor keeps tracking the entry. Other tabs must be untouched.
    let mut tabs = EditorTabs::new();
    tabs.add_tab_with_path(std::path::PathBuf::from("/work/old.txt"));
    tabs.add_tab_with_path(std::path::PathBuf::from("/work/other.txt"));
    let old = std::path::PathBuf::from("/work/old.txt");
    let new = std::path::PathBuf::from("/work/renamed.txt");
    tabs.rename_open_path(&old, &new);
    let paths: Vec<Option<std::path::PathBuf>> = tabs.iter_tabs().map(|e| e.path.clone()).collect();
    assert!(paths.contains(&Some(new.clone())));
    assert!(!paths.contains(&Some(old)));
    assert!(paths.contains(&Some(std::path::PathBuf::from("/work/other.txt"))));
}

#[test]
fn editor_tabs_rename_open_path_is_noop_when_path_not_open() {
    let mut tabs = EditorTabs::new();
    tabs.add_tab_with_path(std::path::PathBuf::from("/work/a.txt"));
    let before: Vec<Option<std::path::PathBuf>> =
        tabs.iter_tabs().map(|e| e.path.clone()).collect();
    tabs.rename_open_path(
        std::path::Path::new("/work/never-opened.txt"),
        std::path::Path::new("/work/whatever.txt"),
    );
    let after: Vec<Option<std::path::PathBuf>> = tabs.iter_tabs().map(|e| e.path.clone()).collect();
    assert_eq!(before, after);
}

// ---- Move Line Up / Down (Alt+Up / Alt+Down) ----

#[test]
fn move_line_down_swaps_with_next_and_follows_cursor() {
    let mut e = editor_with("a\nb\nc");
    e.cursor_row = 0;
    e.move_lines_down();
    assert_eq!(e.lines, vec!["b", "a", "c"]);
    assert_eq!(e.cursor_row, 1, "cursor follows the moved line");
}

#[test]
fn move_line_up_swaps_with_previous_and_follows_cursor() {
    let mut e = editor_with("a\nb\nc");
    e.cursor_row = 2;
    e.move_lines_up();
    assert_eq!(e.lines, vec!["a", "c", "b"]);
    assert_eq!(e.cursor_row, 1);
}

#[test]
fn move_line_up_at_top_is_noop() {
    let mut e = editor_with("a\nb");
    e.cursor_row = 0;
    e.move_lines_up();
    assert_eq!(e.lines, vec!["a", "b"]);
    assert_eq!(e.cursor_row, 0);
}

#[test]
fn move_line_down_at_bottom_is_noop() {
    let mut e = editor_with("a\nb");
    e.cursor_row = 1;
    e.move_lines_down();
    assert_eq!(e.lines, vec!["a", "b"]);
    assert_eq!(e.cursor_row, 1);
}

#[test]
fn move_line_down_moves_whole_selected_block() {
    let mut e = editor_with("a\nb\nc\nd");
    e.selection = Some(EditorSelection {
        anchor: (0, 0),
        head: (1, 1),
    });
    e.cursor_row = 1;
    e.move_lines_down();
    assert_eq!(e.lines, vec!["c", "a", "b", "d"]);
    assert_eq!(e.cursor_row, 2);
    let sel = e.selection.unwrap();
    assert_eq!((sel.anchor.0, sel.head.0), (1, 2), "selection rows shift");
}

// ---- Toggle Line Comment (Cmd+/) ----

#[test]
fn toggle_line_comment_adds_rust_token() {
    let mut e = editor_with("    let x = 1;");
    e.lang = Some(LangKind::Rust);
    e.cursor_row = 0;
    assert!(e.toggle_line_comment());
    assert_eq!(e.lines, vec!["    // let x = 1;"]);
}

#[test]
fn toggle_line_comment_removes_existing_rust_token() {
    let mut e = editor_with("    // let x = 1;");
    e.lang = Some(LangKind::Rust);
    e.cursor_row = 0;
    assert!(e.toggle_line_comment());
    assert_eq!(e.lines, vec!["    let x = 1;"]);
}

#[test]
fn toggle_line_comment_python_hash() {
    let mut e = editor_with("x = 1");
    e.lang = Some(LangKind::Python);
    e.cursor_row = 0;
    assert!(e.toggle_line_comment());
    assert_eq!(e.lines, vec!["# x = 1"]);
}

#[test]
fn toggle_line_comment_codeql_uses_slashes() {
    let mut e = editor_with("select 1");
    e.lang = crate::highlight::lang_for_extension("ql");
    e.cursor_row = 0;
    assert!(e.toggle_line_comment());
    assert_eq!(e.lines, vec!["// select 1"]);
    assert_eq!(language_label(e.lang), "CodeQL");
}

#[test]
fn toggle_line_comment_refreshes_highlights() {
    // Commenting a line must re-run tree-sitter so the line repaints as a
    // comment, not keep the stale pre-comment code spans. Without the
    // refresh the rendered comment shows code colours shifted by `# `.
    let mut e = editor_with("x = 1");
    e.lang = Some(LangKind::Python);
    e.recompute_highlights();
    e.cursor_row = 0;
    assert!(e.toggle_line_comment());
    let after: Vec<(usize, usize, Option<Color>)> = e.highlights[0]
        .iter()
        .map(|s| (s.range().start, s.range().end, s.style.fg))
        .collect();
    e.recompute_highlights();
    let fresh: Vec<(usize, usize, Option<Color>)> = e.highlights[0]
        .iter()
        .map(|s| (s.range().start, s.range().end, s.style.fg))
        .collect();
    assert_eq!(
        after, fresh,
        "toggle_line_comment must refresh syntax highlights to match the new buffer"
    );
}

#[test]
fn toggle_line_comment_block_comments_at_min_indent() {
    let mut e = editor_with("  a\n    b");
    e.lang = Some(LangKind::Python);
    e.selection = Some(EditorSelection {
        anchor: (0, 0),
        head: (1, 5),
    });
    assert!(e.toggle_line_comment());
    assert_eq!(e.lines, vec!["  # a", "  #   b"]);
}

#[test]
fn toggle_line_comment_block_uncomments_when_all_commented() {
    let mut e = editor_with("  # a\n  #   b");
    e.lang = Some(LangKind::Python);
    e.selection = Some(EditorSelection {
        anchor: (0, 0),
        head: (1, 7),
    });
    assert!(e.toggle_line_comment());
    assert_eq!(e.lines, vec!["  a", "    b"]);
}

#[test]
fn toggle_line_comment_unknown_lang_is_noop() {
    let mut e = editor_with("x = 1");
    e.lang = None;
    assert!(!e.toggle_line_comment());
    assert_eq!(e.lines, vec!["x = 1"]);
}

/// The issue's component (#1623).
const APP_TSX: &str = "export function App({ name }: { name: string }) {\n  return (\n    <div className=\"app\">\n      <h1>Hello {name}</h1>\n      <p>Welcome</p>\n    </div>\n  );\n}";

fn jsx_editor(lang: LangKind) -> Editor {
    let mut e = editor_with(APP_TSX);
    e.lang = Some(lang);
    e
}

/// #1623: Ctrl+/ on a line in JSX children writes `{/* … */}`, which
/// React does not render, and a second Ctrl+/ takes it off again.
#[test]
fn toggle_line_comment_in_jsx_children_writes_a_jsx_comment() {
    for lang in [LangKind::Tsx, LangKind::JavaScript] {
        let mut e = jsx_editor(lang);
        e.cursor_row = 4;
        assert!(e.toggle_line_comment());
        assert_eq!(e.lines[4], "      {/* <p>Welcome</p> */}", "{lang:?}");
        assert!(e.toggle_line_comment());
        assert_eq!(e.lines[4], "      <p>Welcome</p>", "{lang:?}");
    }
}

/// #1623: the closing tag and a run of child lines comment the same way,
/// each line on its own, at the block's indent.
#[test]
fn toggle_line_comment_on_jsx_child_lines_and_closing_tags() {
    let mut e = jsx_editor(LangKind::Tsx);
    e.selection = Some(EditorSelection {
        anchor: (3, 0),
        head: (4, 3),
    });
    assert!(e.toggle_line_comment());
    assert_eq!(e.lines[3], "      {/* <h1>Hello {name}</h1> */}");
    assert_eq!(e.lines[4], "      {/* <p>Welcome</p> */}");
    assert!(e.toggle_line_comment());
    assert_eq!(e.lines[3], "      <h1>Hello {name}</h1>");
    assert_eq!(e.lines[4], "      <p>Welcome</p>");
    let mut e = jsx_editor(LangKind::Tsx);
    e.cursor_row = 5;
    assert!(e.toggle_line_comment());
    assert_eq!(e.lines[5], "    {/* </div> */}");
}

/// #1623: a commented-out closing tag leaves the markup unparseable;
/// Ctrl+/ on it, and on the child above it, still takes the JSX
/// comments off instead of adding `//`.
#[test]
fn a_commented_closing_tag_toggles_back() {
    let mut e = jsx_editor(LangKind::Tsx);
    e.cursor_row = 4;
    assert!(e.toggle_line_comment());
    e.cursor_row = 5;
    assert!(e.toggle_line_comment());
    assert_eq!(e.lines[5], "    {/* </div> */}");
    assert!(e.toggle_line_comment());
    assert_eq!(e.lines[5], "    </div>");
    e.cursor_row = 5;
    assert!(e.toggle_line_comment());
    e.cursor_row = 4;
    assert!(e.toggle_line_comment());
    assert_eq!(e.lines[4], "      <p>Welcome</p>");
}

/// #1623: commenting a selection that already holds a JSX comment
/// leaves that comment as it is instead of nesting it.
#[test]
fn toggle_line_comment_on_a_mixed_jsx_selection_does_not_nest() {
    let mut e = jsx_editor(LangKind::Tsx);
    e.cursor_row = 3;
    assert!(e.toggle_line_comment());
    e.selection = Some(EditorSelection {
        anchor: (3, 0),
        head: (4, 3),
    });
    assert!(e.toggle_line_comment());
    assert_eq!(e.lines[3], "      {/* <h1>Hello {name}</h1> */}");
    assert_eq!(e.lines[4], "      {/* <p>Welcome</p> */}");
    assert!(e.toggle_line_comment());
    assert_eq!(e.lines[3], "      <h1>Hello {name}</h1>");
    assert_eq!(e.lines[4], "      <p>Welcome</p>");
}

/// #1623: Toggle Block Comment twice on a closing tag restores it, though
/// the first toggle left the markup unparseable.
#[test]
fn toggle_block_comment_on_a_closing_tag_toggles_back() {
    let mut e = jsx_editor(LangKind::Tsx);
    e.cursor_row = 5;
    assert!(e.toggle_block_comment());
    assert_eq!(e.lines[5], "    {/* </div> */}");
    e.cursor_row = 5;
    assert!(e.toggle_block_comment());
    assert_eq!(e.lines[5], "    </div>");
}

/// #1623: in plain code, `{/* TODO */}` is a block holding a comment;
/// Ctrl+/ comments it out with `//` rather than unwrapping it.
#[test]
fn a_braced_comment_in_plain_code_is_not_a_jsx_comment() {
    for lang in [LangKind::Tsx, LangKind::JavaScript] {
        let mut e = editor_with("function f() {\n  {/* TODO */}\n}");
        e.lang = Some(lang);
        e.cursor_row = 1;
        assert!(e.toggle_line_comment());
        assert_eq!(e.lines[1], "  // {/* TODO */}", "{lang:?}");
    }
}

/// #1623: Toggle Block Comment in JSX children wraps in braces too.
#[test]
fn toggle_block_comment_in_jsx_children_wraps_in_braces() {
    let mut e = jsx_editor(LangKind::Tsx);
    e.cursor_row = 4;
    assert!(e.toggle_block_comment());
    assert_eq!(e.lines[4], "      {/* <p>Welcome</p> */}");
    e.cursor_row = 4;
    assert!(e.toggle_block_comment());
    assert_eq!(e.lines[4], "      <p>Welcome</p>");
}

/// Negative (#1623): code around the JSX, the root element's line, an
/// attribute line and a plain `.ts` file keep `//`.
#[test]
fn toggle_line_comment_outside_jsx_children_keeps_slashes() {
    let mut e = jsx_editor(LangKind::Tsx);
    e.cursor_row = 1;
    assert!(e.toggle_line_comment());
    assert_eq!(e.lines[1], "  // return (");
    let mut e = jsx_editor(LangKind::Tsx);
    e.cursor_row = 2;
    assert!(e.toggle_line_comment());
    assert_eq!(e.lines[2], "    // <div className=\"app\">");
    let mut e = editor_with("const el = (\n  <input\n    disabled\n  />\n);");
    e.lang = Some(LangKind::Tsx);
    e.cursor_row = 2;
    assert!(e.toggle_line_comment());
    assert_eq!(e.lines[2], "    // disabled");
    let mut e = editor_with("const x = 1;\nconst y = 2;");
    e.lang = Some(LangKind::TypeScript);
    e.cursor_row = 0;
    assert!(e.toggle_line_comment());
    assert_eq!(e.lines[0], "// const x = 1;");
    let mut e = jsx_editor(LangKind::Tsx);
    e.selection = Some(EditorSelection {
        anchor: (1, 2),
        head: (1, 8),
    });
    assert!(e.toggle_block_comment());
    assert_eq!(e.lines[1], "  /* return */ (");
}

/// #1161: XML comments are `<!-- -->`, so Toggle Block Comment wraps
/// the caret's line in them and a second toggle takes them off.
#[test]
fn toggle_block_comment_wraps_an_xml_line() {
    let mut e = editor_with("<root>\n  <item id=\"1\">hello</item>\n</root>");
    e.lang = Some(LangKind::Xml);
    e.cursor_row = 1;
    assert!(e.toggle_block_comment());
    assert_eq!(e.lines[1], "  <!-- <item id=\"1\">hello</item> -->");
    assert_eq!(e.lines[0], "<root>");
    assert_eq!(e.lines[2], "</root>");
    assert!(e.toggle_block_comment());
    assert_eq!(e.lines[1], "  <item id=\"1\">hello</item>");
}

/// #1161: Toggle Line Comment in a language with only block comments
/// (XML, HTML) wraps the lines a selection touches, whole, as VS Code
/// does, and toggles back.
#[test]
fn toggle_line_comment_wraps_whole_lines_where_only_block_comments_exist() {
    let mut e = editor_with("<root>\n  <item id=\"1\">hello</item>\n</root>");
    e.lang = Some(LangKind::Xml);
    e.cursor_row = 1;
    e.cursor_col = 5;
    assert!(e.toggle_line_comment());
    assert_eq!(e.lines[1], "  <!-- <item id=\"1\">hello</item> -->");
    e.cursor_row = 1;
    assert!(e.toggle_line_comment());
    assert_eq!(e.lines[1], "  <item id=\"1\">hello</item>");

    let mut e = editor_with("<ul>\n  <li>a</li>\n  <li>b</li>\n</ul>");
    e.lang = Some(LangKind::Html);
    e.selection = Some(EditorSelection {
        anchor: (1, 5),
        head: (2, 3),
    });
    assert!(e.toggle_line_comment());
    assert_eq!(
        e.lines,
        vec!["<ul>", "  <!-- <li>a</li>", "  <li>b</li> -->", "</ul>"]
    );
    // The lines stay selected, so a second toggle uncomments them all.
    assert!(e.selection.is_some_and(|s| s.has_area()));
    assert!(e.toggle_line_comment());
    assert_eq!(
        e.lines,
        vec!["<ul>", "  <li>a</li>", "  <li>b</li>", "</ul>"]
    );
    assert!(e.toggle_line_comment());
    assert_eq!(
        e.lines,
        vec!["<ul>", "  <!-- <li>a</li>", "  <li>b</li> -->", "</ul>"]
    );
}

/// #1161, negative: a selection ending at the start of a line takes that
/// line in, as a line comment does, a language with a line comment
/// (JSON's `//` since #1181) keeps it, and plain text, with neither,
/// still does nothing.
#[test]
fn toggle_line_comment_falls_back_only_without_a_line_comment() {
    let mut e = editor_with("<a/>\n<b/>\n<c/>");
    e.lang = Some(LangKind::Xml);
    e.selection = Some(EditorSelection {
        anchor: (0, 0),
        head: (1, 0),
    });
    assert!(e.toggle_line_comment());
    assert_eq!(e.lines, vec!["<!-- <a/>", "<b/> -->", "<c/>"]);

    let mut e = editor_with("let x = 1;");
    e.lang = Some(LangKind::Rust);
    assert!(e.toggle_line_comment());
    assert_eq!(e.lines, vec!["// let x = 1;"]);

    for lang in [Some(LangKind::Json), Some(LangKind::Yaml), None] {
        let mut e = editor_with("{}");
        e.lang = lang;
        let changed = e.toggle_line_comment();
        assert_eq!(changed, lang.is_some(), "{lang:?}");
        if lang.is_none() {
            assert_eq!(e.lines, vec!["{}"]);
        }
    }
}

/// #1157: a file not written yet opens as an empty, clean buffer bound
/// to its path, with its language, and the first save creates it.
#[test]
fn a_new_file_opens_empty_and_its_first_save_creates_it() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("notes.py");
    let mut e = Editor::new();
    e.open_new_file(&path).unwrap();
    assert_eq!(e.path.as_deref(), Some(path.as_path()));
    assert_eq!(e.lines, vec![String::new()]);
    assert!(!e.dirty);
    assert_eq!(e.language_label(), "Python");
    assert!(e.status.contains("saving creates it"), "{}", e.status);
    assert!(!path.exists(), "opening creates nothing");
    assert!(!e.disk_changed_externally());
    e.insert_str("x = 1");
    assert!(matches!(e.save_to_disk().unwrap(), SaveOutcome::Saved));
    assert_eq!(std::fs::read_to_string(&path).unwrap().trim_end(), "x = 1");
    assert!(!e.disk_changed_externally());
}

/// #1157: a new file that another process creates before the first
/// save is an external change: the save refuses to clobber it and a
/// clean buffer reloads it.
#[test]
fn a_new_file_created_elsewhere_before_saving_is_detected() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("notes.txt");
    let mut e = Editor::new();
    e.open_new_file(&path).unwrap();
    std::fs::write(&path, "theirs\n").unwrap();
    assert!(e.disk_changed_externally());
    e.insert_str("mine");
    assert!(matches!(
        e.save_to_disk().unwrap(),
        SaveOutcome::DiskConflict
    ));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "theirs\n");
    let mut e = Editor::new();
    std::fs::remove_file(&path).unwrap();
    e.open_new_file(&path).unwrap();
    std::fs::write(&path, "theirs\n").unwrap();
    assert!(matches!(
        e.reload_or_flag_conflict(),
        ExternalChange::Reloaded
    ));
    assert_eq!(e.lines[0], "theirs");
    assert!(!e.disk_changed_externally());
}

/// #1157, negative: an existing file opens with its text, and a new file
/// in a missing folder is refused rather than bound to a path no save
/// could write.
#[test]
fn a_new_file_keeps_existing_text_and_needs_its_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("old.txt");
    std::fs::write(&path, "kept\n").unwrap();
    let mut e = Editor::new();
    e.open_new_file(&path).unwrap();
    assert_eq!(e.lines[0], "kept");
    let mut e = Editor::new();
    let err = e
        .open_new_file(&tmp.path().join("gone/new.txt"))
        .unwrap_err();
    assert!(err.to_string().contains("no such directory"), "{err}");
    assert!(e.path.is_none());
}

/// #1181: where the language has only block comments, Toggle Line
/// Comment wraps the lines in one, as VS Code does, and unwraps them.
#[test]
fn toggle_line_comment_wraps_lines_in_a_block_comment_where_there_is_no_line_comment() {
    for (lang, text, commented) in [
        (LangKind::Html, "  <ul>", "  <!-- <ul> -->"),
        (LangKind::Css, "body {", "/* body { */"),
        (LangKind::Markdown, "# Title", "<!-- # Title -->"),
        (LangKind::Xml, "<a/>", "<!-- <a/> -->"),
    ] {
        let mut e = editor_with(text);
        e.lang = Some(lang);
        e.cursor_col = text.chars().count() - 1;
        assert!(e.toggle_line_comment(), "{lang:?}");
        assert_eq!(e.lines, vec![commented], "{lang:?}");
        assert!(e.toggle_line_comment(), "{lang:?}");
        assert_eq!(e.lines, vec![text], "{lang:?} unwraps");
        assert_eq!(e.cursor_col, text.chars().count() - 1, "{lang:?} caret");
    }
    // A selection of lines gets one pair, from the first line's text to
    // the last one's, and blank lines around it are left out.
    let mut e = editor_with("<ul>\n  <li>one</li>\n</ul>\n\n");
    e.lang = Some(LangKind::Html);
    e.selection = Some(EditorSelection {
        anchor: (0, 0),
        head: (3, 0),
    });
    assert!(e.toggle_line_comment());
    assert_eq!(
        e.lines,
        vec!["<!-- <ul>", "  <li>one</li>", "</ul> -->", ""]
    );
    assert!(e.toggle_line_comment());
    assert_eq!(e.lines, vec!["<ul>", "  <li>one</li>", "</ul>", ""]);
}

/// #1181: JSON(C) comments with `//`, and a file croft has no grammar
/// for comments by its name or extension.
#[test]
fn toggle_line_comment_covers_json_and_files_without_a_grammar() {
    let mut e = editor_with("  \"a\": 1,");
    e.lang = Some(LangKind::Json);
    assert!(e.toggle_line_comment());
    assert_eq!(e.lines, vec!["  // \"a\": 1,"]);
    for (name, text, commented) in [
        ("App.kt", "class App {", "// class App {"),
        ("main.swift", "let x = 1", "// let x = 1"),
        (".gitignore", "target/", "# target/"),
        (".env.local", "KEY=1", "# KEY=1"),
        ("run.ps1", "Write-Host hi", "# Write-Host hi"),
        ("Main.hs", "main = pure ()", "-- main = pure ()"),
        ("setup.ini", "[core]", "; [core]"),
        ("paper.tex", "\\section{A}", "% \\section{A}"),
    ] {
        let mut e = editor_with(text);
        e.lang = None;
        e.path = Some(PathBuf::from("/tmp/x").join(name));
        assert!(e.toggle_line_comment(), "{name}");
        assert_eq!(e.lines, vec![commented], "{name}");
        assert!(e.toggle_line_comment(), "{name}");
        assert_eq!(e.lines, vec![text], "{name} uncomments");
    }
}

/// #1181: SCSS, Sass and Less comment with `//`, though SCSS and Sass
/// share the CSS grammar, which still wraps in `/* */`.
#[test]
fn toggle_line_comment_uses_slashes_in_scss_sass_and_less() {
    for (name, lang) in [
        ("a.scss", Some(LangKind::Css)),
        ("a.sass", Some(LangKind::Css)),
        ("a.less", None),
    ] {
        let mut e = editor_with("  color: red;");
        e.lang = lang;
        e.path = Some(PathBuf::from("/tmp/x").join(name));
        assert!(e.toggle_line_comment(), "{name}");
        assert_eq!(e.lines, vec!["  // color: red;"], "{name}");
        assert!(e.toggle_line_comment(), "{name}");
        assert_eq!(e.lines, vec!["  color: red;"], "{name} uncomments");
    }
    let mut e = editor_with("body {");
    e.lang = Some(LangKind::Css);
    e.path = Some(PathBuf::from("/tmp/x/a.css"));
    assert!(e.toggle_line_comment());
    assert_eq!(e.lines, vec!["/* body { */"]);
}

/// #1181: batch files comment with `@REM`, as VS Code does, and
/// uncomment `@REM` or `REM` only as a whole word, so `REMOTE.EXE` is
/// a command, not a comment.
#[test]
fn toggle_line_comment_in_batch_files_uses_at_rem_as_a_word() {
    let bat = |text: &str| {
        let mut e = editor_with(text);
        e.lang = None;
        e.path = Some(PathBuf::from("/tmp/x/run.bat"));
        e
    };
    let mut e = bat("echo hi");
    assert!(e.toggle_line_comment());
    assert_eq!(e.lines, vec!["@REM echo hi"]);
    assert!(e.toggle_line_comment());
    assert_eq!(e.lines, vec!["echo hi"]);
    let mut e = bat("REM note");
    assert!(e.toggle_line_comment());
    assert_eq!(e.lines, vec!["note"], "plain REM uncomments");
    let mut e = bat("REM");
    assert!(e.toggle_line_comment());
    assert_eq!(e.lines, vec![""]);
    for text in ["REMOTE.EXE --sync", "@REMOTE.EXE --sync"] {
        let mut e = bat(text);
        assert!(e.toggle_line_comment());
        assert_eq!(e.lines, vec![format!("@REM {text}")], "{text}");
    }
}

/// #1181 negative: a file of an unknown kind, or a language with no
/// comment of either kind, is still left alone, as are blank lines.
#[test]
fn toggle_line_comment_still_skips_what_has_no_comment() {
    let mut e = editor_with("x = 1");
    e.lang = None;
    e.path = Some(PathBuf::from("/tmp/x/notes.unknownext"));
    assert!(!e.toggle_line_comment());
    assert_eq!(e.lines, vec!["x = 1"]);
    let mut e = editor_with("   ");
    e.lang = Some(LangKind::Html);
    assert!(!e.toggle_line_comment());
    assert_eq!(e.lines, vec!["   "]);
    let mut e = editor_with("x: 1");
    e.lang = Some(LangKind::Yaml);
    assert!(e.toggle_line_comment());
    assert_eq!(e.lines, vec!["# x: 1"], "a line comment still wins");
}

// ---- Toggle Block Comment (Shift+Alt+A) ----

#[test]
fn toggle_block_comment_wraps_selection() {
    let mut e = editor_with("let x = 1;");
    e.lang = Some(LangKind::Rust);
    e.selection = Some(EditorSelection {
        anchor: (0, 4),
        head: (0, 5),
    });
    assert!(e.toggle_block_comment());
    assert_eq!(e.lines, vec!["let /* x */ = 1;"]);
}

#[test]
fn toggle_block_comment_unwraps_when_already_wrapped() {
    let mut e = editor_with("let /* x */ = 1;");
    e.lang = Some(LangKind::Rust);
    e.selection = Some(EditorSelection {
        anchor: (0, 4),
        head: (0, 11),
    });
    assert!(e.toggle_block_comment());
    assert_eq!(e.lines, vec!["let x = 1;"]);
}

#[test]
fn toggle_block_comment_python_wraps_in_triple_quotes() {
    // VS Code's Python language config defines blockComment as `""" """`,
    // so Shift+Alt+A wraps the selection in a triple-quoted string.
    let mut e = editor_with("x = 1");
    e.lang = Some(LangKind::Python);
    e.selection = Some(EditorSelection {
        anchor: (0, 0),
        head: (0, 5),
    });
    assert!(e.toggle_block_comment());
    assert_eq!(e.lines, vec!["\"\"\" x = 1 \"\"\""]);
}

#[test]
fn toggle_block_comment_python_unwraps_triple_quotes() {
    let mut e = editor_with("\"\"\" x = 1 \"\"\"");
    e.lang = Some(LangKind::Python);
    e.selection = Some(EditorSelection {
        anchor: (0, 0),
        head: (0, 13),
    });
    assert!(e.toggle_block_comment());
    assert_eq!(e.lines, vec!["x = 1"]);
}

#[test]
fn toggle_block_comment_keeps_the_indent_and_the_next_line() {
    let mut e = editor_with("    foo\nbar");
    e.lang = Some(LangKind::Rust);
    // A whole-line selection, as Shift+Down makes it.
    e.selection = Some(EditorSelection {
        anchor: (0, 0),
        head: (1, 0),
    });
    assert!(e.toggle_block_comment());
    assert_eq!(e.lines, vec!["    /* foo */", "bar"]);
    assert_eq!((e.cursor_row, e.cursor_col), (0, 13));
    // No selection: the current line, indent kept, and back again.
    e.cursor_row = 0;
    assert!(e.toggle_block_comment());
    assert_eq!(e.lines, vec!["    foo", "bar"]);
}

#[test]
fn carets_that_meet_merge_so_the_next_key_edits_once() {
    let mut e = editor_with("ab\ncd\nef");
    e.cursor_row = 2;
    e.cursor_col = 2;
    e.carets = vec![EditorSelection::new(2, 1)];
    e.multi_backspace();
    assert_eq!(e.lines, vec!["ab", "cd", ""]);
    assert!(e.carets.is_empty(), "{:?}", e.carets);
    e.multi_backspace();
    assert_eq!(e.lines, vec!["ab", "cd"]);
}

#[test]
fn pasting_crlf_or_cr_text_splits_lines_without_stray_returns() {
    let mut e = editor_with("");
    e.insert_str_as("a\r\nb\rc", crate::provenance::Seat::Me);
    assert_eq!(e.lines, vec!["a", "b", "c"]);
}

#[test]
fn moving_or_sorting_lines_drops_the_folds_over_them() {
    let mut e = editor_with("let y = 1;\nfn a() {\n    body\n}\ntail");
    e.toggle_fold(1);
    assert!(e.is_line_hidden(2));
    e.cursor_row = 0;
    e.move_lines_down();
    assert_eq!(e.lines[0], "fn a() {");
    assert!(
        !e.is_line_hidden(2),
        "the fold no longer hides `let y` 's neighbours"
    );
    let mut e = editor_with("b\n  b1\n  b2\na");
    e.toggle_fold(0);
    assert!(e.is_line_hidden(1));
    e.sort_lines(true);
    assert!((0..4).all(|r| !e.is_line_hidden(r)), "{:?}", e.lines);
}

#[test]
fn sorting_lines_clamps_the_cursor_so_word_motion_cannot_panic() {
    let mut e = editor_with("zzzzzzzzzz\nb\na");
    e.cursor_row = 0;
    e.cursor_col = 10;
    e.selection = Some(EditorSelection {
        anchor: (0, 0),
        head: (2, 1),
    });
    e.sort_lines(true);
    assert_eq!(e.lines, vec!["a", "b", "zzzzzzzzzz"]);
    assert!(e.cursor_col <= 1);
    e.selection = None;
    e.move_word_left();
}

// ---- Join Lines ----

#[test]
fn join_lines_merges_current_with_next() {
    let mut e = editor_with("hello\n    world");
    e.cursor_row = 0;
    e.join_lines();
    assert_eq!(e.lines, vec!["hello world"]);
}

#[test]
fn join_lines_merges_whole_selection() {
    let mut e = editor_with("a\n  b\n  c\nd");
    e.selection = Some(EditorSelection {
        anchor: (0, 0),
        head: (2, 3),
    });
    e.cursor_row = 2;
    e.join_lines();
    assert_eq!(e.lines, vec!["a b c", "d"]);
}

#[test]
fn join_lines_on_last_line_is_noop() {
    let mut e = editor_with("a\nb");
    e.cursor_row = 1;
    e.join_lines();
    assert_eq!(e.lines, vec!["a", "b"]);
}

// ---- Transform Case ----

#[test]
fn transform_upper_on_selection() {
    let mut e = editor_with("hello world");
    e.selection = Some(EditorSelection {
        anchor: (0, 0),
        head: (0, 5),
    });
    e.transform_selection_case(CaseTransform::Upper);
    assert_eq!(e.lines, vec!["HELLO world"]);
}

#[test]
fn transform_lower_on_selection() {
    let mut e = editor_with("HELLO world");
    e.selection = Some(EditorSelection {
        anchor: (0, 0),
        head: (0, 5),
    });
    e.transform_selection_case(CaseTransform::Lower);
    assert_eq!(e.lines, vec!["hello world"]);
}

#[test]
fn transform_title_on_selection() {
    let mut e = editor_with("hello there world");
    e.selection = Some(EditorSelection {
        anchor: (0, 0),
        head: (0, 17),
    });
    e.transform_selection_case(CaseTransform::Title);
    assert_eq!(e.lines, vec!["Hello There World"]);
}

/// #956: Enter, Enter inside a block leaves the first new line blank,
/// not holding the indentation, and the next line is still indented.
#[test]
fn enter_twice_leaves_no_indentation_on_the_blank_line() {
    let mut e = editor_with("def area(w, h):\n    return w * h");
    e.cursor_row = 1;
    e.cursor_col = 16;
    e.insert_newline();
    assert_eq!(e.lines[2], "    ", "Enter indents the new line");
    e.insert_newline();
    for c in "total".chars() {
        e.insert_char(c);
    }
    assert_eq!(
        e.lines,
        ["def area(w, h):", "    return w * h", "", "    total"]
    );
    // One Ctrl+Z per Enter, as before: the clear is part of the second.
    e.undo();
    e.undo();
    assert_eq!(e.lines, ["def area(w, h):", "    return w * h", "    "]);
}

/// #956: leaving the indented line by moving off it clears it too; one
/// that got text keeps its indentation.
#[test]
fn leaving_an_indented_blank_line_clears_it_but_typing_keeps_it() {
    let mut e = editor_with("if x:\n    a = 1\n    b = 2");
    e.cursor_row = 1;
    e.cursor_col = 9;
    e.insert_newline();
    e.move_down();
    e.trim_auto_indent(false);
    assert_eq!(e.lines[2], "", "moved off with nothing typed");
    assert!(e.dirty);
    let mut e = editor_with("if x:\n    a = 1\n    b = 2");
    e.cursor_row = 1;
    e.cursor_col = 9;
    e.insert_newline();
    e.insert_char('c');
    e.move_down();
    e.trim_auto_indent(false);
    assert_eq!(e.lines[2], "    c", "typed-on lines keep their indentation");
    // A selection from the line keeps its end on the line once cleared.
    let mut e = editor_with("if x:\n    a = 1\n    b = 2");
    e.cursor_row = 1;
    e.cursor_col = 9;
    e.insert_newline();
    e.selection = Some(EditorSelection {
        anchor: (2, 4),
        head: (3, 4),
    });
    e.cursor_row = 3;
    e.cursor_col = 4;
    e.trim_auto_indent(false);
    assert_eq!(e.lines[2], "");
    assert_eq!(e.selection.map(|s| s.anchor), Some((2, 0)));
    // Negative: staying on the line keeps the indent where the caret is.
    let mut e = editor_with("if x:\n    a = 1");
    e.cursor_row = 1;
    e.cursor_col = 9;
    e.insert_newline();
    e.trim_auto_indent(false);
    assert_eq!(e.lines[2], "    ");
    assert_eq!(e.cursor_col, 4);
}

/// #1016: an apostrophe inside a word doesn't start a new one, so
/// contractions and possessives keep their lower-case tail; one that
/// opens a word (`'tis`) still capitalises the letter after it.
#[test]
fn title_case_keeps_contractions_whole() {
    assert_eq!(
        title_case("## don't stop believing: it's the journey's end"),
        "## Don't Stop Believing: It's The Journey's End"
    );
    assert_eq!(
        title_case("we'll be there by o'clock, won't we?"),
        "We'll Be There By O'clock, Won't We?"
    );
    assert_eq!(
        title_case("don\u{2019}t it\u{2019}s"),
        "Don\u{2019}t It\u{2019}s"
    );
    assert_eq!(title_case("'tis the season"), "'Tis The Season");
    assert_eq!(title_case("say 'hello' twice"), "Say 'Hello' Twice");
}

/// #1573: the four naming styles, from each other.
#[test]
fn identifiers_change_naming_style() {
    use CaseTransform::*;
    for (from, kind, want) in [
        ("user_name", Camel, "userName"),
        ("user_name", Pascal, "UserName"),
        ("user_name", Kebab, "user-name"),
        ("userName", Snake, "user_name"),
        ("UserName", Snake, "user_name"),
        ("user-name", Snake, "user_name"),
        ("parseHTTPResponse", Snake, "parse_http_response"),
        ("parseHTTPResponse", Camel, "parseHttpResponse"),
        ("HTTPServer", Kebab, "http-server"),
        ("Base64Encoder", Snake, "base64_encoder"),
        ("utf8", Pascal, "Utf8"),
        ("_private_var", Camel, "_privateVar"),
        ("__dunder__", Pascal, "__Dunder__"),
        ("user_name total_count", Camel, "userName totalCount"),
        ("a.user_name(x_y)", Pascal, "A.UserName(XY)"),
    ] {
        assert_eq!(identifier_case(from, kind), want, "{from} as {kind:?}");
    }
}

/// Negatives (#1573): text with no letters, and separators that are not
/// part of an identifier, are kept as they were.
#[test]
fn identifier_case_leaves_non_identifiers_alone() {
    assert_eq!(identifier_case("42 - 7", CaseTransform::Camel), "42 - 7");
    assert_eq!(identifier_case("___", CaseTransform::Snake), "___");
    assert_eq!(identifier_case("", CaseTransform::Kebab), "");
    assert_eq!(
        identifier_case("a_b,\n  c_d;", CaseTransform::Camel),
        "aB,\n  cD;"
    );
    assert_eq!(
        identifier_case("already_snake", CaseTransform::Snake),
        "already_snake"
    );
}

/// #1573: two carets on `a_b` and `c_d` (no selection) both change, as
/// the other transforms do.
#[test]
fn camel_case_applies_at_every_caret() {
    let mut e = editor_with("a_b = c_d");
    e.cursor_row = 0;
    e.cursor_col = 1;
    e.carets = vec![EditorSelection::new(0, 7)];
    e.transform_selection_case(CaseTransform::Camel);
    assert_eq!(e.lines, vec!["aB = cD"]);
}

/// #1573: with no selection, a naming-style transform takes the whole
/// kebab-case identifier, whether the caret is on a word or the hyphen.
#[test]
fn snake_case_caret_takes_whole_kebab_identifier() {
    for col in [5, 8, 12] {
        let mut e = editor_with("let user-name = 1;");
        e.cursor_row = 0;
        e.cursor_col = col;
        e.transform_selection_case(CaseTransform::Snake);
        assert_eq!(e.lines, vec!["let user_name = 1;"], "caret at {col}");
    }
}

/// #1573: with no selection, a minus is not part of the identifier, so
/// a transform on `total` in `total-5` leaves the subtraction alone.
#[test]
fn camel_case_caret_stops_at_a_minus() {
    for (text, col) in [("let x = total-5;", 9), ("let x = -count;", 10)] {
        let mut e = editor_with(text);
        e.cursor_row = 0;
        e.cursor_col = col;
        e.transform_selection_case(CaseTransform::Camel);
        assert_eq!(e.lines, vec![text], "caret at {col} in {text}");
    }
}

#[test]
fn transform_upper_falls_back_to_word_at_cursor() {
    let mut e = editor_with("alpha beta");
    e.cursor_row = 0;
    e.cursor_col = 7; // inside "beta"
    e.transform_selection_case(CaseTransform::Upper);
    assert_eq!(e.lines, vec!["alpha BETA"]);
}

// ---- Line commands with several selections (#1467) ----

/// The issue's buffer with every `foo` selected by three Ctrl+D presses.
fn three_foo_selections() -> Editor {
    let mut e = editor_with("let foo = 1;\nlet bar = foo + 2;\nlet baz = foo * 3;");
    e.lang = Some(LangKind::JavaScript);
    e.cursor_row = 0;
    e.cursor_col = 5;
    for _ in 0..3 {
        e.select_next_occurrence();
    }
    assert_eq!(e.carets.len(), 2, "three selections to start from");
    e
}

/// The text under the primary selection and under every extra caret.
fn selected_texts(e: &Editor) -> Vec<String> {
    let mut out: Vec<String> = e
        .carets
        .iter()
        .chain(e.selection.iter())
        .map(|s| {
            let (a, b) = s.normalised();
            e.char_range_text(a, b)
        })
        .collect();
    out.sort();
    out
}

/// A bare caret at each `(row, col)`, the first one primary.
fn carets_at(e: &mut Editor, at: &[(usize, usize)]) {
    e.selection = None;
    e.cursor_row = at[0].0;
    e.cursor_col = at[0].1;
    e.carets = at[1..]
        .iter()
        .map(|&(r, c)| EditorSelection::new(r, c))
        .collect();
}

/// Every caret's row, the primary's included, in order.
fn caret_rows(e: &Editor) -> Vec<usize> {
    let mut rows: Vec<usize> = e.carets.iter().map(|s| s.head.0).collect();
    rows.push(e.cursor_row);
    rows.sort_unstable();
    rows
}

/// #1637: Increment and Decrement Number bump the number after every
/// caret, in one undo step, and keep every caret on its number.
#[test]
fn bump_number_changes_the_number_after_every_caret() {
    let mut e = three_foo_selections();
    assert!(e.bump_number(1));
    assert_eq!(
        e.lines,
        vec!["let foo = 2;", "let bar = foo + 3;", "let baz = foo * 4;"]
    );
    assert_eq!(e.carets.len(), 2, "no caret is dropped");
    let mut at: Vec<(usize, usize)> = e.carets.iter().map(|s| s.head).collect();
    at.push((e.cursor_row, e.cursor_col));
    at.sort_unstable();
    assert_eq!(at, vec![(0, 10), (1, 16), (2, 16)], "each on its number");
    assert!(e.undo());
    assert_eq!(
        e.lines,
        vec!["let foo = 1;", "let bar = foo + 2;", "let baz = foo * 3;"]
    );
    let mut e = three_foo_selections();
    assert!(e.bump_number(-1));
    assert_eq!(
        e.lines,
        vec!["let foo = 0;", "let bar = foo + 1;", "let baz = foo * 2;"]
    );
}

/// #1637: two carets on one number bump it once, two numbers on a line
/// with a caret before each both change, and a caret with no number
/// after it is kept where it was.
#[test]
fn bump_number_bumps_a_shared_number_once() {
    let mut e = editor_with("x = 19; y = 5;");
    carets_at(&mut e, &[(0, 4), (0, 5)]);
    assert!(e.bump_number(1));
    assert_eq!(e.lines, vec!["x = 20; y = 5;"]);
    let mut e = editor_with("x = 19; y = 5;");
    carets_at(&mut e, &[(0, 0), (0, 8)]);
    assert!(e.bump_number(10));
    assert_eq!(e.lines, vec!["x = 29; y = 15;"]);
    assert_eq!(e.carets.len(), 1);
    let mut e = editor_with("n = 1\nno number");
    carets_at(&mut e, &[(0, 0), (1, 3)]);
    assert!(e.bump_number(1));
    assert_eq!(e.lines, vec!["n = 2", "no number"]);
    assert_eq!(e.carets[0].head, (1, 3), "the caret with no number stays");
}

/// #1637: Transpose Characters swaps around every caret, in one undo
/// step, and each caret steps past its swap.
#[test]
fn transpose_swaps_at_every_caret() {
    let mut e = editor_with("let foo = 1;\nlet bar = foo + 2;\nlet baz = foo * 3;");
    carets_at(&mut e, &[(2, 5), (0, 5), (1, 5)]);
    e.transpose_chars();
    assert_eq!(
        e.lines,
        vec!["let ofo = 1;", "let abr = foo + 2;", "let abz = foo * 3;"]
    );
    assert_eq!(caret_rows(&e), vec![0, 1, 2]);
    assert_eq!(
        (e.cursor_row, e.cursor_col),
        (2, 6),
        "the primary stays primary"
    );
    assert!(e.carets.iter().all(|s| s.head.1 == 6));
    assert!(e.undo());
    assert_eq!(e.lines[0], "let foo = 1;");
    assert_eq!(e.lines[2], "let baz = foo * 3;");
}

/// #1637: two carets whose swaps share a character do not act on text
/// the other already moved: `ab` with carets after `a` and after `b`
/// moves `b` to the next line once, and leaves `a` where it was.
#[test]
fn overlapping_transpose_carets_do_not_compound() {
    let mut e = editor_with("ab\nx");
    carets_at(&mut e, &[(0, 1), (0, 2)]);
    e.transpose_chars();
    assert_eq!(e.lines, vec!["a", "bx"]);
    // Two carets with disjoint swaps on one line both still swap.
    let mut e = editor_with("abcd");
    carets_at(&mut e, &[(0, 1), (0, 3)]);
    e.transpose_chars();
    assert_eq!(e.lines, vec!["badc"]);
}

/// #1637 negative: one caret bumps and transposes as before, and a line
/// with no number is still reported as nothing to bump.
#[test]
fn a_single_caret_bumps_and_transposes_as_before() {
    let mut e = editor_with("x = 41");
    assert!(e.bump_number(1));
    assert_eq!(e.lines, vec!["x = 42"]);
    assert_eq!((e.cursor_row, e.cursor_col), (0, 5));
    assert!(e.carets.is_empty());
    let mut e = editor_with("abc\ndef");
    e.cursor_col = 1;
    e.transpose_chars();
    assert_eq!(e.lines, vec!["bac", "def"]);
    assert_eq!((e.cursor_row, e.cursor_col), (0, 2));
    e.cursor_col = 3;
    e.transpose_chars();
    assert_eq!(e.lines, vec!["ba", "cdef"], "end of line moves to the next");
    assert_eq!((e.cursor_row, e.cursor_col), (1, 1));
    assert!(!editor_with("none").bump_number(1));
}

#[test]
fn transform_case_changes_every_selection_and_keeps_them() {
    let mut e = three_foo_selections();
    e.transform_selection_case(CaseTransform::Upper);
    assert_eq!(
        e.lines,
        vec!["let FOO = 1;", "let bar = FOO + 2;", "let baz = FOO * 3;"]
    );
    assert_eq!(e.carets.len(), 2, "no selection is dropped");
    assert_eq!(selected_texts(&e), vec!["FOO", "FOO", "FOO"]);
    // One undo puts all three back.
    assert!(e.undo());
    assert_eq!(
        e.lines,
        vec!["let foo = 1;", "let bar = foo + 2;", "let baz = foo * 3;"]
    );
}

#[test]
fn transform_case_with_bare_carets_changes_the_word_under_each() {
    let mut e = editor_with("alpha beta\ngamma delta");
    carets_at(&mut e, &[(1, 7), (0, 1)]);
    e.transform_selection_case(CaseTransform::Upper);
    assert_eq!(e.lines, vec!["ALPHA beta", "gamma DELTA"]);
    assert_eq!(e.carets.len(), 1);
    assert_eq!((e.cursor_row, e.cursor_col), (1, 7), "the caret stays put");
}

#[test]
fn transform_case_with_two_selections_on_one_line_changes_both() {
    let mut e = editor_with("foo bar foo");
    e.selection = Some(EditorSelection {
        anchor: (0, 8),
        head: (0, 11),
    });
    e.carets = vec![EditorSelection {
        anchor: (0, 0),
        head: (0, 3),
    }];
    e.transform_selection_case(CaseTransform::Title);
    assert_eq!(e.lines, vec!["Foo bar Foo"]);
    assert_eq!(selected_texts(&e), vec!["Foo", "Foo"]);
}

#[test]
fn transform_case_rewrites_all_of_nested_selections() {
    let mut e = editor_with("hello world");
    e.selection = Some(EditorSelection {
        anchor: (0, 2),
        head: (0, 3),
    });
    e.carets = vec![EditorSelection {
        anchor: (0, 0),
        head: (0, 5),
    }];
    e.transform_selection_case(CaseTransform::Upper);
    assert_eq!(e.lines, vec!["HELLO world"]);
    // One inside the other is an overlap: they come back as one (#1570).
    assert_eq!(selected_texts(&e), vec!["HELLO"]);
}

#[test]
fn transform_case_rewrites_all_of_partly_overlapping_selections() {
    let mut e = editor_with("hello world");
    e.selection = Some(EditorSelection {
        anchor: (0, 8),
        head: (0, 3),
    });
    e.carets = vec![EditorSelection {
        anchor: (0, 0),
        head: (0, 5),
    }];
    e.transform_selection_case(CaseTransform::Upper);
    assert_eq!(e.lines, vec!["HELLO WOrld"]);
    // The two overlap, so they come back as one (#1570): kept apart,
    // typing over them would edit the shared text twice.
    assert_eq!(selected_texts(&e), vec!["HELLO WO"]);
    assert_eq!(e.selection.unwrap().head, (0, 0), "direction is kept");
}

#[test]
fn toggle_line_comment_comments_every_selected_line_and_shifts_the_selections() {
    let mut e = three_foo_selections();
    assert!(e.toggle_line_comment());
    assert_eq!(
        e.lines,
        vec![
            "// let foo = 1;",
            "// let bar = foo + 2;",
            "// let baz = foo * 3;"
        ]
    );
    assert_eq!(e.carets.len(), 2, "no selection is dropped");
    // Each selection moved right with its text, so it still covers foo.
    assert_eq!(selected_texts(&e), vec!["foo", "foo", "foo"]);
    // And toggling again uncomments all three.
    assert!(e.toggle_line_comment());
    assert_eq!(
        e.lines,
        vec!["let foo = 1;", "let bar = foo + 2;", "let baz = foo * 3;"]
    );
    assert_eq!(selected_texts(&e), vec!["foo", "foo", "foo"]);
}

#[test]
fn toggle_line_comment_comments_a_line_with_two_carets_once() {
    let mut e = editor_with("let a = 1;\nlet b = 2;\nlet c = 3;");
    e.lang = Some(LangKind::JavaScript);
    carets_at(&mut e, &[(0, 1), (0, 5), (2, 0)]);
    assert!(e.toggle_line_comment());
    assert_eq!(
        e.lines,
        vec!["// let a = 1;", "let b = 2;", "// let c = 3;"]
    );
}

#[test]
fn toggle_line_comment_leaves_a_caret_inside_the_indent_alone() {
    let mut e = editor_with("    let x = 1;");
    e.lang = Some(LangKind::Rust);
    carets_at(&mut e, &[(0, 2)]);
    assert!(e.toggle_line_comment());
    assert_eq!(e.lines, vec!["    // let x = 1;"]);
    assert_eq!(e.cursor_col, 2);
}

#[test]
fn toggle_block_comment_wraps_every_selection() {
    let mut e = three_foo_selections();
    assert!(e.toggle_block_comment());
    assert_eq!(
        e.lines,
        vec![
            "let /* foo */ = 1;",
            "let bar = /* foo */ + 2;",
            "let baz = /* foo */ * 3;"
        ]
    );
    assert_eq!(e.carets.len(), 2, "a caret after each comment");
    assert_eq!((e.cursor_row, e.cursor_col), (2, 19));
}

#[test]
fn duplicate_lines_down_copies_every_caret_line() {
    let mut e = editor_with("a\nb\nc");
    carets_at(&mut e, &[(2, 0), (0, 0)]);
    e.duplicate_lines_down();
    assert_eq!(e.lines, vec!["a", "a", "b", "c", "c"]);
    assert_eq!(caret_rows(&e), vec![1, 4], "each caret moves onto its copy");
    assert!(e.undo());
    assert_eq!(e.lines, vec!["a", "b", "c"]);
}

#[test]
fn duplicate_lines_up_copies_every_caret_line() {
    let mut e = editor_with("a\nb\nc");
    carets_at(&mut e, &[(2, 0), (0, 0)]);
    e.duplicate_lines_up();
    assert_eq!(e.lines, vec!["a", "a", "b", "c", "c"]);
    assert_eq!(
        caret_rows(&e),
        vec![0, 3],
        "each caret stays on the upper copy"
    );
}

#[test]
fn duplicate_lines_down_copies_adjacent_caret_lines_as_one_block() {
    let mut e = editor_with("a\nb\nc");
    carets_at(&mut e, &[(1, 0), (0, 0)]);
    e.duplicate_lines_down();
    assert_eq!(e.lines, vec!["a", "b", "a", "b", "c"]);
    assert_eq!(caret_rows(&e), vec![2, 3]);
}

#[test]
fn move_lines_down_moves_every_caret_line() {
    let mut e = editor_with("a\nb\nc\nd");
    carets_at(&mut e, &[(2, 0), (0, 0)]);
    e.move_lines_down();
    assert_eq!(e.lines, vec!["b", "a", "d", "c"]);
    assert_eq!(caret_rows(&e), vec![1, 3]);
}

#[test]
fn move_lines_down_is_a_noop_when_a_caret_is_on_the_last_line() {
    let mut e = editor_with("a\nb\nc");
    carets_at(&mut e, &[(2, 0), (0, 0)]);
    e.move_lines_down();
    assert_eq!(e.lines, vec!["a", "b", "c"]);
    assert_eq!(caret_rows(&e), vec![0, 2]);
}

#[test]
fn move_lines_up_moves_every_caret_line() {
    let mut e = editor_with("a\nb\nc\nd");
    carets_at(&mut e, &[(3, 0), (1, 0)]);
    e.move_lines_up();
    assert_eq!(e.lines, vec!["b", "a", "d", "c"]);
    assert_eq!(caret_rows(&e), vec![0, 2]);
}

#[test]
fn move_lines_up_is_a_noop_when_a_caret_is_on_the_first_line() {
    let mut e = editor_with("a\nb\nc");
    carets_at(&mut e, &[(2, 0), (0, 0)]);
    e.move_lines_up();
    assert_eq!(e.lines, vec!["a", "b", "c"]);
}

#[test]
fn join_lines_joins_at_every_caret() {
    let mut e = editor_with("a\n  b\nc\n  d");
    carets_at(&mut e, &[(2, 0), (0, 0)]);
    e.join_lines();
    assert_eq!(e.lines, vec!["a b", "c d"]);
    assert_eq!(e.carets.len(), 1);
    assert_eq!((e.cursor_row, e.cursor_col), (1, 1));
    assert_eq!(e.carets[0].head, (0, 1));
}

#[test]
fn join_lines_keeps_a_caret_on_the_last_line_that_has_nothing_to_join() {
    let mut e = editor_with("a\nb\nc");
    carets_at(&mut e, &[(2, 0), (0, 0)]);
    e.join_lines();
    assert_eq!(e.lines, vec!["a b", "c"]);
    assert_eq!(caret_rows(&e), vec![0, 1]);
}

#[test]
fn sort_lines_sorts_each_selection_on_its_own() {
    let mut e = editor_with("c\nb\na\nz\ny\nx\n0");
    e.selection = Some(EditorSelection {
        anchor: (0, 0),
        head: (2, 1),
    });
    e.carets = vec![EditorSelection {
        anchor: (3, 0),
        head: (5, 1),
    }];
    e.sort_lines(true);
    assert_eq!(e.lines, vec!["a", "b", "c", "x", "y", "z", "0"]);
    assert_eq!(e.carets.len(), 1, "the selections are kept");
}

#[test]
fn a_single_caret_still_moves_only_its_own_line() {
    let mut e = editor_with("a\nb\nc");
    carets_at(&mut e, &[(0, 0)]);
    e.move_lines_down();
    assert_eq!(e.lines, vec!["b", "a", "c"]);
    assert_eq!(caret_rows(&e), vec![1]);
}

// ---- Folds follow line inserts and deletes (#1509) ----

const FOLDS_PY: &str = "import os\n\n\ndef alpha():\n    a = 1\n    b = 2\n    return a + b\n\n\ndef beta():\n    return 3\n\n\ndef gamma():\n    x = 1\n    y = 2\n    z = 3\n    return x + y + z\n";

/// The issue's file open from disk, with `alpha` (row 3) and `beta`
/// (row 9) folded.
fn folded_alpha_and_beta() -> (tempfile::TempDir, Editor) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.py");
    std::fs::write(&path, FOLDS_PY).unwrap();
    let mut e = Editor::new();
    e.open(&path).unwrap();
    e.toggle_fold(3);
    e.toggle_fold(9);
    assert_eq!(e.folded, std::collections::BTreeSet::from([3, 9]));
    (dir, e)
}

/// Paint a frame: the render pass is where folds used to be dropped.
fn paint(e: &mut Editor) {
    let area = Rect::new(0, 0, 80, 30);
    let mut buf = Buffer::empty(area);
    (&mut *e).render(area, &mut buf);
}

fn folded_rows(e: &Editor) -> Vec<usize> {
    e.folded.iter().copied().collect()
}

#[test]
fn a_new_line_below_the_folds_keeps_them_folded() {
    let (_dir, mut e) = folded_alpha_and_beta();
    e.cursor_row = 17;
    e.cursor_col = e.lines[17].chars().count();
    e.insert_newline();
    for c in "w = 4".chars() {
        e.insert_char(c);
    }
    paint(&mut e);
    assert_eq!(folded_rows(&e), vec![3, 9]);
    assert!(
        e.is_line_hidden(4) && e.is_line_hidden(10),
        "bodies stay hidden"
    );
}

#[test]
fn a_new_line_above_the_folds_moves_them_down() {
    let (_dir, mut e) = folded_alpha_and_beta();
    e.cursor_row = 0;
    e.cursor_col = e.lines[0].chars().count();
    e.insert_newline();
    paint(&mut e);
    assert_eq!(folded_rows(&e), vec![4, 10]);
    assert_eq!(e.lines[4], "def alpha():");
    assert!(e.is_line_hidden(5) && !e.is_line_hidden(4));
}

#[test]
fn a_line_deleted_between_the_folds_moves_only_the_lower_one() {
    let (_dir, mut e) = folded_alpha_and_beta();
    // The blank line 8 (row 7): backspace from the start of row 8.
    e.cursor_row = 8;
    e.cursor_col = 0;
    e.backspace();
    paint(&mut e);
    assert_eq!(folded_rows(&e), vec![3, 8]);
    assert_eq!(e.lines[8], "def beta():");
}

#[test]
fn undo_and_paste_keep_the_folds_too() {
    let (_dir, mut e) = folded_alpha_and_beta();
    e.cursor_row = 0;
    e.cursor_col = 0;
    e.insert_str("# one\n# two\n");
    paint(&mut e);
    assert_eq!(folded_rows(&e), vec![5, 11]);
    assert!(e.undo());
    paint(&mut e);
    assert_eq!(folded_rows(&e), vec![3, 9]);
}

/// An edit that keeps the line count (one line gone above the folds,
/// one added below) still moves the folded headers with their text.
#[test]
fn a_net_zero_line_count_edit_still_moves_the_folds() {
    let (_dir, mut e) = folded_alpha_and_beta();
    let mut lines = e.lines[1..].to_vec();
    lines.push("# tail".to_string());
    assert_eq!(lines.len(), e.lines.len());
    e.replace_all_lines(lines);
    paint(&mut e);
    assert_eq!(folded_rows(&e), vec![2, 8]);
    assert_eq!(e.lines[2], "def alpha():");
    assert!(e.is_line_hidden(3) && !e.is_line_hidden(2));
}

/// The #1611 file, with `def b()` (row 4, body rows 5-6) folded.
fn folded_b() -> Editor {
    let mut e =
        editor_with("def a():\n    return 1\n\n\ndef b():\n    x = 1\n    return x\nlast = 3");
    e.toggle_fold(4);
    assert_eq!(folded_rows(&e), vec![4]);
    assert!(e.is_line_hidden(5) && e.is_line_hidden(6));
    e
}

/// #1611: Move Line Up below a folded function moves the line above the
/// whole function, not into its hidden body, and the fold stays closed.
#[test]
fn move_line_up_below_a_folded_function_moves_above_it() {
    let mut e = folded_b();
    (e.cursor_row, e.cursor_col) = (7, 0);
    e.move_lines_up();
    assert_eq!(
        e.lines[3..],
        ["", "last = 3", "def b():", "    x = 1", "    return x"]
    );
    assert_eq!(e.cursor_row, 4);
    paint(&mut e);
    assert_eq!(folded_rows(&e), vec![5], "the fold moved with its text");
    assert!(e.is_line_hidden(6) && e.is_line_hidden(7));
    assert!(e.undo());
    assert_eq!(e.lines[7], "last = 3");
}

/// #1611: Move Line Down on a folded header moves the header and its
/// body together, and a line above a fold moves below all of it.
#[test]
fn move_line_down_moves_a_folded_function_whole() {
    let mut e = folded_b();
    (e.cursor_row, e.cursor_col) = (4, 0);
    e.move_lines_down();
    assert_eq!(
        e.lines[3..],
        ["", "last = 3", "def b():", "    x = 1", "    return x"]
    );
    assert_eq!(e.cursor_row, 5, "the caret stays on the header");
    paint(&mut e);
    assert_eq!(folded_rows(&e), vec![5]);
    assert!(e.is_line_hidden(6) && e.is_line_hidden(7));

    let mut e = folded_b();
    (e.cursor_row, e.cursor_col) = (3, 0);
    e.move_lines_down();
    assert_eq!(
        e.lines[3..],
        ["def b():", "    x = 1", "    return x", "", "last = 3"]
    );
    assert_eq!(e.cursor_row, 6);
    paint(&mut e);
    assert_eq!(folded_rows(&e), vec![3]);
    assert!(e.is_line_hidden(4) && e.is_line_hidden(5));
}

/// #1611: a fold from the language server stays closed over its text
/// through a Move Line and its undo: the undo brings back the spans that
/// described the restored text, not the ones the move rewrote.
#[test]
fn undoing_a_move_keeps_a_server_fold_closed() {
    use crate::lsp::manager::{FoldRangeKind, FoldingRangeItem};
    let mut e = editor_with("a\nb {\nc\nd\n}\ne");
    e.set_lsp_folds(vec![FoldingRangeItem {
        start_line: 1,
        end_line: 3,
        kind: FoldRangeKind::Other,
    }]);
    e.toggle_fold(1);
    assert!(e.is_line_hidden(2) && e.is_line_hidden(3));
    (e.cursor_row, e.cursor_col) = (1, 0);
    e.move_lines_down();
    paint(&mut e);
    assert_eq!(folded_rows(&e), vec![2], "{:?}", e.lines);
    assert!(e.undo());
    paint(&mut e);
    assert_eq!(e.lines[1], "b {");
    assert_eq!(folded_rows(&e), vec![1]);
    assert!(
        e.is_line_hidden(2) && e.is_line_hidden(3),
        "the server's fold is closed again over its rows"
    );
}

/// #1611: Copy Line Down on a folded header copies the whole function.
#[test]
fn copy_line_down_on_a_folded_header_copies_its_body_too() {
    let mut e = folded_b();
    (e.cursor_row, e.cursor_col) = (4, 0);
    e.duplicate_lines_down();
    assert_eq!(
        e.lines[4..],
        [
            "def b():",
            "    x = 1",
            "    return x",
            "def b():",
            "    x = 1",
            "    return x",
            "last = 3"
        ]
    );
    assert_eq!(e.cursor_row, 7, "the caret is on the copy's header");
}

/// #1611 negative: with no fold, or with the fold open, Move Line swaps
/// one line as before.
#[test]
fn move_line_without_a_closed_fold_swaps_one_line() {
    let mut e =
        editor_with("def a():\n    return 1\n\n\ndef b():\n    x = 1\n    return x\nlast = 3");
    (e.cursor_row, e.cursor_col) = (7, 0);
    e.move_lines_up();
    assert_eq!(e.lines[6..], ["last = 3", "    return x"]);
    let mut e = folded_b();
    e.toggle_fold(4);
    assert!(e.folded.is_empty());
    (e.cursor_row, e.cursor_col) = (4, 0);
    e.move_lines_down();
    assert_eq!(e.lines[4..6], ["    x = 1", "def b():"]);
    e.duplicate_lines_down();
    assert_eq!(e.lines[5..7], ["def b():", "def b():"]);
}

/// An untitled buffer has no path, and its folds follow edits too.
#[test]
fn an_untitled_buffers_folds_follow_a_new_line_above_them() {
    let mut e = Editor::new();
    assert!(e.path.is_none());
    e.insert_str(FOLDS_PY);
    e.toggle_fold(3);
    e.toggle_fold(9);
    assert_eq!(folded_rows(&e), vec![3, 9]);
    e.cursor_row = 0;
    e.cursor_col = e.lines[0].chars().count();
    e.insert_newline();
    paint(&mut e);
    assert_eq!(folded_rows(&e), vec![4, 10]);
    assert_eq!(e.lines[4], "def alpha():");
}

/// Negative: deleting a folded header's own line drops that fold only.
#[test]
fn deleting_a_folded_header_line_drops_only_that_fold() {
    let (_dir, mut e) = folded_alpha_and_beta();
    e.unfold_all();
    e.toggle_fold(3);
    e.toggle_fold(13);
    // Delete the whole `def beta():` line (row 9) and its body.
    e.selection = Some(EditorSelection {
        anchor: (9, 0),
        head: (11, 0),
    });
    e.cursor_row = 11;
    e.cursor_col = 0;
    e.delete_selection();
    paint(&mut e);
    assert_eq!(folded_rows(&e), vec![3, 11], "gamma moved up by two");
    assert_eq!(e.lines[11], "def gamma():");
}

/// Negative: an edit inside a folded body still opens that fold (the
/// caret goes where the user is typing), and only that one.
#[test]
fn an_edit_inside_a_folded_body_opens_only_that_fold() {
    let (_dir, mut e) = folded_alpha_and_beta();
    e.cursor_row = 5;
    e.cursor_col = 9;
    e.insert_newline();
    paint(&mut e);
    assert_eq!(folded_rows(&e), vec![10]);
}

// ---- Sort Lines ----

#[test]
fn sort_lines_ascending_over_selection() {
    let mut e = editor_with("banana\napple\ncherry");
    e.selection = Some(EditorSelection {
        anchor: (0, 0),
        head: (2, 6),
    });
    e.sort_lines(true);
    assert_eq!(e.lines, vec!["apple", "banana", "cherry"]);
}

#[test]
fn sort_lines_descending_over_selection() {
    let mut e = editor_with("apple\nbanana\ncherry");
    e.selection = Some(EditorSelection {
        anchor: (0, 0),
        head: (2, 6),
    });
    e.sort_lines(false);
    assert_eq!(e.lines, vec!["cherry", "banana", "apple"]);
}

#[test]
fn sort_lines_without_selection_sorts_whole_buffer() {
    let mut e = editor_with("c\na\nb");
    e.selection = None;
    e.sort_lines(true);
    assert_eq!(e.lines, vec!["a", "b", "c"]);
}

// ---- Trim Trailing Whitespace ----

#[test]
fn trim_trailing_whitespace_strips_every_line() {
    let mut e = editor_with("a   \nb\t\n  c  ");
    assert!(e.trim_trailing_whitespace());
    assert_eq!(e.lines, vec!["a", "b", "  c"]);
}

#[test]
fn trim_trailing_whitespace_clamps_cursor() {
    let mut e = editor_with("abc    ");
    e.cursor_row = 0;
    e.cursor_col = 7;
    assert!(e.trim_trailing_whitespace());
    assert_eq!(e.cursor_col, 3, "cursor clamps to trimmed end");
}

#[test]
fn trim_trailing_whitespace_noop_when_clean() {
    let mut e = editor_with("a\nb");
    assert!(!e.trim_trailing_whitespace());
    assert_eq!(e.lines, vec!["a", "b"]);
}

// ---- Insert Cursor Above / Below ----

#[test]
fn add_cursor_below_pushes_caret_on_next_row() {
    let mut e = editor_with("aaaa\nbbbb\ncccc");
    e.cursor_row = 0;
    e.cursor_col = 2;
    e.add_cursor_below();
    assert_eq!(e.carets.len(), 1);
    let c = e.carets[0];
    assert_eq!(c.head, (1, 2));
    assert_eq!(c.anchor, (1, 2), "zero-width caret");
}

#[test]
fn add_cursor_above_pushes_caret_on_previous_row() {
    let mut e = editor_with("aaaa\nbbbb\ncccc");
    e.cursor_row = 2;
    e.cursor_col = 3;
    e.add_cursor_above();
    assert_eq!(e.carets.len(), 1);
    assert_eq!(e.carets[0].head, (1, 3));
}

#[test]
fn add_cursor_below_clamps_to_short_line() {
    let mut e = editor_with("aaaa\nbb");
    e.cursor_row = 0;
    e.cursor_col = 4;
    e.add_cursor_below();
    assert_eq!(e.carets[0].head, (1, 2), "column clamps to line length");
}

#[test]
fn add_cursor_below_at_last_row_is_noop() {
    let mut e = editor_with("aaaa\nbbbb");
    e.cursor_row = 1;
    e.cursor_col = 1;
    e.add_cursor_below();
    assert!(e.carets.is_empty());
}

#[test]
fn document_link_at_answers_inside_the_range_for_the_stamped_file_only() {
    use crate::lsp::manager::DocumentLinkItem;
    let mut e = editor_with("// see https://example.com/docs for more");
    e.path = Some(std::path::PathBuf::from("/tmp/a.rs"));
    e.apply_document_links(
        std::path::PathBuf::from("/tmp/a.rs"),
        vec![DocumentLinkItem {
            line: 0,
            character: 7,
            end_line: 0,
            end_character: 31,
            target: String::from("https://example.com/docs"),
        }],
    );
    assert_eq!(e.document_link_at(0, 7), Some("https://example.com/docs"));
    assert_eq!(e.document_link_at(0, 30), Some("https://example.com/docs"));
    assert_eq!(e.document_link_at(0, 31), None, "range end is exclusive");
    assert_eq!(e.document_link_at(0, 6), None);
    // A set stamped for another file never answers.
    e.path = Some(std::path::PathBuf::from("/tmp/b.rs"));
    assert_eq!(e.document_link_at(0, 10), None);
}

#[test]
fn linked_editing_mirrors_typing_and_deletion_across_paired_tags() {
    let mut e = editor_with("<title>\n  text\n</title>");
    // Spans over both tag names, UTF-16 == char cols here.
    e.set_linked_ranges(&[(0, 1, 0, 6), (2, 2, 2, 7)]);
    assert!(e.has_linked_ranges());
    // Type 's' at the end of the opening tag name.
    e.cursor_row = 0;
    e.cursor_col = 6;
    e.insert_char('s');
    assert!(e.mirror_linked_edit(), "the keystroke mirrors");
    assert_eq!(e.lines[0], "<titles>");
    assert_eq!(e.lines[2], "</titles>");
    // Keep typing: positions were resynced, the set is still live.
    e.insert_char('x');
    assert!(e.mirror_linked_edit());
    assert_eq!(e.lines[2], "</titlesx>");
    // Backspace mirrors too.
    e.backspace();
    assert!(e.mirror_linked_edit());
    assert_eq!(e.lines[0], "<titles>");
    assert_eq!(e.lines[2], "</titles>");
    // One Undo reverts the keystroke AND its mirror together, and
    // the restore drops the set so nothing replays on top.
    assert!(e.undo());
    assert!(!e.mirror_linked_edit());
    assert!(!e.has_linked_ranges());
    assert_eq!(e.lines[0], e.lines[0].clone(), "no panic path");
}

#[test]
fn linked_editing_handles_a_same_row_pair_with_shifting_offsets() {
    let mut e = editor_with("<b>hi</b>");
    e.set_linked_ranges(&[(0, 1, 0, 2), (0, 7, 0, 8)]);
    e.cursor_row = 0;
    e.cursor_col = 2; // end of the opening tag name
    e.insert_char('r');
    assert!(e.mirror_linked_edit());
    assert_eq!(e.lines[0], "<br>hi</br>");
    // And again — the closing tag's start shifted and must resync.
    e.insert_char('x');
    assert!(e.mirror_linked_edit());
    assert_eq!(e.lines[0], "<brx>hi</brx>");
}

#[test]
fn linked_editing_drops_the_set_on_delimiters_and_structural_edits() {
    // A delimiter character must not propagate.
    let mut e = editor_with("<div>\n</div>");
    e.set_linked_ranges(&[(0, 1, 0, 4), (1, 2, 1, 5)]);
    e.cursor_row = 0;
    e.cursor_col = 4;
    e.insert_char(' ');
    assert!(!e.mirror_linked_edit());
    assert!(!e.has_linked_ranges(), "a space cleared the set");
    assert_eq!(e.lines[1], "</div>", "the sibling was untouched");

    // A newline inside the range is a multi-row edit: clear.
    let mut e2 = editor_with("<div>\n</div>");
    e2.set_linked_ranges(&[(0, 1, 0, 4), (1, 2, 1, 5)]);
    e2.cursor_row = 0;
    e2.cursor_col = 3;
    e2.insert_newline();
    assert!(!e2.mirror_linked_edit());
    assert!(!e2.has_linked_ranges());
}

#[test]
fn linked_editing_rejects_boundary_deletes_of_the_outside_delimiters() {
    // Backspace with the caret AT the range start deletes the `<`
    // BEFORE the range; the post-edit caret sits outside the span,
    // so nothing may mirror (the old bug wrote "iv" into siblings).
    let mut e = editor_with("<div>\n</div>");
    e.set_linked_ranges(&[(0, 1, 0, 4), (1, 2, 1, 5)]);
    e.cursor_row = 0;
    e.cursor_col = 1;
    e.backspace();
    assert!(!e.mirror_linked_edit());
    assert!(!e.has_linked_ranges());
    assert_eq!(e.lines[0], "div>", "only the user's own edit landed");
    assert_eq!(e.lines[1], "</div>", "the sibling was untouched");

    // Delete-forward with the caret AT the range end deletes the `>`
    // AFTER the range — same rejection, symmetric case.
    let mut e2 = editor_with("<div>\n</div>");
    e2.set_linked_ranges(&[(0, 1, 0, 4), (1, 2, 1, 5)]);
    e2.cursor_row = 0;
    e2.cursor_col = 4;
    e2.delete_forward();
    assert!(!e2.mirror_linked_edit());
    assert!(!e2.has_linked_ranges());
    assert_eq!(e2.lines[0], "<div");
    assert_eq!(e2.lines[1], "</div>", "the sibling was untouched");
}

#[test]
fn linked_editing_ignores_edits_outside_every_range() {
    let mut e = editor_with("<div>body</div>");
    e.set_linked_ranges(&[(0, 1, 0, 4), (0, 11, 0, 14)]);
    e.cursor_row = 0;
    e.cursor_col = 7; // inside "body"
    e.insert_char('!');
    assert!(!e.mirror_linked_edit());
    assert!(
        !e.has_linked_ranges(),
        "an edit outside the set clears it (positions may have shifted)"
    );
    assert_eq!(e.lines[0], "<div>bo!dy</div>");
}

#[test]
fn set_linked_ranges_rejects_multiline_and_singleton_sets() {
    let mut e = editor_with("<a>\n</a>");
    e.set_linked_ranges(&[(0, 1, 1, 2)]);
    assert!(
        !e.has_linked_ranges(),
        "multi-line span filtered, pair too small"
    );
    e.set_linked_ranges(&[(0, 1, 0, 2)]);
    assert!(!e.has_linked_ranges(), "a single range cannot mirror");
}

#[test]
fn expand_selection_syntax_walks_rust_node_ancestry_and_shrink_retraces() {
    let mut e = editor_with("fn main() {\n    let value = compute(1, 2);\n}");
    e.set_language(Some(crate::highlight::LangKind::Rust));
    // Caret inside `value`, nothing selected.
    e.cursor_row = 1;
    e.cursor_col = 9;
    e.validate_expand_stacks();
    assert!(e.expand_selection_syntax(), "first grow");
    let first = e.selection.expect("a selection appeared").normalised();
    assert!(
        first.0 <= (1, 8) && first.1 >= (1, 13),
        "the identifier (or more) is selected: {first:?}"
    );
    assert!(e.expand_selection_syntax(), "second grow");
    let second = e.selection.unwrap().normalised();
    assert!(
        second.0 <= first.0 && second.1 >= first.1 && second != first,
        "each step strictly grows: {first:?} -> {second:?}"
    );
    // Shrink retraces the EXACT ranges, then bottoms out.
    assert!(e.shrink_selection_step());
    assert_eq!(e.selection.unwrap().normalised(), first);
    assert!(e.shrink_selection_step());
    assert!(
        e.selection.is_none(),
        "the gesture's start was a zero-area caret"
    );
    assert!(!e.shrink_selection_step(), "bottom of the stack");
}

#[test]
fn expand_stack_rebuilds_after_an_edit_or_a_cursor_move() {
    let mut e = editor_with("fn main() {\n    let value = 1;\n}");
    e.set_language(Some(crate::highlight::LangKind::Rust));
    e.cursor_row = 1;
    e.cursor_col = 9;
    e.validate_expand_stacks();
    assert!(e.expand_selection_syntax());
    // A manual cursor move away from the stack's step invalidates it:
    // shrink from the new spot has nothing to retrace.
    e.clear_selection();
    e.cursor_row = 0;
    e.cursor_col = 0;
    assert!(!e.shrink_selection_step(), "stack rebuilt at the new spot");
}

#[test]
fn install_selection_chains_keeps_only_strictly_growing_containing_spans() {
    let mut e = editor_with("alpha beta\ngamma");
    e.cursor_row = 0;
    e.cursor_col = 7; // inside "beta"
    e.validate_expand_stacks();
    // UTF-16 chain: the word, a NON-containing span (filtered), the
    // line, a repeat of the line (filtered), both lines.
    e.install_selection_chains(vec![vec![
        (0, 6, 0, 10),
        (0, 0, 0, 5),
        (0, 0, 0, 10),
        (0, 0, 0, 10),
        (0, 0, 1, 5),
    ]]);
    assert!(e.expand_selection_from_stack());
    assert_eq!(e.selection.unwrap().normalised(), ((0, 6), (0, 10)));
    assert!(e.expand_selection_from_stack());
    assert_eq!(e.selection.unwrap().normalised(), ((0, 0), (0, 10)));
    assert!(e.expand_selection_from_stack());
    assert_eq!(e.selection.unwrap().normalised(), ((0, 0), (1, 5)));
    assert!(!e.expand_selection_from_stack(), "chain exhausted");
}

#[test]
fn textual_fallback_grows_to_line_then_whole_buffer_without_a_grammar() {
    let mut e = editor_with("first line here\nsecond");
    e.cursor_row = 0;
    e.cursor_col = 6;
    e.validate_expand_stacks();
    assert!(e.expand_selection_syntax());
    assert_eq!(
        e.selection.unwrap().normalised(),
        ((0, 0), (0, 15)),
        "no grammar: the line first"
    );
    assert!(e.expand_selection_syntax());
    assert_eq!(
        e.selection.unwrap().normalised(),
        ((0, 0), (1, 6)),
        "then the whole buffer"
    );
    assert!(!e.expand_selection_syntax(), "nothing larger exists");
}

#[test]
fn expand_selection_grows_every_cursor_in_a_multi_cursor_set() {
    let mut e = editor_with("let alpha = 1;\nlet beta = 2;");
    e.set_language(Some(crate::highlight::LangKind::Rust));
    e.cursor_row = 0;
    e.cursor_col = 5; // inside alpha
    e.carets = vec![EditorSelection::new(1, 5)]; // inside beta
    e.validate_expand_stacks();
    assert!(e.expand_selection_syntax());
    let primary = e.selection.expect("primary grew").normalised();
    assert_eq!(primary.0.0, 0, "primary stays on its own row");
    assert_eq!(e.carets.len(), 1);
    let caret = e.carets[0].normalised();
    assert!(
        caret.0.0 == 1 && caret.1.1 > caret.0.1,
        "the secondary caret grew on ITS row: {caret:?}"
    );
}

#[test]
fn document_colors_splice_a_swatch_cell_and_answer_color_at() {
    use crate::lsp::manager::ColorItem;
    let mut e = editor_with("a { color: #ff0000; }");
    e.path = Some(std::path::PathBuf::from("/tmp/x.css"));
    let item = ColorItem {
        line: 0,
        character: 11,
        end_line: 0,
        end_character: 18,
        red: 255,
        green: 0,
        blue: 0,
        raw: (1.0, 0.0, 0.0, 1.0),
    };
    e.apply_document_colors(std::path::PathBuf::from("/tmp/x.css"), vec![item]);
    let spans = e.row_inlay_spans(0);
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].0, 11, "anchored at the value's start");
    assert_eq!(spans[0].1, "\u{25a0}");
    assert_eq!(spans[0].2, Some(Color::Rgb(255, 0, 0)));
    // Range lookup, inclusive of the end boundary.
    assert!(e.color_at(0, 11).is_some());
    assert!(e.color_at(0, 18).is_some());
    assert!(e.color_at(0, 10).is_none());
    assert!(e.color_at(0, 19).is_none());
    // An empty batch clears the swatches.
    e.apply_document_colors(std::path::PathBuf::from("/tmp/x.css"), Vec::new());
    assert!(e.row_inlay_spans(0).is_empty());
    // Colors for ANOTHER file never splice into this one.
    let stale = ColorItem {
        line: 0,
        character: 0,
        end_line: 0,
        end_character: 1,
        red: 1,
        green: 2,
        blue: 3,
        raw: (0.0, 0.0, 0.0, 1.0),
    };
    e.apply_document_colors(std::path::PathBuf::from("/tmp/other.css"), vec![stale]);
    assert!(e.row_inlay_spans(0).is_empty());
    assert!(e.color_at(0, 0).is_none());
}

#[test]
fn select_next_occurrence_grows_selection_one_match_at_a_time() {
    let mut e = editor_with("foo bar foo baz foo");
    e.cursor_row = 0;
    e.cursor_col = 1; // inside the first "foo"
    // First press selects the word under the cursor; no extra carets yet.
    assert_eq!(e.select_next_occurrence(), 1);
    assert!(!e.has_multi_cursor());
    assert_eq!(e.selection.unwrap().normalised(), ((0, 0), (0, 3)));
    // Each further press adds the next occurrence as a secondary caret.
    assert_eq!(e.select_next_occurrence(), 2);
    assert!(e.has_multi_cursor());
    assert_eq!(e.select_next_occurrence(), 3);
    // Every "foo" is now selected: another press wraps and adds nothing.
    assert_eq!(e.select_next_occurrence(), 3);
}

#[test]
fn add_caret_at_screen_adds_and_toggles_a_secondary_caret() {
    let mut e = editor_with("fn main() {}\nlet x = 1;");
    e.last_inner = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 25,
    };
    e.last_gutter_width = 2;
    e.cursor_row = 0;
    e.cursor_col = 0;
    // Alt+click on line 1, char 3: the old primary becomes a caret and the
    // click point becomes the new primary.
    assert!(e.add_caret_at_screen(6, 1));
    assert_eq!((e.cursor_row, e.cursor_col), (1, 3));
    assert_eq!(e.carets.len(), 1, "the previous cursor is now a caret");
    assert_eq!(e.carets[0].head, (0, 0));
    // Alt+click again on that same caret removes it (toggle).
    assert!(e.add_caret_at_screen(3, 0));
    assert_eq!(e.carets.len(), 0, "clicking an existing caret removes it");
}

#[test]
fn box_select_spans_rows_into_a_column_of_carets() {
    let mut e = editor_with("aaaa\nbbbb\ncccc");
    e.last_inner = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 25,
    };
    e.last_gutter_width = 2; // text_x = 3
    // Anchor at row 0 char 1 (screen col 4), drag to row 2 char 3 (col 6).
    assert!(e.begin_box_select(4, 0));
    e.box_drag_to_screen(6, 2);
    assert!(e.box_selecting());
    // Head row is the primary; its selection covers cols 1..3.
    assert_eq!(e.selection.unwrap().normalised(), ((2, 1), (2, 3)));
    // The other two rows become carets over the same column span.
    assert_eq!(e.carets.len(), 2);
    assert_eq!(e.carets[0].normalised(), ((0, 1), (0, 3)));
    assert_eq!(e.carets[1].normalised(), ((1, 1), (1, 3)));
    e.end_box_select();
    assert!(!e.box_selecting());
}

/// #1560: the head may run past a short line's end, as VS Code's does;
/// the short line gets a bare caret at its end and the rows past it keep
/// the box's columns.
#[test]
fn column_select_runs_past_a_short_line() {
    let mut e = editor_with("abcdef\nab\nabcdef");
    e.cursor_row = 0;
    e.cursor_col = 3;
    e.column_select(0, 2);
    e.column_select(2, 0);
    assert_eq!(e.selection.unwrap().normalised(), ((2, 3), (2, 5)));
    assert_eq!(e.carets.len(), 2);
    assert_eq!(e.carets[0].normalised(), ((0, 3), (0, 5)));
    assert_eq!(e.carets[1].normalised(), ((1, 2), (1, 2)));
}

/// Negative (#1560): Right stops at the widest line in the box, and Down
/// at the last line.
#[test]
fn column_select_stops_at_the_box_s_widest_line_and_the_last_row() {
    let mut e = editor_with("abc\nab");
    e.cursor_row = 0;
    e.cursor_col = 0;
    for _ in 0..5 {
        e.column_select(1, 1);
    }
    assert_eq!((e.cursor_row, e.cursor_col), (1, 2));
    assert_eq!(e.carets.len(), 1);
    assert_eq!(e.carets[0].normalised(), ((0, 0), (0, 3)));
}

/// #1560: a caret moved by anything else starts a fresh box there
/// instead of growing the old one.
#[test]
fn a_moved_caret_starts_a_new_column_box() {
    let mut e = editor_with("aaaa\nbbbb\ncccc\ndddd");
    e.cursor_row = 0;
    e.cursor_col = 0;
    e.column_select(1, 0);
    e.carets.clear();
    e.selection = None;
    e.cursor_row = 2;
    e.cursor_col = 2;
    e.column_select(1, 0);
    assert_eq!(e.carets.len(), 1);
    assert_eq!(e.carets[0].normalised(), ((2, 2), (2, 2)));
    assert_eq!((e.cursor_row, e.cursor_col), (3, 2));
}

/// #1560: a one-line selection anchors the box at its start.
#[test]
fn column_select_grows_from_a_one_line_selection() {
    let mut e = editor_with("aaaa\nbbbb");
    e.selection = Some(EditorSelection {
        anchor: (0, 1),
        head: (0, 3),
    });
    e.cursor_row = 0;
    e.cursor_col = 3;
    e.column_select(1, 0);
    assert_eq!(e.selection.unwrap().normalised(), ((1, 1), (1, 3)));
    assert_eq!(e.carets[0].normalised(), ((0, 1), (0, 3)));
}

/// #1560: PageDown moves the box head one screen.
#[test]
fn column_select_page_moves_one_screen() {
    let text: Vec<String> = (0..40).map(|i| format!("line {i}")).collect();
    let mut e = editor_with(&text.join("\n"));
    e.last_inner = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 10,
    };
    e.cursor_row = 0;
    e.cursor_col = 0;
    e.column_select(e.column_select_page(), 0);
    assert_eq!(e.cursor_row, 10);
    assert_eq!(e.carets.len(), 10);
}

/// Negative (#1560): typing over a one-column box leaves the primary
/// position and caret count unchanged, but the next column-select key
/// must grow from the new carets, not restore the old box.
#[test]
fn typing_over_a_column_box_starts_a_new_box() {
    let mut e = editor_with("abcd\nabcd\nabcd");
    e.cursor_row = 0;
    e.cursor_col = 0;
    e.column_select(0, 1);
    e.column_select(2, 0);
    assert_eq!(e.selection.unwrap().normalised(), ((2, 0), (2, 1)));
    e.multi_insert_char('x');
    assert_eq!((e.cursor_row, e.cursor_col), (2, 1));
    assert_eq!(e.carets.len(), 2);
    e.column_select(0, 1);
    assert_eq!(e.selection.unwrap().normalised(), ((2, 1), (2, 2)));
    assert_eq!(e.carets.len(), 0);
}

/// Negative (#1560): with a horizontal scrollbar taking the bottom row,
/// PageDown moves only the rows that show text.
#[test]
fn column_select_page_skips_the_horizontal_scrollbar_row() {
    let mut text: Vec<String> = (0..40).map(|i| format!("line {i}")).collect();
    text[0] = "a".repeat(500);
    let mut e = editor_with(&text.join("\n"));
    e.focused = true;
    let area = Rect {
        x: 0,
        y: 0,
        width: 30,
        height: 10,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    (&mut e).render(area, &mut buf);
    assert!(e.last_hscrollbar.height == 1, "the long line overflows");
    let rows = e.text_rows();
    assert_eq!(rows, e.last_inner.height as usize - 1);
    e.cursor_row = 0;
    e.cursor_col = 0;
    e.column_select(e.column_select_page(), 0);
    assert_eq!(e.cursor_row, rows);
}

#[test]
fn select_next_occurrence_noop_off_a_word() {
    let mut e = editor_with("   foo");
    e.cursor_row = 0;
    e.cursor_col = 0; // on whitespace, not a word
    assert_eq!(e.select_next_occurrence(), 0);
    assert!(e.selection.is_none());
}

// ---- Toggle Word Wrap (Alt+Z) ----

#[test]
fn toggle_wrap_turns_wrap_on_for_code_file() {
    let mut e = editor_with("fn main() {}");
    e.lang = Some(LangKind::Rust);
    assert!(!e.wrap_enabled(), "code files default to no wrap");
    e.toggle_wrap();
    assert!(e.wrap_enabled());
}

#[test]
fn toggle_wrap_turns_wrap_off_for_markdown() {
    let mut e = editor_with("# title");
    e.lang = Some(LangKind::Markdown);
    assert!(e.wrap_enabled(), "markdown defaults to wrap on");
    e.toggle_wrap();
    assert!(!e.wrap_enabled());
}

// ---- Go to Bracket (editor.action.jumpToBracket) ----

#[test]
fn jump_to_bracket_from_open_moves_to_matching_close() {
    let mut e = editor_with("foo(bar)baz");
    e.cursor_row = 0;
    e.cursor_col = 3; // immediately before the '('
    assert!(e.jump_to_matching_bracket());
    assert_eq!((e.cursor_row, e.cursor_col), (0, 7)); // start of ')'
}

#[test]
fn jump_to_bracket_from_close_moves_to_matching_open() {
    let mut e = editor_with("foo(bar)baz");
    e.cursor_row = 0;
    e.cursor_col = 8; // immediately after the ')'
    assert!(e.jump_to_matching_bracket());
    assert_eq!((e.cursor_row, e.cursor_col), (0, 3)); // start of '('
}

#[test]
fn jump_to_bracket_from_inside_goes_to_enclosing_close() {
    let mut e = editor_with("foo(bar)baz");
    e.cursor_row = 0;
    e.cursor_col = 5; // inside, between 'a' and 'r'
    assert!(e.jump_to_matching_bracket());
    assert_eq!((e.cursor_row, e.cursor_col), (0, 7)); // start of ')'
}

#[test]
fn jump_to_bracket_respects_nesting() {
    let mut e = editor_with("(a(b)c)");
    e.cursor_row = 0;
    e.cursor_col = 0; // before the outer '('
    assert!(e.jump_to_matching_bracket());
    assert_eq!((e.cursor_row, e.cursor_col), (0, 6)); // outer ')'
}

#[test]
fn jump_to_bracket_works_across_lines() {
    let mut e = editor_with("fn f() {\n    x\n}");
    e.cursor_row = 0;
    e.cursor_col = 7; // before the '{'
    assert!(e.jump_to_matching_bracket());
    assert_eq!((e.cursor_row, e.cursor_col), (2, 0)); // the '}'
}

/// #1634's `br.rs`, highlighted as Rust so strings and comments are
/// known.
fn bracket_sample() -> Editor {
    let mut e = editor_with(
        "fn main() {\n    let re = Regex::new(\"\\\\(\").unwrap();\n    let n = count(\")\", s) + 1;\n    // closes the block: }\n    println!(\"{}\", n);\n}\n",
    );
    e.set_language(Some(LangKind::Rust));
    e
}

/// #1634: a bracket inside a string or a comment is text, so the
/// highlight, Go to Bracket and Select to Bracket pair the code's own
/// brackets.
#[test]
fn brackets_in_strings_and_comments_are_skipped_when_matching() {
    let mut e = bracket_sample();
    // The `(` after `Regex::new` pairs with the `)` before `.unwrap`.
    (e.cursor_row, e.cursor_col) = (1, 23);
    assert_eq!(e.bracket_match_pair(), Some(((1, 23), (1, 29))));
    assert!(e.jump_to_matching_bracket());
    assert_eq!((e.cursor_row, e.cursor_col), (1, 29));
    // `count(")", s)`: the string's `)` is skipped.
    (e.cursor_row, e.cursor_col) = (2, 17);
    assert_eq!(e.bracket_match_pair(), Some(((2, 17), (2, 24))));
    // The `{` of `fn main` pairs with line 6's `}`, not the comment's.
    (e.cursor_row, e.cursor_col) = (0, 10);
    assert_eq!(e.bracket_match_pair(), Some(((0, 10), (5, 0))));
    assert!(e.select_to_matching_bracket());
    assert_eq!(
        e.selection,
        Some(EditorSelection {
            anchor: (0, 10),
            head: (5, 1)
        })
    );
    // And back from the `}`.
    (e.cursor_row, e.cursor_col) = (5, 1);
    assert_eq!(e.bracket_match_pair(), Some(((0, 10), (5, 0))));
}

/// #1634: from inside a string, Go to Bracket goes to the enclosing
/// pair in code, and a bracket inside a string pairs with nothing.
#[test]
fn a_caret_inside_a_string_uses_the_enclosing_code_brackets() {
    let mut e = bracket_sample();
    (e.cursor_row, e.cursor_col) = (2, 19);
    assert!(e.jump_to_matching_bracket());
    assert_eq!((e.cursor_row, e.cursor_col), (2, 24));
    (e.cursor_row, e.cursor_col) = (2, 20);
    assert_eq!(e.bracket_match_pair(), None, "the string's `)` is text");
    // A balanced pair inside a string is text too: `"{}"` on line 5.
    assert_eq!(&e.lines[4][13..17], "\"{}\"");
    (e.cursor_row, e.cursor_col) = (4, 15);
    assert_eq!(e.bracket_match_pair(), None, "the string's `{{}}` is text");
}

/// #1634 negative: brackets in code match as before, across lines and
/// nested, and a buffer with no grammar still counts every bracket.
#[test]
fn brackets_in_code_match_as_before() {
    let mut e = bracket_sample();
    (e.cursor_row, e.cursor_col) = (0, 7);
    assert_eq!(e.bracket_match_pair(), Some(((0, 7), (0, 8))));
    (e.cursor_row, e.cursor_col) = (4, 12);
    assert_eq!(e.bracket_match_pair(), Some(((4, 12), (4, 20))));
    let mut plain = editor_with("f(\")\")\n");
    (plain.cursor_row, plain.cursor_col) = (0, 1);
    assert_eq!(plain.bracket_match_pair(), Some(((0, 1), (0, 3))));
}

#[test]
fn jump_to_bracket_noop_without_a_bracket() {
    let mut e = editor_with("hello world");
    e.cursor_row = 0;
    e.cursor_col = 2;
    assert!(!e.jump_to_matching_bracket());
    assert_eq!((e.cursor_row, e.cursor_col), (0, 2));
}

// ---- Breadcrumbs bar ----

#[test]
fn sticky_scroll_never_covers_the_caret_row() {
    // The sticky band floats over the topmost content rows, and
    // `scroll_view_to` pulls the caret down to `scroll` whenever it drifts
    // above the viewport — so the caret lands on exactly the row the band
    // repaints, and vanishes. VS Code keeps the cursor line clear of its
    // sticky widget.
    let mut e = editor_with(&format!("fn a() {{\n{}}}\n", "    x\n".repeat(60)));
    e.scroll = 3;
    e.cursor_row = 3;
    e.sticky_lines = vec![0];
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 20,
    };
    let mut buf = Buffer::empty(area);
    (&mut e).render(area, &mut buf);
    let caret = e
        .cursor_screen_pos()
        .expect("the caret must have a screen row");
    assert!(
        !e.sticky_click_rows.iter().any(|&(y, _)| y == caret.1),
        "the sticky band painted over the caret's own row {}",
        caret.1
    );
}

/// #1169: accept buttons switch to their short labels instead of being
/// dropped when a row cannot fit every full label, and never run past
/// the row's end.
#[test]
fn accept_buttons_shorten_before_they_drop() {
    let chips = [
        AcceptChip::new("Accept Current", "Current", ACCEPT_CURRENT_FILL),
        AcceptChip::new("Accept Incoming", "Incoming", ACCEPT_INCOMING_FILL),
        AcceptChip::new("Accept Both", "Both", ACCEPT_BOTH_FILL),
    ];
    let texts = |limit: u16| -> Vec<String> {
        lay_out_accept_chips(&chips, 10, limit)
            .into_iter()
            .map(|(x, t)| {
                assert!(x + t.chars().count() as u16 <= limit, "{t:?} overruns");
                t
            })
            .collect()
    };
    assert_eq!(
        texts(100),
        [" Accept Current ", " Accept Incoming ", " Accept Both "]
    );
    assert_eq!(texts(40), [" Current ", " Incoming ", " Both "]);
    assert_eq!(texts(30), [" Current ", " Incoming "]);
    assert!(texts(12).is_empty());
    let ticked = AcceptChip::new("Accept Both", "Both", ACCEPT_BOTH_FILL).ticked(true);
    assert_eq!(ticked.label, "\u{2713} Accept Both");
    assert_eq!(
        ticked.short, "\u{2713} Both",
        "a narrow pane still shows which side was taken"
    );
}

#[test]
fn sticky_band_erases_accept_spans_it_paints_over() {
    // The band overpaints the topmost content rows AFTER the row loop
    // records the [Accept …] spans; a span left under it would make a
    // click on a visible scope header resolve an unseen conflict.
    let mut e = editor_with(
        "fn outer() {\n    a();\n<<<<<<< HEAD\n    ours();\n=======\n    theirs();\n>>>>>>> feature\n    b();\n}",
    );
    e.scroll = 2; // the conflict header is the viewport's top row
    e.cursor_row = 7;
    e.sticky_lines = vec![0];
    let area = Rect {
        x: 0,
        y: 0,
        width: 120,
        height: 12,
    };
    let mut buf = Buffer::empty(area);
    (&mut e).render(area, &mut buf);
    assert!(
        !e.sticky_click_rows.is_empty(),
        "precondition: the sticky band must have painted"
    );
    let band_rows: Vec<u16> = e.sticky_click_rows.iter().map(|&(y, _)| y).collect();
    assert!(
        !e.merge_action_spans
            .iter()
            .any(|(y, _, _, _)| band_rows.contains(y)),
        "spans under the sticky band must be dropped; spans={:?} band={band_rows:?}",
        e.merge_action_spans
    );
    assert!(
        e.merge_action_spans.iter().all(|(_, _, row, _)| *row == 2),
        "the header's spans below the band (if visible elsewhere) still target the real block"
    );
}

#[test]
fn breadcrumb_row_renders_below_tab_strip_and_maps_symbol_clicks() {
    let mut tabs = EditorTabs::new();
    tabs.breadcrumbs = vec![
        Crumb {
            label: "src".into(),
            target: None,
            icon: None,
        },
        Crumb {
            label: "m.rs".into(),
            target: None,
            icon: None,
        },
        Crumb {
            label: "build".into(),
            target: Some((5, 2)),
            icon: None,
        },
    ];
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 10,
    };
    let mut buf = Buffer::empty(area);
    (&mut tabs).render(area, &mut buf);
    // The bar sits on the row directly below the one-row tab strip.
    let by = area.y + 1;
    // Somewhere on the bar a click lands on the "build" symbol crumb and
    // resolves to its jump target; the path crumbs resolve to nothing.
    let hit = (0..area.width).find_map(|x| tabs.breadcrumb_target_at(x, by));
    assert_eq!(hit, Some((5, 2)), "clicking the symbol crumb jumps there");
    // A click off the bar (the tab strip row) never maps to a crumb.
    assert_eq!(tabs.breadcrumb_target_at(10, area.y), None);
}

#[test]
fn breadcrumbs_measure_wide_glyphs_in_display_cells() {
    // The bar budgeted and advanced in `char`s while the buffer advances by
    // display CELLS, so every crumb after a CJK one was painted right of
    // the rect recorded for it and its click missed. Same class as the
    // terminal pane pill.
    let mut tabs = EditorTabs::new();
    tabs.breadcrumbs = vec![
        Crumb {
            label: "文档文档".into(),
            target: None,
            icon: None,
        },
        Crumb {
            label: "build".into(),
            target: Some((5, 2)),
            icon: None,
        },
    ];
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 10,
    };
    let mut buf = Buffer::empty(area);
    (&mut tabs).render(area, &mut buf);
    let by = area.y + 1;
    // The column where "build" was actually PAINTED: scan cells, since a
    // wide glyph takes one cell and leaves the next as a spacer.
    let row: String = (0..area.width)
        .map(|x| buf[(x, by)].symbol().to_string())
        .collect::<Vec<_>>()
        .concat();
    // The advance was a char count while the glyphs take two cells each,
    // so the separator and the next crumb were painted four cells INTO
    // this label and destroyed its second half.
    // A wide glyph owns one cell and leaves the next as a spacer, so the
    // concatenated row interleaves blanks: count the glyphs instead.
    assert_eq!(
        (row.matches('文').count(), row.matches('档').count()),
        (2, 2),
        "the whole wide label must survive, got {row:?}"
    );
    assert!(
        row.contains("build"),
        "and the next crumb must still be painted, got {row:?}"
    );
    // The recorded hit rect must cover every cell the label was painted on,
    // or clicking its right half misses.
    let (x, w, _) = tabs.breadcrumb_ranges[0];
    assert_eq!(w, 8, "a 4-char CJK label occupies 8 cells, not 4");
    let (next_x, _, _) = tabs.breadcrumb_ranges[1];
    assert!(
        next_x >= x + w,
        "the next crumb must start past this one, got {next_x} vs {x}+{w}"
    );
}

#[test]
fn breadcrumb_bar_hidden_when_there_are_no_crumbs() {
    let mut tabs = EditorTabs::new();
    tabs.breadcrumbs.clear();
    let area = Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 10,
    };
    let mut buf = Buffer::empty(area);
    (&mut tabs).render(area, &mut buf);
    assert_eq!(tabs.breadcrumb_target_at(10, area.y + 1), None);
}

// ---- Sticky scroll ----

#[test]
fn sticky_scroll_pins_header_rows_and_maps_clicks_to_their_lines() {
    let text = (0..40)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut e = editor_with(&text);
    e.focused = true;
    e.scroll = 20;
    // The caret must sit inside the viewport and below the band: with it on
    // the top visible row there is nowhere for the band to go that would
    // not hide it, and render pulls `scroll` back to the caret.
    e.cursor_row = 25;
    e.sticky_lines = vec![0, 10];
    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 15,
    };
    let mut buf = Buffer::empty(area);
    (&mut e).render(area, &mut buf);
    let top = e.last_inner.y;
    assert_eq!(e.sticky_line_at(5, top), Some(0), "row 0 pins line 0");
    assert_eq!(e.sticky_line_at(5, top + 1), Some(10), "row 1 pins line 10");
    assert_eq!(
        e.sticky_line_at(5, top + 5),
        None,
        "a content row below the band is not sticky"
    );
}

/// The wheel drags the caret along with the viewport (`scroll_view_to`
/// clamps it to `scroll`), which used to park it on the very row the band
/// wants. Suppressing the band there would delete sticky scroll for the
/// one gesture it exists to serve, so the caret lands below the band
/// instead.
#[test]
fn a_wheel_scroll_keeps_the_band_and_parks_the_caret_below_it() {
    let text = (0..80)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut e = editor_with(&text);
    e.focused = true;
    e.sticky_lines = vec![0, 10];
    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 15,
    };
    let mut buf = Buffer::empty(area);
    (&mut e).render(area, &mut buf); // seats `last_inner` for scroll_down
    assert_eq!(e.cursor_row, 0, "caret starts on the first line");
    e.scroll_down(20);
    (&mut e).render(area, &mut buf);
    let top = e.last_inner.y;
    assert_eq!(
        e.sticky_line_at(5, top),
        Some(0),
        "the band must survive a wheel scroll"
    );
    assert_eq!(e.sticky_line_at(5, top + 1), Some(10));
    assert!(
        e.cursor_row >= e.scroll + 2,
        "caret dragged below the two-row band, not under it (row {}, scroll {})",
        e.cursor_row,
        e.scroll
    );
}

/// The band stops above the caret's PAINTED row. A collapsed fold between
/// the top of the viewport and the caret removes rows, so counting buffer
/// lines overstates how far down the caret is and the band paints over it.
#[test]
fn the_band_stops_above_the_caret_with_a_fold_between_them() {
    let mut text: Vec<String> = (0..10).map(|i| format!("head{i}")).collect();
    text.push(String::from("def f():"));
    text.extend((0..3).map(|i| format!("    body{i}")));
    text.extend((0..10).map(|i| format!("tail{i}")));
    let mut e = editor_with(&text.join("\n"));
    e.focused = true;
    e.scroll = 10;
    e.toggle_fold(10); // hides lines 11..=13
    assert!(e.is_line_hidden(12), "the fold body is hidden");
    // Line 14 is the second PAINTED row (row 0 is the fold header on 10),
    // even though it is four buffer lines below the top.
    e.cursor_row = 14;
    e.sticky_lines = vec![0, 5];
    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 15,
    };
    let mut buf = Buffer::empty(area);
    (&mut e).render(area, &mut buf);
    let top = e.last_inner.y;
    assert_eq!(e.sticky_line_at(5, top), Some(0), "outermost header pins");
    assert_eq!(
        e.sticky_line_at(5, top + 1),
        None,
        "the caret's own painted row must stay clear of the band"
    );
}

#[test]
fn sticky_scroll_hidden_without_pinned_lines() {
    let mut e = editor_with("a\nb\nc\n");
    e.focused = true;
    e.sticky_lines.clear();
    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 10,
    };
    let mut buf = Buffer::empty(area);
    (&mut e).render(area, &mut buf);
    assert_eq!(e.sticky_line_at(5, e.last_inner.y), None);
}

// ---- Bracket match highlight (editorBracketMatch) ----

#[test]
fn bracket_match_pair_from_open_returns_both_brackets() {
    let mut e = editor_with("foo(bar)baz");
    e.cursor_col = 3; // immediately before '('
    assert_eq!(e.bracket_match_pair(), Some(((0, 3), (0, 7))));
}

#[test]
fn bracket_match_pair_from_after_close_returns_both_brackets() {
    let mut e = editor_with("foo(bar)baz");
    e.cursor_col = 8; // immediately after ')'
    assert_eq!(e.bracket_match_pair(), Some(((0, 3), (0, 7))));
}

#[test]
fn bracket_match_pair_none_when_caret_touches_no_bracket() {
    let mut e = editor_with("foo(bar)baz");
    e.cursor_col = 5; // inside the pair but not adjacent to a bracket
    assert_eq!(e.bracket_match_pair(), None);
}

/// #959: a minified line's opening bracket, its partner a whole line
/// away. Each step of the scan used to walk the line from its start, so
/// this took hours; one pass takes milliseconds.
#[test]
fn a_bracket_far_along_a_long_line_matches_in_one_pass() {
    let body = "{\"id\":1,\"tags\":[\"a\",\"b\"]}, ".repeat(15_000);
    let mut e = editor_with(&format!("[{body}]"));
    let close = (0, e.lines[0].chars().count() - 1);
    let started = std::time::Instant::now();
    assert_eq!(e.bracket_match_pair(), Some(((0, 0), close)));
    e.cursor_col = close.1 + 1;
    assert_eq!(e.bracket_match_pair(), Some(((0, 0), close)));
    // Between the last `,` and ` `: no bracket beside the caret, so Go
    // to Bracket walks back to the enclosing `[` and on to its `]`.
    e.cursor_col = close.1 - 1;
    assert!(e.jump_to_matching_bracket(), "the enclosing `[`");
    assert_eq!((e.cursor_row, e.cursor_col), close);
    assert!(
        started.elapsed() < crate::test_budget::spawn_budget(std::time::Duration::from_secs(2)),
        "{:?}",
        started.elapsed()
    );
}

/// Backward and forward scans count columns in chars, not bytes, after
/// multi-byte text, across lines.
#[test]
fn bracket_scans_count_chars_after_multibyte_text() {
    let mut e = editor_with("é(ü\n日本)x");
    e.cursor_col = 1;
    assert_eq!(e.bracket_match_pair(), Some(((0, 1), (1, 2))));
    (e.cursor_row, e.cursor_col) = (1, 3);
    assert_eq!(e.bracket_match_pair(), Some(((0, 1), (1, 2))));
    (e.cursor_row, e.cursor_col) = (1, 1);
    assert!(e.jump_to_matching_bracket(), "from inside the pair");
    assert_eq!((e.cursor_row, e.cursor_col), (1, 2));
}

/// The highlight is worked out once per edit and caret place, and
/// follows both (#959).
#[test]
fn the_cached_bracket_highlight_follows_the_caret_and_edits() {
    let mut e = editor_with("f(x) g[y]");
    e.cursor_col = 1;
    assert_eq!(e.cached_bracket_match_pair(), Some(((0, 1), (0, 3))));
    assert_eq!(e.cached_bracket_match_pair(), Some(((0, 1), (0, 3))));
    e.cursor_col = 6;
    assert_eq!(e.cached_bracket_match_pair(), Some(((0, 6), (0, 8))));
    e.cursor_col = 1;
    assert_eq!(e.cached_bracket_match_pair(), Some(((0, 1), (0, 3))));
    // An edit that leaves the caret where it was still counts.
    e.cursor_col = 2;
    e.insert_char('(');
    e.cursor_col = 1;
    let pair = e.cached_bracket_match_pair();
    assert_ne!(pair, Some(((0, 1), (0, 3))), "{:?}", e.lines);
    assert_eq!(pair, e.bracket_match_pair());
}

/// A language switch re-highlights without an edit, and can put the
/// pair beside the caret into a comment or take it out of one.
#[test]
fn the_cached_bracket_highlight_follows_a_language_switch() {
    let mut e = editor_with("// (x)");
    e.set_language(Some(LangKind::Python));
    e.cursor_col = 3;
    assert_eq!(e.cached_bracket_match_pair(), Some(((0, 3), (0, 5))));
    e.set_language(Some(LangKind::Rust));
    assert_eq!(e.cached_bracket_match_pair(), None, "a Rust comment");
    e.set_language(Some(LangKind::Python));
    assert_eq!(e.cached_bracket_match_pair(), Some(((0, 3), (0, 5))));
}

#[test]
fn render_paints_bracket_match_background_on_both_brackets() {
    let mut e = editor_with("foo(bar)baz");
    e.focused = true;
    e.cursor_col = 3; // beside '('
    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 6,
    };
    let mut buf = Buffer::empty(area);
    (&mut e).render(area, &mut buf);
    let text_x = e.last_inner.x + e.last_gutter_width + 1;
    let y = e.last_inner.y;
    let bm = e.theme.bracket_match_bg();
    assert_eq!(buf[(text_x + 3, y)].bg, bm, "the '(' is highlighted");
    assert_eq!(buf[(text_x + 7, y)].bg, bm, "the ')' is highlighted");
    assert_ne!(
        buf[(text_x + 5, y)].bg,
        bm,
        "a non-bracket cell is not highlighted"
    );
}

// ---- Indentation guides (editor.guides.indentation) ----

fn guide_buf(e: &mut Editor, w: u16, h: u16) -> Buffer {
    let area = Rect {
        x: 0,
        y: 0,
        width: w,
        height: h,
    };
    let mut buf = Buffer::empty(area);
    (e as &mut Editor).render(area, &mut buf);
    buf
}

#[test]
fn render_paints_indent_guides_at_each_indent_level() {
    let mut e = editor_with("fn main() {\n    if x {\n        y();\n    }\n}");
    let buf = guide_buf(&mut e, 40, 8);
    let text_x = e.last_inner.x + e.last_gutter_width + 1;
    let y0 = e.last_inner.y;
    let g = e.theme.indent_guide();
    assert_eq!(buf[(text_x, y0 + 1)].symbol(), "\u{2502}", "level-1 guide");
    assert_eq!(buf[(text_x, y0 + 1)].fg, g);
    assert_eq!(
        buf[(text_x, y0 + 2)].symbol(),
        "\u{2502}",
        "level-1 guide, row 2"
    );
    assert_eq!(
        buf[(text_x + 4, y0 + 2)].symbol(),
        "\u{2502}",
        "level-2 guide"
    );
    assert_eq!(buf[(text_x + 4, y0 + 2)].fg, g);
    assert_eq!(
        buf[(text_x, y0)].symbol(),
        "f",
        "a top-level line gets no guide and keeps its text"
    );
    assert_eq!(
        buf[(text_x + 4, y0 + 1)].symbol(),
        "i",
        "guides never overwrite text"
    );
}

#[test]
fn indent_guides_continue_through_blank_lines_inside_a_block_only() {
    let mut e = editor_with("def f():\n    a = 1\n\n    b = 2\n\ndef g():\n    pass");
    let buf = guide_buf(&mut e, 40, 9);
    let text_x = e.last_inner.x + e.last_gutter_width + 1;
    let y0 = e.last_inner.y;
    assert_eq!(
        buf[(text_x, y0 + 2)].symbol(),
        "\u{2502}",
        "a blank line inside a block continues the guide"
    );
    assert_eq!(
        buf[(text_x, y0 + 4)].symbol(),
        " ",
        "a blank line between top-level blocks shows no guide"
    );
}

#[test]
fn active_indent_guide_highlights_the_cursor_block_only() {
    let mut e = editor_with("fn a() {\n    x();\n}\nfn b() {\n    y();\n}");
    e.focused = true;
    e.cursor_row = 4;
    e.cursor_col = 4;
    let buf = guide_buf(&mut e, 40, 8);
    let text_x = e.last_inner.x + e.last_gutter_width + 1;
    let y0 = e.last_inner.y;
    assert_eq!(
        buf[(text_x, y0 + 4)].fg,
        e.theme.indent_guide_active(),
        "the cursor's block guide is highlighted"
    );
    assert_eq!(
        buf[(text_x, y0 + 1)].fg,
        e.theme.indent_guide(),
        "the other block's guide stays dim"
    );
}

#[test]
fn header_line_activates_the_guide_of_the_block_it_opens() {
    let mut e = editor_with("fn a() {\n    x();\n    y();\n}");
    e.focused = true;
    e.cursor_row = 0;
    e.cursor_col = 0;
    let buf = guide_buf(&mut e, 40, 8);
    let text_x = e.last_inner.x + e.last_gutter_width + 1;
    let y0 = e.last_inner.y;
    assert_eq!(buf[(text_x, y0 + 1)].fg, e.theme.indent_guide_active());
    assert_eq!(buf[(text_x, y0 + 2)].fg, e.theme.indent_guide_active());
}

#[test]
fn indent_guides_toggle_off_paints_plain_whitespace() {
    let mut e = editor_with("fn main() {\n    if x {\n        y();\n    }\n}");
    e.show_indent_guides = false;
    let buf = guide_buf(&mut e, 40, 8);
    let text_x = e.last_inner.x + e.last_gutter_width + 1;
    let y0 = e.last_inner.y;
    assert_eq!(buf[(text_x, y0 + 1)].symbol(), " ");
    assert_eq!(buf[(text_x + 4, y0 + 2)].symbol(), " ");
}

#[test]
fn indent_guides_honour_horizontal_scroll() {
    let mut e = editor_with(
        "fn main() {\n    if x {\n        y(); // padding so the pane really scrolls\n    }\n}",
    );
    e.scroll_col = 4;
    let buf = guide_buf(&mut e, 40, 8);
    let text_x = e.last_inner.x + e.last_gutter_width + 1;
    let y0 = e.last_inner.y;
    assert_eq!(
        buf[(text_x, y0 + 2)].symbol(),
        "\u{2502}",
        "the level-2 guide shifts left with the scroll"
    );
    assert_eq!(
        buf[(text_x, y0 + 1)].symbol(),
        "i",
        "the scrolled-off level-1 guide leaves the text alone"
    );
}

// ---- Rulers (editor.rulers, #1524) ----

#[test]
fn rulers_draw_down_blank_cells_and_past_the_end_of_the_file() {
    let mut e = editor_with("xxxxxxxxxxxxxxxxxxxx\n\nshort");
    e.rulers = vec![10, 15];
    let buf = guide_buf(&mut e, 50, 8);
    let text_x = e.last_inner.x + e.last_gutter_width + 1;
    let y0 = e.last_inner.y;
    let r = e.theme.ruler();
    assert_eq!(
        buf[(text_x + 10, y0)].symbol(),
        "x",
        "a long line runs over the ruler: text wins"
    );
    for row in [1, 2, 4] {
        for col in [10, 15] {
            let cell = &buf[(text_x + col, y0 + row)];
            assert_eq!(cell.symbol(), "\u{2502}", "ruler {col} on row {row}");
            assert_eq!(cell.fg, r);
        }
    }
    assert_eq!(
        buf[(text_x + 9, y0 + 1)].symbol(),
        " ",
        "only at the column"
    );
}

#[test]
fn rulers_follow_the_horizontal_scroll() {
    // A long first line, so the pane really scrolls.
    let mut e = editor_with(&format!("{}\n\nb", "w".repeat(90)));
    e.rulers = vec![10];
    e.scroll_col = 4;
    let buf = guide_buf(&mut e, 40, 6);
    let text_x = e.last_inner.x + e.last_gutter_width + 1;
    let y0 = e.last_inner.y;
    assert_eq!(buf[(text_x + 6, y0 + 1)].symbol(), "\u{2502}");
    assert_eq!(buf[(text_x + 10, y0 + 1)].symbol(), " ");
    e.scroll_col = 12;
    let buf = guide_buf(&mut e, 40, 6);
    let row: String = (0..30)
        .map(|c| buf[(text_x + c, y0 + 1)].symbol().to_string())
        .collect();
    assert!(
        !row.contains('\u{2502}'),
        "a ruler scrolled off is not drawn: {row:?}"
    );
}

#[test]
fn no_rulers_or_rulers_past_the_pane_draw_nothing() {
    for rulers in [vec![], vec![500]] {
        let mut e = editor_with("a\n\nb");
        e.rulers = rulers.clone();
        let buf = guide_buf(&mut e, 40, 6);
        let text_x = e.last_inner.x + e.last_gutter_width + 1;
        let y0 = e.last_inner.y;
        for row in 0..4 {
            let text: String = (0..30)
                .map(|c| buf[(text_x + c, y0 + row)].symbol().to_string())
                .collect();
            assert!(
                !text.contains('\u{2502}'),
                "{rulers:?}, row {row}: {text:?}"
            );
        }
    }
}

/// A repository config can list any number of rulers; an editor keeps
/// them sorted, deduplicated, and at most `MAX_RULERS`.
#[test]
fn ruler_lists_are_sorted_deduplicated_and_capped() {
    assert_eq!(normalize_rulers(&[100, 80, 100, 80]), vec![80, 100]);
    let huge: Vec<u16> = (0..50_000u32).map(|i| (i % 4000) as u16).collect();
    let kept = normalize_rulers(&huge);
    assert_eq!(kept.len(), MAX_RULERS);
    assert!(kept.windows(2).all(|w| w[0] < w[1]));
}

#[test]
fn blank_line_guides_follow_the_neighbours_tab_cells() {
    let mut e = editor_with("\t\tfoo\n\n\t\tbar");
    e.set_indent_style(IndentStyle {
        width: 4,
        use_spaces: false,
    });
    e.show_indent_guides = true;
    let buf = guide_buf(&mut e, 40, 6);
    let text_x = e.last_inner.x + e.last_gutter_width + 1;
    let y = e.last_inner.y + 1;
    let width = e.indent_style().width as u16;
    let at = |dx: u16| buf[(text_x + dx, y)].symbol().to_string();
    assert_eq!(at(0), "\u{2502}", "first level");
    assert_eq!(at(width), "\u{2502}", "second level, one tab further");
    assert_eq!(at(1), " ", "no guide inside the tab");
}

#[test]
fn guide_indent_width_resolves_blank_lines_from_neighbours() {
    let e = editor_with("def f():\n    a = 1\n\n    b = 2\n\ndef g():\n    pass");
    assert_eq!(e.guide_indent_width(2), 4, "blank inside a block");
    assert_eq!(e.guide_indent_width(4), 0, "blank between blocks");
}

#[test]
fn active_indent_guide_spans_the_enclosing_block() {
    let mut e = editor_with("fn a() {\n    x();\n}\nfn b() {\n    y();\n    z();\n}");
    e.cursor_row = 4;
    assert_eq!(e.active_indent_guide(), Some((0, 4, 5)));
    e.cursor_row = 0; // header activates the body it opens
    assert_eq!(e.active_indent_guide(), Some((0, 1, 1)));
    e.cursor_row = 2; // "}" is top-level: no active guide
    assert_eq!(e.active_indent_guide(), None);
}

// ---- Bracket-pair colorization (#131) ----

#[test]
fn scan_colors_brackets_by_shared_nesting_depth() {
    let lines = vec![String::from("a(b[c{d}e]f)g")];
    let out = scan_bracket_colors(&lines, &[]);
    assert_eq!(
        out[0],
        vec![(1, 0), (3, 1), (5, 2), (7, 2), (9, 1), (11, 0)],
        "openers colour at their depth, closers at their opener's depth"
    );
}

#[test]
fn scan_marks_unmatched_and_mismatched_closers_unexpected() {
    let lines = vec![String::from("x)"), String::from("(]y)")];
    let out = scan_bracket_colors(&lines, &[]);
    assert_eq!(out[0], vec![(1, UNEXPECTED_BRACKET)], "no opener at all");
    assert_eq!(
        out[1],
        vec![(0, 0), (1, UNEXPECTED_BRACKET), (3, 0)],
        "a mismatched closer reddens without consuming the open bracket"
    );
}

#[test]
fn scan_skips_brackets_inside_protected_ranges() {
    // "s(" with the '(' at byte 1 protected: only line 2's bracket shows.
    let lines = vec![String::from("s("), String::from("f()")];
    let out = scan_bracket_colors(&lines, &[(1, 2)]);
    assert!(out[0].is_empty(), "the protected '(' does not participate");
    assert_eq!(out[1], vec![(1, 0), (2, 0)]);
}

/// #1510: a bracket in a C++ string or comment, or in a C or C++ char
/// literal, is text. One `'('` reddened every later closing brace.
#[test]
fn brackets_in_c_and_cpp_strings_comments_and_chars_are_text() {
    let cpp = "// greet the user (politely\n#include <string>\nint main() {\n    std::string s = \"smile :)\";\n    char c = '(';\n    return s.size() + c;\n}\n";
    let c = "int main() {\n    char c = '(';\n    int v[] = {1};\n    return c;\n}\n";
    for (kind, src) in [(LangKind::Cpp, cpp), (LangKind::C, c)] {
        let pass = run_highlight_pass(kind, src.to_string(), None);
        let red: Vec<(usize, usize)> = pass
            .brackets
            .iter()
            .enumerate()
            .flat_map(|(row, bs)| {
                bs.iter()
                    .filter(|b| b.1 == UNEXPECTED_BRACKET)
                    .map(move |b| (row, b.0 as usize))
            })
            .collect();
        assert!(
            red.is_empty(),
            "{kind:?}: brackets marked unmatched at {red:?}"
        );
    }
}

/// #1510 negative: a brace that really is unmatched in C++ code is still
/// marked.
#[test]
fn an_unmatched_cpp_brace_in_code_is_still_marked() {
    let pass = run_highlight_pass(
        LangKind::Cpp,
        String::from("int f() {\n    return 0;\n}}\n"),
        None,
    );
    assert_eq!(pass.brackets[2], vec![(0, 0), (1, UNEXPECTED_BRACKET)]);
}

#[test]
fn scan_depth_cycles_past_the_palette() {
    let lines = vec![String::from("([{(x)}])")];
    let out = scan_bracket_colors(&lines, &[]);
    assert_eq!(
        out[0][3],
        (3, 0),
        "depth 3 wraps back to the first cycle colour"
    );
}

#[test]
fn recompute_highlights_skips_brackets_in_rust_strings_and_comments() {
    let mut e = editor_with("let s = \"(prose)\";\nf(1); // (note)\n");
    e.set_language(Some(LangKind::Rust));
    assert!(
        e.bracket_colors[0].is_empty(),
        "string-literal brackets are prose: {:?}",
        e.bracket_colors[0]
    );
    assert_eq!(
        e.bracket_colors[1],
        vec![(1, 0), (3, 0)],
        "code brackets colour; comment brackets do not: {:?}",
        e.bracket_colors[1]
    );
}

#[test]
fn plain_text_bracket_scan_is_gated_by_buffer_size() {
    // A no-grammar buffer has no tree-sitter pass absorbing a per-edit
    // full scan, so an oversized one must skip bracket colorization
    // entirely (typing in an 8MB log must stay O(lines), not O(chars)).
    let big_line = "x".repeat(64 * 1024);
    let mut e = editor_with("");
    e.lines = vec![big_line; (BRACKET_SCAN_MAX_BYTES / (64 * 1024)) + 2];
    e.lines.push(String::from("f()"));
    e.recompute_highlights();
    assert!(
        e.bracket_colors.iter().all(Vec::is_empty),
        "an oversized plain-text buffer opts out of the scan"
    );
    let mut small = editor_with("f()");
    small.recompute_highlights();
    assert_eq!(small.bracket_colors[0], vec![(1, 0), (2, 0)]);
}

fn span_tuples(spans: &[Vec<HiSpan>]) -> Vec<Vec<(usize, usize, Style)>> {
    spans
        .iter()
        .map(|line| {
            line.iter()
                .map(|s| (s.range().start, s.range().end, s.style))
                .collect()
        })
        .collect()
}

#[test]
fn carry_stretches_a_span_over_text_typed_inside_it_and_shifts_what_follows() {
    let (kw, id, num) = (
        Style::default().fg(Color::Red),
        Style::default().fg(Color::Green),
        Style::default().fg(Color::Blue),
    );
    let span = |start, end, style| HiSpan { start, end, style };
    let mut base = PackedLines::from(vec![String::from("let name = 1;"), String::from("f(x)")]);
    let mut spans = vec![
        vec![span(0, 3, kw), span(4, 8, id), span(11, 12, num)],
        Vec::new(),
    ];
    let mut brackets = vec![Vec::new(), vec![(1, 0), (3, 0)]];
    let lines = vec![String::from("let naXme = 1;"), String::from("f(x)")];
    assert!(carry_highlights(
        &mut base,
        &mut spans,
        &mut brackets,
        &lines
    ));
    assert_eq!(
        span_tuples(&spans),
        vec![vec![(0, 3, kw), (4, 9, id), (12, 13, num)], Vec::new()],
        "the identifier covers the typed X; the number moves right"
    );
    assert_eq!(brackets[1], vec![(1, 0), (3, 0)], "an untouched line");
    assert_eq!(
        base,
        PackedLines::from(&lines[..]),
        "the base follows the text"
    );
    assert!(
        !carry_highlights(&mut base, &mut spans, &mut brackets, &lines),
        "nothing left to carry"
    );
}

#[test]
fn carry_lets_text_typed_onto_a_span_take_its_color() {
    let (a, b) = (
        Style::default().fg(Color::Red),
        Style::default().fg(Color::Green),
    );
    let span = |start, end, style| HiSpan { start, end, style };
    for (typed, want) in [
        // Onto the end of the first span: it grows (a word, or a line
        // comment, being written).
        ("abX cd", vec![(0, 3, a), (4, 6, b)]),
        // Before the second, after a gap no span covers: nobody's.
        ("ab Xcd", vec![(0, 2, a), (4, 6, b)]),
    ] {
        let mut base = PackedLines::from(vec![String::from("ab cd")]);
        let mut spans = vec![vec![span(0, 2, a), span(3, 5, b)]];
        let mut brackets = vec![Vec::new()];
        carry_highlights(&mut base, &mut spans, &mut brackets, &[String::from(typed)]);
        assert_eq!(span_tuples(&spans), vec![want], "typed: {typed}");
    }
}

#[test]
fn carry_never_paints_replacement_text_in_the_colors_it_replaced() {
    // A tab reused for another file, or a line retyped wholesale: no
    // color of the old text may land on unrelated new text.
    let (comment, kw) = (
        Style::default().fg(Color::Gray),
        Style::default().fg(Color::Blue),
    );
    let span = |start, end, style| HiSpan { start, end, style };
    let mut base = PackedLines::from(vec![
        String::from("// a comment"),
        String::from("fn x() {}"),
    ]);
    let mut spans = vec![vec![span(0, 12, comment)], vec![span(0, 2, kw)]];
    let mut brackets = vec![Vec::new(), vec![(4, 0), (5, 0), (7, 0), (8, 0)]];
    let lines = vec![String::from("let y = 2;"), String::from("fn x() {}")];
    carry_highlights(&mut base, &mut spans, &mut brackets, &lines);
    assert!(spans[0].is_empty(), "{:?}", spans[0]);
    assert_eq!(span_tuples(&spans)[1], vec![(0, 2, kw)]);
    assert_eq!(brackets[1], vec![(4, 0), (5, 0), (7, 0), (8, 0)]);
}

#[test]
fn carry_keeps_the_lines_between_two_edits() {
    // Two carets far apart: every line between them keeps its colors,
    // and so does each edited line's untouched text.
    let style = Style::default().fg(Color::Blue);
    let span = |start, end| HiSpan { start, end, style };
    let mut lines: Vec<String> = (0..6).map(|i| format!("word{i} tail")).collect();
    let mut base = PackedLines::from(&lines[..]);
    let mut spans: Vec<Vec<HiSpan>> = (0..6).map(|_| vec![span(6, 10)]).collect();
    let mut brackets = vec![Vec::new(); 6];
    lines[0].insert(0, 'X');
    lines[5].insert(0, 'X');
    carry_highlights(&mut base, &mut spans, &mut brackets, &lines);
    let want: Vec<Vec<(usize, usize, Style)>> = (0..6)
        .map(|i| {
            let at = if i == 0 || i == 5 { 7 } else { 6 };
            vec![(at, at + 4, style)]
        })
        .collect();
    assert_eq!(span_tuples(&spans), want);
}

#[test]
fn carry_splits_a_span_at_a_new_line_break() {
    let (s, z) = (
        Style::default().fg(Color::Red),
        Style::default().fg(Color::Green),
    );
    let span = |start, end, style| HiSpan { start, end, style };
    let mut base = PackedLines::from(vec![
        String::from("a"),
        String::from("\"hello\""),
        String::from("z"),
    ]);
    let mut spans = vec![Vec::new(), vec![span(0, 7, s)], vec![span(0, 1, z)]];
    let mut brackets = vec![Vec::new(); 3];
    let lines = vec![
        String::from("a"),
        String::from("\"hel"),
        String::from("lo\""),
        String::from("z"),
    ];
    carry_highlights(&mut base, &mut spans, &mut brackets, &lines);
    assert_eq!(
        span_tuples(&spans),
        vec![
            Vec::new(),
            vec![(0, 4, s)],
            vec![(0, 3, s)],
            vec![(0, 1, z)]
        ],
        "the string's color follows both halves; the line below moved down"
    );
    assert_eq!(brackets.len(), lines.len());
}

#[test]
fn carry_moves_brackets_with_their_text_and_drops_deleted_ones() {
    let carried = |before: &str, brackets: Vec<(u32, u8)>, after: &str| {
        let mut base = PackedLines::from(vec![String::from(before)]);
        let mut spans = vec![Vec::new()];
        let mut brackets = vec![brackets];
        carry_highlights(&mut base, &mut spans, &mut brackets, &[String::from(after)]);
        brackets.remove(0)
    };
    let nested = vec![(1, 0), (5, 1), (7, 1), (8, 0)];
    assert_eq!(
        carried("f(a, [b])", nested.clone(), "f([b])"),
        vec![(1, 0), (2, 1), (4, 1), (5, 0)],
        "deleting `a, ` pulls the rest left"
    );
    assert_eq!(
        carried("f(a, [b])", nested, "f(a, b])"),
        vec![(1, 0), (6, 1), (7, 0)],
        "a deleted bracket takes its color with it"
    );
    assert_eq!(
        carried("é(x)", vec![(1, 0), (3, 0)], "→é(x)"),
        vec![(2, 0), (4, 0)],
        "columns count chars, not bytes"
    );
}

/// One minified line, as a bundler writes it: a bracket every few chars.
fn minified_line(functions: usize) -> String {
    (0..functions)
        .map(|i| format!("function f{i}(a,b){{return a*{i}+b}}"))
        .collect::<Vec<_>>()
        .join(";")
}

/// Each bracket of `line` at its char column, the way a pass colors them.
fn bracket_columns(line: &str) -> Vec<(u32, u8)> {
    line.chars()
        .enumerate()
        .filter(|(_, c)| "(){}[]".contains(*c))
        .map(|(col, _)| (col as u32, (col % 3) as u8))
        .collect()
}

#[test]
fn carry_moves_the_brackets_of_a_long_minified_line_in_one_pass() {
    // #1214: each bracket's new column was counted from the start of
    // its line, so a keystroke on a 914 KB line (96,000 brackets) cost
    // about 3 s before the key was drawn.
    let before = minified_line(24_000);
    let brackets = bracket_columns(&before);
    assert!(brackets.len() > 90_000, "{}", brackets.len());
    let after = format!("x{before}");
    // A token every few bytes, each with its color, as a pass leaves them.
    let style = Style::default().fg(Color::Blue);
    let tokens: Vec<HiSpan> = (0..before.len() / 4)
        .map(|k| HiSpan::new(4 * k, 4 * k + 3, style))
        .collect();
    let mut base = PackedLines::from(vec![before]);
    let mut spans = vec![tokens.clone()];
    let mut carried = vec![brackets.clone()];
    let started = std::time::Instant::now();
    carry_highlights(&mut base, &mut spans, &mut carried, &[after]);
    let took = started.elapsed();
    // The one pass takes a few hundred milliseconds in a debug build,
    // more on a loaded runner, where a fixed 500 ms failed one run in
    // four. The per-bracket count it replaced took seconds, so a budget
    // scaled for the load still catches it.
    let budget = crate::test_budget::spawn_budget(std::time::Duration::from_millis(250));
    assert!(
        took < budget,
        "one keystroke's carry took {took:?}, over its {budget:?} budget"
    );
    let shifted: Vec<(u32, u8)> = brackets.iter().map(|&(c, d)| (c + 1, d)).collect();
    assert_eq!(carried[0], shifted, "every bracket moved one column right");
    assert_eq!(spans[0].len(), tokens.len());
    assert_eq!(
        (spans[0][1].start, spans[0][1].end),
        (5, 8),
        "the spans moved too"
    );
}

#[test]
fn carry_places_brackets_as_counting_from_the_line_start_would() {
    // The single pass must land every bracket where counting its
    // column from scratch does: across new line breaks, deletions
    // that take brackets with them, and multi-byte text on either side.
    fn counted(old: &str, brackets: &[(u32, u8)], new: &str) -> Vec<Vec<(u32, u8)>> {
        let (head, tail) = common_affixes(old, new);
        let (old_end, new_end) = (old.len() - tail, new.len() - tail);
        let new_lines: Vec<&str> = new.split('\n').collect();
        let mut out = vec![Vec::new(); new_lines.len()];
        for &(col, depth) in brackets {
            let at = old.char_indices().nth(col as usize).unwrap().0;
            if at >= head && at < old_end {
                continue;
            }
            let moved = if at < head {
                at
            } else {
                at - old_end + new_end
            };
            let li = new[..moved].matches('\n').count();
            let start = new[..moved].rfind('\n').map_or(0, |n| n + 1);
            out[li].push((new[start..moved].chars().count() as u32, depth));
        }
        out
    }
    let old = "é(a, [b]) → {c}";
    let brackets = bracket_columns(old);
    for new in [
        "é(a, [b]) → {c}",
        "Xé(a, [b]) → {c}",
        "é(a,\n [b]) → {c}",
        "é(a, [b])\n→\n{c}",
        "é(a, b]) → {c}",
        "é() → {c}",
        "é(a, [b]) → {ç}",
        "日本(a, [b]) → {c}",
        "é(a, [b]) → {c}✓",
        "",
    ] {
        let mut base = PackedLines::from(vec![String::from(old)]);
        let mut spans = vec![Vec::new()];
        let mut carried = vec![brackets.clone()];
        let lines: Vec<String> = new.split('\n').map(String::from).collect();
        carry_highlights(&mut base, &mut spans, &mut carried, &lines);
        assert_eq!(carried, counted(old, &brackets, new), "edit: {new:?}");
    }
}

#[test]
fn carry_starts_clean_when_the_spans_do_not_describe_the_base() {
    // A non-text view resets `highlights` to one empty line without
    // touching the base; carrying from that must not misplace anything.
    let mut base = PackedLines::from(vec![String::from("a"), String::from("b")]);
    let mut spans = vec![Vec::new()];
    let mut brackets = Vec::new();
    let lines = vec![String::from("x()")];
    assert!(carry_highlights(
        &mut base,
        &mut spans,
        &mut brackets,
        &lines
    ));
    assert_eq!((spans.len(), brackets.len()), (1, 1));
    assert!(spans[0].is_empty() && brackets[0].is_empty());
    assert_eq!(base, PackedLines::from(&lines[..]));
}

/// What a syntax pass leaves is kept for as long as the tab (#1707): each
/// line's spans and brackets are held at their size, not at whatever the
/// list had doubled to while it grew.
#[test]
fn a_landed_pass_holds_no_spare_capacity() {
    let src: String = (0..200)
        .map(|i| format!("fn f{i}(a: u8, b: [u8; 4]) -> u8 {{ a + b[0] * {i} }}\n"))
        .collect();
    let mut e = editor_with(&src);
    e.lang = Some(LangKind::Rust);
    e.recompute_highlights();
    assert!(
        e.highlights.iter().any(|line| line.len() > 4),
        "lines carry several spans"
    );
    for (i, line) in e.highlights.iter().enumerate() {
        assert_eq!(line.capacity(), line.len(), "line {i}'s spans");
    }
    for (i, line) in e.bracket_colors.iter().enumerate() {
        assert_eq!(line.capacity(), line.len(), "line {i}'s brackets");
    }
}

/// The semantic overlay is decoded into lists sized to each line's tokens
/// (#1707), not grown to the next power of two.
#[test]
fn a_decoded_semantic_overlay_holds_no_spare_capacity() {
    let mut e = editor_with("def f(x, y, z):\n    return x + y + z + x + y\n");
    let p = std::path::PathBuf::from("/tmp/sem_fit.py");
    e.path = Some(p.clone());
    let legend = std::sync::Arc::new(vec!["parameter".to_string()]);
    let data = vec![
        0, 6, 1, 0, 0, 0, 3, 1, 0, 0, 0, 3, 1, 0, 0, // x, y, z
        1, 11, 1, 0, 0, 0, 4, 1, 0, 0, 0, 4, 1, 0, 0, // x + y + z
        0, 4, 1, 0, 0, 0, 4, 1, 0, 0, // + x + y
    ];
    e.apply_semantic_tokens(p, data, legend, true);
    let overlay = e.semantic_overlay_for_test();
    assert_eq!((overlay[0].len(), overlay[1].len()), (3, 5));
    for (i, line) in overlay.iter().enumerate() {
        assert_eq!(line.capacity(), line.len(), "line {i}");
    }
}

#[test]
fn edits_made_while_a_pass_runs_ride_one_follow_up_pass() {
    let mut e = editor_with("fn main() {\n    let value = compute(1, \"two\");\n}");
    e.set_language(Some(LangKind::Rust));
    let settled = e.highlight_generation();
    e.defer_highlights_for_test();
    e.cursor_row = 1;
    e.cursor_col = 10;
    for c in "xyz".chars() {
        e.insert_char(c);
    }
    assert_eq!(e.lines[1], "    let vaxyzlue = compute(1, \"two\");");
    assert!(e.highlight_pending(), "the keystrokes did not wait");
    assert_eq!(
        e.highlight_generation(),
        settled,
        "no pass landed during the keystrokes"
    );
    let covering = |at: usize| {
        e.highlights[1]
            .iter()
            .find(|s| s.range().contains(&at))
            .map(|s| s.range())
    };
    assert_eq!(
        covering(8),
        covering(12),
        "the identifier's span stretched over the typed text"
    );
    // The pass the first key started saw one key; the two after it ride
    // a single follow-up, not one pass each.
    e.await_highlight_pass(None);
    assert_eq!(e.highlight_generation(), settled + 2);
    assert!(!e.highlight_pending());
    let mut fresh = editor_with(&e.lines.join("\n"));
    fresh.set_language(Some(LangKind::Rust));
    assert_eq!(span_tuples(&e.highlights), span_tuples(&fresh.highlights));
    assert_eq!(e.bracket_colors, fresh.bracket_colors);
}

#[test]
fn a_pass_for_a_language_the_buffer_left_is_dropped() {
    let mut e = editor_with("fn a() {}");
    e.set_language(Some(LangKind::Rust));
    e.defer_highlights_for_test();
    e.insert_char('x');
    assert!(e.highlight_pending());
    // What the non-text viewers do (`open_image` and the like).
    e.lang = None;
    e.highlights = vec![Vec::new()];
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while e.highlight_pending() {
        assert!(!e.poll_highlights(), "a Rust pass never lands on it");
        assert!(std::time::Instant::now() < deadline, "the pass never ended");
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(e.highlights[0].is_empty());
}

/// Every carried span sits inside its line on char boundaries, and every
/// carried bracket color still sits on a bracket.
fn assert_highlights_sound(e: &Editor, after: &str) {
    assert_eq!(e.highlights.len(), e.lines.len(), "{after}");
    assert_eq!(e.bracket_colors.len(), e.lines.len(), "{after}");
    for (i, line) in e.lines.iter().enumerate() {
        for s in &e.highlights[i] {
            let r = s.range();
            assert!(
                r.start < r.end
                    && r.end <= line.len()
                    && line.is_char_boundary(r.start)
                    && line.is_char_boundary(r.end),
                "{after}: span {}..{} on line {i} {line:?}",
                s.start,
                s.end
            );
        }
        let chars: Vec<char> = line.chars().collect();
        for &(col, _) in &e.bracket_colors[i] {
            assert!(
                matches!(
                    chars.get(col as usize),
                    Some('(' | ')' | '[' | ']' | '{' | '}')
                ),
                "{after}: bracket color at {col} on line {i} {line:?}"
            );
        }
    }
}

#[test]
fn random_edits_keep_carried_colors_sound_and_settle_on_a_fresh_pass() {
    let docs = [
        (
            LangKind::Rust,
            "// head (note)\nfn main() {\n    let s = \"a (b) é\";\n    if x[0] { call(1, [2, 3]); }\n}\n/* block\n   { comment */\nstruct P { x: i32 }",
        ),
        (
            LangKind::Markdown,
            "# Title (one)\n\nSome *em* and `code (x)` → [link](http://x).\n\n```rust\nfn f() { g(1) }\n```\n\n- item **bold** [2]",
        ),
    ];
    let pool: Vec<char> = "a(]{}\"/*é→ `#\n".chars().collect();
    let mut state: u64 = 0x2545_f491_4f6c_dd1d;
    let mut next = |n: usize| {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((state >> 33) as usize) % n
    };
    for (kind, text) in docs {
        let mut e = editor_with(text);
        e.set_language(Some(kind));
        e.defer_highlights_for_test();
        for step in 0..300 {
            e.cursor_row = next(e.lines.len());
            e.cursor_col = next(e.lines[e.cursor_row].chars().count() + 1);
            match next(6) {
                0..=2 => e.insert_char(pool[next(pool.len())]),
                3 => e.backspace(),
                4 => e.delete_forward(),
                _ => e.insert_newline(),
            }
            let after = format!("{kind:?} step {step}");
            assert_highlights_sound(&e, &after);
            if next(5) == 0 {
                e.poll_highlights();
                assert_highlights_sound(&e, &after);
            }
        }
        e.await_highlight_pass(None);
        let mut fresh = editor_with(&e.lines.join("\n"));
        fresh.lines = e.lines.clone();
        fresh.set_language(Some(kind));
        assert_eq!(
            span_tuples(&e.highlights),
            span_tuples(&fresh.highlights),
            "{kind:?}: settled colors are a whole-buffer pass's"
        );
        assert_eq!(e.bracket_colors, fresh.bracket_colors, "{kind:?}");
    }
}

#[test]
fn scan_color_swatches_recognizes_hex_forms() {
    let lines = vec![String::from("bg: #f00; border: #00ff0080; a: #abcabcabc")];
    let out = scan_color_swatches(&lines, false);
    // #f00 -> (255,0,0); #00ff0080 -> (0,255,0); the trailing 9-digit
    // run is not any valid CSS hex length and must not match at all.
    assert_eq!(out[0].len(), 2, "found: {:?}", out[0]);
    assert_eq!(out[0][0].2, Color::Rgb(255, 0, 0));
    assert_eq!(out[0][1].2, Color::Rgb(0, 255, 0));
}

#[test]
fn scan_color_swatches_recognizes_rgb_and_hsl() {
    let lines = vec![String::from(
        "color: rgb(255, 0, 0); fill: hsl(0, 100%, 50%);",
    )];
    let out = scan_color_swatches(&lines, false);
    assert_eq!(out[0].len(), 2, "found: {:?}", out[0]);
    assert_eq!(out[0][0].2, Color::Rgb(255, 0, 0));
    // hsl(0, 100%, 50%) is pure red too.
    assert_eq!(out[0][1].2, Color::Rgb(255, 0, 0));
}

#[test]
fn scan_color_swatches_ignores_hex_like_identifiers() {
    // A hex digit run of invalid CSS length (12 digits, immediately
    // followed by another hex digit) must not match as any color form.
    let lines = vec![String::from("let x = #abcdefabcdefg;")];
    let out = scan_color_swatches(&lines, false);
    assert!(out[0].is_empty(), "found: {:?}", out[0]);
}

#[test]
fn scan_color_swatches_skips_issue_refs_fragments_and_entities() {
    let lines = vec![String::from(
        "fixes #555 and #1234; see page#fade; &#123; but #fade and #000000",
    )];
    let out = scan_color_swatches(&lines, false);
    let found: Vec<_> = out[0].iter().map(|h| h.0).collect();
    // Only the free-standing `#fade` (col 48) and `#000000` (col 58).
    assert_eq!(found, vec![48, 58], "found: {:?}", out[0]);

    // In a stylesheet `#555` is a color.
    let css = scan_color_swatches(&[String::from("color: #555;")], true);
    assert_eq!(css[0].len(), 1);
    assert_eq!(css[0][0].2, Color::Rgb(0x55, 0x55, 0x55));
}

#[test]
fn scan_color_swatches_counts_columns_past_multibyte_text() {
    let lines = vec![String::from("é → #ff0000")];
    let out = scan_color_swatches(&lines, false);
    assert_eq!((out[0][0].0, out[0][0].1), (4, 11));
}

#[test]
fn scan_color_swatches_picks_a_legible_foreground() {
    let lines = vec![String::from("#ffffff #000000")];
    let out = scan_color_swatches(&lines, false);
    assert_eq!(out[0][0].3, Color::Black, "white bg needs black text");
    assert_eq!(out[0][1].3, Color::White, "black bg needs white text");
}

#[test]
fn color_swatch_scan_runs_in_any_language_including_comments() {
    // Unlike syntax highlighting, a color swatch has no notion of
    // "inside a comment" — the whole point is spotting color literals
    // wherever a human left one, e.g. a Rust doc comment.
    let mut e = editor_with("// legacy brand color: #336699\nfn f() {}");
    e.set_language(Some(LangKind::Rust));
    assert_eq!(e.color_swatches[0].len(), 1);
    assert_eq!(e.color_swatches[0][0].2, Color::Rgb(0x33, 0x66, 0x99));
}

#[test]
fn render_paints_a_color_swatch_background_over_the_literal() {
    let mut e = editor_with("bg: #ff0000;");
    e.recompute_highlights();
    let buf = guide_buf(&mut e, 40, 6);
    let text_x = e.last_inner.x + e.last_gutter_width + 1;
    let y0 = e.last_inner.y;
    // "#ff0000" starts at column 4 ("bg: " is 4 chars).
    assert_eq!(buf[(text_x + 4, y0)].bg, Color::Rgb(255, 0, 0));
    assert_eq!(buf[(text_x + 4, y0)].fg, Color::White);
    // The text before the literal is untouched.
    assert_ne!(buf[(text_x, y0)].bg, Color::Rgb(255, 0, 0));
}

#[test]
fn plain_text_color_swatch_scan_is_gated_by_buffer_size() {
    let big_line = "x".repeat(64 * 1024);
    let mut e = editor_with("");
    e.lines = vec![big_line; (COLOR_SWATCH_SCAN_MAX_BYTES / (64 * 1024)) + 2];
    e.lines.push(String::from("#ff0000"));
    e.recompute_highlights();
    assert!(
        e.color_swatches.iter().all(Vec::is_empty),
        "an oversized buffer opts out of the color-swatch scan"
    );
}

#[test]
fn render_colors_brackets_by_nesting_depth() {
    let mut e = editor_with("a(b[c{d}e]f)g");
    e.recompute_highlights();
    let buf = guide_buf(&mut e, 40, 6);
    let text_x = e.last_inner.x + e.last_gutter_width + 1;
    let y0 = e.last_inner.y;
    for (col, depth) in [(1u16, 0), (3, 1), (5, 2), (7, 2), (9, 1), (11, 0)] {
        assert_eq!(
            buf[(text_x + col, y0)].fg,
            e.theme.bracket_pair_color(depth),
            "bracket at col {col} wears depth-{depth} colour"
        );
    }
    assert_ne!(
        buf[(text_x + 2, y0)].fg,
        e.theme.bracket_pair_color(0),
        "non-bracket text keeps its syntax colour"
    );
}

#[test]
fn render_paints_unmatched_closer_red() {
    let mut e = editor_with("x)");
    e.recompute_highlights();
    let buf = guide_buf(&mut e, 40, 6);
    let text_x = e.last_inner.x + e.last_gutter_width + 1;
    assert_eq!(
        buf[(text_x + 1, e.last_inner.y)].fg,
        e.theme.bracket_unexpected_fg()
    );
}

#[test]
fn render_bracket_colors_toggle_off_keeps_syntax_color() {
    let mut e = editor_with("a(b)c");
    e.recompute_highlights();
    e.show_bracket_colors = false;
    let buf = guide_buf(&mut e, 40, 6);
    let text_x = e.last_inner.x + e.last_gutter_width + 1;
    assert_ne!(
        buf[(text_x + 1, e.last_inner.y)].fg,
        e.theme.bracket_pair_color(0)
    );
}

// ---- Overlays after wide characters (#873) ----

/// A cell Croft Loch has tinted as a find match: an amber fill, red
/// well above blue, which no other Loch editor fill (canvas, current
/// line, selection, occurrence grey) is.
fn loch_find_tint(c: &ratatui::buffer::Cell) -> bool {
    matches!(c.bg, Color::Rgb(r, _, b) if r > b.saturating_add(24))
}

/// The glyphs on the first text row whose cells `pick` selects, left to
/// right: what an overlay covers on screen.
fn glyphs_where(e: &Editor, buf: &Buffer, pick: impl Fn(&ratatui::buffer::Cell) -> bool) -> String {
    let y = e.last_inner.y;
    let text_x = e.last_inner.x + e.last_gutter_width + 1;
    (text_x..e.last_inner.x + e.last_inner.width)
        .filter(|&x| pick(&buf[(x, y)]))
        .map(|x| buf[(x, y)].symbol())
        .collect()
}

#[test]
fn row_cells_count_a_wide_character_as_two_cells_and_splice_hints() {
    let mut row = RowCells::default();
    row.fill("a\u{8cbb}b", 0, 0, [(2, 3)]);
    assert_eq!((row.cell(1), row.width(1)), (1, 2), "費 takes two cells");
    assert_eq!(row.cell(2), 6, "b sits past 費 and the hint anchored at it");
    assert_eq!(row.caret(2), 3, "a caret at the anchor is left of it");
    assert_eq!(row.end(2), 3, "a range ending there leaves it out");
    assert_eq!(row.end(4), 8, "the newline cell follows the text");
    row.fill("\u{8cbb}b", 1, 0, []);
    assert_eq!((row.cell(0), row.cell(2)), (0, 2), "scrolled to 費");
}

/// #873: a grapheme cluster is one glyph sized as a whole, as ratatui
/// paints it, so the row agrees with the painter's own `CellMap` after
/// an emoji with a skin tone, one with VS16, a ZWJ family and a
/// combining mark, not only after a lone wide character.
#[test]
fn row_cells_size_a_grapheme_cluster_as_ratatui_paints_it() {
    let line = "a\u{8cbb} \u{1f44d}\u{1f3fd} \u{2764}\u{fe0f} \u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467} e\u{301} z";
    let row = RowCells::of_line(line, 0);
    let painted = crate::cell_map::CellMap::new(line);
    for col in 0..=line.chars().count() {
        assert_eq!(
            row.text_before(col),
            usize::from(painted.cell_of_char(col)),
            "column {col}"
        );
    }
}

#[test]
fn find_highlight_covers_its_glyphs_after_emoji_sequences() {
    let mut e = editor_with("x = \"\u{1f44d}\u{1f3fd}\u{2764}\u{fe0f}\"  # food");
    e.set_search_highlight(Some("food".into()));
    let buf = guide_buf(&mut e, 60, 5);
    assert_eq!(glyphs_where(&e, &buf, loch_find_tint), "food");
}

#[test]
fn find_and_occurrence_highlights_cover_their_glyphs_after_wide_characters() {
    let mut e = editor_with("label = \"\u{98df}\u{8cbb}\"  # food (groceries)");
    e.set_search_highlight(Some("food".into()));
    e.occurrences = vec![(0, 22, 31, false)];
    let buf = guide_buf(&mut e, 60, 5);
    assert_eq!(glyphs_where(&e, &buf, loch_find_tint), "food");
    let occ_bg = e.theme.occurrence_bg();
    assert_eq!(glyphs_where(&e, &buf, |c| c.bg == occ_bg), "groceries");
}

#[test]
fn the_selection_band_and_its_whitespace_dots_cover_the_selected_glyphs_after_wide_characters() {
    let mut e = editor_with("label = \"\u{98df}\u{8cbb}\"  # food");
    e.selection = Some(EditorSelection {
        anchor: (0, 14),
        head: (0, 18),
    });
    let buf = guide_buf(&mut e, 60, 5);
    let band = e.theme.ui(Color::Rgb(0x26, 0x4f, 0x78));
    assert_eq!(glyphs_where(&e, &buf, |c| c.bg == band), "#\u{b7}fo");
}

#[test]
fn bracket_colours_and_squiggles_land_on_their_glyphs_after_wide_characters() {
    use crate::lsp::manager::DiagnosticSeverity;
    let mut e = editor_with("items = {\"\u{8cbb}\": (1, x)}");
    e.recompute_highlights();
    let p = std::path::PathBuf::from("/tmp/diag.ts");
    e.path = Some(p.clone());
    e.apply_diagnostics(p, vec![diag(0, 18, 0, 19, DiagnosticSeverity::Error)]);
    let buf = guide_buf(&mut e, 60, 5);
    let pair = [e.theme.bracket_pair_color(0), e.theme.bracket_pair_color(1)];
    assert_eq!(glyphs_where(&e, &buf, |c| pair.contains(&c.fg)), "{()}");
    let squiggle = |c: &ratatui::buffer::Cell| c.modifier.contains(Modifier::UNDERLINED);
    assert_eq!(glyphs_where(&e, &buf, squiggle), "x");
}

// ---- Render whitespace (#133) ----

#[test]
fn selection_mode_paints_whitespace_glyphs_only_inside_the_selection() {
    let mut e = editor_with("a b c d");
    e.selection = Some(EditorSelection {
        anchor: (0, 3),
        head: (0, 6),
    });
    let buf = guide_buf(&mut e, 40, 6);
    let text_x = e.last_inner.x + e.last_gutter_width + 1;
    let y0 = e.last_inner.y;
    assert_eq!(buf[(text_x + 3, y0)].symbol(), "\u{b7}", "selected space");
    assert_eq!(buf[(text_x + 3, y0)].fg, e.theme.whitespace_fg());
    assert_eq!(buf[(text_x + 5, y0)].symbol(), "\u{b7}");
    assert_eq!(
        buf[(text_x + 1, y0)].symbol(),
        " ",
        "an unselected space stays invisible"
    );
    assert_eq!(buf[(text_x + 4, y0)].symbol(), "c", "text is untouched");
}

#[test]
fn all_mode_paints_every_space_and_tab() {
    let mut e = editor_with("a b\n\tc");
    e.whitespace_mode = WhitespaceMode::All;
    let buf = guide_buf(&mut e, 40, 6);
    let text_x = e.last_inner.x + e.last_gutter_width + 1;
    let y0 = e.last_inner.y;
    assert_eq!(buf[(text_x + 1, y0)].symbol(), "\u{b7}");
    assert_eq!(
        buf[(text_x, y0 + 1)].symbol(),
        "\u{2192}",
        "a tab renders as an arrow"
    );
}

/// #891: a tab's arrow sits on the tab's own cell. While the tab was
/// drawn as no cell, the arrow covered the `r` of `return` and the dots
/// after a mid-line tab covered the letters that followed it.
#[test]
fn all_mode_glyphs_after_a_tab_leave_the_text_alone() {
    let mut e = editor_with("\treturn 1\n  x = 2\t# tab");
    e.whitespace_mode = WhitespaceMode::All;
    let buf = guide_buf(&mut e, 40, 6);
    let text_x = e.last_inner.x + e.last_gutter_width + 1;
    let y0 = e.last_inner.y;
    assert_eq!(
        row_text(&buf, y0, text_x, text_x + 12),
        "\u{2192}   return\u{b7}1"
    );
    assert_eq!(
        row_text(&buf, y0 + 1, text_x, text_x + 13),
        "\u{b7}\u{b7}x\u{b7}=\u{b7}2\u{2192}#\u{b7}tab"
    );
}

#[test]
fn none_mode_paints_no_whitespace_glyphs_even_selected() {
    let mut e = editor_with("a b");
    e.whitespace_mode = WhitespaceMode::None;
    e.selection = Some(EditorSelection {
        anchor: (0, 0),
        head: (0, 3),
    });
    let buf = guide_buf(&mut e, 40, 6);
    let text_x = e.last_inner.x + e.last_gutter_width + 1;
    assert_eq!(buf[(text_x + 1, e.last_inner.y)].symbol(), " ");
}

#[test]
fn selection_mode_covers_every_line_of_a_multiline_selection() {
    let mut e = editor_with("a b\nc d");
    e.selection = Some(EditorSelection {
        anchor: (0, 0),
        head: (1, 3),
    });
    let buf = guide_buf(&mut e, 40, 6);
    let text_x = e.last_inner.x + e.last_gutter_width + 1;
    let y0 = e.last_inner.y;
    assert_eq!(buf[(text_x + 1, y0)].symbol(), "\u{b7}");
    assert_eq!(buf[(text_x + 1, y0 + 1)].symbol(), "\u{b7}");
}

// ---- Debugger inline values (#135) ----

fn locals(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(n, v)| ((*n).to_string(), (*v).to_string()))
        .collect()
}

#[test]
fn inline_values_annotate_function_lines_up_to_the_stop_only() {
    let mut e = editor_with("def f():\n    x = 1\n    y = x + 1\n    print(x, y)\n\nz = 9");
    // Stopped ON line 3 (1-based): `y = x + 1` is about to run.
    e.set_inline_values_from_locals(3, &locals(&[("x", "1"), ("y", "2")]));
    assert_eq!(e.inline_values.get(&1).map(String::as_str), Some("x = 1"));
    assert_eq!(
        e.inline_values.get(&2).map(String::as_str),
        Some("y = 2, x = 1"),
        "first-mention order on the stop line"
    );
    assert!(
        !e.inline_values.contains_key(&3),
        "lines past the execution point stay bare"
    );
    assert!(!e.inline_values.contains_key(&5), "outside the function");
}

#[test]
fn inline_values_span_the_whole_function_from_a_nested_block_stop() {
    let mut e =
        editor_with("def f():\n    x = 1\n    if True:\n        y = 2\n        print(x, y)");
    // Stopped on the print, inside the `if` block: the scan must climb to
    // `def f():`, not stop at `if True:` (#136 review).
    e.set_inline_values_from_locals(5, &locals(&[("x", "1"), ("y", "2")]));
    assert_eq!(
        e.inline_values.get(&1).map(String::as_str),
        Some("x = 1"),
        "a line above the nested block still annotates"
    );
    assert_eq!(
        e.inline_values.get(&4).map(String::as_str),
        Some("x = 1, y = 2")
    );
}

#[test]
fn inline_values_clear_when_a_reused_tab_opens_a_different_file() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.py");
    let b = dir.path().join("b.py");
    std::fs::write(&a, "x = 1\n").unwrap();
    std::fs::write(&b, "y = 2\n").unwrap();
    let mut e = Editor::new();
    e.open(&a).unwrap();
    e.inline_values.insert(0, String::from("x = 1"));
    e.open(&b).unwrap();
    assert!(
        e.inline_values.is_empty(),
        "a different file must not inherit the old file's trailers"
    );
}

#[test]
fn inline_values_match_whole_identifiers_only() {
    let mut e = editor_with("def f():\n    max = 9\n    q = max");
    e.set_inline_values_from_locals(3, &locals(&[("x", "1"), ("q", "7")]));
    assert!(
        !e.inline_values.contains_key(&1),
        "`x` must not match inside `max`"
    );
    assert_eq!(e.inline_values.get(&2).map(String::as_str), Some("q = 7"));
}

#[test]
fn inline_values_elide_long_values_in_the_middle() {
    let mut e = editor_with("def f():\n    x = 1");
    let long = "a".repeat(60);
    e.set_inline_values_from_locals(2, &locals(&[("x", &long)]));
    let note = e.inline_values.get(&1).unwrap();
    assert!(note.contains('\u{2026}'), "middle ellipsis: {note}");
    assert!(note.chars().count() < 60, "elided: {note}");
}

#[test]
fn inline_values_clear_when_locals_are_empty() {
    let mut e = editor_with("def f():\n    x = 1");
    e.set_inline_values_from_locals(2, &locals(&[("x", "1")]));
    assert!(!e.inline_values.is_empty());
    e.set_inline_values_from_locals(2, &[]);
    assert!(e.inline_values.is_empty());
}

#[test]
fn render_paints_inline_value_trailer_after_the_line_end() {
    let mut e = editor_with("x = 1\ny = 2");
    e.inline_values.insert(0, String::from("x = 1"));
    let buf = guide_buf(&mut e, 40, 6);
    let text_x = e.last_inner.x + e.last_gutter_width + 1;
    let y0 = e.last_inner.y;
    // line 0 is 5 chars wide; the trailer starts 2 cells past it.
    let start = text_x + 5 + 2;
    assert_eq!(buf[(start, y0)].symbol(), "x");
    assert_eq!(buf[(start, y0)].fg, e.theme.ignored_fg());
    assert_eq!(buf[(start + 2, y0)].symbol(), "=");
    assert_eq!(
        buf[(text_x + 5 + 2, y0 + 1)].symbol(),
        " ",
        "an unannotated line gets no trailer"
    );
}

fn live_view(lines: &[&str], notes: &[(usize, &str)]) -> crate::live_run::View {
    let mut report = crate::live_run::Report::default();
    for &(line, text) in notes {
        report
            .notes
            .insert(line, vec![(text.to_string(), crate::live_run::Kind::Value)]);
        report.coverage.insert(line, crate::live_run::Coverage::Ran);
    }
    crate::live_run::View {
        lines: lines.iter().map(|l| l.to_string()).collect(),
        report,
    }
}

#[test]
fn render_paints_a_live_run_trailer_and_tints_the_line_number() {
    let mut e = editor_with("x = f()\ny = 2");
    e.live_run = Some(live_view(&["x = f()", "y = 2"], &[(0, "x = 7")]));
    let buf = guide_buf(&mut e, 40, 6);
    let text_x = e.last_inner.x + e.last_gutter_width + 1;
    let y0 = e.last_inner.y;
    let start = text_x + 7 + 2;
    assert_eq!(buf[(start, y0)].symbol(), "x");
    assert_eq!(buf[(start + 4, y0)].symbol(), "7");
    let number_x = (e.last_inner.x..text_x)
        .find(|&x| buf[(x, y0)].symbol() == "1")
        .expect("line number painted");
    assert_eq!(
        buf[(number_x, y0)].fg,
        e.theme.git_added(),
        "a line that ran is green"
    );
}

#[test]
fn a_live_run_trailer_hides_once_its_line_is_edited() {
    let mut e = editor_with("x = f() + 1");
    e.live_run = Some(live_view(&["x = f()"], &[(0, "x = 7")]));
    let buf = guide_buf(&mut e, 40, 6);
    let text_x = e.last_inner.x + e.last_gutter_width + 1;
    assert_eq!(
        buf[(text_x + 11 + 2, e.last_inner.y)].symbol(),
        " ",
        "the value belongs to the text that ran, not this one"
    );
}

#[test]
fn debugger_inline_values_outrank_the_live_run_trailer() {
    let mut e = editor_with("x = f()");
    e.live_run = Some(live_view(&["x = f()"], &[(0, "x = 7")]));
    e.inline_values.insert(0, String::from("x = 9"));
    let buf = guide_buf(&mut e, 40, 6);
    let text_x = e.last_inner.x + e.last_gutter_width + 1;
    assert_eq!(buf[(text_x + 7 + 2 + 4, e.last_inner.y)].symbol(), "9");
}

#[test]
fn identifier_tokens_split_on_word_boundaries() {
    assert_eq!(
        identifier_tokens("print(x, _y2) + max"),
        vec!["print", "x", "_y2", "max"]
    );
}

// ---- Select to Bracket (editor.action.selectToBracket) ----

#[test]
fn select_to_bracket_includes_both_brackets() {
    let mut e = editor_with("foo(bar)baz");
    e.cursor_row = 0;
    e.cursor_col = 3; // on the '('
    assert!(e.select_to_matching_bracket());
    assert_eq!(e.selection_text(), "(bar)");
}

#[test]
fn select_to_bracket_from_inside_selects_enclosing_pair() {
    let mut e = editor_with("foo(bar)baz");
    e.cursor_row = 0;
    e.cursor_col = 5; // inside the parentheses
    assert!(e.select_to_matching_bracket());
    assert_eq!(e.selection_text(), "(bar)");
}

#[test]
fn select_to_bracket_spans_multiple_lines() {
    let mut e = editor_with("fn f() {\n    x\n}");
    e.cursor_row = 0;
    e.cursor_col = 7; // on the '{'
    assert!(e.select_to_matching_bracket());
    assert_eq!(e.selection_text(), "{\n    x\n}");
}

// ---- Transpose Characters (editor.action.transpose) ----

#[test]
fn transpose_swaps_chars_around_cursor_and_advances() {
    let mut e = editor_with("abcd");
    e.cursor_row = 0;
    e.cursor_col = 2; // between 'b' and 'c'
    e.transpose_chars();
    assert_eq!(e.lines, vec!["acbd"]);
    assert_eq!((e.cursor_row, e.cursor_col), (0, 3));
}

#[test]
fn transpose_at_line_start_only_advances() {
    let mut e = editor_with("abcd");
    e.cursor_row = 0;
    e.cursor_col = 0;
    e.transpose_chars();
    assert_eq!(e.lines, vec!["abcd"]);
    assert_eq!((e.cursor_row, e.cursor_col), (0, 1));
}

#[test]
fn transpose_at_end_of_line_moves_char_across_break() {
    let mut e = editor_with("ab\ncd");
    e.cursor_row = 0;
    e.cursor_col = 2; // end of "ab", not the last line
    e.transpose_chars();
    assert_eq!(e.lines, vec!["a", "bcd"]);
    assert_eq!((e.cursor_row, e.cursor_col), (1, 1));
}

#[test]
fn transpose_at_end_of_final_line_is_noop() {
    let mut e = editor_with("abc");
    e.cursor_row = 0;
    e.cursor_col = 3; // end of the only line
    e.transpose_chars();
    assert_eq!(e.lines, vec!["abc"]);
    assert_eq!((e.cursor_row, e.cursor_col), (0, 3));
}

// ---- Increment / Decrement Number (vim Ctrl-A / Ctrl-X) ----

fn num_editor(text: &str, col: usize) -> Editor {
    let mut e = editor_with(text);
    e.cursor_row = 0;
    e.cursor_col = col;
    e
}

// ---- Emmet: Expand Abbreviation ----

fn html_editor(text: &str, col: usize) -> Editor {
    let mut e = editor_with(text);
    e.lang = Some(LangKind::Html);
    e.cursor_row = e.lines.len() - 1;
    e.cursor_col = col;
    e
}

#[test]
fn increment_bumps_the_number_the_cursor_sits_in() {
    let mut e = num_editor("port = 8080", 8); // on the '0' of 8080
    assert!(e.bump_number(1));
    assert_eq!(e.lines, vec!["port = 8081"]);
}

/// A bump is an edit like any other: it pins a preview tab, or the next
/// preview open reuses the tab and the edit is lost with it.
#[test]
fn a_bump_pins_a_preview_tab() {
    let mut e = num_editor("port = 8080", 8);
    e.preview = true;
    assert!(e.bump_number(1));
    assert!(!e.preview);
    // A bump that finds no number edits nothing, so it pins nothing.
    let mut e = num_editor("no digits", 0);
    e.preview = true;
    assert!(!e.bump_number(1));
    assert!(e.preview);
}

#[test]
fn decrement_bumps_the_number_down() {
    let mut e = num_editor("retries = 3", 10);
    assert!(e.bump_number(-1));
    assert_eq!(e.lines, vec!["retries = 2"]);
}

/// vim's rule, and the reason the chord is usable without aiming: a
/// caret before the number still finds it.
#[test]
fn a_caret_left_of_the_number_still_finds_it() {
    let mut e = num_editor("port = 8080", 0);
    assert!(e.bump_number(1));
    assert_eq!(e.lines, vec!["port = 8081"]);
}

#[test]
fn the_nearest_number_to_the_right_wins_over_a_later_one() {
    let mut e = num_editor("a 1 b 2", 0);
    assert!(e.bump_number(1));
    assert_eq!(e.lines, vec!["a 2 b 2"]);
}

/// The caret lands on the last digit, so holding the chord keeps
/// bumping the same literal even as it grows.
#[test]
fn repeated_bumps_stay_on_the_same_number_across_a_width_change() {
    let mut e = num_editor("x = 9", 4);
    assert!(e.bump_number(1));
    assert_eq!(e.lines, vec!["x = 10"]);
    assert!(e.bump_number(1));
    assert_eq!(e.lines, vec!["x = 11"]);
    assert!(e.bump_number(1));
    assert_eq!(e.lines, vec!["x = 12"]);
}

#[test]
fn zero_padding_keeps_its_width() {
    let mut e = num_editor("v1.0.007", 6);
    assert!(e.bump_number(1));
    assert_eq!(e.lines, vec!["v1.0.008"]);
}

#[test]
fn zero_padding_widens_only_when_the_value_needs_the_room() {
    let mut e = num_editor("099", 0);
    assert!(e.bump_number(1));
    assert_eq!(e.lines, vec!["100"]);
}

#[test]
fn hex_literals_keep_their_prefix_and_digit_case() {
    let mut lower = num_editor("mask = 0xff", 9);
    assert!(lower.bump_number(1));
    assert_eq!(lower.lines, vec!["mask = 0x100"]);

    let mut upper = num_editor("mask = 0xFF", 9);
    assert!(upper.bump_number(1));
    assert_eq!(upper.lines, vec!["mask = 0x100"]);

    let mut wide = num_editor("mask = 0X00FF", 9);
    assert!(wide.bump_number(1));
    assert_eq!(wide.lines, vec!["mask = 0X0100"], "prefix case is kept");
}

#[test]
fn binary_literals_bump_in_base_two() {
    let mut e = num_editor("flags = 0b0111", 10);
    assert!(e.bump_number(1));
    assert_eq!(e.lines, vec!["flags = 0b1000"]);
}

/// vim wraps `0x0` round to `0xffffffffffffffff`, which is never what
/// the keystroke meant.
#[test]
fn a_hex_literal_clamps_at_zero_rather_than_wrapping() {
    let mut e = num_editor("0x00", 3);
    assert!(e.bump_number(-1));
    assert_eq!(e.lines, vec!["0x00"]);
}

/// A clamped bump changes nothing, so it must not dirty a clean buffer
/// or leave an undo step that undoes nothing. It still reports true: a
/// number was found, and "no number here" would be the wrong message.
#[test]
fn a_clamped_bump_neither_dirties_the_buffer_nor_records_an_undo_step() {
    let mut e = num_editor("mask = 0x00", 0);
    assert!(!e.dirty);
    assert!(e.bump_number(-1));
    assert_eq!(e.lines, vec!["mask = 0x00"]);
    assert!(!e.dirty, "an unchanged literal must not dirty the buffer");
    assert!(e.undo_stack.is_empty(), "no undo step for a no-op");
    // A bump that does change the literal still records both, so the
    // two assertions above cannot pass vacuously.
    assert!(e.bump_number(1));
    assert_eq!(e.lines, vec!["mask = 0x01"]);
    assert!(e.dirty);
    assert_eq!(e.undo_stack.len(), 1);
}

/// vim ignores a leading `-` on hex and binary literals ("ignore leading
/// '-' for hex and octal and bin numbers" in `do_addsub`), treating them
/// as unsigned bit patterns: `-0x10` bumps to `-0x11`.
#[test]
fn a_minus_before_a_hex_literal_is_not_a_sign() {
    let mut e = num_editor("x = -0x10", 0);
    assert!(e.bump_number(1));
    assert_eq!(e.lines, vec!["x = -0x11"]);
}

#[test]
fn a_decimal_can_cross_zero_into_negative() {
    let mut e = num_editor("delta = 0", 8);
    assert!(e.bump_number(-1));
    assert_eq!(e.lines, vec!["delta = -1"]);
    assert!(e.bump_number(-1));
    assert_eq!(e.lines, vec!["delta = -2"]);
    assert!(e.bump_number(1));
    assert_eq!(e.lines, vec!["delta = -1"]);
}

/// A `-` that follows a digit or an identifier is a subtraction, not a
/// sign. Dates are the case that makes this matter: bumping the month
/// of `2026-09-21` must not turn it into `2026-08-21`.
#[test]
fn a_minus_after_a_digit_is_subtraction_not_a_sign() {
    let mut e = num_editor("2026-09-21", 5);
    assert!(e.bump_number(1));
    assert_eq!(e.lines, vec!["2026-10-21"]);
}

/// The identifier rule holds for non-ASCII identifiers too: the byte
/// before the `-` in `λ-5` is a UTF-8 continuation byte, not a letter,
/// so the check has to look at the character.
#[test]
fn a_minus_after_a_non_ascii_identifier_is_subtraction_not_a_sign() {
    let mut e = num_editor("λ-5", 0);
    assert!(e.bump_number(1));
    assert_eq!(e.lines, vec!["λ-6"]);

    let mut cjk = num_editor("变量-5", 0);
    assert!(cjk.bump_number(1));
    assert_eq!(cjk.lines, vec!["变量-6"]);

    // Non-ASCII punctuation is not an identifier, so the sign stands.
    let mut dash = num_editor("x—-5", 0);
    assert!(dash.bump_number(1));
    assert_eq!(dash.lines, vec!["x—-4"]);
}

#[test]
fn a_minus_after_a_space_is_a_sign() {
    let mut e = num_editor("offset = -5", 10);
    assert!(e.bump_number(1));
    assert_eq!(e.lines, vec!["offset = -4"]);
}

#[test]
fn a_line_with_no_number_is_left_alone_and_reports_it() {
    let mut e = num_editor("let value = name;", 4);
    assert!(!e.bump_number(1));
    assert_eq!(e.lines, vec!["let value = name;"]);
}

/// A number entirely to the LEFT of the caret is not picked up: vim
/// searches forward, and grabbing backwards would edit a literal the
/// user has already moved past.
#[test]
fn a_number_behind_the_caret_is_not_picked_up() {
    let mut e = num_editor("x = 5; y = name", 10);
    assert!(!e.bump_number(1));
    assert_eq!(e.lines, vec!["x = 5; y = name"]);
}

#[test]
fn each_bump_is_its_own_undo_step() {
    let mut e = num_editor("x = 1", 4);
    assert!(e.bump_number(1));
    assert!(e.bump_number(1));
    assert_eq!(e.lines, vec!["x = 3"]);
    e.undo();
    assert_eq!(e.lines, vec!["x = 2"], "one bump undone, not both");
    e.undo();
    assert_eq!(e.lines, vec!["x = 1"]);
}

#[test]
fn a_digit_inside_an_identifier_is_still_a_number() {
    let mut e = num_editor("let sha256 = 1;", 0);
    assert!(e.bump_number(1));
    assert_eq!(e.lines, vec!["let sha257 = 1;"]);
}

/// Byte offsets and char columns are not the same thing once the line
/// holds multibyte text, and the caret is tracked in columns.
#[test]
fn a_multibyte_line_bumps_at_the_right_place() {
    let mut e = num_editor("héllo — count 41", 0);
    assert!(e.bump_number(1));
    assert_eq!(e.lines, vec!["héllo — count 42"]);
}

#[test]
fn emmet_replaces_the_abbreviation_with_markup() {
    let mut e = html_editor("ul>li*2", 7);
    assert!(e.expand_emmet_abbreviation());
    assert_eq!(
        e.lines,
        vec!["<ul>", "    <li></li>", "    <li></li>", "</ul>"]
    );
}

/// #1550: `!` in an empty HTML file becomes the page skeleton, with the
/// caret on the indented line inside `<body>`.
#[test]
fn emmet_turns_an_exclamation_mark_into_a_page() {
    let mut e = html_editor("!", 1);
    assert!(e.expand_emmet_abbreviation());
    assert_eq!(e.lines.first().map(String::as_str), Some("<!DOCTYPE html>"));
    assert_eq!(e.lines.last().map(String::as_str), Some("</html>"));
    assert_eq!(e.lines[e.cursor_row - 1], "<body>");
    assert_eq!(e.lines[e.cursor_row], "    ");
    assert_eq!(e.cursor_col, 4);
}

/// #1550 negative: an exclamation mark ending a sentence is no snippet.
#[test]
fn emmet_leaves_an_exclamation_in_prose_alone() {
    let mut e = html_editor("Hello!", 6);
    assert!(!e.expand_emmet_abbreviation());
    assert_eq!(e.lines, vec!["Hello!"]);
}

/// The caret has to end up somewhere useful, or the user just has to go
/// looking for the hole they meant to type into.
#[test]
fn emmet_leaves_the_caret_in_the_first_empty_element() {
    let mut e = html_editor("div>span", 8);
    assert!(e.expand_emmet_abbreviation());
    assert_eq!(e.cursor_row, 1);
    let line = &e.lines[1];
    let byte = byte_index_of_char(line, e.cursor_col);
    assert_eq!(&line[byte..], "</span>");
}

#[test]
fn emmet_uses_the_buffers_indent_style() {
    let mut e = html_editor("div>p", 5);
    e.set_indent_style(IndentStyle {
        width: 2,
        use_spaces: true,
    });
    assert!(e.expand_emmet_abbreviation());
    assert_eq!(e.lines[1], "  <p></p>");
}

/// An abbreviation typed inside a nested block must stay inside it, so
/// the expansion is prefixed with the line's own indentation.
#[test]
fn emmet_keeps_the_lines_existing_indentation() {
    let mut e = html_editor("        ul>li", 13);
    assert!(e.expand_emmet_abbreviation());
    assert_eq!(
        e.lines,
        vec!["        <ul>", "            <li></li>", "        </ul>"]
    );
}

#[test]
fn emmet_keeps_text_that_followed_the_cursor() {
    let mut e = html_editor("br</div>", 2);
    assert!(e.expand_emmet_abbreviation());
    assert_eq!(e.lines, vec!["<br></div>"]);
}

#[test]
fn emmet_keeps_text_that_preceded_the_abbreviation() {
    let mut e = html_editor("<p>strong", 9);
    assert!(e.expand_emmet_abbreviation());
    assert_eq!(e.lines, vec!["<p><strong></strong>"]);
}

/// One undo step, and it restores the abbreviation rather than peeling
/// the markup off a character at a time.
#[test]
fn emmet_expansion_undoes_in_one_step() {
    let mut e = html_editor("ul>li*3", 7);
    assert!(e.expand_emmet_abbreviation());
    assert_eq!(e.lines.len(), 5);
    e.undo();
    assert_eq!(e.lines, vec!["ul>li*3"]);
}

/// With no empty element the caret goes to the end of the expansion,
/// not back to the start of the line.
#[test]
fn emmet_leaves_the_caret_after_the_expansion_when_nothing_is_empty() {
    let mut e = html_editor("p{Hello}</div>", 8);
    assert!(e.expand_emmet_abbreviation());
    assert_eq!(e.lines, vec!["<p>Hello</p></div>"]);
    assert_eq!((e.cursor_row, e.cursor_col), (0, 12));
}

/// An expansion is an edit like any other: it pins a preview tab so
/// the next preview open does not reuse the tab and discard it.
#[test]
fn emmet_expansion_pins_a_preview_tab() {
    let mut e = html_editor("div", 3);
    e.preview = true;
    assert!(e.expand_emmet_abbreviation());
    assert!(!e.preview);
}

/// `.jsx` shares `LangKind::JavaScript` with plain `.js`, so the file
/// extension decides: JSX expands, ordinary JavaScript does not.
#[test]
fn emmet_runs_in_jsx_files_but_not_plain_javascript() {
    let mut e = html_editor("img", 3);
    e.lang = Some(LangKind::JavaScript);
    e.path = Some(PathBuf::from("App.jsx"));
    assert!(e.expand_emmet_abbreviation());
    assert_eq!(e.lines, vec!["<img />"]);

    let mut e = html_editor("img", 3);
    e.lang = Some(LangKind::JavaScript);
    e.path = Some(PathBuf::from("app.js"));
    assert!(!e.expand_emmet_abbreviation());
    assert_eq!(e.lines, vec!["img"]);
}

#[test]
fn emmet_self_closes_void_elements_in_tsx() {
    let mut e = html_editor("br", 2);
    e.lang = Some(LangKind::Tsx);
    assert!(e.expand_emmet_abbreviation());
    assert_eq!(e.lines, vec!["<br />"]);
}

#[test]
fn emmet_is_inert_outside_markup_buffers() {
    let mut e = html_editor("ul>li*2", 7);
    e.lang = Some(LangKind::Rust);
    assert!(!e.expand_emmet_abbreviation());
    assert_eq!(e.lines, vec!["ul>li*2"]);
}

#[test]
fn emmet_runs_in_xml_and_jsx_too() {
    for lang in [LangKind::Xml, LangKind::Tsx] {
        let mut e = html_editor("div", 3);
        e.lang = Some(lang);
        assert!(e.expand_emmet_abbreviation(), "{lang:?}");
        assert_eq!(e.lines, vec!["<div></div>"], "{lang:?}");
    }
}

/// Prose in an HTML buffer must not turn into tags.
#[test]
fn emmet_is_inert_on_prose() {
    let mut e = html_editor("the quick brown fox", 19);
    assert!(!e.expand_emmet_abbreviation());
    assert_eq!(e.lines, vec!["the quick brown fox"]);
}

#[test]
fn emmet_is_inert_on_an_unparseable_abbreviation() {
    let mut e = html_editor("div>(span", 9);
    assert!(!e.expand_emmet_abbreviation());
    assert_eq!(e.lines, vec!["div>(span"]);
}

#[test]
fn emmet_is_inert_at_the_start_of_a_line() {
    let mut e = html_editor("div", 0);
    assert!(!e.expand_emmet_abbreviation());
    assert_eq!(e.lines, vec!["div"]);
}

#[test]
fn emmet_marks_the_buffer_dirty() {
    let mut e = html_editor("div", 3);
    e.dirty = false;
    assert!(e.expand_emmet_abbreviation());
    assert!(e.dirty, "an expansion is an edit");
}

/// #1557: a wrap or tag removal that changes the line count moves the
/// provenance of the rows below with them, and leaves none past the end.
#[test]
fn emmet_rewrites_keep_provenance_on_the_rows_below() {
    use crate::provenance::Seat;
    let mut e = html_editor("Home\nAbout\ntail", 0);
    e.cursor_row = 0;
    e.provenance.record(2..3, Seat::Peer(String::from("ada")));
    e.selection = Some(EditorSelection {
        anchor: (0, 0),
        head: (1, 5),
    });
    assert!(e.emmet_wrap("ul>li*"));
    assert_eq!(e.lines.len(), 5);
    assert_eq!(e.provenance.seat(4), Some(&Seat::Peer(String::from("ada"))));
    assert_eq!(e.provenance.seat(0), Some(&Seat::Generated));
    e.cursor_row = 0;
    e.cursor_col = 1;
    assert!(e.emmet_remove_tag());
    assert_eq!(e.lines.len(), 3);
    assert_eq!(e.provenance.seat(2), Some(&Seat::Peer(String::from("ada"))));
    assert!(e.provenance.seat(3).is_none() && e.provenance.seat(4).is_none());
}

#[test]
fn emmet_drops_any_active_selection() {
    let mut e = html_editor("div", 3);
    e.selection = Some(EditorSelection {
        anchor: (0, 0),
        head: (0, 3),
    });
    assert!(e.expand_emmet_abbreviation());
    assert!(e.selection.is_none());
}

// ---- Convert Indentation to Spaces / Tabs ----

#[test]
fn indentation_to_spaces_converts_only_leading_tabs() {
    let mut e = editor_with("\tfoo\n\t\tbar\nbaz\ta");
    e.indentation_to_spaces();
    assert_eq!(e.lines, vec!["    foo", "        bar", "baz\ta"]);
}

#[test]
fn indentation_to_tabs_converts_leading_space_groups() {
    let mut e = editor_with("    foo\n      bar\nb  c");
    e.indentation_to_tabs();
    // 4 spaces -> 1 tab; 6 spaces -> 1 tab + 2 leftover; interior spaces stay.
    assert_eq!(e.lines, vec!["\tfoo", "\t  bar", "b  c"]);
}

/// Issue #211: VS Code's editor.detectIndentation. Opening a file
/// whose content indents with 2 spaces must set the buffer's indent
/// style to 2 spaces even though the language default is 4; tabs win
/// when the content uses tabs; a flat file keeps the language default;
/// the manual status-bar override still beats detection.
#[test]
fn open_detects_indentation_from_file_content() {
    let tmp = tempfile::tempdir().unwrap();
    let two = tmp.path().join("two.rs");
    std::fs::write(
        &two,
        "fn main() {\n  let a = 1;\n  if a > 0 {\n    println!();\n  }\n}\n",
    )
    .unwrap();
    let mut e = Editor::new();
    e.open(&two).unwrap();
    assert_eq!(
        e.indent_style(),
        IndentStyle {
            width: 2,
            use_spaces: true
        },
        "2-space content must be detected over the 4-space rust default"
    );

    let tabs = tmp.path().join("tabs.rs");
    std::fs::write(
        &tabs,
        "fn main() {\n\tlet a = 1;\n\tif a > 0 {\n\t\tprintln!();\n\t}\n}\n",
    )
    .unwrap();
    let mut t = Editor::new();
    t.open(&tabs).unwrap();
    assert!(
        !t.indent_style().use_spaces,
        "tab-indented content must detect tabs"
    );

    let flat = tmp.path().join("flat.rs");
    std::fs::write(&flat, "fn main() {}\nfn other() {}\n").unwrap();
    let mut f = Editor::new();
    f.open(&flat).unwrap();
    assert_eq!(
        f.indent_style(),
        IndentStyle {
            width: 4,
            use_spaces: true
        },
        "a file with no indented lines keeps the language default"
    );

    // The manual override still wins over detection.
    let mut o = Editor::new();
    o.open(&two).unwrap();
    o.set_indent_style(IndentStyle {
        width: 8,
        use_spaces: true,
    });
    assert_eq!(o.indent_style().width, 8, "override beats detection");
}

/// `.editorconfig` outranks content detection: a project that STATES its
/// style beats a guess at it, including on a file whose current content
/// disagrees — which is the case the property exists to correct.
#[test]
fn editorconfig_indent_beats_detection_but_not_the_manual_override() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join(".editorconfig"),
        "root = true\n\n[*]\nindent_style = space\nindent_size = 2\n",
    )
    .unwrap();
    // Content is unambiguously 4-space, so detection and the config
    // genuinely disagree and the winner is observable.
    let f = tmp.path().join("a.rs");
    std::fs::write(&f, "fn main() {\n    let a = 1;\n    let b = 2;\n}\n").unwrap();

    let mut e = Editor::new();
    e.open(&f).unwrap();
    assert_eq!(
        e.indent_style(),
        IndentStyle {
            width: 2,
            use_spaces: true
        },
        ".editorconfig must beat the 4-space content detection"
    );
    assert_eq!(e.indent_style().unit(), "  ");

    e.set_indent_style(IndentStyle {
        width: 8,
        use_spaces: true,
    });
    assert_eq!(
        e.indent_style().width,
        8,
        "the manual status-bar override still beats .editorconfig"
    );
}

/// A section naming only one of the two indent properties leaves the
/// other one to detection, rather than resetting it to a default.
#[test]
fn editorconfig_resolves_indent_style_and_width_independently() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join(".editorconfig"),
        "root = true\n\n[*]\nindent_style = tab\n",
    )
    .unwrap();
    let f = tmp.path().join("a.rs");
    std::fs::write(&f, "fn main() {\n  let a = 1;\n  let b = 2;\n}\n").unwrap();

    let mut e = Editor::new();
    e.open(&f).unwrap();
    let s = e.indent_style();
    assert!(!s.use_spaces, "indent_style = tab must apply");
    assert_eq!(s.width, 2, "the unnamed width stays with detection");
}

/// A per-glob section beats the `[*]` catch-all, and files the section
/// does not match are unaffected.
#[test]
fn editorconfig_per_language_section_applies_to_matching_files_only() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join(".editorconfig"),
        "root = true\n\n[*]\nindent_style = space\nindent_size = 2\n\n[*.py]\nindent_size = 8\n",
    )
    .unwrap();
    let rs = tmp.path().join("a.rs");
    std::fs::write(&rs, "fn main() {}\n").unwrap();
    let py = tmp.path().join("a.py");
    std::fs::write(&py, "def f():\n    pass\n").unwrap();

    let mut r = Editor::new();
    r.open(&rs).unwrap();
    assert_eq!(r.indent_style().label(), "Spaces: 2");

    let mut p = Editor::new();
    p.open(&py).unwrap();
    assert_eq!(p.indent_style().label(), "Spaces: 8");
}

/// `end_of_line` overrides the CRLF sniff, so a file that currently
/// disagrees with the project is rewritten to match it on save.
#[test]
fn editorconfig_end_of_line_overrides_the_sniffed_eol() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join(".editorconfig"),
        "root = true\n\n[*]\nend_of_line = crlf\n",
    )
    .unwrap();
    let f = tmp.path().join("a.txt");
    std::fs::write(&f, "one\ntwo\n").unwrap();

    let mut e = Editor::new();
    e.open(&f).unwrap();
    assert_eq!(e.eol, LineEnding::Crlf, "the config must beat the sniff");
    e.insert_char('!');
    e.save_to_disk().unwrap();
    let on_disk = std::fs::read_to_string(&f).unwrap();
    assert!(on_disk.contains("\r\n"), "saved as CRLF: {on_disk:?}");
}

/// The two save-time properties both fire from the single write choke
/// point, and the whole normalisation is undoable.
#[test]
fn editorconfig_trims_whitespace_and_adds_a_final_newline_on_save() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join(".editorconfig"),
        "root = true\n\n[*]\ntrim_trailing_whitespace = true\ninsert_final_newline = true\n",
    )
    .unwrap();
    let f = tmp.path().join("a.txt");
    std::fs::write(&f, "one   \ntwo\t\nthree").unwrap();

    let mut e = Editor::new();
    e.open(&f).unwrap();
    // The caret's own line is spared (see the test below), so it sits on
    // the one line with nothing to trim.
    e.cursor_row = 2;
    e.save_to_disk().unwrap();
    assert_eq!(
        std::fs::read_to_string(&f).unwrap(),
        "one\ntwo\nthree\n",
        "trailing whitespace trimmed and a final newline added"
    );
}

/// Auto-save fires a second after typing stops, so trimming the line
/// under a caret would turn `foo ` into `foo` mid-word and the next key
/// would produce `foobar`. Caret lines keep their trailing whitespace
/// (primary and extra carets alike); every other line is trimmed, and a
/// later save with the caret elsewhere normalises the rest.
#[test]
fn editorconfig_trim_on_save_spares_the_lines_under_a_caret() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join(".editorconfig"),
        "root = true\n\n[*]\ntrim_trailing_whitespace = true\n",
    )
    .unwrap();
    let f = tmp.path().join("a.txt");
    std::fs::write(&f, "foo \nbar  \nbaz\t").unwrap();
    let mut e = Editor::new();
    e.open(&f).unwrap();
    e.cursor_row = 0;
    e.cursor_col = 4;
    e.carets.push(EditorSelection::new(2, 4));
    e.auto_save_to_disk().unwrap();
    assert_eq!(
        std::fs::read_to_string(&f).unwrap(),
        "foo \nbar\nbaz\t",
        "only the line without a caret is trimmed"
    );
    assert_eq!((e.cursor_row, e.cursor_col), (0, 4), "the caret stays put");
}

/// #1455: an explicit save (Cmd+S, Save All) trims the caret lines too,
/// as VS Code does: sparing them is for auto-save only, and the line
/// just edited is the one with the stray spaces. The carets stay,
/// clamped to their trimmed lines.
#[test]
fn editorconfig_trim_on_an_explicit_save_includes_the_caret_lines() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join(".editorconfig"),
        "root = true\n\n[*]\ntrim_trailing_whitespace = true\n",
    )
    .unwrap();
    let f = tmp.path().join("a.txt");
    std::fs::write(&f, "foo \nbar  \nbaz\t").unwrap();
    let mut e = Editor::new();
    e.open(&f).unwrap();
    e.cursor_row = 0;
    e.cursor_col = 4;
    e.carets.push(EditorSelection::new(2, 4));
    e.save_to_disk().unwrap();
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "foo\nbar\nbaz");
    assert_eq!(
        (e.cursor_row, e.cursor_col),
        (0, 3),
        "clamped to the line end"
    );
    assert_eq!(e.carets.len(), 1, "the extra caret is kept");
    assert_eq!(e.carets[0].head, (2, 3));
    assert!(e.undo(), "the trim is one undo step");
    assert_eq!(e.lines[0], "foo ");
}

/// #1455 negative: an auto-save after an explicit one still spares the
/// caret line: the explicit save leaves no mode behind.
#[test]
fn an_auto_save_after_an_explicit_save_still_spares_the_caret_line() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join(".editorconfig"),
        "root = true\n\n[*]\ntrim_trailing_whitespace = true\n",
    )
    .unwrap();
    let f = tmp.path().join("a.txt");
    std::fs::write(&f, "foo\n").unwrap();
    let mut e = Editor::new();
    e.open(&f).unwrap();
    e.save_to_disk().unwrap();
    e.cursor_col = 3;
    e.insert_char(' ');
    e.auto_save_to_disk().unwrap();
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "foo \n");
}

/// Neither save-time property fires unless a `.editorconfig` asked for
/// it: rewriting whitespace on an unconfigured project is a surprise.
#[test]
fn editorconfig_save_normalisation_is_off_by_default() {
    let tmp = tempfile::tempdir().unwrap();
    // `root = true` with no properties, so no ancestor config can leak
    // in and make this pass for the wrong reason.
    std::fs::write(tmp.path().join(".editorconfig"), "root = true\n").unwrap();
    let f = tmp.path().join("a.txt");
    std::fs::write(&f, "one   \ntwo").unwrap();

    let mut e = Editor::new();
    e.open(&f).unwrap();
    e.insert_char('!');
    e.save_to_disk().unwrap();
    assert_eq!(
        std::fs::read_to_string(&f).unwrap(),
        "!one   \ntwo",
        "untouched whitespace and no newline appended"
    );
}

/// An empty buffer has no last line to terminate, so
/// `insert_final_newline` must not turn a zero-byte file into a
/// one-byte one on every save.
#[test]
fn editorconfig_final_newline_leaves_an_empty_buffer_empty() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join(".editorconfig"),
        "root = true\n\n[*]\ninsert_final_newline = true\n",
    )
    .unwrap();
    let f = tmp.path().join("a.txt");
    std::fs::write(&f, "").unwrap();

    let mut e = Editor::new();
    e.open(&f).unwrap();
    e.save_to_disk().unwrap();
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "");
}

/// The save-time final newline is its own undo step: it must not
/// coalesce into the typing burst before the save, or one Cmd+Z would
/// take the user's keystrokes along with the newline.
#[test]
fn editorconfig_final_newline_is_its_own_undo_step() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join(".editorconfig"),
        "root = true\n\n[*]\ninsert_final_newline = true\n",
    )
    .unwrap();
    let f = tmp.path().join("a.txt");
    std::fs::write(&f, "one").unwrap();

    let mut e = Editor::new();
    e.open(&f).unwrap();
    e.insert_char('!');
    e.save_to_disk().unwrap();
    assert_eq!(e.lines, vec!["!one", ""]);
    assert!(e.undo());
    assert_eq!(e.lines, vec!["!one"], "undo removes only the newline");
    assert!(e.undo());
    assert_eq!(e.lines, vec!["one"], "the next undo removes the typing");
}

/// A file already ending in a newline must not collect another one on
/// each save.
#[test]
fn editorconfig_final_newline_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join(".editorconfig"),
        "root = true\n\n[*]\ninsert_final_newline = true\n",
    )
    .unwrap();
    let f = tmp.path().join("a.txt");
    std::fs::write(&f, "one\n").unwrap();

    let mut e = Editor::new();
    e.open(&f).unwrap();
    e.save_to_disk().unwrap();
    e.save_to_disk().unwrap();
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "one\n");
}

/// The pure detector: majority rules between tabs and spaces, and the
/// space width comes from the most common indentation step.
#[test]
fn detect_indentation_picks_majority_and_step() {
    let lines = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(
        detect_indentation(&lines(&["a {", "    b;", "        c;", "    d;", "}"])),
        Some(IndentStyle {
            width: 4,
            use_spaces: true
        })
    );
    assert_eq!(
        detect_indentation(&lines(&["a {", "\tb;", "\t\tc;", "}"])),
        Some(IndentStyle {
            width: 4,
            use_spaces: false
        })
    );
    assert_eq!(detect_indentation(&lines(&["a", "b", "c"])), None);
    // Mixed content: the majority style wins (three tab lines, one
    // spaced line).
    assert_eq!(
        detect_indentation(&lines(&["a {", "\tb;", "\tc;", "\td;", "  e;", "}"])),
        Some(IndentStyle {
            width: 4,
            use_spaces: false
        })
    );
    // Whitespace-only lines carry no intent on EITHER side: tab-only
    // blank lines must not outvote real space indentation (review
    // round 1).
    assert_eq!(
        detect_indentation(&lines(&["a {", "  b;", "  c;", "\t", "\t\t", "\t \t", "}"])),
        Some(IndentStyle {
            width: 2,
            use_spaces: true
        })
    );
}

#[test]
fn indent_override_switches_tab_to_a_literal_tab() {
    let mut e = editor_with("foo");
    // Default: spaces, width 4 -> Tab pads to the next stop with spaces.
    e.cursor_row = 0;
    e.cursor_col = 0;
    e.indent_at_cursor();
    assert_eq!(e.lines[0], "    foo");
    assert_eq!(e.indent_preference(), (4, true));

    // Pin tabs: Tab now inserts one literal tab, LSP prefs flip insert_spaces.
    let mut t = editor_with("foo");
    t.set_indent_style(IndentStyle {
        width: 4,
        use_spaces: false,
    });
    t.cursor_row = 0;
    t.cursor_col = 0;
    t.indent_at_cursor();
    assert_eq!(t.lines[0], "\tfoo");
    assert_eq!(t.indent_preference(), (4, false));
}

#[test]
fn indent_override_width_changes_status_label_and_unit() {
    let mut e = editor_with("");
    e.set_indent_style(IndentStyle {
        width: 2,
        use_spaces: true,
    });
    assert_eq!(e.indent_style().label(), "Spaces: 2");
    assert_eq!(e.indent_style().unit(), "  ");
    e.set_indent_style(IndentStyle {
        width: 4,
        use_spaces: false,
    });
    assert_eq!(e.indent_style().label(), "Tab Size: 4");
    assert_eq!(e.indent_style().unit(), "\t");
}

// ---- Trim Final Newlines (files.trimFinalNewlines) ----

#[test]
fn trim_final_newlines_removes_trailing_blank_lines() {
    let mut e = editor_with("a\nb\n\n\n");
    // editor_with drops the empty trailing element from `lines()`, so build
    // the buffer explicitly with trailing blanks.
    e.lines = vec![
        String::from("a"),
        String::from("b"),
        String::new(),
        String::new(),
    ];
    assert!(e.trim_final_newlines());
    assert_eq!(e.lines, vec!["a", "b"]);
}

#[test]
fn trim_final_newlines_keeps_at_least_one_line() {
    let mut e = editor_with("");
    e.lines = vec![String::new(), String::new(), String::new()];
    assert!(e.trim_final_newlines());
    assert_eq!(e.lines, vec![""]);
}

#[test]
fn trim_final_newlines_noop_when_no_trailing_blanks() {
    let mut e = editor_with("a\nb");
    assert!(!e.trim_final_newlines());
    assert_eq!(e.lines, vec!["a", "b"]);
}

#[test]
fn fold_range_covers_an_indented_block() {
    let e = editor_with("fn a() {\n    let x = 1;\n    let y = 2;\n}\nfn b() {}");
    // Header line 0; body lines 1-2 are more indented; the closing brace on
    // line 3 returns to base indent and is not part of the region.
    assert_eq!(e.fold_range(0), Some((0, 2)));
    // A leaf line with no deeper line below it is not foldable.
    assert_eq!(e.fold_range(1), None);
    assert_eq!(e.fold_range(4), None);
}

#[test]
fn fold_range_absorbs_interior_blank_lines_but_not_trailing_ones() {
    let e = editor_with("fn a() {\n    x\n\n    y\n}\n");
    // The interior blank (line 2) sits between two deeper lines, so it is
    // inside the region; the region ends at the last deeper line (3).
    assert_eq!(e.fold_range(0), Some((0, 3)));
}

#[test]
fn region_markers_fold_in_the_fallback_scanner_including_nesting() {
    use crate::lsp::manager::FoldRangeKind;
    let mut e =
        editor_with("top\n#region outer\na\n// region inner\nb\n// endregion\nc\n#endregion\ntail");
    e.refresh_fold_tables();
    assert_eq!(e.fold_range(1), Some((1, 7)), "outer #region pair");
    assert_eq!(e.fold_range(3), Some((3, 5)), "nested // region pair");
    assert_eq!(e.fold_kind_at(1), Some(FoldRangeKind::Region));
    assert_eq!(e.fold_kind_at(0), None, "plain text has no kind");
    // A line that merely CONTAINS the word is not a marker.
    let mut e2 = editor_with("let region = 1;\nregional stuff\n#endregion");
    e2.refresh_fold_tables();
    assert_eq!(e2.fold_range(0), None);
    assert_eq!(e2.fold_range(1), None);
}

#[test]
fn comment_runs_fold_but_rust_attributes_do_not_count_as_comments() {
    use crate::lsp::manager::FoldRangeKind;
    let mut e = editor_with(
        "// docs line one\n// docs line two\n// docs line three\nfn a() {}\n#[derive(Debug)]\n#[cfg(test)]\nstruct S;",
    );
    e.refresh_fold_tables();
    assert_eq!(e.fold_range(0), Some((0, 2)), "a 3-line comment run folds");
    assert_eq!(e.fold_kind_at(0), Some(FoldRangeKind::Comment));
    assert_eq!(
        e.fold_kind_at(4),
        None,
        "attribute lines are code, not a comment run"
    );
    // A single comment line is not a run.
    let mut e2 = editor_with("// alone\nfn a() {}");
    e2.refresh_fold_tables();
    assert_eq!(e2.fold_kind_at(0), None);
}

#[test]
fn server_fold_spans_replace_the_heuristics_until_the_buffer_moves() {
    use crate::lsp::manager::{FoldRangeKind, FoldingRangeItem};
    let mut e = editor_with("fn a() {\n    x\n    y\n}\nfn b() {}");
    // Server says the whole item folds through its closing brace —
    // something indentation cannot express.
    e.set_lsp_folds(vec![FoldingRangeItem {
        start_line: 0,
        end_line: 3,
        kind: FoldRangeKind::Other,
    }]);
    assert_eq!(e.fold_range(0), Some((0, 3)), "server span wins");
    assert_eq!(
        e.fold_range(4),
        None,
        "server authoritative: no indentation fallthrough"
    );
    // An edit that changes the line count orphans the spans; the
    // indentation scan is back in charge until the next reply.
    e.lines.push(String::from("tail"));
    assert_eq!(e.fold_range(0), Some((0, 2)), "stale spans ignored");
}

#[test]
fn fold_all_regions_and_unfold_all_regions_round_trip() {
    use crate::lsp::manager::FoldRangeKind;
    let mut e = editor_with("#region a\nx\n#endregion\n// one\n// two\nfn f() {\n    body\n}");
    e.fold_all_of_kind(FoldRangeKind::Region);
    assert!(e.is_line_hidden(1), "the region body folded");
    assert!(!e.is_line_hidden(4), "the comment run did not");
    assert!(!e.is_line_hidden(6), "the indented block did not");
    e.fold_all_of_kind(FoldRangeKind::Comment);
    assert!(e.is_line_hidden(4), "now the comment run folded too");
    e.unfold_all_of_kind(FoldRangeKind::Region);
    assert!(!e.is_line_hidden(1), "regions expanded");
    assert!(e.is_line_hidden(4), "comment folds untouched");
}

#[test]
fn folding_a_header_hides_its_body_only() {
    let mut e = editor_with("fn a() {\n    let x = 1;\n    let y = 2;\n}\nfn b() {}");
    e.toggle_fold(0);
    assert!(!e.is_line_hidden(0), "the header line stays visible");
    assert!(e.is_line_hidden(1));
    assert!(e.is_line_hidden(2));
    assert!(
        !e.is_line_hidden(3),
        "the closing brace at base indent stays"
    );
    assert!(!e.is_line_hidden(4));
}

#[test]
fn moving_down_steps_over_a_folded_block_instead_of_into_it() {
    // `is_line_hidden` was consulted only by the render loops and by the
    // fold toggles' own cursor snap. No cursor-movement path knew about
    // folds, so Down walked straight into a collapsed region: the caret
    // vanished (`cursor_screen_pos` cannot map a hidden line) and anything
    // typed went into content the user could not see.
    let mut e = editor_with("fn a() {\n    x\n    y\n}\nafter");
    e.toggle_fold(0);
    assert_eq!(e.cursor_row, 0, "the fold snapped the caret to the header");
    e.move_down();
    assert_eq!(
        e.cursor_row, 3,
        "Down clears the whole collapsed body, landing on the next visible line"
    );
    assert!(!e.is_line_hidden(e.cursor_row));
    e.move_up();
    assert_eq!(e.cursor_row, 0, "Up steps back over it the same way");
}

#[test]
fn editing_reveals_a_fold_the_caret_ended_up_inside() {
    // Not every way into a fold is an arrow key — search, go-to-definition
    // and goto-line all set `cursor_row` directly. Wherever the caret came
    // from, an edit must never land on a line the user cannot see.
    let mut e = editor_with("fn a() {\n    x\n    y\n}\nafter");
    e.toggle_fold(0);
    e.cursor_row = 1;
    // Past the indent: typing at column 0 would strip line 1's leading
    // whitespace and collapse the fold RANGE, which unhides the line for a
    // reason that has nothing to do with the caret.
    e.cursor_col = 5;
    assert!(e.is_line_hidden(1), "the caret is parked inside the fold");
    e.insert_char('Z');
    assert!(
        !e.is_line_hidden(1),
        "the edit revealed the region it landed in"
    );
    assert!(e.lines[1].contains('Z'), "and the text went to line 1");
}

/// Waiting for an EDIT to reveal the fold is too late for a jump: a caret
/// on a hidden line has no painted row at all, so `cursor_screen_pos`
/// returns `None` and no caret is drawn anywhere on screen. Ctrl+G stands
/// in for every direct `cursor_row` setter (go-to-definition, search).
#[test]
fn jumping_into_a_collapsed_block_opens_it_so_the_caret_is_painted() {
    let mut e = editor_with("fn a() {\n    x\n    y\n}\nafter");
    e.focused = true;
    e.toggle_fold(0);
    assert!(e.is_line_hidden(1), "the block starts collapsed");
    e.goto_line(2); // one-based: line index 1, inside the fold
    assert_eq!(e.cursor_row, 1);
    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 10,
    };
    let mut buf = Buffer::empty(area);
    (&mut e).render(area, &mut buf);
    assert!(
        !e.is_line_hidden(1),
        "the jump target's own block must open"
    );
    assert!(
        e.cursor_screen_pos().is_some(),
        "so the caret has somewhere to be painted"
    );
}

#[test]
fn opening_another_file_clears_fold_state() {
    // `open` resets scroll, cursor, selection, undo, tokens and hints, but
    // left `folded` alone. A preview tab is REUSED for the next
    // single-click, so the new file arrived with the old file's fold
    // headers collapsed — and the line-count guard only fires when the two
    // files differ in length.
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.rs");
    let b = dir.path().join("b.rs");
    std::fs::write(&a, "fn a() {\n    x\n}\n").unwrap();
    std::fs::write(&b, "fn b() {\n    y\n}\n").unwrap();
    let mut e = Editor::new();
    e.open(&a).unwrap();
    e.toggle_fold(0);
    assert!(e.is_line_hidden(1), "a.rs is folded");
    e.open(&b).unwrap();
    assert!(
        !e.is_line_hidden(1),
        "the next file must not inherit the previous file's folds"
    );
}

#[test]
fn the_fold_lookup_is_derived_once_per_fold_change_not_per_query() {
    // `is_line_hidden` ran on every rendered row of every frame and
    // re-derived itself from `folded`, scanning each folded region. Fold
    // All on a large file turned a single frame into millions of
    // iterations. The spans are now built once per fold write.
    let text = (0..400)
        .map(|i| format!("fn f{i}() {{\n    body\n}}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut e = editor_with(&text);
    e.fold_all();
    assert!(
        !e.folded.is_empty(),
        "Fold All must actually fold something"
    );

    FOLD_RANGE_REBUILDS.with(|c| c.set(0));
    for line in 0..e.lines.len() {
        std::hint::black_box(e.is_line_hidden(line));
    }
    assert_eq!(
        FOLD_RANGE_REBUILDS.with(|c| c.get()),
        0,
        "querying must never rebuild the spans"
    );

    e.toggle_fold(0);
    assert_eq!(
        FOLD_RANGE_REBUILDS.with(|c| c.get()),
        1,
        "changing the fold set rebuilds exactly once"
    );
}

#[test]
fn toggling_a_fold_twice_restores_visibility() {
    let mut e = editor_with("fn a() {\n    body\n}");
    e.toggle_fold(0);
    assert!(e.is_line_hidden(1));
    e.toggle_fold(0);
    assert!(!e.is_line_hidden(1), "a second toggle unfolds the block");
}

#[test]
fn folding_from_inside_the_body_folds_the_enclosing_block() {
    let mut e = editor_with("fn a() {\n    body\n}");
    e.cursor_row = 1;
    e.toggle_fold(e.cursor_row);
    assert!(
        e.is_line_hidden(1),
        "folding at a body line folds its owner"
    );
    assert_eq!(
        e.cursor_row, 0,
        "cursor on a hidden line snaps up to the header"
    );
}

#[test]
fn render_omits_folded_body_lines() {
    let mut e = editor_with("fn a() {\n    x\n    y\n}\nfn b() {}");
    e.toggle_fold(0);
    render_at(&mut e, 40, 10);
    let shown: Vec<usize> = (0..e.last_wrap_rows.len())
        .filter_map(|i| e.text_row(i).map(|t| t.0))
        .collect();
    assert!(shown.contains(&0), "the header row is drawn");
    assert!(!shown.contains(&1), "a folded body line is not drawn");
    assert!(!shown.contains(&2), "a folded body line is not drawn");
    assert!(
        shown.contains(&3) && shown.contains(&4),
        "lines after the fold are still drawn"
    );
}

#[test]
fn fold_all_collapses_every_block_and_unfold_all_restores() {
    let mut e = editor_with("fn a() {\n    x\n}\nfn b() {\n    y\n}");
    e.fold_all();
    assert!(
        e.is_line_hidden(1) && e.is_line_hidden(4),
        "every body is hidden"
    );
    e.unfold_all();
    assert!(
        !e.is_line_hidden(1) && !e.is_line_hidden(4),
        "unfold restores all"
    );
}

#[test]
fn clicking_the_gutter_chevron_toggles_the_fold() {
    let mut e = editor_with("fn a() {\n    x\n}\nfn b() {}");
    render_at(&mut e, 40, 10);
    // The chevron is the second sign-margin cell; the header sits on the
    // first visual row at the top of the inner rect.
    let cx = e.last_inner.x + 1;
    let cy = e.last_inner.y;
    assert!(
        e.fold_chevron_at(cx, cy),
        "a click on the header chevron hits"
    );
    assert!(e.is_line_hidden(1), "the body is now folded");
    assert!(e.fold_chevron_at(cx, cy), "a second click unfolds");
    assert!(!e.is_line_hidden(1));
}

#[test]
fn the_breakpoint_lane_is_the_sign_cell_of_each_source_line() {
    // #1544: the lane is the sign cell left of the chevron, on every
    // visual row of a line.
    let mut e = editor_with("fn a() {\n    x\n}\nfn b() {}");
    render_at(&mut e, 40, 10);
    let x = e.last_inner.x;
    let y = e.last_inner.y;
    assert_eq!(e.breakpoint_lane_line_at(x, y), Some(0));
    assert_eq!(e.breakpoint_lane_line_at(x, y + 3), Some(3));
}

#[test]
fn the_chevron_line_number_body_and_rows_past_the_end_are_not_the_lane() {
    // #1544 negative: only the sign cell is the lane.
    let mut e = editor_with("fn a() {\n    x\n}\nfn b() {}");
    render_at(&mut e, 40, 10);
    let x = e.last_inner.x;
    let y = e.last_inner.y;
    let text_x = x + e.last_gutter_width + 1;
    assert_eq!(e.breakpoint_lane_line_at(x + 1, y), None, "the chevron");
    assert_eq!(e.breakpoint_lane_line_at(x + 2, y), None, "the number");
    assert_eq!(e.breakpoint_lane_line_at(text_x, y), None, "the body");
    assert_eq!(e.breakpoint_lane_line_at(x, y + 6), None, "past the end");
    e.lang = Some(LangKind::Markdown);
    assert!(e.toggle_markdown_preview());
    assert_eq!(
        e.breakpoint_lane_line_at(x, y),
        None,
        "a preview has no source lines"
    );
}

#[test]
fn clicking_off_the_chevron_column_is_not_a_fold_toggle() {
    let mut e = editor_with("fn a() {\n    x\n}");
    render_at(&mut e, 40, 10);
    let text_x = e.last_inner.x + e.last_gutter_width + 1;
    assert!(
        !e.fold_chevron_at(text_x, e.last_inner.y),
        "the text area is not the chevron"
    );
    assert!(!e.is_line_hidden(1));
}

#[test]
fn ghost_carets_paint_participant_colored_cells() {
    let f = NamedTempFile::new().unwrap();
    std::fs::write(f.path(), "hello world\nsecond line\n").unwrap();
    let mut ed = Editor::new();
    ed.open(f.path()).unwrap();
    let color = Color::Rgb(1, 2, 3);
    // One caret at (0, 6); one on a row far past the buffer, which must
    // simply never paint.
    ed.ghost_carets = vec![(0, 6, color), (99, 0, color)];
    let area = Rect {
        x: 0,
        y: 0,
        width: 30,
        height: 10,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut ed, area, &mut buf);
    let hits: Vec<(u16, u16)> = (area.y..area.bottom())
        .flat_map(|y| (area.x..area.right()).map(move |x| (x, y)))
        .filter(|&(x, y)| buf[(x, y)].style().bg == Some(color))
        .collect();
    assert_eq!(hits.len(), 1, "exactly one ghost caret cell: {hits:?}");
    let (x, y) = hits[0];
    assert_eq!(y, ed.last_inner.y, "ghost sits on the first content row");
    let text_x = ed.last_inner.x + ed.last_gutter_width + 1;
    assert_eq!(x, text_x + 6, "ghost sits at char column 6");
}

/// A ghost caret's name tag paints on the visual row above the caret at
/// the caret's column; on the viewport's top row it falls back below.
#[test]
fn ghost_caret_labels_paint_name_above_and_fall_back_below() {
    let f = NamedTempFile::new().unwrap();
    std::fs::write(f.path(), "hello world\nsecond line\nthird line\n").unwrap();
    let mut ed = Editor::new();
    ed.open(f.path()).unwrap();
    let color = Color::Rgb(9, 8, 7);
    let area = Rect {
        x: 0,
        y: 0,
        width: 30,
        height: 10,
    };
    let text_x = |ed: &Editor| ed.last_inner.x + ed.last_gutter_width + 1;

    // Caret on line 1: the tag paints on the row above (line 0's row).
    ed.ghost_caret_labels = vec![(1, 2, "alice".into(), color)];
    let mut buf = ratatui::buffer::Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut ed, area, &mut buf);
    let x0 = text_x(&ed) + 2;
    let y0 = ed.last_inner.y;
    let shown: String = (0..5)
        .map(|i| buf[(x0 + i, y0)].symbol().to_string())
        .collect();
    assert_eq!(shown, "alice", "tag paints above the caret at its column");
    assert_eq!(
        buf[(x0, y0)].style().bg,
        Some(color),
        "tag wears the participant color"
    );

    // Caret on the top row: the tag falls back to the row below.
    ed.ghost_caret_labels = vec![(0, 0, "bob".into(), color)];
    let mut buf = ratatui::buffer::Buffer::empty(area);
    ratatui::widgets::Widget::render(&mut ed, area, &mut buf);
    let x1 = text_x(&ed);
    let y1 = ed.last_inner.y + 1;
    let shown: String = (0..3)
        .map(|i| buf[(x1 + i, y1)].symbol().to_string())
        .collect();
    assert_eq!(shown, "bob", "tag falls back below on the top row");
}
// ---- Auto-closing pairs (#121) ----

#[test]
fn typing_an_opener_auto_closes_with_the_caret_between() {
    let mut e = editor_with("fn main() ");
    e.cursor_row = 0;
    e.cursor_col = 10; // end of line
    e.insert_char('{');
    assert_eq!(e.lines[0], "fn main() {}");
    assert_eq!(e.cursor_col, 11, "the caret sits between the pair");
    // One undo removes BOTH characters.
    e.undo();
    assert_eq!(e.lines[0], "fn main() ");
}

#[test]
fn openers_do_not_auto_close_before_a_word_character() {
    let mut e = editor_with("foo");
    e.cursor_row = 0;
    e.cursor_col = 0;
    e.insert_char('(');
    assert_eq!(
        e.lines[0], "(foo",
        "no closer may be jammed into the following word"
    );
}

fn typed(text: &str) -> String {
    let mut e = editor_with("");
    for c in text.chars() {
        e.insert_char(c);
    }
    e.lines[0].clone()
}

#[test]
fn a_bracket_typed_alone_inside_a_string_does_not_auto_close() {
    // #1183: each row typed char by char comes out as VS Code has it.
    for text in [
        "print(\"(\")",
        "x = \"[a\"",
        "parts = line.split(\"[\")",
        "smiley = ':-('",
    ] {
        assert_eq!(typed(text), text);
    }
}

#[test]
fn openers_still_auto_close_before_spaces_closers_and_punctuation() {
    for next in [")", "]", "}", ";", ",", ".", ":", "=", ">", " ", "\t"] {
        let mut e = editor_with(next);
        e.cursor_row = 0;
        e.cursor_col = 0;
        e.insert_char('(');
        assert_eq!(e.lines[0], format!("(){next}"), "before {next:?}");
    }
}

#[test]
fn openers_do_not_auto_close_before_a_quote_or_other_symbols() {
    for next in ["\"", "'", "`", "-", "+", "$", "#"] {
        let mut e = editor_with(next);
        e.cursor_row = 0;
        e.cursor_col = 0;
        e.insert_char('[');
        assert_eq!(e.lines[0], format!("[{next}"), "before {next:?}");
    }
}

#[test]
fn typing_the_closer_steps_over_instead_of_inserting() {
    let mut e = editor_with("");
    e.insert_char('(');
    assert_eq!(e.lines[0], "()");
    e.insert_char(')');
    assert_eq!(e.lines[0], "()", "the closer types over, never doubles");
    assert_eq!(e.cursor_col, 2);
}

#[test]
fn an_apostrophe_inside_a_word_stays_single() {
    let mut e = editor_with("don");
    e.cursor_row = 0;
    e.cursor_col = 3;
    e.insert_char('\'');
    assert_eq!(e.lines[0], "don'", "no auto-pair after a word character");
}

/// An editor in `lang` with `text` typed into it one key at a time.
fn typed_in(lang: Option<LangKind>, text: &str) -> Editor {
    let mut e = editor_with("");
    e.lang = lang;
    for c in text.chars() {
        e.insert_char(c);
    }
    e
}

#[test]
fn python_strings_typed_key_by_key_come_out_exactly_as_typed() {
    // The f-string's closing `"` used to pair (it follows `}`), leaving a
    // stray `")` behind; the prefix now opens the pair instead (#844).
    for text in [
        "print(f\"{category:<12} {total:>10}\")",
        "print(f\"{x}\")",
        "r'\\d'",
        "x = Rb'\\x00' + fr\"{y}\"",
    ] {
        let e = typed_in(Some(LangKind::Python), text);
        assert_eq!(e.lines, [text], "typing {text:?}");
    }
    let mut e = typed_in(Some(LangKind::Python), "print(f");
    e.insert_char('"');
    assert_eq!(e.lines[0], "print(f\"\")", "a string prefix opens the pair");
    let mut e = typed_in(Some(LangKind::Python), "elif");
    e.insert_char('"');
    assert_eq!(e.lines[0], "elif\"", "a word that is not a prefix does not");
}

/// `text` in `lang` with the caret at its `|` (which is not kept), and
/// the text either side of the caret.
fn with_caret(lang: Option<LangKind>, text: &str) -> (Editor, &str, &str) {
    let (head, tail) = text.split_once('|').expect("a caret marker");
    let mut e = editor_with(&format!("{head}{tail}"));
    e.lang = lang;
    e.cursor_col = head.chars().count();
    (e, head, tail)
}

#[test]
fn a_quote_typed_inside_a_string_or_comment_never_pairs() {
    for (lang, text) in [
        // The #844 line once `f"` had not paired but the `(` had.
        (Some(LangKind::Python), "print(f\"{x}|)"),
        (Some(LangKind::Python), "s = 'it is |"),
        (Some(LangKind::Python), "doc = \"\"\"say \"hi\" |"),
        (Some(LangKind::Python), "x = 1  # see |"),
        (Some(LangKind::Rust), "let s = \"a \\\" b |"),
        (Some(LangKind::Rust), "f(x); // see |"),
        (Some(LangKind::JavaScript), "/* see |"),
        (Some(LangKind::Bash), "echo 'a\\' \"b |"),
        (None, "He said \"hi, |"),
    ] {
        let (mut e, head, tail) = with_caret(lang, text);
        e.insert_char('"');
        assert_eq!(e.lines[0], format!("{head}\"{tail}"), "{lang:?}: {text:?}");
    }
}

#[test]
fn a_quote_outside_strings_and_comments_still_pairs() {
    for (lang, text) in [
        (None, "x = |"),
        (Some(LangKind::Python), "d = {'a': 1, |}"),
        (Some(LangKind::Python), "doc = \"\"\"a\"\"\" + |"),
        (Some(LangKind::Rust), "fn f<'a>(s: &'a str) { g(|"),
        (Some(LangKind::Rust), "let c = '\"'; h(|"),
        (Some(LangKind::Cpp), "int n = 1'000; f(|"),
        (Some(LangKind::Bash), "echo ${#a[@]} |"),
        (Some(LangKind::JavaScript), "/* a */ f(|"),
        (None, "don't say |"),
    ] {
        let (mut e, head, tail) = with_caret(lang, text);
        e.insert_char('"');
        assert_eq!(
            e.lines[0],
            format!("{head}\"\"{tail}"),
            "{lang:?}: {text:?}"
        );
    }
}

/// #1163: in Rust, a `'` that starts a lifetime or a loop label gets no
/// closing quote, so `fn foo<'a>` types as written.
#[test]
fn a_rust_lifetime_or_label_quote_does_not_pair() {
    for text in [
        "fn foo<|",
        "impl<|",
        "fn f(s: &|",
        "fn f<'a, |",
        "struct S<T: Clone + |",
        "        break |",
        "    continue |",
        "fn f<'a, 'b>() where |",
    ] {
        let (mut e, head, tail) = with_caret(Some(LangKind::Rust), text);
        e.insert_char('\'');
        assert_eq!(e.lines[0], format!("{head}'{tail}"), "{text:?}");
    }
    let (mut e, _, _) = with_caret(Some(LangKind::Rust), "fn foo|");
    for c in "<'a>".chars() {
        e.insert_char(c);
    }
    assert_eq!(e.lines[0], "fn foo<'a>");
}

/// #1163, negative: a Rust char literal still pairs, and so does a
/// `'` after `<` in a language without lifetimes.
#[test]
fn a_rust_char_literal_and_other_languages_still_pair_quotes() {
    for (lang, text) in [
        (Some(LangKind::Rust), "s.push(|"),
        (Some(LangKind::Rust), "let v = vec!['a', |"),
        (Some(LangKind::Rust), "let c = |"),
        (Some(LangKind::Rust), "match c { 'a' => |"),
        (Some(LangKind::Rust), "let t = (Vec::<u8>::new(), |"),
        (Some(LangKind::Rust), "if a < b { f(x, |"),
        (Some(LangKind::Rust), "if c < |"),
        (Some(LangKind::Python), "x = a < |"),
        (Some(LangKind::JavaScript), "f(a & |"),
    ] {
        let (mut e, head, tail) = with_caret(lang, text);
        e.insert_char('\'');
        assert_eq!(e.lines[0], format!("{head}''{tail}"), "{lang:?}: {text:?}");
    }
}

/// The line scan starts every line outside a string, so a quote typed in
/// a docstring body, a template literal or a block comment that an
/// earlier line opened still paired; the highlight pass knows (#844).
#[test]
fn a_quote_in_a_string_or_comment_from_an_earlier_line_pairs_only_once_it_closes() {
    let typed_at_caret = |lang: LangKind, text: &str| {
        let (head, tail) = text.split_once('|').expect("a caret marker");
        let mut e = editor_with(&format!("{head}{tail}"));
        e.lang = Some(lang);
        e.recompute_highlights();
        e.cursor_row = head.matches('\n').count();
        e.cursor_col = head.rsplit('\n').next().unwrap_or("").chars().count();
        e.insert_char('"');
        (e.lines.join("\n"), head.to_string(), tail.to_string())
    };
    for (lang, text) in [
        (
            LangKind::Python,
            "def f():\n    \"\"\"Doc\n    said | here\n    \"\"\"",
        ),
        (LangKind::JavaScript, "const s = `a\nb | c`;"),
        (LangKind::Rust, "/* start\n   see | */"),
    ] {
        let (got, head, tail) = typed_at_caret(lang, text);
        assert_eq!(got, format!("{head}\"{tail}"), "{lang:?}: {text:?}");
    }
    for (lang, text) in [
        (LangKind::Python, "doc = \"\"\"a\nb\"\"\" + |"),
        (LangKind::Rust, "/* a\n b */ f(|"),
    ] {
        let (got, head, tail) = typed_at_caret(lang, text);
        assert_eq!(got, format!("{head}\"\"{tail}"), "{lang:?}: {text:?}");
    }
}

#[test]
fn a_string_prefix_opens_a_pair_only_in_python_and_only_as_a_prefix() {
    // #844 guard: `f"` pairs because Python's `f` is part of the
    // opener. In any other language, or after a word Python does not
    // take as a prefix, a quote after a word character stays single.
    for lang in [None, Some(LangKind::Rust), Some(LangKind::JavaScript)] {
        let mut e = typed_in(lang, "f");
        e.insert_char('"');
        assert_eq!(e.lines[0], "f\"", "{lang:?}");
    }
    for word in ["ub", "fu", "bb", "rbf", "print"] {
        let mut e = typed_in(Some(LangKind::Python), word);
        e.insert_char('\'');
        assert_eq!(e.lines[0], format!("{word}'"), "{word:?} is no prefix");
    }
    let mut e = typed_in(Some(LangKind::Python), "x = U");
    e.insert_char('\'');
    assert_eq!(e.lines[0], "x = U''", "a prefix in either case");
}

#[test]
fn a_closing_quote_inside_a_string_is_still_typed_over() {
    // #844 guard: refusing to pair inside a string comes after
    // type-over, so the quote that closes a string steps over the one
    // already at the caret instead of adding a second.
    for (lang, text) in [
        (Some(LangKind::Python), "s = \"abc|\""),
        (Some(LangKind::Rust), "let s = \"a \\\" b|\";"),
        (None, "say \"hi|\""),
    ] {
        let (mut e, head, tail) = with_caret(lang, text);
        e.insert_char('"');
        assert_eq!(e.lines[0], format!("{head}{tail}"), "{lang:?}: {text:?}");
        assert_eq!(e.cursor_col, head.chars().count() + 1, "{lang:?}: {text:?}");
    }
}

#[test]
fn a_selection_inside_a_string_is_still_wrapped_in_quotes() {
    // #844 guard: wrapping a selection is not auto-pairing. A quote
    // typed over a selection inside a string still surrounds it.
    let mut e = editor_with("s = \"a word here\"");
    e.set_language(Some(LangKind::Python));
    e.selection = Some(EditorSelection {
        anchor: (0, 7),
        head: (0, 11),
    }); // "word"
    e.cursor_col = 11;
    e.insert_char('\'');
    assert_eq!(e.lines[0], "s = \"a 'word' here\"");
}

#[test]
fn a_docstring_an_edit_removed_no_longer_holds_back_a_pair() {
    // #844 guard: the string ranges are the highlight pass's after every
    // edit, never stale. Inside a docstring a quote does not pair; once
    // its opener is deleted, the same quote on the same line does.
    let mut e = editor_with("doc = \"\"\"\nx = \n\"\"\"");
    e.set_language(Some(LangKind::Python));
    e.cursor_row = 1;
    e.cursor_col = 4;
    e.insert_char('"');
    assert_eq!(e.lines[1], "x = \"", "inside the docstring: no pair");
    e.backspace();
    e.cursor_row = 0;
    e.cursor_col = 9;
    for _ in 0..3 {
        e.backspace();
    }
    assert_eq!(e.lines[0], "doc = ");
    e.cursor_row = 1;
    e.cursor_col = 4;
    e.insert_char('"');
    assert_eq!(e.lines[1], "x = \"\"", "no docstring left: the quote pairs");
}

#[test]
fn string_ranges_are_not_read_while_the_pass_over_an_edit_is_out() {
    // #844 x #850 guard: the string and comment ranges are byte offsets
    // into the text one syntax pass read, and are not carried through
    // edits. While the pass over an edit is still out (a file past the
    // inline budget), the docstring the edit removed must not hold back
    // a pair: the caret's line decides, as before #844.
    let mut e = editor_with("doc = \"\"\"\nx = \n\"\"\"");
    e.set_language(Some(LangKind::Python));
    e.defer_highlights_for_test();
    e.cursor_row = 0;
    e.cursor_col = 9;
    for _ in 0..3 {
        e.backspace();
    }
    assert_eq!(e.lines[0], "doc = ");
    assert!(e.highlight_pending(), "the edit's pass has not landed");
    e.cursor_row = 1;
    e.cursor_col = 4;
    e.insert_char('"');
    assert_eq!(e.lines[1], "x = \"\"", "the stale docstring is not read");
}

#[test]
fn a_pass_that_edits_overtook_does_not_make_its_string_ranges_count() {
    // #844 x #850 guard: a pass that lands after more edits were made
    // read older text. Its spans are carried over; its string ranges
    // are not, so the docstring the later edits removed stays unread.
    let mut e = editor_with("doc = \"\"\"\nx = \n\"\"\"");
    e.set_language(Some(LangKind::Python));
    e.defer_highlights_for_test();
    let settled = e.highlight_generation();
    e.insert_char('a'); // starts a pass over text that has the docstring
    e.cursor_row = 0;
    e.cursor_col = 10;
    for _ in 0..3 {
        e.backspace();
    }
    assert_eq!(e.lines[0], "adoc = ");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !e.poll_highlights() {
        assert!(
            std::time::Instant::now() < deadline,
            "the pass never landed"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert_eq!(
        e.highlight_generation(),
        settled + 1,
        "the first pass landed"
    );
    assert!(
        e.highlight_pending(),
        "and the edits after it started another"
    );
    e.cursor_row = 1;
    e.cursor_col = 4;
    e.insert_char('"');
    assert_eq!(
        e.lines[1], "x = \"\"",
        "the older text's docstring is not read"
    );
}

#[test]
fn string_ranges_count_again_once_the_pass_over_the_edit_lands() {
    // #844 x #850 guard against going too far: the fallback lasts only
    // while the pass is out. Once it lands on the edited text, a quote
    // inside the docstring an earlier line opened is text again.
    let mut e = editor_with("doc = \"\"\"\nx = \n\"\"\"");
    e.set_language(Some(LangKind::Python));
    e.defer_highlights_for_test();
    e.cursor_row = 1;
    e.cursor_col = 4;
    e.insert_char(' ');
    e.await_highlight_pass(None);
    assert!(!e.highlight_pending());
    e.insert_char('"');
    assert_eq!(e.lines[1], "x =  \"", "inside the docstring: no pair");
}

#[test]
fn typing_a_bracket_with_a_selection_surrounds_it() {
    let mut e = editor_with("abc def");
    e.cursor_row = 0;
    e.selection = Some(EditorSelection {
        anchor: (0, 0),
        head: (0, 3),
    }); // "abc"
    e.insert_char('(');
    assert_eq!(e.lines[0], "(abc) def");
    let sel = e
        .selection
        .expect("the selection survives, on the inner text");
    let ((sr, sc), (er, ec)) = sel.normalised();
    assert_eq!((sr, sc, er, ec), (0, 1, 0, 4));
    e.undo();
    assert_eq!(e.lines[0], "abc def", "surround is one undo step");
}

#[test]
fn backspace_between_an_empty_pair_deletes_both() {
    let mut e = editor_with("");
    e.insert_char('(');
    assert_eq!(e.lines[0], "()");
    e.backspace();
    assert_eq!(e.lines[0], "", "the empty pair dies together");
}

fn tag_editor(text: &str, lang: LangKind) -> Editor {
    let mut e = editor_with(text);
    e.lang = Some(lang);
    e.cursor_row = 0;
    e.cursor_col = e.lines[0].chars().count();
    e
}

#[test]
fn typing_the_closing_angle_of_an_html_tag_inserts_the_closing_tag() {
    let mut e = tag_editor("<div", LangKind::Html);
    e.insert_char('>');
    assert_eq!(e.lines[0], "<div></div>");
    assert_eq!(e.cursor_col, 5, "the caret sits between the two tags");
}

#[test]
fn tag_auto_close_carries_attributes_but_closes_on_the_name_only() {
    let mut e = tag_editor("<a href=\"/x\" class=\"link\"", LangKind::Html);
    e.insert_char('>');
    assert_eq!(e.lines[0], "<a href=\"/x\" class=\"link\"></a>");
}

#[test]
fn tag_auto_close_is_one_undo_step() {
    let mut e = tag_editor("<div", LangKind::Html);
    e.insert_char('>');
    assert_eq!(e.lines[0], "<div></div>");
    e.undo();
    assert_eq!(e.lines[0], "<div", "undo restores the pre-keystroke text");
}

#[test]
fn void_elements_never_get_a_closing_tag() {
    for void in ["<br", "<img src=\"a.png\"", "<input", "<hr", "<meta"] {
        let mut e = tag_editor(void, LangKind::Html);
        e.insert_char('>');
        assert_eq!(
            e.lines[0],
            format!("{void}>"),
            "a void element takes no closing tag"
        );
    }
}

#[test]
fn a_closing_tag_does_not_close_again() {
    let mut e = tag_editor("<div></div", LangKind::Html);
    e.insert_char('>');
    assert_eq!(e.lines[0], "<div></div>");
}

#[test]
fn retyping_the_angle_of_an_already_closed_tag_adds_no_second_close() {
    for (text, want) in [("<div</div>", "<div></div>"), ("<div>", "<div>>")] {
        let mut e = tag_editor(text, LangKind::Html);
        e.cursor_col = 4;
        e.insert_char('>');
        assert_eq!(e.lines[0], want);
    }
}

#[test]
fn a_self_closed_tag_is_left_alone() {
    let mut e = tag_editor("<Foo /", LangKind::Tsx);
    e.insert_char('>');
    assert_eq!(e.lines[0], "<Foo />");
}

#[test]
fn jsx_components_and_namespaced_tags_close_with_their_full_name() {
    let mut e = tag_editor("<Foo.Bar", LangKind::Tsx);
    e.insert_char('>');
    assert_eq!(e.lines[0], "<Foo.Bar></Foo.Bar>");
    // A void element name is HTML's rule, not JSX's: `<Input>` is an
    // ordinary component there and does take a closing tag.
    let mut e = tag_editor("<input", LangKind::Tsx);
    e.insert_char('>');
    assert_eq!(e.lines[0], "<input></input>");
}

#[test]
fn generics_and_comparisons_never_sprout_a_closing_tag() {
    // A `<` right after a word character is a generic or a comparison,
    // which is what these all are — none may auto-close.
    for src in ["const a = b<c", "let x: Array<string", "foo<Bar"] {
        let mut e = tag_editor(src, LangKind::Tsx);
        e.insert_char('>');
        assert_eq!(e.lines[0], format!("{src}>"), "{src} must not auto-close");
    }
    // A standalone generic declaration: the `,` is not tag syntax.
    let mut e = tag_editor("const f = <T,", LangKind::Tsx);
    e.insert_char('>');
    assert_eq!(e.lines[0], "const f = <T,>");
    // The other two generic arrow forms: a constraint and a default.
    for src in ["const f = <T extends unknown", "const f = <T = unknown"] {
        let mut e = tag_editor(src, LangKind::Tsx);
        e.insert_char('>');
        assert_eq!(e.lines[0], format!("{src}>"), "{src} must not auto-close");
    }
    // A `>` typed inside a `{…}` expression or a quoted attribute value is
    // part of that value (an arrow, a comparison, text), not the tag's end.
    for (src, lang) in [
        ("return <Foo onClick={() =", LangKind::Tsx),
        ("<div hidden={a ", LangKind::Tsx),
        ("<a title=\"1", LangKind::Html),
        ("<a title='x", LangKind::Html),
        ("return <Foo label={`a", LangKind::Tsx),
    ] {
        let mut e = tag_editor(src, lang);
        e.insert_char('>');
        assert_eq!(e.lines[0], format!("{src}>"), "{src} must not auto-close");
    }
    // Once the value or expression is closed, the tag closes as usual.
    let mut e = tag_editor("<a title=\"1\"", LangKind::Html);
    e.insert_char('>');
    assert_eq!(e.lines[0], "<a title=\"1\"></a>");
    // An ordinary attribute after the name still closes, so the two
    // assertions above cannot pass by closing nothing at all.
    let mut e = tag_editor("return <Foo bar={1}", LangKind::Tsx);
    e.insert_char('>');
    assert_eq!(e.lines[0], "return <Foo bar={1}></Foo>");
}

#[test]
fn a_tag_inside_a_script_string_or_comment_never_auto_closes() {
    // The caret sits before the partner quote auto-pairing inserted.
    for (src, lang) in [
        ("const s = \"<div\"", LangKind::JavaScript),
        ("const s = '<div'", LangKind::Tsx),
        ("const s = `<div`", LangKind::Tsx),
        ("const s = \"a\\\"<div\"", LangKind::Tsx),
    ] {
        let mut e = tag_editor(src, lang);
        e.cursor_col -= 1;
        e.insert_char('>');
        let want = format!("{}>{}", &src[..src.len() - 1], &src[src.len() - 1..]);
        assert_eq!(e.lines[0], want, "{src} is a string, not markup");
    }
    let mut e = tag_editor("// render <div", LangKind::Tsx);
    e.insert_char('>');
    assert_eq!(e.lines[0], "// render <div>", "a comment is not markup");
    // Control: balanced strings earlier on the line do not suppress it.
    let mut e = tag_editor("f(\"x\", 'y', <div", LangKind::Tsx);
    e.insert_char('>');
    assert_eq!(e.lines[0], "f(\"x\", 'y', <div></div>");
}

#[test]
fn tag_auto_close_only_runs_in_markup_languages() {
    // The same text in Rust or TypeScript is a comparison or a generic.
    for lang in [LangKind::Rust, LangKind::TypeScript, LangKind::Python] {
        let mut e = tag_editor("<div", lang);
        e.insert_char('>');
        assert_eq!(e.lines[0], "<div>", "{lang:?} must not auto-close tags");
    }
}

/// #1632: a Vue or Svelte component's template closes tags like HTML,
/// but its `<script>` and `<style>` blocks are code, where a `>` after
/// a generic or inside a string must stay a single `>`.
#[test]
fn sfc_script_and_style_blocks_never_auto_close_tags() {
    for lang in [LangKind::Vue, LangKind::Svelte] {
        for (open, line) in [
            ("<script lang=\"ts\">", "const f = <T extends unknown"),
            ("<script>", "const s = \"<div"),
            ("<style>", "a <b"),
        ] {
            let mut e = editor_with(&format!("{open}\n{line}"));
            e.lang = Some(lang);
            e.cursor_row = 1;
            e.cursor_col = e.lines[1].chars().count();
            e.insert_char('>');
            assert_eq!(e.lines[1], format!("{line}>"), "{lang:?} {open} {line}");
        }
        // After the block ends, the template closes tags again.
        let mut e = editor_with("<script>\nlet n = 1;\n</script>\n<div");
        e.lang = Some(lang);
        e.cursor_row = 3;
        e.cursor_col = 4;
        e.insert_char('>');
        assert_eq!(e.lines[3], "<div></div>", "{lang:?}");
        // Typing the `<script>` tag itself still closes it.
        let mut e = tag_editor("<script lang=\"ts\"", lang);
        e.insert_char('>');
        assert_eq!(e.lines[0], "<script lang=\"ts\"></script>", "{lang:?}");
    }
}

#[test]
fn tag_auto_close_can_be_switched_off() {
    let mut e = tag_editor("<div", LangKind::Html);
    e.auto_close_tags = false;
    e.insert_char('>');
    assert_eq!(e.lines[0], "<div>");
}

#[test]
fn comments_doctypes_and_processing_instructions_are_left_alone() {
    for src in ["<!-- note --", "<!DOCTYPE html", "<?xml version=\"1.0\"?"] {
        let mut e = tag_editor(src, LangKind::Xml);
        e.insert_char('>');
        assert_eq!(e.lines[0], format!("{src}>"), "{src} must not auto-close");
    }
}

#[test]
fn typing_the_angle_with_a_selection_replaces_it_without_closing() {
    let mut e = tag_editor("<div>old</div>", LangKind::Html);
    e.selection = Some(EditorSelection {
        anchor: (0, 5),
        head: (0, 8),
    }); // "old"
    e.insert_char('>');
    assert_eq!(e.lines[0], "<div>></div>");
}

#[test]
fn typing_an_opener_into_a_fresh_empty_buffer_never_panics() {
    // Editor::new starts with NO lines at all; the pair path indexed
    // lines[0] and panicked on the first keystroke of an untitled
    // buffer (#122 review, Critical).
    let mut e = Editor::new();
    e.auto_close_pairs = true;
    e.insert_char('(');
    assert_eq!(e.lines[0], "()");
}

#[test]
fn backspace_keeps_the_closer_of_a_pre_existing_pair() {
    // Only the pair the last auto-close inserted may die together; a
    // `()` already in the file keeps its closer (#122 review).
    let mut e = editor_with("()");
    e.cursor_row = 0;
    e.cursor_col = 1;
    e.backspace();
    assert_eq!(e.lines[0], ")", "the manual pair loses only one side");
}

#[test]
fn the_toggle_disables_every_pair_behavior() {
    let mut e = editor_with("");
    e.auto_close_pairs = false;
    e.insert_char('(');
    assert_eq!(e.lines[0], "(", "off means plain inserts");
    e.insert_char(')');
    assert_eq!(e.lines[0], "()");
    e.cursor_col = 1;
    e.backspace();
    assert_eq!(e.lines[0], ")", "off means plain backspace");
}
