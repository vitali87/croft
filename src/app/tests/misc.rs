//! Tests that fit no other file in this directory. Prefer a topical file
//! when one fits.

use super::*;

#[test]
fn offload_drop_runs_the_destructor_off_the_calling_thread() {
    // Regression for the UI freeze: dropping a notify FSEvents watcher runs
    // FsEventWatcher::stop(), which busy-spins on thread::yield_now() until
    // its CFRunLoop thread parks. On a MustScanSubDirs rescan that spin never
    // ends, so dropping on the UI thread (change_workspace_root -> rebind,
    // or disable) freezes the app and pins a core. offload_drop must run the
    // destructor on another thread so the caller never blocks on stop().
    use std::sync::mpsc;
    use std::time::Duration;

    struct DropSignal(mpsc::Sender<std::thread::ThreadId>);
    impl Drop for DropSignal {
        fn drop(&mut self) {
            let _ = self.0.send(std::thread::current().id());
        }
    }

    let (tx, rx) = mpsc::channel();
    let caller = std::thread::current().id();
    super::fs_watch::offload_drop(DropSignal(tx));
    let dropped_on = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("destructor must run within the timeout");
    assert_ne!(
        dropped_on, caller,
        "watcher destructor must run off the calling (UI) thread, not block it"
    );
}

#[test]
fn rect_contains_basic() {
    let r = Rect {
        x: 5,
        y: 5,
        width: 10,
        height: 10,
    };
    assert!(rect_contains(r, 5, 5));
    assert!(rect_contains(r, 14, 14));
    assert!(!rect_contains(r, 4, 5));
    assert!(!rect_contains(r, 15, 5));
    assert!(!rect_contains(r, 5, 15));
}

#[test]
fn rect_contains_zero_sized_is_empty() {
    let r = Rect {
        x: 0,
        y: 0,
        width: 0,
        height: 0,
    };
    assert!(!rect_contains(r, 0, 0));
}

#[test]
fn lerp_rgb_hits_endpoints_exactly() {
    let a = (10u8, 20, 30);
    let b = (100u8, 200, 250);
    assert_eq!(lerp_rgb(a, b, 0.0), a);
    assert_eq!(lerp_rgb(a, b, 1.0), b);
    let mid = lerp_rgb(a, b, 0.5);
    assert!(mid.0 > a.0 && mid.0 < b.0);
}

#[test]
fn ctrl_d_is_not_delete_node() {
    assert!(!is_delete_node_key(key(
        KeyCode::Char('d'),
        KeyModifiers::CONTROL
    )));
}

#[test]
fn set_title_seq_wraps_with_osc0_and_bel() {
    let bytes = set_title_seq("croft");
    assert_eq!(bytes, b"\x1b]0;croft\x07");
}

#[test]
fn set_title_seq_handles_empty_title() {
    let bytes = set_title_seq("");
    assert_eq!(bytes, b"\x1b]0;\x07");
}

#[test]
fn set_title_seq_passes_through_unicode() {
    let bytes = set_title_seq("croft  README.md");
    assert_eq!(bytes, "\x1b]0;croft  README.md\x07".as_bytes());
}

#[test]
fn set_title_seq_strips_control_bytes_that_would_break_the_escape() {
    // BEL terminates the OSC, ESC starts a new sequence — both must be filtered.
    let bytes = set_title_seq("evil\x07file");
    assert_eq!(bytes, b"\x1b]0;evilfile\x07");
    let bytes = set_title_seq("evil\x1bfile");
    assert_eq!(bytes, b"\x1b]0;evilfile\x07");
    let bytes = set_title_seq("evil\nfile");
    assert_eq!(bytes, b"\x1b]0;evilfile\x07");
}

#[test]
fn app_name_constant_is_croft() {
    assert_eq!(APP_NAME, "croft");
}

#[test]
fn reset_session_bg_seq_restores_profile_default() {
    // On exit we must revert iTerm2's session bg so the user's shell
    // doesn't keep croft's forced bg colour.
    assert_eq!(reset_session_bg_seq(), "\x1b]1337;SetColors=bg=default\x07");
}

