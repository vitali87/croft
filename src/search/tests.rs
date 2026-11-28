//! The search engine's own tests, moved beside it from the Search view's (#2062).

use super::*;
use std::fs;
use tempfile::TempDir;

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

/// Search run `run` for `query` with the default options.
fn query_request(run: u64, query: &str) -> SearchRequest {
    SearchRequest::Query {
        run,
        query: query.into(),
        opts: SearchOpts::default(),
    }
}

/// The events up to the first `Done`: the run the last event was for, the
/// hits, and whether the `Done` came.
fn collect_until_done(
    rx: &std::sync::mpsc::Receiver<SearchEvent>,
    deadline: std::time::Duration,
) -> (Option<u64>, Vec<SearchHit>, bool) {
    let until = std::time::Instant::now() + deadline;
    let mut last_run: Option<u64> = None;
    let mut hits: Vec<SearchHit> = Vec::new();
    let mut saw_done = false;
    while std::time::Instant::now() < until {
        let remaining = until.saturating_duration_since(std::time::Instant::now());
        match rx.recv_timeout(remaining.max(std::time::Duration::from_millis(50))) {
            Ok(SearchEvent::Hits(run, batch)) => {
                last_run = Some(run);
                hits.extend(batch);
            }
            Ok(SearchEvent::Done(run)) => {
                last_run = Some(run);
                saw_done = true;
                break;
            }
            Ok(SearchEvent::Cancelled(run) | SearchEvent::Crashed(run)) => last_run = Some(run),
            Err(_) => break,
        }
    }
    (last_run, hits, saw_done)
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

    q_tx.send(query_request(1, "newname")).unwrap();
    let (_, hits, done) = collect_until_done(&e_rx, std::time::Duration::from_secs(2));
    assert!(done);
    assert_eq!(hits.len(), 1, "the unsaved buffer's new name must be found");
    assert_eq!(hits[0].line_text, "newname");

    q_tx.send(query_request(2, "oldname")).unwrap();
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
    q_tx.send(query_request(1, "needle")).unwrap();
    let (run, hits, done) = collect_until_done(&e_rx, std::time::Duration::from_secs(2));
    assert_eq!(run, Some(1));
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
    q_tx.send(query_request(1, "needle")).unwrap();
    let mut total = 0usize;
    let mut hits_events = 0usize;
    let mut done_events = 0usize;
    let until = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while std::time::Instant::now() < until {
        match e_rx.recv_timeout(std::time::Duration::from_millis(500)) {
            Ok(SearchEvent::Hits(_, batch)) => {
                hits_events += 1;
                total += batch.len();
            }
            Ok(SearchEvent::Done(_)) => {
                done_events += 1;
                break;
            }
            Ok(SearchEvent::Cancelled(_) | SearchEvent::Crashed(_)) | Err(_) => break,
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
    q_tx.send(query_request(1, "o")).unwrap();
    q_tx.send(query_request(2, "on")).unwrap();
    q_tx.send(query_request(3, "one")).unwrap();
    let (run, hits, done) = collect_until_done(&e_rx, std::time::Duration::from_secs(2));
    assert_eq!(run, Some(3), "coalesce must drop intermediate prefixes");
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
    q_tx.send(query_request(1, "o")).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(100));
    q_tx.send(query_request(2, "on")).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(100));
    q_tx.send(query_request(3, "one")).unwrap();
    // Now stop typing and let the debounce expire + the scan finish.
    let mut dones: Vec<u64> = Vec::new();
    let until = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while std::time::Instant::now() < until {
        if let Ok(SearchEvent::Done(run)) = e_rx.recv_timeout(std::time::Duration::from_millis(200))
        {
            dones.push(run)
        }
    }
    drop(q_tx);
    join.join().unwrap();
    assert_eq!(
        dones,
        vec![3],
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
    q_tx.send(query_request(1, "c")).unwrap();
    // Stage on the scan observably RUNNING — the first Hits batch —
    // not on sleeping past the debounce: a sleep equal to the
    // debounce races the worker's own timer on a knife edge, and
    // when the send wins the new query displaces "c" in the debounce
    // loop, silently skipping the cancellation path this test exists
    // to cover (#68).
    let mut c_hits = 0usize;
    let staging = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while c_hits == 0 && std::time::Instant::now() < staging {
        if let Ok(SearchEvent::Hits(1, hits)) =
            e_rx.recv_timeout(std::time::Duration::from_millis(100))
        {
            c_hits += hits.len();
        }
    }
    assert!(
        c_hits > 0,
        "staging: the \"c\" scan never started streaming"
    );
    q_tx.send(query_request(2, "zzznoneatall")).unwrap();
    // The cancellation proof is structural, not wall-clock (#68): a
    // cancelled "c" scan is cut short, streaming fewer than its full
    // 4M matches before its Done; an uncancelled one streams every
    // single match first. A tight arrival deadline on the second Done
    // measured the same fact through scheduler latency and flaked
    // under suite load — the long timeout here only guards a hang.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut second_done = false;
    // How the cut-short "c" run ended (#2608): it never looked at every
    // file, so it must not claim it did.
    let mut first_end = None;
    while std::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        match e_rx.recv_timeout(remaining.max(std::time::Duration::from_millis(50))) {
            Ok(SearchEvent::Hits(1, hits)) => c_hits += hits.len(),
            Ok(end @ (SearchEvent::Done(1) | SearchEvent::Cancelled(1))) => first_end = Some(end),
            Ok(SearchEvent::Done(2)) => {
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
    assert_eq!(
        first_end,
        Some(SearchEvent::Cancelled(1)),
        "the cut-short run ends Cancelled, before the new run's events"
    );
    assert!(
        c_hits < 4_000_000,
        "the in-flight scan must be cancelled, not run to completion; it streamed all {c_hits} matches"
    );
}

/// #2608: a run stopped by `Cancel` while it waits for the pause in typing
/// never scans, and says so, so the panel does not wait on it.
#[test]
fn search_worker_loop_cancels_a_run_still_waiting_for_the_pause() {
    let tmp = TempDir::new().unwrap();
    write(&tmp.path().join("a.txt"), "needle\n");
    let (q_tx, q_rx) = std::sync::mpsc::channel::<SearchRequest>();
    let (e_tx, e_rx) = std::sync::mpsc::channel::<SearchEvent>();
    let root = tmp.path().to_path_buf();
    let join = std::thread::spawn(move || search_worker_loop(root, q_rx, e_tx));
    q_tx.send(query_request(1, "needle")).unwrap();
    q_tx.send(SearchRequest::Cancel).unwrap();
    assert_eq!(
        e_rx.recv_timeout(std::time::Duration::from_secs(5)),
        Ok(SearchEvent::Cancelled(1))
    );
    assert!(
        e_rx.recv_timeout(super::worker::SEARCH_DEBOUNCE * 2)
            .is_err(),
        "a cancelled run sends nothing else"
    );
    drop(q_tx);
    join.join().unwrap();
}

/// A stand-in for the walker whose scan crashes.
fn crashing_scan(
    _: &Path,
    _: &str,
    _: SearchOpts,
    _: &Arc<AtomicBool>,
    _: &HashSet<PathBuf>,
    _: &PathFilter,
    _: super::worker::Emit,
) {
    panic!("the scan crashed (on purpose, in a test)")
}

/// #2611: a run whose scan crashed says so, instead of leaving the panel
/// waiting on it for good.
#[test]
fn search_worker_loop_reports_a_scan_that_crashed() {
    let tmp = TempDir::new().unwrap();
    write(&tmp.path().join("a.txt"), "needle\n");
    let (q_tx, q_rx) = std::sync::mpsc::channel::<SearchRequest>();
    let (e_tx, e_rx) = std::sync::mpsc::channel::<SearchEvent>();
    let root = tmp.path().to_path_buf();
    let join = std::thread::spawn(move || {
        super::worker::search_worker_loop_with(root, q_rx, e_tx, crashing_scan)
    });
    q_tx.send(query_request(1, "needle")).unwrap();
    assert_eq!(
        e_rx.recv_timeout(std::time::Duration::from_secs(10)),
        Ok(SearchEvent::Crashed(1))
    );
    // The worker carries on: the next run is scanned too.
    q_tx.send(query_request(2, "needle")).unwrap();
    assert_eq!(
        e_rx.recv_timeout(std::time::Duration::from_secs(10)),
        Ok(SearchEvent::Crashed(2))
    );
    drop(q_tx);
    join.join().unwrap();
}

/// Set by [`scan_crashing_when_stopped`] once it is scanning.
static STOPPABLE_SCAN_STARTED: AtomicBool = AtomicBool::new(false);

/// A stand-in for the walker that scans until it is stopped, then crashes.
fn scan_crashing_when_stopped(
    _: &Path,
    _: &str,
    _: SearchOpts,
    cancel: &Arc<AtomicBool>,
    _: &HashSet<PathBuf>,
    _: &PathFilter,
    _: super::worker::Emit,
) {
    STOPPABLE_SCAN_STARTED.store(true, Ordering::SeqCst);
    while !cancel.load(Ordering::Relaxed) {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("the scan crashed as it stopped (on purpose, in a test)")
}

/// #2611: a scan that crashes as `Cancel` stops it still ends its run, so
/// a panel that left Search is not left waiting on it.
#[test]
fn search_worker_loop_reports_a_scan_that_crashed_as_it_was_stopped() {
    let tmp = TempDir::new().unwrap();
    let (q_tx, q_rx) = std::sync::mpsc::channel::<SearchRequest>();
    let (e_tx, e_rx) = std::sync::mpsc::channel::<SearchEvent>();
    let root = tmp.path().to_path_buf();
    let join = std::thread::spawn(move || {
        super::worker::search_worker_loop_with(root, q_rx, e_tx, scan_crashing_when_stopped)
    });
    q_tx.send(query_request(1, "needle")).unwrap();
    crate::test_budget::await_spawned(
        std::time::Duration::from_secs(1),
        "the scan to start",
        || STOPPABLE_SCAN_STARTED.load(Ordering::SeqCst),
    );
    q_tx.send(SearchRequest::Cancel).unwrap();
    assert_eq!(
        e_rx.recv_timeout(std::time::Duration::from_secs(10)),
        Ok(SearchEvent::Crashed(1))
    );
    assert!(
        e_rx.recv_timeout(std::time::Duration::from_millis(300))
            .is_err(),
        "said once"
    );
    drop(q_tx);
    join.join().unwrap();
}

#[test]
fn search_worker_loop_short_circuits_empty_queries() {
    let tmp = TempDir::new().unwrap();
    write(&tmp.path().join("a.txt"), "anything\n");
    let (q_tx, q_rx) = std::sync::mpsc::channel::<SearchRequest>();
    let (e_tx, e_rx) = std::sync::mpsc::channel::<SearchEvent>();
    let root = tmp.path().to_path_buf();
    let join = std::thread::spawn(move || search_worker_loop(root, q_rx, e_tx));
    q_tx.send(query_request(1, "")).unwrap();
    let (run, hits, done) = collect_until_done(&e_rx, std::time::Duration::from_secs(2));
    assert_eq!(run, Some(1));
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
    q_tx.send(query_request(1, "needle")).unwrap();
    let (run, hits, done) = collect_until_done(&e_rx, std::time::Duration::from_secs(2));
    assert_eq!(run, Some(1));
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
    q_tx.send(query_request(1, "needle")).unwrap();
    let (_, hits, done) = collect_until_done(&e_rx, std::time::Duration::from_secs(2));
    assert!(done);
    assert_eq!(hits.len(), 1, "filter must restrict the scan to *.rs");
    assert!(hits[0].path.ends_with("keep.rs"));
    drop(q_tx);
    join.join().unwrap();
}

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