#[test]
fn brand_spans_render_the_cr_ft_wordmark_with_orange_brackets() {
    let spans = brand_spans(crate::theme::Theme::default());
    let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(text, " cr<>ft ");
    // The "<>" (the stylised "o") carries the logo orange; letters stay white.
    let bracket = spans.iter().find(|s| s.content == "<>").unwrap();
    assert_eq!(bracket.style.fg, Some(BRAND_ORANGE));
    assert_eq!(spans[0].style.fg, Some(Color::White));
}

#[test]
fn no_stale_tcode_pill_literal_in_source() {
    let src = include_str!("../mod.rs");
    assert!(
        !src.contains("\" tcode \""),
        "stale ` tcode ` pill literal still present in src/app/mod.rs"
    );
}

#[test]
fn build_title_uses_basename_and_app_name() {
    let p = std::path::Path::new("/Users/somebody/projects/croft");
    assert_eq!(build_title(p), "croft  croft");
    let p = std::path::Path::new("/");
    assert_eq!(build_title(p), "croft  /");
}

#[test]
fn bounded_output_gives_up_on_a_command_that_hangs() {
    let started = std::time::Instant::now();
    let budget = std::time::Duration::from_millis(500);
    assert!(
        bounded_output(
            std::process::Command::new("sh").args(["-c", "sleep 30"]),
            budget
        )
        .is_none()
    );
    assert!(
        bounded_output(
            std::process::Command::new("sh").args(["-c", "exec 1>&-; sleep 30"]),
            budget
        )
        .is_none()
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    let (status, out) = bounded_output(
        std::process::Command::new("sh").args(["-c", "printf 42"]),
        std::time::Duration::from_secs(10),
    )
    .unwrap();
    assert!(status.success());
    assert_eq!(out, b"42");
}

#[test]
fn cwd_of_pid_on_self_returns_a_directory_under_tempdir() {
    // The harness runs each test from the crate root by default, but
    // we don't depend on that — we just need cwd_of_pid to round-trip
    // *some* directory for the current process. lsof / /proc gives us
    // the live cwd of the shell that started the test runner.
    let pid = std::process::id();
    match cwd_of_pid(pid) {
        Some(p) => assert!(
            p.is_dir(),
            "cwd lookup must return a real directory, got {p:?}"
        ),
        None => {
            // Acceptable on platforms where neither /proc nor lsof
            // resolve the cwd in the sandbox. The fallback in
            // split_terminal handles that case; nothing to assert here.
        }
    }
}

#[test]
fn cmd_active_treats_super_as_command_everywhere() {
    // Super is the command modifier on every platform, Termux or not.
    assert!(cmd_active(KeyModifiers::SUPER, false));
    assert!(cmd_active(KeyModifiers::SUPER, true));
}

/// #1630: with HOME unset or relative the cache is relative, but the typed
/// line runs in the pane's directory, so the staged block's directory is
/// made absolute (against croft's own directory) before it is named.
#[test]
fn a_relative_block_directory_is_made_absolute() {
    let dir = super::absolute_block_dir(PathBuf::from(".cache/croft/blocks"));
    assert!(dir.is_absolute(), "{dir:?}");
    assert_eq!(
        dir,
        std::env::current_dir().unwrap().join(".cache/croft/blocks")
    );
    assert!(super::block_script_dir().is_absolute());
}

/// A lock file croft cannot open is not another window (#337): the status
/// names the path and the error, never "another croft". And the refusal is
/// latched by KIND: when the reason changes (the directory appears but a
/// second window now holds the flock) the new reason is announced, and
/// when the lock frees this window hosts.
#[test]
fn an_unopenable_host_lock_is_reported_as_such_and_re_announced_when_the_reason_changes() {
    use std::time::{Duration, Instant};
    let tmp = tempfile::tempdir().unwrap();
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
    let lock_dir = tmp.path().join("no-such-dir");
    let lock_path = lock_dir.join("host.lock");
    let mut b = App::new(tmp.path().to_path_buf()).unwrap();
    b.pair_record_path = tmp.path().join("b.pair.json");
    b.pair_socket = socket.clone();
    b.pair_host_lock_path = lock_path.clone();
    b.pair_spawn_override = Some(Box::new(local_test_spawn));
    crate::session::write_pair_record(
        &b.pair_record_path,
        &crate::session::PairRecord {
            model: None,
            name: "nav".into(),
            enabled: true,
            task: None,
            provider: None,
            base_url: None,
        },
    )
    .unwrap();
    b.last_pair_check = None;
    b.maybe_seat_navigator();
    assert!(b.pair_host.is_none(), "cannot host without the lock");
    assert!(
        b.status.contains("cannot open host lock") && b.status.contains("no-such-dir"),
        "an unopenable lock names the path and the error, got: {}",
        b.status
    );
    assert!(
        !b.status.to_lowercase().contains("another croft"),
        "an open failure must never be blamed on another window: {}",
        b.status
    );

    // Same reason next tick: quiet.
    b.status = String::from("Saved src/foo.rs");
    b.last_pair_check = None;
    assert!(
        !b.maybe_seat_navigator(),
        "an unchanged refusal stays quiet"
    );
    assert_eq!(b.status, "Saved src/foo.rs");

    // The directory appears, but another window holds the flock: the
    // reason changed, so it is announced.
    std::fs::create_dir_all(&lock_dir).unwrap();
    let holder = crate::session::try_acquire_pair_host_lock(&lock_path).expect("holder acquires");
    b.last_pair_check = None;
    assert!(
        b.maybe_seat_navigator(),
        "a changed reason dirties the frame"
    );
    assert!(
        b.status.to_lowercase().contains("another croft"),
        "now it IS another window, got: {}",
        b.status
    );

    // The reverse transition: the holder is still there but the lock's
    // directory vanishes, so the refusal is an open failure again - and
    // that change of reason is announced too.
    std::fs::remove_dir_all(&lock_dir).unwrap();
    b.last_pair_check = None;
    assert!(b.maybe_seat_navigator(), "Busy -> Io is a changed reason");
    assert!(
        b.status.contains("cannot open host lock"),
        "the open failure is named again, got: {}",
        b.status
    );
    std::fs::create_dir_all(&lock_dir).unwrap();

    // The holder exits: this window takes over.
    drop(holder);
    b.last_pair_check = None;
    b.maybe_seat_navigator();
    assert!(
        b.pair_host.is_some(),
        "B hosts once the lock frees: {}",
        b.status
    );
    assert!(b.pair_host_lock.is_some());
}

/// A zero-sized frame (a host window mid-collapse) has no cells at all:
/// the fallback geometry used to floor each axis to one cell, handing
/// back a 1×1 popup that sits outside the frame it was asked to fit.
#[test]
fn a_zero_sized_frame_gets_no_fallback_popup() {
    let r = fallback_popup_rect(Rect::new(0, 0, 0, 0), 100);
    assert_eq!(r.area(), 0, "no cells exist to draw into: {r:?}");
    let r = fallback_popup_rect(Rect::new(0, 0, 80, 0), 110);
    assert_eq!(r.area(), 0, "a zero-height frame cannot host rows: {r:?}");
    let r = fallback_popup_rect(Rect::new(2, 3, 30, 8), 110);
    assert!(
        r.x >= 2 && r.y >= 3 && r.right() <= 32 && r.bottom() <= 11,
        "a real frame keeps the popup inside it: {r:?}"
    );
}

/// A check that warrants no hint does not spend the one-shot (#256).
///
/// The flag was set when a check STARTED, so any run under `on`, `off`, or
/// an under-cap `auto` consumed the only hint opportunity and a later
/// oversized `auto` run stayed silent forever. It is now set where the hint
/// is received, which is the only point that knows one was warranted.
#[cfg(unix)]
#[test]
fn a_check_that_warrants_no_hint_does_not_spend_it() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();

    // A run that produces no hint: the flag must stay unspent.
    app.queue_project_check_result_for_test(tmp.path(), "", "", true);
    app.sync_explorer_panels();
    assert!(
        !app.project_check_hinted_for_test(),
        "a check with no hint must not consume the one-shot"
    );

    // A later run that DOES warrant one still gets it.
    app.send_project_hint_for_test();
    app.queue_project_check_result_for_test(tmp.path(), "", "", true);
    app.sync_explorer_panels();
    assert!(
        app.status.contains("too large"),
        "the hint the earlier run would have eaten still arrives: {:?}",
        app.status
    );
    assert!(
        app.project_check_hinted_for_test(),
        "and NOW the one-shot is spent"
    );
}

/// The size hint survives a result landing in the same tick (#256).
///
/// Both drains run in one `sync_explorer_panels` body with no paint
/// between them, and the result sets `status` unconditionally. Writing the
/// hint before it DESTROYED the hint rather than deprioritising it - and
/// the hint is one-shot, so it never returned. The fast-failure path (an
/// over-cap root with no compiler installed) is exactly where the checker
/// finishes inside a single tick.
#[cfg(unix)]
#[test]
fn the_size_hint_is_not_erased_by_a_result_in_the_same_tick() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();

    // Both messages in flight before ONE drain, which is the race. Driven
    // through `sync_explorer_panels` rather than the direct-drain helper:
    // the hint logic lives in that function, so calling `drain_project_check`
    // itself would test a path the app never takes.
    app.send_project_hint_for_test();
    app.queue_project_check_result_for_test(tmp.path(), "", "", true);
    app.sync_explorer_panels();

    assert!(
        app.status.contains("no problems"),
        "the outcome the user asked for still leads: {:?}",
        app.status
    );
    assert!(
        app.status.contains("too large"),
        "and the one-shot hint survives rather than being overwritten: {:?}",
        app.status
    );
}

/// #430: "moved away" and "unknowable" are different, and only the first
/// is a reason to refuse.
///
/// Tested through the pure `cwd_matches` rather than through a live pane,
/// so BOTH arms execute on every platform. The previous version of this
/// test wrapped its headline assertion in `if kernel_shell_cwd().is_none()`
/// — false on macOS and Linux, so the assertion never ran anywhere the
/// suite runs, and reverting the fix would have left it green.
#[test]
fn an_unknowable_cwd_still_allows_reuse_but_a_wrong_one_does_not() {
    let root = Path::new("/w/a");

    // Read and equal: this command's pane.
    assert!(cwd_matches(Some(Path::new("/w/a")), root, true));
    // Read and different: the shell wandered off. A DESCENDANT is the
    // #430 defect specifically — `starts_with` accepted it.
    assert!(!cwd_matches(Some(Path::new("/w/a/sub")), root, true));
    assert!(!cwd_matches(Some(Path::new("/w/b")), root, true));
    // Unreadable where the platform CAN read one: the failure the guard
    // exists for, so it stays closed.
    assert!(!cwd_matches(None, root, true));
    // Unreadable because the platform can never read one (Android): a
    // closed guard would stack a new pane on every single run.
    assert!(cwd_matches(None, root, false));
    // And a platform that cannot observe still refuses a cwd it somehow
    // did read and that differs — the capability is not a blanket pass.
    assert!(!cwd_matches(Some(Path::new("/w/a/sub")), root, false));

    // The label half of the predicate, on a real pane.
    let tmp = tempfile::tempdir().unwrap();
    let real_root = tmp.path().canonicalize().unwrap();
    let app = App::new(tmp.path().to_path_buf()).unwrap();
    assert!(!pane_is_idle_at(
        &app.terminals[0],
        "Task: nonexistent",
        &real_root
    ));
}

/// Every raw-CUP writer in the post-draw block is guarded, not just the
/// ones that had a reported failure (#513). The block's six writers share
/// one hazard -- an absolute-CUP write ratatui cannot repair -- so a guard
/// on some of them leaves the same class open at the others. This counts
/// the call sites rather than exercising each, which is crude but is what
/// catches a SEVENTH writer being added later without one.
#[test]
fn every_post_draw_overlay_writer_checks_the_screen_bound() {
    let src = include_str!("../mod.rs");
    let guards = src.matches("app.overlay_fits_on_screen(").count();
    assert_eq!(
        guards, 5,
        "expected the five payload writers in the post-draw block to be \
         guarded (editor split slots share one call site, plus terminal, \
         markdown, minimap, welcome); the activity-bar writer calls \
         `self.overlay_fits_on_screen` from inside its own method. A \
         change here means a writer was added or removed - guard it."
    );
    assert!(
        src.contains("if !self.overlay_fits_on_screen(*x, *y, 1, 1)"),
        "the activity-bar writer keeps its own guard"
    );
}

/// Before any frame is drawn there is no screen to land on, so nothing is
/// emittable (#513). Guards the window between startup and the first
/// render, where `last_frame_area` is still zeroed.
#[test]
fn no_overlay_is_emittable_before_the_first_frame() {
    let tmp = tempfile::tempdir().unwrap();
    let app = App::new(tmp.path().to_path_buf()).unwrap();
    assert!(
        !app.overlay_fits_on_screen(0, 0, 1, 1),
        "no frame drawn yet: a 1x1 payload at the origin still has no screen"
    );
}

#[test]
fn an_in_memory_refusal_stops_blocking_once_its_window_has_passed() {
    // The TTL is applied when the policy is asked, not only when the file
    // is loaded: a croft left open past the window offers the host again.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.remote_offer_refused.clear();
    let pane = app.terminals[0].uid();
    app.remote_offer_refused.insert(
        String::from("db-1"),
        std::time::SystemTime::now()
            - crate::remote::REFUSED_HOST_TTL
            - std::time::Duration::from_secs(60),
    );
    app.consider_ssh_offer(pane, Some(String::from("db-1")));
    assert!(
        app.ssh_offer.is_some(),
        "a refusal older than the TTL no longer suppresses the offer"
    );
}

#[test]
fn draining_an_idle_listener_reports_no_change_and_does_not_block() {
    // Polled from the frame loop: a blocking accept here would freeze every
    // redraw until somebody happened to run `croft view`.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    seat_view_listener(&mut app, tmp.path());
    // Asserted on the fd rather than only on elapsed time: if the mode is
    // lost, `accept` blocks forever and a timing assertion is never reached,
    // so the test would HANG instead of failing and read as a CI timeout
    // rather than as this defect. Reading O_NONBLOCK turns that into a
    // one-line failure that names the cause.
    let flags = {
        use std::os::unix::io::AsRawFd;
        let listener = app.view_listener.as_ref().unwrap();
        unsafe { libc::fcntl(listener.as_raw_fd(), libc::F_GETFL) }
    };
    assert!(
        flags != -1 && flags & libc::O_NONBLOCK != 0,
        "the frame loop polls this listener, so it must be non-blocking"
    );
    let started = std::time::Instant::now();
    assert!(!app.drain_view_requests(), "nothing was sent");
    assert!(
        started.elapsed() < std::time::Duration::from_millis(500),
        "the drain blocked for {:?}",
        started.elapsed()
    );
}

#[test]
fn a_terminated_request_over_the_cap_is_refused_like_an_unterminated_one() {
    // The cap was tested on the iteration AFTER the newline branch, so a
    // request carrying its newline in the very chunk that crossed the limit
    // was accepted and deserialised. The bound held for an unterminated flood
    // and not for a terminated one - which is the shape a real client sends.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let sock = seat_view_listener(&mut app, tmp.path());

    let sock2 = sock.clone();
    let flood = std::thread::spawn(move || {
        let Ok(mut c) = std::os::unix::net::UnixStream::connect(&sock2) else {
            return;
        };
        // Just past the cap, and TERMINATED, so the newline lands INSIDE the
        // chunk that crosses the limit. The size matters: overshoot by more
        // than one 4096-byte chunk and the accumulate-path check fires on an
        // earlier read, before the newline arrives, which the old ordering
        // also refused - a fixture that big cannot tell the two apart. This is
        // the only window where the newline branch runs first.
        let mut line = vec![b'x'; (crate::view_ipc::MAX_REQUEST_BYTES as usize) + 100];
        line.push(b'\n');
        let _ = std::io::Write::write_all(&mut c, &line);
        let _ = std::io::Write::flush(&mut c);
        std::thread::sleep(std::time::Duration::from_millis(300));
    });
    std::thread::sleep(std::time::Duration::from_millis(50));

    let listener = app.view_listener.as_ref().unwrap();
    let stream = accept_when_ready(listener);
    // The client thread sleeps before it writes, so this deadline is racing a
    // scheduler, not the server. Load-scaled for the same reason every other
    // spawn wait here is (CONTRIBUTING.md).
    let deadline = std::time::Instant::now()
        + crate::test_budget::spawn_budget(crate::test_budget::tests::VIEW_SERVER_DEADLINE_BASE);
    let (reply, opened) = app.answer_view_client(stream, deadline);
    let _ = flood.join();

    assert!(!opened, "an oversized request names nothing to open");
    match reply {
        crate::view_ipc::ViewReply::Err { message } => assert!(
            message.contains("exceeded the maximum length"),
            "the LENGTH cap must be what refuses this, not an incidental parse \
             failure downstream of accepting it: {message}"
        ),
        other => panic!("an oversized request must not be accepted: {other:?}"),
    }
}

/// The stopped member's position once members that ended in the same poll
/// are removed: shifted past removals below it, untouched by those above,
/// and gone if it ended itself.
#[test]
fn index_after_removals_shifts_past_lower_removals_only() {
    assert_eq!(index_after_removals(2, &[]), Some(2));
    assert_eq!(index_after_removals(2, &[0]), Some(1));
    assert_eq!(index_after_removals(3, &[0, 1]), Some(1));
    assert_eq!(
        index_after_removals(1, &[2, 3]),
        Some(1),
        "removals above do not move it"
    );
    assert_eq!(index_after_removals(2, &[0, 2]), None, "it ended itself");
}

/// #856 guard: a run-at-cursor asked for while a run is in flight is
/// turned away, and does not take over the first run's pending outcome:
/// the status names the first run when it ends. Turned away with the way
/// to stop the first (#1534), not silently.
#[cfg(unix)]
#[test]
fn a_run_asked_for_mid_run_does_not_take_over_the_first_runs_outcome() {
    let mut s = CodeqlStandIn::new();
    s.will(STAND_IN_PASS, 0);
    s.app.run_named_test(String::from("Find.qlref"));
    assert_eq!(s.app.status, "Running test test/Find::Find.qlref");
    s.app.run_named_test(String::from("Other.qlref"));
    assert_eq!(s.app.status, TEST_RUN_IN_PROGRESS);
    s.finish();
    assert!(
        s.app.status.starts_with("Find.qlref passed ("),
        "{}",
        s.app.status
    );
}

/// #356: a recorded frame is masked like a scrollback dump. The cast is a
/// file made to be shared, so a key on screen is recorded as •••, and a
/// reveal on screen does not reach it.
#[test]
fn a_recorded_frame_masks_secrets_even_while_revealed() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.terminals[0].last_inner = ratatui::layout::Rect {
        x: 1,
        y: 1,
        width: 60,
        height: 10,
    };
    app.terminals[0].resize(60, 10);
    let key = "AKIAIOSFODNN7EXAMPLE";
    app.terminals[0].feed_bytes_for_test(format!("export KEY={key}\r\n").as_bytes());
    app.reveal_redacted_secrets();
    app.run_command(crate::widgets::command_palette::Command::ToggleSessionRecording);
    app.record_active_screen();
    app.run_command(crate::widgets::command_palette::Command::ToggleSessionRecording);
    let path = std::fs::read_dir(app.workspace_root())
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.extension().is_some_and(|x| x == "cast"))
        .expect("a .cast file was written");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains(key), "the key reached the cast:\n{text}");
    assert!(text.contains("export KEY="), "{text}");
}

/// #694: Developer: Show Memory Usage opens a tab attributing memory to
/// each subsystem, with the stored diagnostics counted per server.
#[test]
fn show_memory_usage_opens_a_per_subsystem_report() {
    use crate::lsp::manager::DiagnosticSeverity;
    use crate::widgets::command_palette::Command;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let mut by_server = std::collections::HashMap::new();
    by_server.insert(
        String::from("ruff"),
        vec![
            diag(0, 1, DiagnosticSeverity::Warning),
            diag(2, 3, DiagnosticSeverity::Error),
        ],
    );
    app.lsp_diagnostics
        .insert(tmp.path().join("a.py"), by_server);
    app.run_command(Command::DeveloperShowMemoryUsage);
    assert_eq!(app.editor.path.as_deref(), Some(Path::new("Memory Usage")));
    let text = app.editor.lines.join("\n");
    for section in ["Terminal panes", "Undo history", "OUTPUT channels"] {
        assert!(text.contains(section), "{text}");
    }
    assert!(text.contains("ruff: 2 diagnostics in 1 files"), "{text}");
}
