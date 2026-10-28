//! Agent lanes, approvals, agent hooks, MCP, inline AI completion and the
//! resident pair navigator.

use super::*;

#[test]
fn mcp_success_opens_a_scratch_tab_and_tells_the_user_where() {
    // Regression: a successful MCP command rendered its output into a scratch
    // tab but the only feedback was a terse "— done", so the user couldn't find
    // the output. poll_mcp must open the tab AND name where it went.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    tx.send(crate::mcp::McpOutcome {
        title: String::from("Web: Fetch URL as Markdown"),
        body: Ok(String::from("# Example Domain\n\nbody text")),
    })
    .unwrap();
    app.mcp_rx = Some(rx);
    assert!(app.poll_mcp(), "a delivered outcome must request a redraw");
    let active_path = app
        .editor
        .path
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    assert!(
        active_path.contains("Web: Fetch URL as Markdown"),
        "the fetched markdown must be the active editor tab; active path was {active_path:?}"
    );
    assert!(
        app.status.to_lowercase().contains("tab"),
        "the status must tell the user the output opened in a tab; got: {}",
        app.status
    );
}

/// `diff_worktree_lane` refuses rather than diffing the wrong thing, and
/// each refusal names its own reason (#348).
///
/// The three arms are separate because they need separate answers: the
/// primary folder is not a lane at all, a lane pane may not be open for
/// this root, and a lane made before the base was recorded has nothing to
/// diff against. Collapsing them would tell the user to fix the wrong
/// thing.
#[test]
fn diffing_a_lane_refuses_when_there_is_no_base_to_diff_against() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();

    // The primary folder is not a lane.
    app.diff_worktree_lane();
    assert_eq!(app.graph_revspec, None, "no diff was set up");
    assert!(
        app.status.contains("not a lane"),
        "the primary folder says so: {}",
        app.status
    );

    // A non-primary root with no lane pane recorded for it.
    let lane_dir = tmp.path().join("lane");
    std::fs::create_dir_all(&lane_dir).unwrap();
    app.active_scm_root = lane_dir.clone();
    app.diff_worktree_lane();
    assert_eq!(app.graph_revspec, None);
    assert!(
        app.status.contains("No lane is open"),
        "an unrecorded root says so: {}",
        app.status
    );

    // A lane whose base was never recorded (made before #348, or cut from a
    // detached HEAD).
    app.lane_panes.insert(
        1,
        crate::terminal_session::LaneRecord {
            path: lane_dir.to_string_lossy().into_owned(),
            branch: String::from("agent/fix"),
            agent: None,
            base: None,
        },
    );
    app.diff_worktree_lane();
    assert_eq!(
        app.graph_revspec, None,
        "no base means no diff, not a diff against something else"
    );
    assert!(
        app.status.contains("no base branch"),
        "a baseless lane says so: {}",
        app.status
    );

    // With a base recorded, the range is the lane against what it was cut
    // from - the positive control that the refusals above are refusals and
    // not just an inert function.
    app.lane_panes.insert(
        1,
        crate::terminal_session::LaneRecord {
            path: lane_dir.to_string_lossy().into_owned(),
            branch: String::from("agent/fix"),
            agent: None,
            base: Some(String::from("main")),
        },
    );
    app.diff_worktree_lane();
    assert_eq!(
        app.graph_revspec,
        Some(String::from("main..agent/fix")),
        "the range is base..lane"
    );
}

/// The lane diff belongs to the root it was asked for, and does not outlive
/// it (#348).
///
/// `branch_history_range` answers an unresolvable range with an EMPTY vec
/// rather than an error, so a revspec carried into another repo - or kept
/// after the lane is gone - renders a repo with commits as having none,
/// with nothing on screen saying why.
#[test]
fn switching_roots_drops_the_lane_diff() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let other = tmp.path().join("other");
    std::fs::create_dir_all(&other).unwrap();

    app.graph_revspec = Some(String::from("main..agent/fix"));
    app.seed_active_scm(other);
    assert_eq!(
        app.graph_revspec, None,
        "a lane diff must not follow the user into another repository"
    );
}

/// Marking a file reviewed records a snapshot of the bytes it just hashed,
/// so the row can be diffed against what the user actually approved (#345).
///
/// The store is written only by croft's own saves; the agent-write path
/// records nothing. Adopting whatever snapshot was newest anchored the
/// review to an unrelated earlier save and then labelled it "(reviewed)".
#[test]
fn reviewing_anchors_to_the_content_it_baselined_not_an_older_save() {
    let tmp = tempfile::tempdir().unwrap();
    let hist = tempfile::tempdir().unwrap();
    let f = tmp.path().join("touched.rs");
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.history_root = hist.path().to_path_buf();

    // An OLD, unrelated save is already in the store - the trap: it is the
    // newest snapshot at review time but is not what the agent wrote.
    crate::history::record_in(&app.history_root, &f, b"an older user save\n", 1000).unwrap();

    // The agent writes; the user never saved this content in croft.
    std::fs::write(&f, "what the agent wrote\n").unwrap();
    let working = vec![String::from("claude")];
    app.agent_ledger.record_write(
        &f,
        crate::agent_lane::content_hash(b"what the agent wrote\n"),
        &working,
    );

    assert!(app.mark_agent_file_reviewed("claude", &f));
    let millis = app.agent_ledger.lane("claude")[0]
        .reviewed_millis
        .expect("the review recorded an anchor");
    let snap = crate::history::snapshot_file_in(&app.history_root, &f, millis)
        .expect("the anchor names a snapshot that exists");
    assert_eq!(
        std::fs::read(&snap).unwrap(),
        b"what the agent wrote\n",
        "the anchor must hold the reviewed content, not the older save"
    );
    assert_ne!(
        millis, 1000,
        "it must not adopt the unrelated older snapshot"
    );

    // And the diff opens against that content rather than refusing.
    std::fs::write(&f, "what the agent wrote next\n").unwrap();
    app.diff_agent_lane_row("claude", &f);
    assert!(
        app.status.contains("since you last reviewed"),
        "the diff opens against the reviewed baseline: {}",
        app.status
    );
}

#[test]
fn a_local_launch_still_brings_back_an_agent_lane_and_focuses_it() {
    // A lane is more than a shell: its worktree and agent are still there,
    // and seating the agent again is the point of restoring it (#348). So
    // a local launch drops the plain pane beside it but keeps the lane, and
    // focus follows the lane that had it.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    init_lane_repo(&repo);
    let session = tmp.path().join("sessions.json");
    let lane_dir = tmp.path().join("repo-fix-login");
    std::fs::create_dir_all(&lane_dir).unwrap();
    let root_key = repo.display().to_string();
    let rec = crate::terminal_session::SessionRecord {
        panes: vec![
            crate::terminal_session::PaneRecord {
                host: None,
                cwd: root_key.clone(),
                name: Some(String::from("plain")),
                transcript: Vec::new(),
                lane: None,
            },
            crate::terminal_session::PaneRecord {
                host: None,
                cwd: lane_dir.display().to_string(),
                name: Some(String::from("Lane: fix-login")),
                transcript: Vec::new(),
                lane: Some(crate::terminal_session::LaneRecord {
                    path: lane_dir.display().to_string(),
                    branch: String::from("agent/fix-login"),
                    agent: None,
                    base: None,
                }),
            },
        ],
        active: 1,
    };
    crate::terminal_session::save_for_root(&session, &root_key, rec).unwrap();
    let mut app = App::new(repo.clone()).unwrap();
    app.terminal_session_path = session;
    app.restore_terminal_session_with(false);
    assert_eq!(app.terminals.len(), 1, "only the lane comes back");
    assert_eq!(app.terminals[0].manual_name(), Some("Lane: fix-login"));
    assert_eq!(
        app.lane_panes
            .get(&app.terminals[0].uid())
            .map(|l| l.branch.as_str()),
        Some("agent/fix-login")
    );
    assert_eq!(app.active_terminal, 0, "focus is on the lane");
}

#[test]
fn a_local_launch_drops_a_lane_whose_worktree_is_gone() {
    // A lane whose worktree was removed is an ordinary pane now, and an
    // ordinary pane does not come back locally: the default terminal stays.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    init_lane_repo(&repo);
    let session = tmp.path().join("sessions.json");
    let root_key = repo.display().to_string();
    let rec = crate::terminal_session::SessionRecord {
        panes: vec![crate::terminal_session::PaneRecord {
            host: None,
            cwd: root_key.clone(),
            name: Some(String::from("Lane: gone")),
            transcript: Vec::new(),
            lane: Some(crate::terminal_session::LaneRecord {
                path: tmp.path().join("repo-gone").display().to_string(),
                branch: String::from("agent/gone"),
                agent: None,
                base: None,
            }),
        }],
        active: 0,
    };
    crate::terminal_session::save_for_root(&session, &root_key, rec).unwrap();
    let mut app = App::new(repo.clone()).unwrap();
    app.terminal_session_path = session;
    app.restore_terminal_session_with(false);
    assert_eq!(app.terminals.len(), 1);
    assert_eq!(app.terminals[0].manual_name(), None);
    assert!(app.lane_panes.is_empty());
}

#[test]
fn participant_action_ids_round_trip() {
    assert_eq!(
        parse_participant_action("grant:3"),
        Some((ParticipantVerb::Grant, 3))
    );
    assert_eq!(
        parse_participant_action("revoke:0"),
        Some((ParticipantVerb::Revoke, 0))
    );
    assert_eq!(
        parse_participant_action("kick:12"),
        Some((ParticipantVerb::Kick, 12))
    );
    assert_eq!(parse_participant_action("grant:"), None);
    assert_eq!(parse_participant_action("promote:1"), None);
    assert_eq!(parse_participant_action("grant"), None);
}

#[test]
fn participant_rows_show_role_and_size() {
    let p = crate::session_host::Participant {
        id: 7,
        name: String::from("vitali@mac"),
        cols: 120,
        rows: 40,
        control: true,
        version: String::new(),
    };
    let row = participant_row(&p);
    assert_eq!(row.id, "7");
    assert!(row.label.contains("vitali@mac"));
    assert!(row.label.contains("120\u{d7}40"));
    assert!(row.label.contains("write"));
    // Read-only guest with an unknown size (0x0 under `script`).
    let p = crate::session_host::Participant {
        id: 8,
        name: String::from("guest@box"),
        cols: 0,
        rows: 0,
        control: false,
        version: String::new(),
    };
    let row = participant_row(&p);
    assert!(row.label.contains("read-only"));
    assert!(!row.label.contains("0\u{d7}0"));
}

/// The cancel affordances around the AI stream: Cmd+K X and the palette
/// entry both route through `collab_cancel_stream` (hint when idle), and
/// the gutter stop button's row follows the pilot's caret in the streamed
/// file, falling back to the top visible row.
#[test]
fn cmd_k_x_cancels_the_ai_stream_and_the_stop_row_follows_the_pilot_caret() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("f.txt");
    std::fs::write(&file, "l0\nl1\nl2\nl3\nl4\nl5\nl6").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.open_at(&file, 0, 0).unwrap();

    // The chord is always consumed; idle it hints instead of broadcasting.
    assert!(app.handle_cmd_k_chord(key(KeyCode::Char('x'), KeyModifiers::NONE)));
    assert!(
        app.status.to_lowercase().contains("no ai stream"),
        "idle chord hints, got: {}",
        app.status
    );

    // No stream: no stop row.
    assert_eq!(app.stream_stop_row(), None);

    // A stream into the active file with no pilot caret yet: the top
    // visible row wears the button.
    app.collab_stream = Some(CollabStreamInfo {
        file: "f.txt".into(),
        name: "pilot".into(),
        site: 7,
        undo_group: 0,
    });
    assert_eq!(app.stream_stop_row(), Some(app.editor.scroll));

    // The pilot's ghost caret pins the row.
    app.collab_carets.insert(
        7,
        CollabCaret {
            file: "f.txt".into(),
            row: 5,
            col: 2,
            name: "pilot".into(),
            last_moved: std::time::Instant::now(),
        },
    );
    assert_eq!(app.stream_stop_row(), Some(5));

    // A stream into a file that is not active here shows no button.
    app.collab_stream.as_mut().unwrap().file = "other.txt".into();
    assert_eq!(app.stream_stop_row(), None);

    // The palette entry drives the same action.
    app.status.clear();
    app.run_command(crate::widgets::command_palette::Command::CollabCancelStream);
    assert!(
        app.status.to_lowercase().contains("no ai stream"),
        "palette cancel hints when idle, got: {}",
        app.status
    );
}

/// The resident navigator: an enabled pair record seats the pilot within a
/// throttled tick, a plain-launch croft self-appoints the collab OWNER seat
/// (nobody else would answer the pilot's snapshot requests), and a disabled
/// record unseats it.
#[test]
fn navigator_record_seats_the_pilot_and_self_appoints_owner() {
    use std::time::{Duration, Instant};
    if !crate::lsp::manager::is_on_path("python3") {
        eprintln!("SKIPPED: python3 not on PATH");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("f.txt"), "l0\nl1\nl2").unwrap();
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
    let script = tmp.path().join("fake_claude.py");
    std::fs::write(&script, crate::pair::FAKE_CLAUDE).unwrap();
    let log = tmp.path().join("stdin.log");

    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.pair_record_path = tmp.path().join("x.pair.json");
    app.pair_socket = socket.clone();
    app.pair_host_lock_path = tmp.path().join("x.pair-host.lock");
    let (script2, log2, socket2) = (script.clone(), log.clone(), socket.clone());
    app.pair_spawn_override = Some(Box::new(move |cfg| {
        let mut cmd = std::process::Command::new("python3");
        cmd.arg(&script2).arg(&log2).arg("notes");
        crate::pair_host::PairHost::spawn_cmd(&socket2, &cfg.name, cfg.task.as_deref(), cmd)
    }));

    // No record yet: nothing seats.
    assert!(!app.maybe_seat_navigator());
    assert!(app.pair_host.is_none());

    crate::session::write_pair_record(
        &app.pair_record_path,
        &crate::session::PairRecord {
            model: None,
            name: "navigator".into(),
            enabled: true,
            task: None,
            provider: None,
            base_url: None,
        },
    )
    .unwrap();
    assert!(app.collab_config.is_none(), "plain launch precondition");
    app.last_pair_check = None;
    assert!(app.maybe_seat_navigator());
    assert!(app.pair_host.is_some(), "pilot must seat");
    assert_eq!(
        app.collab_config,
        Some((socket.clone(), crate::collab::CollabRole::Owner)),
        "plain launch self-appoints the owner seat"
    );
    assert!(
        app.status.to_lowercase().contains("seated"),
        "got: {}",
        app.status
    );
    // The self-appointed seat actually connects.
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.collab.is_none() {
        assert!(Instant::now() < deadline, "owner seat never connected");
        app.poll_collab();
        std::thread::sleep(Duration::from_millis(5));
    }

    // A real parked caret: an ask turn on f.txt (served by this window's
    // owner seat) bootstraps the pilot's replica and parks its caret at the
    // asked row. The seat's identity is its site ids, never its name.
    app.pair_host
        .as_ref()
        .unwrap()
        .send_ask_turn("f.txt", (1, 1), "", "look here", "l0\nl1\nl2")
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let pilot_sites = loop {
        app.poll_collab();
        let sites = app.pair_host.as_ref().unwrap().caret_sites();
        if sites.iter().any(|s| app.collab_carets.contains_key(s)) {
            break sites;
        }
        assert!(
            Instant::now() < deadline,
            "the pilot's parked caret never reached the app"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    // A human who picked the very same display name is not the navigator.
    app.collab_carets.insert(
        9999,
        CollabCaret {
            file: "f.txt".into(),
            row: 0,
            col: 0,
            name: "navigator".into(),
            last_moved: Instant::now(),
        },
    );

    // Disabled record: the host unseats on the next check.
    crate::session::write_pair_record(
        &app.pair_record_path,
        &crate::session::PairRecord {
            model: None,
            name: "navigator".into(),
            enabled: false,
            task: None,
            provider: None,
            base_url: None,
        },
    )
    .unwrap();
    app.last_pair_check = None;
    assert!(app.maybe_seat_navigator());
    assert!(app.pair_host.is_none(), "pilot must unseat");
    assert!(
        !pilot_sites
            .iter()
            .any(|s| app.collab_carets.contains_key(s)),
        "the unseated navigator's caret must not linger"
    );
    assert!(
        app.collab_carets.contains_key(&9999),
        "a human named like the navigator keeps their caret"
    );
}

/// The navigator's caret wears its fixed identity accent (the same orange
/// as its comment boxes); human collaborators keep the join-order palette.
/// Identity is the seat's site ids, never its display name: names are
/// neither unique (a human may pick the same one) nor length-stable (caret
/// names truncate to 24 chars at ingest).
#[test]
fn navigator_caret_wears_its_accent_and_humans_keep_the_palette() {
    assert_eq!(
        collab_caret_color(&[3, 7], 3),
        crate::widgets::editor::NAVIGATOR_ACCENT
    );
    assert_eq!(collab_caret_color(&[3, 7], 5), participant_color(5));
    assert_eq!(collab_caret_color(&[], 3), participant_color(3));
}

/// A record activated with `--provider ollama` seats the pilot with the
/// Local provider and its recorded endpoint, not the claude default.
#[test]
fn app_seats_local_provider_from_record() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = tmp.path().join("collab.sock");
    type SeenConfig = Option<(crate::pair::Provider, Option<String>)>;
    let seen: std::rc::Rc<std::cell::RefCell<SeenConfig>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.pair_record_path = tmp.path().join("x.pair.json");
    app.pair_socket = socket.clone();
    app.pair_host_lock_path = tmp.path().join("x.pair-host.lock");
    // Already the owner (skip the relay bootstrap): the test exercises the
    // provider mapping, not collab connect.
    app.collab_config = Some((socket, crate::collab::CollabRole::Owner));
    let seen2 = seen.clone();
    app.pair_spawn_override = Some(Box::new(move |cfg| {
        *seen2.borrow_mut() = Some((cfg.provider.clone(), cfg.model.clone()));
        anyhow::bail!("captured the config; no seat needed")
    }));
    crate::session::write_pair_record(
        &app.pair_record_path,
        &crate::session::PairRecord {
            model: Some("qwen3-coder:30b".into()),
            name: "navigator".into(),
            enabled: true,
            task: None,
            provider: Some("ollama".into()),
            base_url: Some("http://localhost:11500".into()),
        },
    )
    .unwrap();
    app.last_pair_check = None;
    assert!(app.maybe_seat_navigator());
    assert_eq!(
        *seen.borrow(),
        Some((
            crate::pair::Provider::Local {
                base_url: "http://localhost:11500".into()
            },
            Some(String::from("qwen3-coder:30b")),
        )),
        "the recorded provider and model must reach the seat config"
    );
}

/// A dead pilot is not respawned every second: after a death (or a failed
/// seat) the navigator stays down until the record is deactivated and
/// re-enabled, matching the "re-activate to reseat" contract and the LSP
/// no-auto-respawn precedent.
#[test]
fn a_dead_navigator_is_not_respawned_until_reactivated() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = tmp.path().join("collab.sock");
    let seats = std::rc::Rc::new(std::cell::Cell::new(0usize));
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.pair_record_path = tmp.path().join("x.pair.json");
    app.pair_socket = socket.clone();
    app.pair_host_lock_path = tmp.path().join("x.pair-host.lock");
    // Already the owner (skip the relay bootstrap): the test exercises the
    // death latch, not collab connect.
    app.collab_config = Some((socket, crate::collab::CollabRole::Owner));
    let seats2 = seats.clone();
    app.pair_spawn_override = Some(Box::new(move |_cfg| {
        seats2.set(seats2.get() + 1);
        anyhow::bail!("scripted seat failure");
    }));

    let enabled = |a: &mut App| {
        crate::session::write_pair_record(
            &a.pair_record_path,
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
        a.last_pair_check = None;
    };

    // First tick: it tries once, fails, and latches down.
    enabled(&mut app);
    app.maybe_seat_navigator();
    assert_eq!(seats.get(), 1, "one seat attempt");
    assert!(
        app.navigator_down,
        "a failed seat latches the navigator down"
    );

    // Many more ticks with the record still enabled: NO respawn storm.
    for _ in 0..5 {
        app.last_pair_check = None;
        app.maybe_seat_navigator();
    }
    assert_eq!(seats.get(), 1, "a latched navigator must not be respawned");

    // Deactivate, then re-enable: the latch clears and it tries again.
    crate::session::write_pair_record(
        &app.pair_record_path,
        &crate::session::PairRecord {
            model: None,
            name: "nav".into(),
            enabled: false,
            task: None,
            provider: None,
            base_url: None,
        },
    )
    .unwrap();
    app.last_pair_check = None;
    app.maybe_seat_navigator();
    assert!(!app.navigator_down, "deactivation clears the latch");
    enabled(&mut app);
    app.maybe_seat_navigator();
    assert_eq!(seats.get(), 2, "re-activation retries the seat");
}

/// The palette off/on round trip must keep the seat's backend: writing
/// `provider: None` silently reseated a local-only ollama navigator on the
/// cloud claude CLI - shipping the workspace to a remote service the user
/// explicitly avoided, under a model name claude does not have.
#[test]
fn toggling_the_navigator_preserves_its_provider() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.pair_record_path = tmp.path().join("pair.json");
    crate::session::write_pair_record(
        &app.pair_record_path,
        &crate::session::PairRecord {
            model: Some(String::from("qwen3-coder:30b")),
            name: String::from("navigator"),
            enabled: true,
            task: None,
            provider: Some(String::from("ollama")),
            base_url: Some(String::from("http://localhost:11434")),
        },
    )
    .unwrap();
    app.toggle_navigator(); // off
    let mid = crate::session::read_pair_record(&app.pair_record_path).unwrap();
    assert!(!mid.enabled, "the first toggle really deactivates");
    assert_eq!(mid.name, "navigator");
    assert_eq!(mid.provider.as_deref(), Some("ollama"));
    assert_eq!(mid.base_url.as_deref(), Some("http://localhost:11434"));
    assert_eq!(mid.model.as_deref(), Some("qwen3-coder:30b"));
    app.toggle_navigator(); // back on
    let r = crate::session::read_pair_record(&app.pair_record_path).unwrap();
    assert!(r.enabled);
    assert_eq!(r.provider.as_deref(), Some("ollama"));
    assert_eq!(r.base_url.as_deref(), Some("http://localhost:11434"));
    assert_eq!(r.model.as_deref(), Some("qwen3-coder:30b"));
    assert_eq!(r.name, "navigator", "the seat keeps its name too");
}

/// Only one croft self-appoints the navigator owner per workspace: a second
/// plain croft is refused the single-host lock and must NOT seat (two owners
/// would both claim collab site id 1 and corrupt the shared buffer).
#[test]
fn only_one_croft_self_appoints_the_navigator_owner() {
    use std::time::{Duration, Instant};
    let tmp = tempfile::tempdir().unwrap();
    let socket = tmp.path().join("collab.sock");
    let lock_path = tmp.path().join("host.lock");
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
    let enable = |a: &mut App| {
        crate::session::write_pair_record(
            &a.pair_record_path,
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
        a.last_pair_check = None;
    };

    // Croft A grabs the single-host lock, self-appoints, and HOSTS (a
    // local-provider seat spawns no child): the lock is only kept by a
    // window that actually holds the seat.
    let mut a = App::new(tmp.path().to_path_buf()).unwrap();
    a.pair_record_path = tmp.path().join("a.pair.json");
    a.pair_socket = socket.clone();
    a.pair_host_lock_path = lock_path.clone();
    a.pair_spawn_override = Some(Box::new(local_test_spawn));
    enable(&mut a);
    a.maybe_seat_navigator();
    assert!(a.pair_host.is_some(), "A must host: {}", a.status);
    assert!(
        a.pair_host_lock.is_some(),
        "A must hold the single-host lock"
    );

    // Croft B, same workspace: with a live relay, WITHOUT the lock gate it
    // would self-appoint and reach the spawn (panic). The gate must refuse it.
    let mut b = App::new(tmp.path().to_path_buf()).unwrap();
    b.pair_record_path = tmp.path().join("b.pair.json");
    b.pair_socket = socket.clone();
    b.pair_host_lock_path = lock_path.clone();
    b.pair_spawn_override = Some(Box::new(|_| panic!("a second croft must never host")));
    enable(&mut b);
    b.maybe_seat_navigator();
    assert!(b.pair_host.is_none(), "B must not host");
    assert!(b.pair_host_lock.is_none(), "B must not hold the lock");
    assert!(
        b.status.to_lowercase().contains("another croft"),
        "B must say another croft hosts it, got: {}",
        b.status
    );

    // The refusal is announced ONCE. B keeps silently polling for takeover,
    // but re-announcing every second clobbered its status line forever and
    // dirtied a frame at 1 Hz.
    b.status = String::from("Saved src/foo.rs");
    b.last_pair_check = None;
    let redraw = b.maybe_seat_navigator();
    assert!(!redraw, "an unchanged refusal must not dirty the frame");
    assert_eq!(
        b.status, "Saved src/foo.rs",
        "the refusal must not clobber the status line again"
    );
}

/// Unseating the navigator mid-turn must reset the turn's note counters:
/// they survived into the NEXT seat's first TurnDone, which then reported
/// a dead seat's notes (inflated count, wrong file).
#[test]
fn unseating_the_navigator_resets_the_turn_note_counters() {
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
    let mut record = crate::session::PairRecord {
        model: None,
        name: "nav".into(),
        enabled: true,
        task: None,
        provider: None,
        base_url: None,
    };
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.pair_record_path = tmp.path().join("x.pair.json");
    app.pair_socket = socket.clone();
    app.pair_host_lock_path = tmp.path().join("x.host.lock");
    app.pair_spawn_override = Some(Box::new(local_test_spawn));
    crate::session::write_pair_record(&app.pair_record_path, &record).unwrap();
    app.last_pair_check = None;
    app.maybe_seat_navigator();
    assert!(app.pair_host.is_some(), "seated: {}", app.status);

    // Mid-turn state: the navigator commented three times in a.rs.
    app.pair_notes_this_turn = 3;
    app.pair_last_noted_file = Some(String::from("a.rs"));
    app.pair_noted_files.insert(String::from("a.rs"));

    record.enabled = false;
    crate::session::write_pair_record(&app.pair_record_path, &record).unwrap();
    app.last_pair_check = None;
    app.maybe_seat_navigator();
    assert!(app.pair_host.is_none(), "unseated");
    assert_eq!(
        app.pair_notes_this_turn, 0,
        "a dead seat's note count must not leak into the next seat's TurnDone"
    );
    assert!(
        app.pair_last_noted_file.is_none(),
        "a dead seat's noted file must not leak either"
    );
    assert!(app.pair_noted_files.is_empty());
}

/// Re-running `croft pair` after a failed seat must retry: the CLI promises
/// "a running croft seats it within a second", but an enabled→enabled
/// rewrite has no transition for the death latch to observe. The record's
/// mtime is the signal.
#[test]
fn a_record_rewrite_re_arms_a_downed_navigator() {
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
    let record = crate::session::PairRecord {
        model: None,
        name: "nav".into(),
        enabled: true,
        task: None,
        provider: None,
        base_url: None,
    };
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.pair_record_path = tmp.path().join("x.pair.json");
    app.pair_socket = socket.clone();
    app.pair_host_lock_path = tmp.path().join("x.host.lock");
    app.pair_spawn_override = Some(Box::new(|_| anyhow::bail!("claude is broken")));
    crate::session::write_pair_record(&app.pair_record_path, &record).unwrap();
    app.last_pair_check = None;
    app.maybe_seat_navigator();
    assert!(app.navigator_down, "the first seat fails and latches");

    // Latched: an unchanged record must NOT retry every second.
    app.last_pair_check = None;
    app.maybe_seat_navigator();
    assert!(app.pair_host.is_none());

    // The user fixes the backend and re-runs `croft pair`: the rewritten
    // record re-arms seating — even when the rewrite lands within the same
    // mtime grain (coarse-timestamp filesystems): pin the mtime to the old
    // value so only the CONTENT can carry the signal (every write stamps
    // the record, so an enabled→enabled rewrite is never byte-identical).
    let old_mtime = std::fs::metadata(&app.pair_record_path)
        .unwrap()
        .modified()
        .unwrap();
    crate::session::write_pair_record(&app.pair_record_path, &record).unwrap();
    let f = std::fs::File::options()
        .write(true)
        .open(&app.pair_record_path)
        .unwrap();
    f.set_modified(old_mtime).unwrap();
    app.pair_spawn_override = Some(Box::new(local_test_spawn));
    // Poll like the app's once-a-second tick: a child forked by a parallel
    // test can share the host lock's file until its exec closes it, which
    // reads as Busy for that one tick.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        app.last_pair_check = None;
        app.maybe_seat_navigator();
        if app.pair_host.is_some() || Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        app.pair_host.is_some(),
        "a rewritten record must re-arm the downed navigator: {}",
        app.status
    );
}

/// The ask box: Cmd+K Q opens the navigator input scoped to the caret line
/// (or the selection), right-click menus carry the entries, and submitting
/// sends the composed ask turn (instruction, 0-based range, numbered
/// buffer) to the pilot's claude stdin.
#[test]
fn ask_box_opens_from_chord_and_menus_and_sends_the_ask_turn() {
    use crate::widgets::input_prompt::InputPurpose;
    use crossterm::event::{MouseButton, MouseEventKind};
    use std::time::{Duration, Instant};
    if !crate::lsp::manager::is_on_path("python3") {
        eprintln!("SKIPPED: python3 not on PATH");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("f.txt");
    std::fs::write(&file, "a\nb\nc\nd\ne").unwrap();
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
    let script = tmp.path().join("fake_claude.py");
    std::fs::write(&script, crate::pair::FAKE_CLAUDE).unwrap();
    let log = tmp.path().join("stdin.log");

    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.open_file_at_launch(&file);
    let mut cmd = std::process::Command::new("python3");
    cmd.arg(&script).arg(&log).arg("notes");
    app.pair_host =
        Some(crate::pair_host::PairHost::spawn_cmd(&socket, "navigator", None, cmd).unwrap());

    // Cmd+K Q on the caret line (no selection): single-line scope, 1-based
    // title, Esc closes through the shared input-prompt path.
    app.editor.cursor_row = 2;
    assert!(app.handle_cmd_k_chord(key(KeyCode::Char('q'), KeyModifiers::NONE)));
    {
        let p = app.input_prompt.as_ref().expect("ask box opens");
        assert!(
            matches!(&p.purpose, InputPurpose::AskNavigator { range: (2, 2), .. }),
            "caret-line scope"
        );
        assert!(p.title.contains("line 3"), "title: {}", p.title);
    }
    app.handle_input_prompt_key(key(KeyCode::Esc, KeyModifiers::NONE));
    assert!(app.input_prompt.is_none());

    // With a selection the chord scopes to the selected lines; submitting
    // sends the composed turn to the pilot.
    app.editor.selection = Some(crate::widgets::editor::EditorSelection {
        anchor: (1, 0),
        head: (3, 1),
    });
    assert!(app.handle_cmd_k_chord(key(KeyCode::Char('q'), KeyModifiers::NONE)));
    assert!(matches!(
        &app.input_prompt.as_ref().unwrap().purpose,
        InputPurpose::AskNavigator { range: (1, 3), .. }
    ));
    for c in "tighten this".chars() {
        app.handle_input_prompt_key(key(KeyCode::Char(c), KeyModifiers::NONE));
    }
    app.handle_input_prompt_key(key(KeyCode::Enter, KeyModifiers::NONE));
    assert!(app.input_prompt.is_none(), "submit closes the box");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let sent = std::fs::read_to_string(&log).unwrap_or_default();
        if sent.contains("tighten this") && sent.contains("lines 1-3") && sent.contains("2|c") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "composed ask never reached claude; log: {sent}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    // The gutter right-click menu offers the ask entry; the body menu adds
    // the selection-scoped one while a selection is active.
    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|frame| app.render(frame)).unwrap();
    let gutter_col = app.editor.last_inner.x;
    let row = app.editor.last_inner.y + 1;
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Right),
        gutter_col,
        row,
    ));
    let labels = menu_labels(&app.context_menu.as_ref().unwrap().items);
    assert!(labels.contains(&"Ask Navigator"), "gutter menu: {labels:?}");
    app.context_menu = None;
    app.editor.selection = Some(crate::widgets::editor::EditorSelection {
        anchor: (0, 0),
        head: (2, 1),
    });
    let text_x = app.editor.last_inner.x + app.editor.last_gutter_width + 2;
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Right), text_x, row));
    let labels = menu_labels(&app.context_menu.as_ref().unwrap().items);
    assert!(
        labels.contains(&"Ask Navigator About Selection"),
        "body menu: {labels:?}"
    );
}

/// Re-rooting the workspace (Cmd+Z jump / Make Root) rebinds the resident
/// navigator: the activation record, relay socket, and single-host lock all
/// key off the root, so a `croft pair` run in the NEW root must be seen by
/// this window. The old workspace's seat (and its comment boxes) unseats.
#[test]
fn reroot_rebinds_the_navigator_activation_watch() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    std::fs::write(a.path().join("a.txt"), "a").unwrap();
    std::fs::write(b.path().join("b.txt"), "b").unwrap();
    let mut app = App::new(a.path().to_path_buf()).unwrap();
    assert_eq!(
        app.pair_record_path,
        crate::session::pair_record_path(a.path())
    );

    // A seat and its boxes from root A are torn down by the move.
    app.navigator_notes
        .insert("a.txt".into(), vec![(1, 0, "n".into())]);
    app.editor.comment_focus = Some(crate::widgets::editor::CommentFocus {
        id: 1,
        reply: String::new(),
        cursor: 0,
    });
    app.navigator_down = true; // a dead pilot in A must not block B
    app.last_pair_check = Some(std::time::Instant::now());

    app.change_workspace_root(b.path().to_path_buf());

    assert_eq!(
        app.pair_record_path,
        crate::session::pair_record_path(b.path()),
        "the activation watch follows the root"
    );
    assert_eq!(
        app.pair_socket,
        crate::session::collab_socket_path(b.path())
    );
    assert_eq!(
        app.pair_host_lock_path,
        crate::session::pair_host_lock_path(b.path())
    );
    assert!(app.navigator_notes.is_empty(), "old root's boxes are gone");
    assert!(app.editor.comment_focus.is_none());
    assert!(
        !app.navigator_down,
        "the death latch belongs to the old root"
    );
    assert!(
        app.last_pair_check.is_none(),
        "the new record is checked on the next tick, not in 1s"
    );
}

/// The proactive trigger: after the navigator's first look at a file, a
/// newly COMPLETED construct plus a typing pause makes it take a
/// comment-only look on its own. A pause alone never fires, the pref kills
/// the behavior, and one buffer state never fires twice.
#[test]
fn a_completed_construct_plus_pause_triggers_a_proactive_look() {
    use std::time::{Duration, Instant};
    if !crate::lsp::manager::is_on_path("python3") {
        eprintln!("SKIPPED: python3 not on PATH");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("f.rs");
    std::fs::write(&file, "fn alpha() {}\n").unwrap();
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
    let script = tmp.path().join("fake_claude.py");
    std::fs::write(&script, crate::pair::FAKE_CLAUDE).unwrap();
    let log = tmp.path().join("stdin.log");

    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.open_file_at_launch(&file);
    // Own the workspace's collab session so the pilot's bootstraps answer.
    app.collab_config = Some((socket.clone(), crate::collab::CollabRole::Owner));
    app.last_collab_connect = None;
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.collab.is_none() {
        assert!(Instant::now() < deadline, "owner seat never connected");
        app.poll_collab();
        std::thread::sleep(Duration::from_millis(5));
    }
    let mut cmd = std::process::Command::new("python3");
    cmd.arg(&script).arg(&log).arg("notes");
    app.pair_host = Some(crate::pair_host::PairHost::spawn_cmd(&socket, "nav", None, cmd).unwrap());

    // The navigator's first look: a manual yield; wait the turn out.
    app.yield_to_navigator();
    let deadline = Instant::now() + Duration::from_secs(15);
    while app.pair_host.as_ref().unwrap().is_busy() {
        assert!(Instant::now() < deadline, "the first look never finished");
        app.poll_collab();
        app.poll_pair();
        std::thread::sleep(Duration::from_millis(10));
    }

    // A pause with no new construct never fires.
    app.editor.last_edit_at = Some(Instant::now() - Duration::from_secs(3));
    app.editor.edit_seq += 1;
    assert!(!app.tick_proactive_navigator(), "no new construct: no look");

    // Complete a new function, pause — but the pref is off: no fire.
    let beta_row = app.editor.lines.len(); // where the new fn will sit
    app.editor.lines.push("fn beta() {}".into());
    app.editor.edit_seq += 1;
    app.editor.last_edit_at = Some(Instant::now() - Duration::from_secs(3));
    app.proactive_navigator_enabled = false;
    assert!(
        !app.tick_proactive_navigator(),
        "the pref must gate the look"
    );

    // Pref back on but the user is mid-typing in a prompt (the ask box):
    // stealing the seat now would make their Enter fail against a busy
    // host and lose the typed draft. An open prompt suppresses the look
    // without consuming the scan.
    app.proactive_navigator_enabled = true;
    app.open_ask_navigator((0, 0), String::new());
    assert!(app.input_prompt.is_some(), "the ask box must open");
    assert!(
        !app.tick_proactive_navigator(),
        "an open prompt must suppress the proactive look"
    );
    app.close_input_prompt();

    // Prompt closed: the completed construct fires a comment-only look
    // anchored at the new function's row.
    assert!(app.tick_proactive_navigator(), "new fn + pause must fire");
    assert_eq!(
        app.pair_turn_origin,
        Some((String::from("f.rs"), beta_row)),
        "the look anchors at the completed construct"
    );
    assert!(
        app.pair_host.as_ref().unwrap().is_busy(),
        "the proactive look is a real turn"
    );

    // The same buffer state never fires twice (latch + busy host).
    assert!(!app.tick_proactive_navigator());
}

fn set_stream(owner: &mut App, pilot: &mut crate::collab::CollabSession, active: bool) {
    use std::time::{Duration, Instant};
    pilot.send_stream_state("f.txt", active);
    let deadline = Instant::now() + Duration::from_secs(5);
    while owner.collab_stream.is_some() != active {
        assert!(Instant::now() < deadline, "the stream state never arrived");
        owner.poll_collab();
        std::thread::sleep(Duration::from_millis(5));
    }
}

const ADD: &str = "def add(a, b):\n    return a - b";

/// #1207: a navigator edit streamed over many ticks undoes in one Ctrl+Z,
/// back to the text from before the stream, instead of one fragment at a
/// time through broken code.
#[test]
fn a_navigator_stream_undoes_in_one_step() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut owner, mut pilot) = owner_and_pilot_on(&tmp, ADD);
    set_stream(&mut owner, &mut pilot, true);
    let mut text = String::from("def add(a, b):\n");
    for frag in [
        "    if not isins",
        "tance(a, (int, float)):\n",
        "        raise TypeError(\"a mus",
        "t be a number\")\n",
        "    return a + b",
    ] {
        text.push_str(frag);
        pilot_writes(&mut owner, &mut pilot, &text);
    }
    set_stream(&mut owner, &mut pilot, false);
    assert!(owner.editor.undo());
    assert_eq!(
        owner.editor.lines.join("\n"),
        ADD,
        "one undo rejects the edit"
    );
    assert!(!owner.editor.undo(), "nothing else to undo");
}

/// #1207 (comment): after a cancelled stream's revert, the text is back and
/// there is nothing left to undo, never a hybrid of the two texts.
#[test]
fn a_cancelled_navigator_stream_leaves_nothing_to_undo() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut owner, mut pilot) = owner_and_pilot_on(&tmp, ADD);
    set_stream(&mut owner, &mut pilot, true);
    pilot_writes(
        &mut owner,
        &mut pilot,
        "def add(a, b):\n    \"\"\"Return the sum of a and b.\"\"\"\n    raise TypeError(\"a mus",
    );
    pilot_writes(&mut owner, &mut pilot, ADD);
    set_stream(&mut owner, &mut pilot, false);
    assert_eq!(owner.editor.lines.join("\n"), ADD);
    assert!(
        !owner.editor.undo(),
        "the cancelled stream left no undo step"
    );
    assert_eq!(owner.editor.lines.join("\n"), ADD);
}

/// The window keeps the NEWEST lines. An inverted slice would still be
/// bounded and still look right in a small test, so pin the direction.
#[test]
fn the_transcript_window_keeps_the_newest_lines() {
    let tmp = tempfile::tempdir().unwrap();
    let session_path = tmp.path().join("sessions.json");
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.terminal_session_path = session_path.clone();
    app.split_terminal().unwrap();
    app.terminals[0].resize(80, 24);

    let n = crate::terminal_session::TRANSCRIPT_LINES + 50;
    for i in 1..=n {
        app.terminals[0].feed_bytes_for_test(format!("line-{i}\r\n").as_bytes());
    }
    app.save_terminal_session();

    // The store is keyed by workspace root; read it back through the module's
    // own loader rather than reparsing its shape here.
    let store = crate::terminal_session::load(&session_path);
    let rec = store
        .values()
        .next()
        .expect("a session record for this workspace");

    let t = &rec.panes[0].transcript;
    assert!(
        t.len() <= crate::terminal_session::TRANSCRIPT_LINES,
        "bounded to the window: {}",
        t.len()
    );
    let joined = t.join("\n");
    assert!(
        joined.contains(&format!("line-{n}")),
        "the newest line survives"
    );
    assert!(
        !joined.contains("line-1\n") && !joined.ends_with("line-1"),
        "the oldest line was dropped, not the newest"
    );
}

/// The transcript is plain text by design: a replay must not be able to
/// re-run a cursor move or a screen clear that the original output carried.
#[test]
fn a_replayed_transcript_cannot_execute_control_sequences() {
    let tmp = tempfile::tempdir().unwrap();
    let app = App::new(tmp.path().to_path_buf()).unwrap();
    // A hostile line: were this fed raw, the erase would wipe the screen and
    // the cursor move would scramble the layout.
    app.terminals[0].replay_transcript(&[String::from("before\u{1b}[2J\u{1b}[Hafter")]);
    let (lines, _) = app.terminals[0].grid_lines();
    let joined = lines.join("\n");
    assert!(
        joined.contains("before") && joined.contains("after"),
        "the text survives: {joined:?}"
    );
    assert!(
        !joined.contains('\u{1b}'),
        "and no escape byte reaches the grid: {joined:?}"
    );
}

/// #344: a pane whose foreground process is a coding agent is badged, its
/// status is read off the screen once it goes quiet, and the `Waiting`
/// transition fires exactly once per prompt. The fake agent is a shell that
/// prints a permission question and then sits; the sample names it `claude`
/// the way the label sampler would.
#[test]
fn an_agent_pane_is_badged_and_a_prompt_fires_waiting_once() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.terminals[0] = crate::widgets::terminal::PtyTerminal::new_running(
        "/bin/sh",
        &[
            String::from("-c"),
            String::from(
                "printf 'Edited src/main.rs\\nDo you want to proceed? (y/n)\\n'; sleep 30",
            ),
        ],
        tmp.path(),
    )
    .unwrap();
    let shell_pid = app.terminals[0]
        .shell_pid()
        .expect("a running pane has a pid");
    crate::test_budget::await_spawned(
        crate::test_budget::tests::SHELL_PAINT_BASE,
        "the fake agent to paint its prompt",
        || app.terminals[0].visible_text().contains("proceed"),
    );
    let zero = std::time::Duration::ZERO;

    // First sample: seated straight into a prompt.
    let samples = vec![(shell_pid, String::from("claude"))];
    assert!(app.apply_agent_samples(&samples, zero), "the lane changed");
    let lane = app.terminals[0].agent().cloned().expect("badged");
    assert_eq!(lane.name, "claude");
    assert_eq!(lane.status, crate::agents::AgentStatus::Waiting);
    assert!(app.drain_agent_events(), "a waiting agent is announced");
    assert!(
        app.status.contains("claude") && app.status.contains("waiting"),
        "{}",
        app.status
    );
    assert_eq!(app.agent_counts(), (1, 1));

    // The same prompt on the next sample: no second Waiting.
    app.status.clear();
    assert!(!app.apply_agent_samples(&samples, zero), "nothing changed");
    assert!(!app.drain_agent_events(), "the prompt fires once");
    assert!(app.status.is_empty());

    // The pill wears the badge and the chip counts it.
    let backend = ratatui::backend::TestBackend::new(120, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    app.split_terminal().unwrap();
    term.draw(|frame| app.render(frame)).unwrap();
    let buf = term.backend().buffer().clone();
    let text: String = (0..buf.area.height)
        .map(|y| {
            (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        text.contains("\u{25c6} claude \u{25d0}"),
        "the pill wears the badge:\n{text}"
    );
    assert!(
        text.contains("1 agent \u{b7} 1 waiting"),
        "the chip counts it:\n{text}"
    );
    assert!(app.status_agents_rect.width > 0, "and is clickable");

    // Clicking the chip from the editor with the panel collapsed reveals
    // the pane, not just focuses it.
    app.show_terminal = false;
    app.focus_pane(Pane::Editor);
    term.draw(|frame| app.render(frame)).unwrap();
    let chip = app.status_agents_rect;
    assert!(
        chip.width > 0,
        "the chip is painted while the panel is hidden"
    );
    app.handle_mouse(mouse(
        crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        chip.x,
        chip.y,
    ));
    assert!(app.show_terminal, "the click reveals the panel");
    assert!(matches!(app.focus, Pane::Terminal), "and focuses the pane");

    // The agent leaving the foreground unseats it: the shell reclaiming the
    // pane is one way, and a pane that produced no sample at all (the agent
    // took the shell with it, or the pid was missed) is the other.
    let gone = vec![(shell_pid, String::from("zsh"))];
    assert!(app.apply_agent_samples(&gone, zero));
    assert!(app.terminals[0].agent().is_none());
    assert_eq!(app.agent_counts(), (0, 0));
    assert!(app.apply_agent_samples(&samples, zero), "re-seated");
    assert!(app.apply_agent_samples(&[], zero), "no sample unseats");
    assert!(app.terminals[0].agent().is_none());
    assert!(
        matches!(
            app.agent_events.back(),
            Some(crate::agents::AgentEvent::Gone { .. })
        ),
        "and says so: {:?}",
        app.agent_events
    );

    // A pane that has not output yet is quiet since it spawned, not since
    // 1970, so a just-launched agent reads as working.
    let fresh = crate::widgets::terminal::PtyTerminal::new_running(
        "/bin/sh",
        &[String::from("-c"), String::from("sleep 30")],
        tmp.path(),
    )
    .unwrap();
    assert!(
        fresh.quiet_for() < std::time::Duration::from_secs(5),
        "quiet for {:?}",
        fresh.quiet_for()
    );
}

/// #374: Fix with Navigator from PROBLEMS. With no navigator seated the
/// action lands on the diagnostic and says the navigator is not active
/// rather than doing nothing; the caret-side entry points resolve the
/// diagnostic under the caret with its server as the source; the palette
/// carries the command; and the Quick Fix picker offers the navigator row
/// after the server's own actions.
#[test]
fn fix_with_navigator_lands_on_the_diagnostic_and_refuses_without_a_seat() {
    use crate::lsp::manager::DiagnosticSeverity;
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("a.rs");
    std::fs::write(&file, "use std::io;\nfn main() {}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&file).unwrap();
    let mut by_server = std::collections::HashMap::new();
    by_server.insert(
        String::from("rust-analyzer"),
        vec![crate::lsp::manager::Diagnostic {
            start_line: 0,
            start_char: 4,
            end_line: 0,
            end_char: 11,
            severity: DiagnosticSeverity::Warning,
            message: String::from("unused import: `std::io`"),
            tags: Default::default(),
            code: None,
        }],
    );
    app.lsp_diagnostics.insert(file.clone(), by_server);
    assert!(app.drain_lsp_diagnostics());
    assert!(app.rebuild_problems());
    let (path, item) = {
        let groups = app.problems.groups();
        (groups[0].path.clone(), groups[0].items[0].clone())
    };
    assert_eq!(item.source, "rust-analyzer");

    // Move the caret away, then fix: with no navigator the action refuses
    // up front and has no side effect, so the caret stays where it was
    // rather than jumping to a diagnostic nothing is going to fix.
    app.editor.cursor_row = 1;
    app.fix_problem_with_navigator(path.clone(), item.clone(), Vec::new());
    assert!(
        app.status.to_lowercase().contains("not active"),
        "{}",
        app.status
    );
    assert_eq!(app.editor.path.as_deref(), Some(file.as_path()));
    assert_eq!(app.editor.cursor_row, 1, "a refusal moves nothing");
    assert!(
        app.problems.fixing.is_none(),
        "no turn was sent, so no spinner"
    );
    app.editor.cursor_row = 0;

    // The caret-side candidate is the same diagnostic, with its server.
    let (cpath, citem) = app
        .navigator_fix_candidate()
        .expect("the caret sits on a diagnostic");
    assert_eq!(cpath, file);
    assert_eq!(citem.message, item.message);
    assert_eq!(citem.source, "rust-analyzer");
    assert!(citem.col_utf16);
    app.editor.cursor_row = 1;
    assert!(
        app.navigator_fix_candidate().is_none(),
        "no diagnostic on the second line"
    );
    app.editor.cursor_row = 0;

    // The Quick Fix picker: server actions first, the navigator row last.
    app.open_code_action_picker(vec![crate::lsp::manager::CodeActionItem {
        title: String::from("Remove unused import"),
        server: String::from("rust-analyzer"),
        edits: Vec::new(),
        command: None,
        needs_resolve: false,
        resolve_action: None,
        is_preferred: false,
    }]);
    let picker = app.list_picker.as_ref().expect("the picker opened");
    let labels: Vec<String> = picker.rows.iter().map(|r| r.label.clone()).collect();
    assert_eq!(labels, vec!["Remove unused import", "Fix with Navigator"]);
    assert_eq!(picker.rows[1].id, "navigator");

    // Confirming the navigator row must route to the navigator, never to
    // the server's first action: the picker's shared numeric-id decode
    // parses "navigator" to 0 (#374 review, the blocker).
    if let Some(picker) = app.list_picker.as_mut() {
        picker.select_next();
        assert_eq!(picker.selected_row().unwrap().id, "navigator");
    }
    app.status.clear();
    app.confirm_list_picker();
    assert!(
        app.status.to_lowercase().contains("not active"),
        "the navigator row reached the navigator path: {}",
        app.status
    );
    assert_eq!(
        app.editor.lines.join("\n"),
        "use std::io;\nfn main() {}",
        "and no server action touched the buffer"
    );

    // The control: the server row still applies the server action.
    app.open_code_action_picker(vec![crate::lsp::manager::CodeActionItem {
        title: String::from("Remove unused import"),
        server: String::from("rust-analyzer"),
        edits: Vec::new(),
        command: None,
        needs_resolve: false,
        resolve_action: None,
        is_preferred: false,
    }]);
    app.status.clear();
    app.confirm_list_picker();
    assert!(
        !app.status.to_lowercase().contains("not active"),
        "row 0 is the server's, not the navigator's: {}",
        app.status
    );

    // Deactivating the seat clears the marker (#374 review): with the
    // navigator gone no TurnDone can arrive, so the unseat path is the
    // only thing standing between the row and an eternal spinner.
    app.problems.fixing = Some((file.clone(), 0));
    app.problems_fix_started = Some(std::time::Instant::now());
    app.last_pair_check = None;
    app.maybe_seat_navigator();
    assert!(
        app.problems.fixing.is_none(),
        "the deactivation teardown clears the fix marker"
    );
    assert_eq!(app.problems_fix_spinner_phase(), 0);

    // The palette carries the command; the menu entry has no chord.
    assert!(
        crate::widgets::command_palette::ALL_COMMANDS
            .contains(&crate::widgets::command_palette::Command::FixProblemWithNavigator)
    );
    assert_eq!(
        crate::widgets::command_palette::Command::FixProblemWithNavigator.id(),
        "problems_fix_navigator"
    );
    assert_eq!(
        shortcut_for(&MenuAction::FixProblemWithNavigator {
            path: path.clone(),
            item: item.clone()
        }),
        None
    );

    // The spinner stops with the turn, however it ended (#374): the same
    // clear runs on TurnDone, on the seat dying, and on unseat.
    app.problems.fixing = Some((path, item.line));
    app.problems_fix_started = Some(std::time::Instant::now());
    assert!(app.problems_fix_spinner_phase() < 100);
    app.clear_problem_fix();
    assert!(app.problems.fixing.is_none());
    assert_eq!(
        app.problems_fix_spinner_phase(),
        0,
        "idle never wakes the renderer"
    );
}

/// #366: a navigator note and a review comment on ONE line keep a stable
/// order, and the review box carries its own text.
///
/// The merge appends review boxes after the navigator's, which is the order
/// the render comment promises. Asserting on ids alone would pass under a
/// merge that mangled every other field, so this pins the whole tuple:
/// a swap of the two sources, a wrong line, or a body taken from the wrong
/// thread all fail here.
#[test]
fn a_navigator_note_and_a_review_comment_on_one_line_keep_a_stable_order() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("a.rs");
    std::fs::write(&file, "one\ntwo\nthree\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.open_file_at_launch(&file);
    app.navigator_notes
        .insert("a.rs".into(), vec![(1, 1, "the navigator's note".into())]);
    app.review_boxes = Some((
        file.clone(),
        vec![crate::review_threads::Thread {
            id: 4_000_000_001,
            author: String::from("ada"),
            body: String::from("the reviewer's objection"),
            path: String::from("a.rs"),
            // Same row as the navigator's note.
            anchor: crate::review_threads::Anchor::At(1),
            resolved: false,
        }],
    ));

    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();

    let got: Vec<(u64, usize, String)> = app
        .editor
        .comment_boxes
        .iter()
        .map(|b| (b.id, b.line, b.body.clone()))
        .collect();
    assert_eq!(
        got,
        vec![
            (1, 1, String::from("the navigator's note")),
            (4_000_000_001, 1, String::from("the reviewer's objection")),
        ],
        "the navigator's note must come first and each box keep its own text"
    );
}

/// #345: a write reported through a symlinked root is still attributed.
///
/// Found by driving the real fs-watch drain rather than calling the
/// attribution helper directly. The watcher reports a file's REALPATH while
/// a root is stored as the user spelled it, so a workspace reached through a
/// symlink — `/var` on macOS, which is `/private/var`, and any symlinked
/// checkout on Linux — matched no root and every agent write was dropped.
/// The queue stayed empty and nothing failed, which is why a test that
/// substituted for the drain could not see it.
#[test]
#[cfg(unix)]
fn a_write_under_a_symlinked_root_is_still_the_agents() {
    let tmp = tempfile::tempdir().unwrap();
    let real = tmp.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let link = tmp.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    // The workspace is opened through the LINK; the file is written and
    // reported through the real path, as a watcher would.
    let via_link = link.join("f.rs");
    std::fs::write(&via_link, "fn a() {}\n").unwrap();
    let mut app = App::new(link.clone()).unwrap();
    app.terminals[0].set_agent(Some(crate::agents::AgentLane {
        name: String::from("claude"),
        status: crate::agents::AgentStatus::Working,
    }));

    let mut changed = std::collections::BTreeSet::new();
    changed.insert(real.join("f.rs"));
    assert!(
        app.attribute_writes_to_working_agents(&changed),
        "a realpath write under a symlinked root must still be attributed"
    );

    // And it is keyed the way the ROOT is spelled, so the Explorer's rows —
    // which come from the tree, not the watcher — find it.
    app.sync_agent_lane_decorations();
    assert!(
        app.tree.agent_touched.contains(&via_link),
        "the ledger keyed the row under the realpath, so no tree row matches \
         it: {:?}",
        app.tree.agent_touched
    );

    // And a DELETION clears it. `canonicalize` stats the final component, so
    // resolving the whole path fails on a file that is gone — which would
    // make the ledger's forget-path arm unreachable and strand the row on a
    // file that no longer exists, with no later write able to clear it. That
    // is strictly worse than the original bug, which dropped writes and
    // deletions alike and so at least stayed self-consistent.
    std::fs::remove_file(&via_link).unwrap();
    assert!(
        app.attribute_writes_to_working_agents(&changed),
        "a deletion reported by realpath must reach the ledger too"
    );
    app.sync_agent_lane_decorations();
    assert!(
        app.tree.agent_touched.is_empty(),
        "the row outlived the file it described: {:?}",
        app.tree.agent_touched
    );

    // The parent going too, which makes the walk climb more than one level.
    // A tail-reattachment bug hides here rather than in the single-level
    // case: the key the delete resolves to must still be the key the write
    // created, or `forget_path` finds nothing to forget.
    let sub = link.join("sub");
    std::fs::create_dir(&sub).unwrap();
    let nested = sub.join("g.rs");
    std::fs::write(&nested, "fn g() {}\n").unwrap();
    let mut deep = std::collections::BTreeSet::new();
    deep.insert(real.join("sub/g.rs"));
    assert!(app.attribute_writes_to_working_agents(&deep));
    app.sync_agent_lane_decorations();
    assert!(
        app.tree.agent_touched.contains(&nested),
        "staging: the nested file is queued"
    );

    std::fs::remove_dir_all(&sub).unwrap();
    assert!(
        app.attribute_writes_to_working_agents(&deep),
        "a deletion whose PARENT is gone must still resolve"
    );
    app.sync_agent_lane_decorations();
    assert!(
        app.tree.agent_touched.is_empty(),
        "the row outlived its whole directory: {:?}",
        app.tree.agent_touched
    );
}

/// #345: the agent lane attributes workspace writes to whichever agents
/// were WORKING when they landed, keeps a review baseline per file, and
/// never blames an agent for the user's own saves.
#[test]
fn the_agent_lane_ledger_attributes_writes_and_tracks_review_baselines() {
    let tmp = tempfile::tempdir().unwrap();
    let touched = tmp.path().join("touched.rs");
    std::fs::write(&touched, "fn a() {}\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();

    // With no agent working, a write belongs to the user: no lane at all.
    let mut changed = std::collections::BTreeSet::new();
    changed.insert(touched.clone());
    assert!(!app.attribute_writes_to_working_agents(&changed));
    assert!(app.agent_ledger.agents().is_empty());

    // Seat a WORKING agent in the pane, then replay the same write.
    app.terminals[0].set_agent(Some(crate::agents::AgentLane {
        name: String::from("claude"),
        status: crate::agents::AgentStatus::Working,
    }));
    assert_eq!(app.working_agents(), vec![String::from("claude")]);
    assert!(app.attribute_writes_to_working_agents(&changed));
    assert_eq!(app.agent_ledger.unreviewed_count("claude"), 1);
    assert!(app.agent_ledger.is_unreviewed(&touched));

    // A SECONDARY workspace root's files are attributed too: a multi-root
    // session must not have an empty queue for every folder but the first.
    let second = tempfile::tempdir().unwrap();
    let in_second = second.path().join("lib.rs");
    std::fs::write(&in_second, "fn b() {}\n").unwrap();
    app.roots.add(second.path().to_path_buf());
    let mut second_set = std::collections::BTreeSet::new();
    second_set.insert(in_second.clone());
    assert!(
        app.attribute_writes_to_working_agents(&second_set),
        "a write under a secondary root is still the agent's"
    );
    assert!(app.agent_ledger.is_unreviewed(&in_second));
    app.mark_agent_file_reviewed("claude", &in_second);

    // A file OUTSIDE the workspace is not this workspace's review queue.
    let outside = tempfile::tempdir().unwrap();
    let stray = outside.path().join("elsewhere.rs");
    std::fs::write(&stray, "x\n").unwrap();
    let mut outside_set = std::collections::BTreeSet::new();
    outside_set.insert(stray.clone());
    assert!(!app.attribute_writes_to_working_agents(&outside_set));
    assert!(!app.agent_ledger.is_unreviewed(&stray));

    // Reviewing baselines the content on disk NOW, so a later edit by the
    // user does not resurrect the row but a later agent write does.
    assert!(app.mark_agent_file_reviewed("claude", &touched));
    assert_eq!(app.agent_ledger.unreviewed_count("claude"), 0);
    std::fs::write(&touched, "fn a() { todo!() }\n").unwrap();
    assert!(app.attribute_writes_to_working_agents(&changed));
    assert_eq!(
        app.agent_ledger.unreviewed_count("claude"),
        1,
        "a fresh agent write re-queues the file"
    );

    // A file DELETED after its agent goes idle still leaves the queue: the
    // row would otherwise point at a file that is not there forever, since
    // a deleted file generates no later write to clear it.
    let later = tmp.path().join("later.rs");
    std::fs::write(&later, "fn c() {}\n").unwrap();
    let mut later_set = std::collections::BTreeSet::new();
    later_set.insert(later.clone());
    assert!(app.attribute_writes_to_working_agents(&later_set));
    assert!(app.agent_ledger.is_unreviewed(&later));
    app.terminals[0].set_agent(Some(crate::agents::AgentLane {
        name: String::from("claude"),
        status: crate::agents::AgentStatus::Idle,
    }));
    std::fs::remove_file(&later).unwrap();
    assert!(
        app.attribute_writes_to_working_agents(&later_set),
        "a deletion is processed with no agent working"
    );
    assert!(
        !app.agent_ledger.is_unreviewed(&later),
        "the row leaves rather than stranding"
    );
    app.terminals[0].set_agent(Some(crate::agents::AgentLane {
        name: String::from("claude"),
        status: crate::agents::AgentStatus::Working,
    }));

    // A QUIET agent stops being attributed: only Working counts.
    app.terminals[0].set_agent(Some(crate::agents::AgentLane {
        name: String::from("claude"),
        status: crate::agents::AgentStatus::Idle,
    }));
    assert!(app.working_agents().is_empty());
    std::fs::write(&touched, "fn a() { 1 }\n").unwrap();
    assert!(!app.attribute_writes_to_working_agents(&changed));

    // Mark-all clears the lane and says how much it cleared.
    assert_eq!(app.mark_agent_lane_reviewed("claude"), 1);
    assert!(
        app.status.contains("1 file marked reviewed"),
        "{}",
        app.status
    );
    assert_eq!(app.agent_ledger.total_unreviewed(), 0);
    assert_eq!(app.mark_agent_lane_reviewed("claude"), 0);
    assert!(app.status.contains("nothing waiting"), "{}", app.status);

    // The lane summary names each agent, its count, and any shared row.
    app.terminals[0].set_agent(Some(crate::agents::AgentLane {
        name: String::from("claude"),
        status: crate::agents::AgentStatus::Working,
    }));
    std::fs::write(&touched, "fn a() { 2 }\n").unwrap();
    assert!(app.attribute_writes_to_working_agents(&changed));
    app.run_command(crate::widgets::command_palette::Command::ShowAgentLane);
    assert!(
        app.status.contains("claude: 1 to review") && app.status.contains("touched.rs"),
        "{}",
        app.status
    );

    // "Mark this file reviewed" acts on the ACTIVE file, in every lane
    // holding it, and refuses when there is nothing waiting.
    app.editor.open_pinned(&touched).unwrap();
    app.run_command(crate::widgets::command_palette::Command::MarkAgentFileReviewed);
    assert!(
        app.status.contains("marked reviewed in 1 agent lane"),
        "{}",
        app.status
    );
    assert_eq!(app.agent_ledger.total_unreviewed(), 0);
    app.run_command(crate::widgets::command_palette::Command::MarkAgentFileReviewed);
    assert!(
        app.status.contains("No agent change waiting"),
        "{}",
        app.status
    );

    // The palette commands are registered.
    for (cmd, id) in [
        (
            crate::widgets::command_palette::Command::MarkAgentLaneReviewed,
            "agents_mark_reviewed",
        ),
        (
            crate::widgets::command_palette::Command::ShowAgentLane,
            "agents_show_lane",
        ),
        (
            crate::widgets::command_palette::Command::MarkAgentFileReviewed,
            "agents_mark_file_reviewed",
        ),
    ] {
        assert!(crate::widgets::command_palette::ALL_COMMANDS.contains(&cmd));
        assert_eq!(cmd.id(), id);
    }
}

#[test]
fn a_new_lane_opens_a_named_pane_in_the_worktree_running_the_agent_command() {
    // One chord's worth (#348): worktree, root, and a pane named after the
    // lane sitting in it with the configured agent's launch line typed in.
    // The "agent" is a printf whose typed line does not spell the probe.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    init_lane_repo(&repo);
    let mut app = App::new(repo.clone()).unwrap();
    app.terminal_session_path = tmp.path().join("sessions.json");
    app.agents = crate::agents::AgentTable::from_json(
        r#"[{ "name": "probe", "launch": "printf '%s%s\\n' 'croft-lane' '-seated-1'" }]"#,
    );
    app.lane_agent = Some(String::from("Probe"));
    let before = app.terminals.len();
    app.create_worktree_lane("fix login");
    let lane = tmp.path().join("repo-fix-login");
    assert!(lane.is_dir(), "the worktree exists: {}", app.status);
    assert!(
        app.roots.iter().any(|r| r == lane.canonicalize().unwrap()),
        "and is a workspace root"
    );
    assert_eq!(app.terminals.len(), before + 1, "one new pane");
    let pane = &app.terminals[app.active_terminal];
    assert_eq!(pane.manual_name(), Some("Lane: fix-login"));
    let rec = app
        .lane_panes
        .get(&pane.uid())
        .expect("the pane is recorded as the lane's");
    assert_eq!(rec.branch, "agent/fix-login");
    assert_eq!(rec.agent.as_deref(), Some("probe"), "lower-cased row name");
    assert!(app.status.contains("probe started"), "{}", app.status);
    let budget = crate::test_budget::spawn_budget(crate::test_budget::tests::TERMINAL_PROBE_BASE);
    let started = std::time::Instant::now();
    while started.elapsed() < budget {
        if app.terminals[app.active_terminal]
            .visible_text()
            .contains("croft-lane-seated-1")
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        app.terminals[app.active_terminal]
            .visible_text()
            .contains("croft-lane-seated-1"),
        "the launch line ran in the lane's pane; screen:\n{}",
        app.terminals[app.active_terminal].visible_text()
    );
    // The saved session carries the lane, so a relaunch can seat it again.
    let saved = crate::terminal_session::load(&app.terminal_session_path);
    let root_key = app.workspace_root().display().to_string();
    let lanes: Vec<_> = saved[&root_key]
        .panes
        .iter()
        .filter_map(|p| p.lane.clone())
        .collect();
    assert_eq!(lanes.len(), 1, "{saved:?}");
    assert_eq!(lanes[0].agent.as_deref(), Some("probe"));
    assert_eq!(
        std::path::Path::new(&lanes[0].path),
        lane.canonicalize().unwrap(),
        "recorded canonical, like the roots"
    );
}

#[test]
fn a_lane_without_a_configured_agent_opens_a_plain_shell_and_says_so() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    init_lane_repo(&repo);
    let mut app = App::new(repo.clone()).unwrap();
    app.terminal_session_path = tmp.path().join("sessions.json");
    app.lane_agent = None;
    app.create_worktree_lane("docs");
    let pane = &app.terminals[app.active_terminal];
    assert_eq!(pane.manual_name(), Some("Lane: docs"));
    assert_eq!(app.lane_panes[&pane.uid()].agent, None);
    assert!(app.status.contains("set lane_agent"), "{}", app.status);
    // An agent named but without a launch line: shell, and the status names
    // the gap rather than reading as a working lane.
    app.agents = crate::agents::AgentTable::from_json(r#"[{ "name": "mute" }]"#);
    app.lane_agent = Some(String::from("mute"));
    app.create_worktree_lane("tests");
    let pane = &app.terminals[app.active_terminal];
    assert_eq!(pane.manual_name(), Some("Lane: tests"));
    assert_eq!(
        app.lane_panes[&pane.uid()].agent,
        None,
        "no agent was seated"
    );
    assert!(
        app.status.contains("no launch line for \"mute\""),
        "{}",
        app.status
    );
}

#[test]
fn a_restored_lane_pane_is_a_lane_again_with_its_agent_seated() {
    // The association survives a relaunch (#348): the saved lane record
    // brings the pane back in the worktree, re-registered as the lane's, and
    // the agent's CURRENT launch line typed in. A lane whose worktree is
    // gone comes back as an ordinary pane.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    init_lane_repo(&repo);
    let session = tmp.path().join("sessions.json");
    let lane_dir = tmp.path().join("repo-fix-login");
    std::fs::create_dir_all(&lane_dir).unwrap();
    let root_key = repo.display().to_string();
    let rec = crate::terminal_session::SessionRecord {
        panes: vec![
            crate::terminal_session::PaneRecord {
                host: None,
                cwd: root_key.clone(),
                name: None,
                transcript: Vec::new(),
                lane: None,
            },
            crate::terminal_session::PaneRecord {
                host: None,
                cwd: lane_dir.display().to_string(),
                name: Some(String::from("Lane: fix-login")),
                transcript: Vec::new(),
                lane: Some(crate::terminal_session::LaneRecord {
                    path: lane_dir.display().to_string(),
                    branch: String::from("agent/fix-login"),
                    agent: Some(String::from("probe")),
                    base: None,
                }),
            },
            crate::terminal_session::PaneRecord {
                host: None,
                cwd: root_key.clone(),
                name: Some(String::from("Lane: gone")),
                transcript: Vec::new(),
                lane: Some(crate::terminal_session::LaneRecord {
                    path: tmp.path().join("repo-gone").display().to_string(),
                    branch: String::from("agent/gone"),
                    agent: Some(String::from("probe")),
                    base: None,
                }),
            },
        ],
        active: 1,
    };
    crate::terminal_session::save_for_root(&session, &root_key, rec).unwrap();
    let mut app = App::new(repo.clone()).unwrap();
    app.terminal_session_path = session;
    app.agents = crate::agents::AgentTable::from_json(
        r#"[{ "name": "probe", "launch": "printf '%s%s\\n' 'croft-lane' '-back-2'" }]"#,
    );
    app.restore_terminal_session_with(true);
    assert_eq!(app.terminals.len(), 3);
    let uid = app.terminals[1].uid();
    assert_eq!(
        app.lane_panes.get(&uid).map(|l| l.branch.as_str()),
        Some("agent/fix-login")
    );
    assert!(
        !app.lane_panes.contains_key(&app.terminals[2].uid()),
        "a lane whose worktree is gone is not a lane"
    );
    let budget = crate::test_budget::spawn_budget(crate::test_budget::tests::TERMINAL_PROBE_BASE);
    let started = std::time::Instant::now();
    while started.elapsed() < budget
        && !app.terminals[1]
            .visible_text()
            .contains("croft-lane-back-2")
    {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        app.terminals[1]
            .visible_text()
            .contains("croft-lane-back-2"),
        "the agent was seated again; screen:\n{}",
        app.terminals[1].visible_text()
    );
    assert!(
        !app.terminals[2]
            .visible_text()
            .contains("croft-lane-back-2"),
        "nothing was typed into the pane whose lane is gone"
    );
}

#[test]
fn a_restored_lane_pane_spawns_in_its_worktree_not_where_the_shell_had_wandered() {
    // The launch line is typed into the restored shell, so that shell must
    // be in the lane: not in the directory the old shell had cd'd to at save
    // time, and not, when that directory is gone, in the PRIMARY repo the
    // cwd fallback names. The "agent" prints its cwd so the test can read
    // where it was seated.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    init_lane_repo(&repo);
    let session = tmp.path().join("sessions.json");
    let lane_dir = tmp.path().join("repo-fix-login");
    std::fs::create_dir_all(&lane_dir).unwrap();
    let elsewhere = tmp.path().join("notes");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let vanished = lane_dir.join("scratch");
    let root_key = repo.display().to_string();
    let lane = |agent: &str| crate::terminal_session::LaneRecord {
        path: lane_dir.display().to_string(),
        branch: String::from("agent/fix-login"),
        agent: Some(String::from(agent)),
        base: None,
    };
    let rec = crate::terminal_session::SessionRecord {
        panes: vec![
            crate::terminal_session::PaneRecord {
                host: None,
                cwd: elsewhere.display().to_string(),
                name: Some(String::from("Lane: fix-login")),
                transcript: Vec::new(),
                lane: Some(lane("probe-a")),
            },
            crate::terminal_session::PaneRecord {
                host: None,
                cwd: vanished.display().to_string(),
                name: Some(String::from("Lane: fix-login")),
                transcript: Vec::new(),
                lane: Some(lane("probe-b")),
            },
        ],
        active: 0,
    };
    crate::terminal_session::save_for_root(&session, &root_key, rec).unwrap();
    let mut app = App::new(repo.clone()).unwrap();
    app.terminal_session_path = session;
    app.agents = crate::agents::AgentTable::from_json(
        r#"[
          { "name": "probe-a", "launch": "printf '%s%s\\n' 'croft-seat' \"-a:$(pwd)\"" },
          { "name": "probe-b", "launch": "printf '%s%s\\n' 'croft-seat' \"-b:$(pwd)\"" }
        ]"#,
    );
    app.restore_terminal_session_with(true);
    assert_eq!(app.terminals.len(), 2);
    let lane_canon = lane_dir.canonicalize().unwrap().display().to_string();
    let budget = crate::test_budget::spawn_budget(crate::test_budget::tests::TERMINAL_PROBE_BASE);
    let started = std::time::Instant::now();
    let seated = |app: &App, tag: &str| {
        app.terminals
            .iter()
            .any(|t| t.visible_text().contains(&format!("croft-seat-{tag}:")))
    };
    while started.elapsed() < budget && !(seated(&app, "a") && seated(&app, "b")) {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    for tag in ["a", "b"] {
        let screen = app
            .terminals
            .iter()
            .map(|t| t.visible_text())
            .find(|s| s.contains(&format!("croft-seat-{tag}:")))
            .unwrap_or_else(|| panic!("probe {tag} never printed its cwd"));
        let line = screen
            .lines()
            .find(|l| l.contains(&format!("croft-seat-{tag}:")))
            .unwrap();
        assert!(
            line.trim_end().ends_with(&lane_canon)
                || line.trim_end().ends_with(&lane_dir.display().to_string()),
            "agent {tag} was seated in the lane, got: {line:?}"
        );
    }
    for t in &app.terminals {
        assert!(
            app.lane_panes.contains_key(&t.uid()),
            "both restored panes are the lane's"
        );
    }
}

#[test]
fn a_lane_pane_closed_by_any_route_is_no_longer_the_lanes_even_when_reopened() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    init_lane_repo(&repo);
    let mut app = App::new(repo.clone()).unwrap();
    app.terminal_session_path = tmp.path().join("sessions.json");
    app.lane_agent = None;
    app.create_worktree_lane("fix login");
    let uid = app.terminals[app.active_terminal].uid();
    assert!(app.lane_panes.contains_key(&uid));
    assert!(app.close_active_terminal(), "the ordinary close");
    assert!(
        app.lane_panes.is_empty(),
        "the map does not keep dead panes"
    );
    app.undo_close_terminal();
    assert!(
        app.terminals.iter().any(|t| t.uid() == uid),
        "the pane came back"
    );
    assert!(
        app.lane_panes.is_empty(),
        "but not as the lane's: nobody asked for an agent to be re-seated"
    );
}

#[test]
fn closing_a_lane_whose_pane_is_the_last_one_replaces_it_with_a_shell_in_the_primary_root() {
    // The close refuses the last pane, and a lane pane left standing would
    // sit in a directory git just removed. So a fresh shell in the primary
    // root is opened first, and the lane pane closes like any other.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    init_lane_repo(&repo);
    let mut app = App::new(repo.clone()).unwrap();
    app.terminal_session_path = tmp.path().join("sessions.json");
    app.lane_agent = None;
    app.create_worktree_lane("solo");
    // Drop the original pane so the lane's is the only one left.
    assert!(app.close_terminal_at(0));
    assert_eq!(app.terminals.len(), 1);
    let lane_uid = app.terminals[0].uid();
    assert_eq!(app.terminals[0].manual_name(), Some("Lane: solo"));
    let lane = tmp.path().join("repo-solo").canonicalize().unwrap();
    app.active_scm_root = lane.clone();
    app.close_worktree_lane();
    assert!(!lane.exists(), "{}", app.status);
    assert_eq!(app.terminals.len(), 1, "one pane remains: the replacement");
    assert_ne!(app.terminals[0].uid(), lane_uid, "and it is not the lane's");
    assert_eq!(app.terminals[0].manual_name(), None);
    assert!(app.lane_panes.is_empty());
    assert!(
        app.closed_terminals
            .iter()
            .any(|c| c.term.uid() == lane_uid),
        "the lane pane went through the undoable close"
    );
    assert!(app.status.contains("closed its pane"), "{}", app.status);
    // The replacement is a live shell in the primary root, not in the
    // removed directory: it can print its cwd.
    app.terminals[0].write_input(b"printf '%s%s\\n' 'croft-repl' \"-at:$(pwd)\"\r");
    let budget = crate::test_budget::spawn_budget(crate::test_budget::tests::TERMINAL_PROBE_BASE);
    let started = std::time::Instant::now();
    while started.elapsed() < budget && !app.terminals[0].visible_text().contains("croft-repl-at:")
    {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let screen = app.terminals[0].visible_text();
    let line = screen
        .lines()
        .find(|l| l.contains("croft-repl-at:"))
        .unwrap_or_else(|| panic!("the replacement shell never answered; screen:\n{screen}"));
    let repo_canon = repo.canonicalize().unwrap().display().to_string();
    assert!(
        line.trim_end().ends_with(&repo_canon)
            || line.trim_end().ends_with(&repo.display().to_string()),
        "the replacement sits in the primary root, got: {line:?}"
    );
}

#[test]
fn closing_a_lane_closes_its_pane_through_the_undoable_path() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    init_lane_repo(&repo);
    let mut app = App::new(repo.clone()).unwrap();
    app.terminal_session_path = tmp.path().join("sessions.json");
    app.lane_agent = None;
    app.create_worktree_lane("fix login");
    let lane = tmp.path().join("repo-fix-login").canonicalize().unwrap();
    assert_eq!(app.terminals.len(), 2);
    let lane_uid = app.terminals[app.active_terminal].uid();
    // Close targets the ACTIVE root; the lane is what the user is in.
    app.active_scm_root = lane.clone();
    app.close_worktree_lane();
    assert!(!lane.exists(), "the worktree is gone: {}", app.status);
    assert!(!app.roots.iter().any(|r| r == lane), "and the root");
    assert_eq!(
        app.terminals.len(),
        1,
        "and the lane's pane: {}",
        app.status
    );
    assert!(!app.terminals.iter().any(|t| t.uid() == lane_uid));
    assert!(app.lane_panes.is_empty());
    assert!(
        app.closed_terminals
            .iter()
            .any(|c| c.term.uid() == lane_uid),
        "closed through the undoable path"
    );
    assert!(app.status.contains("closed its pane"), "{}", app.status);
}

#[test]
fn cmd_k_shift_l_asks_for_a_new_lane_and_plain_l_still_folds() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    assert!(app.handle_cmd_k_chord(key(KeyCode::Char('L'), KeyModifiers::SHIFT)));
    assert_eq!(
        app.input_prompt.as_ref().map(|p| p.purpose.clone()),
        Some(crate::widgets::input_prompt::InputPurpose::NewWorktreeLane)
    );
    app.close_input_prompt();
    // CSI-u hosts report Shift+L as a lower-case char with SHIFT.
    assert!(app.handle_cmd_k_chord(key(KeyCode::Char('l'), KeyModifiers::SHIFT)));
    assert_eq!(
        app.input_prompt.as_ref().map(|p| p.purpose.clone()),
        Some(crate::widgets::input_prompt::InputPurpose::NewWorktreeLane)
    );
    app.close_input_prompt();
    assert!(app.handle_cmd_k_chord(key(KeyCode::Char('l'), KeyModifiers::NONE)));
    assert!(
        app.input_prompt.is_none(),
        "plain L is still the fold toggle"
    );
}

#[test]
fn a_lanes_root_row_wears_its_branch_and_its_agent() {
    // The badge text in its three shapes, then the app-side sync: a lane's
    // root gets a badge keyed by its canonical path, a plain-shell lane shows
    // the branch alone, and a closed lane's badge goes with it.
    use crate::agents::{AgentLane, AgentStatus};
    let seated = AgentLane {
        name: String::from("claude"),
        status: AgentStatus::Waiting,
    };
    assert_eq!(
        lane_root_badge("agent/fix-login", Some(&seated), Some("claude")),
        "agent/fix-login \u{b7} \u{25c6} claude \u{25d0}"
    );
    assert_eq!(
        lane_root_badge("agent/fix-login", None, Some("claude")),
        "agent/fix-login \u{b7} claude",
        "configured but not yet seated: the bare name"
    );
    assert_eq!(lane_root_badge("agent/docs", None, None), "agent/docs");

    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    init_lane_repo(&repo);
    let mut app = App::new(repo.clone()).unwrap();
    app.terminal_session_path = tmp.path().join("sessions.json");
    app.lane_agent = None;
    app.create_worktree_lane("docs");
    app.sync_lane_root_badges();
    let lane = tmp.path().join("repo-docs").canonicalize().unwrap();
    assert_eq!(
        app.tree.root_badges.get(&lane).map(String::as_str),
        Some("agent/docs"),
        "{:?}",
        app.tree.root_badges
    );
    assert!(
        !app.tree.root_badges.contains_key(app.roots.primary()),
        "the primary is not a lane"
    );
    app.active_scm_root = lane.clone();
    app.close_worktree_lane();
    app.sync_lane_root_badges();
    assert!(
        app.tree.root_badges.is_empty(),
        "the badge went with the lane"
    );
}

#[test]
fn a_lanes_badge_follows_the_seat_and_a_seated_pane_outranks_a_plain_one() {
    // App-side: the configured name shows until the sampler seats the
    // agent, the seated badge replaces it, and it falls back when the agent
    // leaves. With two panes on one lane, the seated one decides whichever
    // uid order the map walks.
    use crate::agents::{AgentLane, AgentStatus};
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    init_lane_repo(&repo);
    let mut app = App::new(repo.clone()).unwrap();
    app.terminal_session_path = tmp.path().join("sessions.json");
    // A table of the test's own, not this machine's agents.json: claude
    // needs a launch line for the lane to record the agent, and that line
    // must not start the real thing in the test's shell.
    app.agents =
        crate::agents::AgentTable::from_json(r#"[{ "name": "claude", "launch": "true" }]"#);
    app.lane_agent = Some(String::from("claude"));
    app.create_worktree_lane("fix login");
    let lane = tmp.path().join("repo-fix-login").canonicalize().unwrap();
    let idx = app.active_terminal;
    app.sync_lane_root_badges();
    assert_eq!(
        app.tree.root_badges.get(&lane).map(String::as_str),
        Some("agent/fix-login \u{b7} claude"),
        "configured, not yet seated"
    );
    app.terminals[idx].set_agent(Some(AgentLane {
        name: String::from("claude"),
        status: AgentStatus::Working,
    }));
    app.sync_lane_root_badges();
    assert_eq!(
        app.tree.root_badges.get(&lane).map(String::as_str),
        Some("agent/fix-login \u{b7} \u{25c6} claude \u{25cf}"),
        "seated"
    );
    // A second pane on the same lane with a HIGHER uid, so the map walks the
    // first pane before it. Seated-first-then-plain and plain-first-then-
    // seated must both end on the seated badge.
    let plain = crate::widgets::terminal::PtyTerminal::new(&lane).unwrap();
    let second_uid = plain.uid();
    assert!(app.terminals[idx].uid() < second_uid);
    app.lane_panes.insert(
        second_uid,
        crate::terminal_session::LaneRecord {
            path: lane.display().to_string(),
            branch: String::from("agent/fix-login"),
            agent: None,
            base: None,
        },
    );
    app.terminals.push(plain);
    let last = app.terminals.len() - 1;
    app.sync_lane_root_badges();
    assert_eq!(
        app.tree.root_badges.get(&lane).map(String::as_str),
        Some("agent/fix-login \u{b7} \u{25c6} claude \u{25cf}"),
        "seated first, plain second: the plain sibling does not hide it"
    );
    app.terminals[idx].set_agent(None);
    app.terminals[last].set_agent(Some(AgentLane {
        name: String::from("claude"),
        status: AgentStatus::Idle,
    }));
    app.sync_lane_root_badges();
    assert_eq!(
        app.tree.root_badges.get(&lane).map(String::as_str),
        Some("agent/fix-login \u{b7} \u{25c6} claude \u{25cb}"),
        "plain first, seated second: the seated pane still decides"
    );
    app.terminals[last].set_agent(None);
    app.sync_lane_root_badges();
    assert_eq!(
        app.tree.root_badges.get(&lane).map(String::as_str),
        Some("agent/fix-login \u{b7} claude"),
        "back to the configured name once nobody is seated"
    );
}

#[test]
fn two_lanes_sharing_a_branch_are_named_by_their_folders_instead() {
    // A lane's branch comes from its slug alone, so lanes cut from two
    // different repos can carry the same one. Naming both groups
    // `agent/fix` would be worse than naming neither: the fallback is the
    // disambiguated root label the workspace already computes.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    init_lane_repo(&repo);
    let mut app = App::new(repo.clone()).unwrap();
    app.terminal_session_path = tmp.path().join("sessions.json");
    let one = tmp.path().join("one-fix");
    let two = tmp.path().join("two-fix");
    for dir in [&one, &two] {
        std::fs::create_dir_all(dir).unwrap();
        app.add_workspace_folder(dir.clone());
    }
    let (one, two) = (one.canonicalize().unwrap(), two.canonicalize().unwrap());
    // Two panes, two lanes, one branch name between them.
    for (i, dir) in [(0u64, &one), (1, &two)] {
        app.lane_panes.insert(
            1000 + i,
            crate::terminal_session::LaneRecord {
                path: dir.display().to_string(),
                branch: String::from("agent/fix"),
                agent: None,
                base: None,
            },
        );
    }
    let working = vec![String::from("claude")];
    app.agent_ledger
        .record_write(&one.join("a.rs"), 1, &working);
    app.agent_ledger
        .record_write(&two.join("b.rs"), 2, &working);
    let row = app.agent_lane_rows().remove(0);
    assert!(
        !row.contains("agent/fix"),
        "a branch shared by two groups names neither: {row}"
    );
    assert!(
        row.contains("one-fix 1") && row.contains("two-fix 1"),
        "both groups fall back to their folder labels: {row}"
    );
    // One lane alone: the branch is unique again, so it names its group.
    app.lane_panes.remove(&1001);
    let row = app.agent_lane_rows().remove(0);
    assert!(
        row.contains("agent/fix 1"),
        "a unique branch still names its lane: {row}"
    );
}

/// #1368: a first-time user's `croft .` serves its agent hook. With no
/// `~/.cache/croft/sessions` yet, binding made nothing and the hook was
/// told "no croft is open".
#[test]
fn the_agent_hook_is_served_on_a_machine_with_no_sessions_folder() {
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            crate::session_state::CACHE_DIR_FOR_THIS_TEST_THREAD.with(|d| *d.borrow_mut() = None);
        }
    }
    let home = tempfile::tempdir().unwrap();
    let _restore = Restore;
    crate::session_state::CACHE_DIR_FOR_THIS_TEST_THREAD
        .with(|d| *d.borrow_mut() = Some(home.path().join(".cache").join("croft")));
    let ws = home.path().join("w");
    std::fs::create_dir_all(&ws).unwrap();
    assert!(!crate::session::sessions_dir().exists());
    let _listener = crate::agent_approval::bind(&ws).expect("the hook socket binds");
    assert!(
        crate::agent_hook::connect_croft(&ws).is_some(),
        "the hook finds this croft"
    );
}

/// #1368: a hook socket that cannot be bound says so at startup, except
/// when another croft already serves the workspace, which is by design.
#[test]
fn a_hook_socket_that_cannot_bind_says_why() {
    let gone = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "Permission denied");
    assert_eq!(
        crate::app::hook_bind_status(&gone).as_deref(),
        Some("Agent edit approvals are unavailable: Permission denied")
    );
    let taken = std::io::Error::new(std::io::ErrorKind::AddrInUse, "already served");
    assert_eq!(crate::app::hook_bind_status(&taken), None);
}

// ---- Inline AI suggestions (#607) ----

/// A one-shot fake `/v1/messages`: answers the first request with `text`
/// and hands back the raw request it read.
fn fake_messages_endpoint(text: &'static str) -> (String, std::sync::mpsc::Receiver<String>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let mut buf = Vec::new();
        let mut chunk = [0u8; 8192];
        // Headers, then as much body as Content-Length says.
        loop {
            let n = stream.read(&mut chunk).unwrap_or(0);
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            let s = String::from_utf8_lossy(&buf).to_string();
            if let Some(h) = s.find("\r\n\r\n") {
                let len = s[..h]
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                    })
                    .unwrap_or(0);
                if buf.len() >= h + 4 + len {
                    break;
                }
            }
        }
        let body = serde_json::json!({"stop_reason": "end_turn", "content": [{"type": "text", "text": text}]}).to_string();
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = tx.send(String::from_utf8_lossy(&buf).to_string());
    });
    (url, rx)
}

#[test]
fn a_failed_inline_suggestion_is_not_asked_again_until_the_caret_moves() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.rs", "let x = \n");
    app.inline_config = Some(crate::inline_complete::Config {
        backend: crate::inline_complete::Backend::Claude {
            base_url: String::from("http://127.0.0.1:1"),
            api_key: String::from("test-key"),
        },
        model: String::from("claude-opus-5"),
    });
    app.inline_worker = Some(crate::inline_complete::Worker::spawn());
    app.inline_enabled = true;
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 8;
    app.editor.last_edit_at = Some(std::time::Instant::now() - std::time::Duration::from_secs(2));
    let first = app.inline_next_id;
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
    while std::time::Instant::now() < deadline {
        app.tick_inline_complete();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(app.inline_next_id - first, 1, "one request for one caret");
}

#[test]
fn an_inline_suggestion_appears_after_a_pause_and_tab_takes_it() {
    let (url, requests) = fake_messages_endpoint("1 + 1;");
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.rs", "let x = \n");
    app.inline_config = Some(crate::inline_complete::Config {
        backend: crate::inline_complete::Backend::Claude {
            base_url: url,
            api_key: String::from("test-key"),
        },
        model: String::from("claude-opus-5"),
    });
    app.inline_worker = Some(crate::inline_complete::Worker::spawn());
    app.inline_enabled = true;
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 8;
    app.editor.last_edit_at = Some(std::time::Instant::now() - std::time::Duration::from_secs(2));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while app.editor.live_ghost().is_none() && std::time::Instant::now() < deadline {
        app.tick_inline_complete();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(app.editor.live_ghost(), Some("1 + 1;"));
    let req = requests
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    assert!(req.contains("x-api-key: test-key"), "{req}");
    assert!(req.contains("anthropic-version: 2023-06-01"));
    assert!(
        req.contains("let x = <CURSOR>"),
        "the caret is marked: {req}"
    );
    app.handle_key(crossterm::event::KeyEvent::new(
        KeyCode::Tab,
        KeyModifiers::NONE,
    ))
    .unwrap();
    assert_eq!(app.editor.lines[0], "let x = 1 + 1;");
    assert!(app.editor.live_ghost().is_none());
}

/// The issue's file (#1625) with the caret at the end of `def add(a, b):`
/// and the stub's suggestion live there.
fn app_with_body_suggestion(tmp: &std::path::Path) -> App {
    let mut app = app_with_open_file(tmp, "m.py", "def add(a, b):\n\n\nprint(add(1, 2))\n");
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 14;
    let seq = app.editor.edit_seq;
    app.editor.ghost = Some((
        0,
        14,
        seq,
        String::from("\n    total = a + b\n    return total"),
    ));
    app
}

/// The screen as rows, at `w` x `h`.
fn screen_rows(app: &mut App, w: u16, h: u16) -> Vec<String> {
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    screen_text(&term).lines().map(str::to_string).collect()
}

/// #1625: every line of a multi-line suggestion is painted under the
/// caret's line, where only a "(+2 lines)" count stood, and the buffer's
/// own next lines move down below it.
#[test]
fn a_multi_line_suggestion_paints_every_line_under_the_caret() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_body_suggestion(tmp.path());
    let before = app.editor.lines.clone();
    let rows = screen_rows(&mut app, 120, 30);
    let at = |needle: &str| {
        rows.iter()
            .position(|r| r.contains(needle))
            .unwrap_or_else(|| panic!("{needle} is on screen:\n{}", rows.join("\n")))
    };
    let def = at("def add(a, b):");
    assert_eq!(at("total = a + b"), def + 1);
    assert_eq!(at("return total"), def + 2);
    assert!(
        at("print(add(1, 2))") > def + 2,
        "the buffer's lines move down"
    );
    assert!(!rows.iter().any(|r| r.contains("(+2 lines)")));
    assert_eq!(app.editor.lines, before, "nothing is inserted by painting");
}

/// #1625 negative: a suggestion taller than the rows below the caret
/// shows what fits and counts only the lines that did not.
#[test]
fn a_suggestion_taller_than_the_pane_counts_the_lines_left_over() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_body_suggestion(tmp.path());
    let seq = app.editor.edit_seq;
    let body: String = (1..=60).map(|i| format!("\n    x{i} = {i}")).collect();
    app.editor.ghost = Some((0, 14, seq, body));
    let rows = screen_rows(&mut app, 120, 30);
    let shown = (1..=60)
        .filter(|i| rows.iter().any(|r| r.contains(&format!("x{i} = {i}"))))
        .count();
    assert!(shown > 0 && shown < 60, "{shown} shown");
    let more = 60 - shown;
    let suffix = format!("(+{more} lines)");
    assert!(
        rows.iter()
            .any(|r| r.contains("def add(a, b):") && r.contains(&suffix)),
        "{suffix} on the caret row:\n{}",
        rows.join("\n")
    );
}

/// #1625: Ctrl+Right takes the next word of the suggestion (here the line
/// break, the indent and `total`) and leaves the rest live at the caret.
#[test]
fn ctrl_right_takes_the_next_word_and_keeps_the_rest_live() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_body_suggestion(tmp.path());
    app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL))
        .unwrap();
    assert_eq!(app.editor.lines[0], "def add(a, b):");
    assert_eq!(app.editor.lines[1], "    total");
    assert_eq!(app.editor.live_ghost(), Some(" = a + b\n    return total"));
    app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(app.editor.lines[1], "    total = a + b");
    assert_eq!(app.editor.lines[2], "    return total");
    assert!(app.editor.live_ghost().is_none());
}

/// #1625: the caret a Ctrl+Right leaves a remainder at counts as asked,
/// so Esc on that remainder does not send a fresh request there.
#[test]
fn escaping_a_partly_taken_suggestion_does_not_ask_again() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_body_suggestion(tmp.path());
    app.inline_config = Some(crate::inline_complete::Config {
        backend: crate::inline_complete::Backend::Claude {
            base_url: String::from("http://127.0.0.1:1"),
            api_key: String::from("test-key"),
        },
        model: String::from("claude-opus-5"),
    });
    app.inline_worker = Some(crate::inline_complete::Worker::spawn());
    app.inline_enabled = true;
    app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL))
        .unwrap();
    assert!(app.editor.live_ghost().is_some(), "a remainder is live");
    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert!(app.editor.live_ghost().is_none());
    app.editor.last_edit_at = Some(std::time::Instant::now() - std::time::Duration::from_secs(2));
    let first = app.inline_next_id;
    for _ in 0..5 {
        app.tick_inline_complete();
    }
    assert_eq!(app.inline_next_id, first, "no request after Esc");
}

/// #1625: Ctrl+Down takes one line of the suggestion at a time.
#[test]
fn ctrl_down_takes_the_suggestion_a_line_at_a_time() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_body_suggestion(tmp.path());
    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::CONTROL))
        .unwrap();
    assert_eq!(app.editor.lines[1], "    total = a + b");
    assert_eq!(app.editor.live_ghost(), Some("\n    return total"));
    assert_eq!(app.editor.lines[2], "", "the next line is not taken yet");
    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::CONTROL))
        .unwrap();
    assert_eq!(app.editor.lines[2], "    return total");
    assert!(app.editor.live_ghost().is_none(), "nothing is left");
}

/// #1625 negative: with no suggestion live, Ctrl+Right is the usual word
/// move and edits nothing.
#[test]
fn ctrl_right_without_a_suggestion_moves_by_word_and_edits_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "m.py", "def add(a, b):\n");
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 0;
    app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL))
        .unwrap();
    assert_eq!(app.editor.lines[0], "def add(a, b):");
    assert!(app.editor.cursor_col > 0, "the caret moved by a word");
    assert!(!app.editor.dirty);
}

#[test]
fn an_edit_makes_a_suggestion_stale_and_esc_dismisses_one() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.rs", "let x = \n");
    app.editor.cursor_col = 8;
    let seq = app.editor.edit_seq;
    app.editor.ghost = Some((0, 8, seq, String::from("1;")));
    assert!(app.editor.live_ghost().is_some());
    app.handle_key(crossterm::event::KeyEvent::new(
        KeyCode::Esc,
        KeyModifiers::NONE,
    ))
    .unwrap();
    assert!(app.editor.ghost.is_none(), "Esc dismisses");
    app.editor.ghost = Some((0, 8, seq, String::from("1;")));
    app.handle_key(crossterm::event::KeyEvent::new(
        KeyCode::Char('2'),
        KeyModifiers::NONE,
    ))
    .unwrap();
    assert!(app.editor.live_ghost().is_none(), "typing makes it stale");
}

/// #624 review: the tool trust gate refuses a changed tool definition with
/// the recovery it names, and lets the first and the unchanged one through.
#[test]
fn the_mcp_tool_gate_refuses_a_changed_definition() {
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(mcp_tool_trust(tmp.path(), "ext.go", "go", "fp1"), Ok(()));
    assert_eq!(mcp_tool_trust(tmp.path(), "ext.go", "go", "fp1"), Ok(()));
    let err = mcp_tool_trust(tmp.path(), "ext.go", "go", "fp2").unwrap_err();
    assert!(err.starts_with("refusing to run: the 'go' tool"), "{err}");
    assert!(err.contains("toggle the extension off and on"), "{err}");
}

fn popup_screen(app: &mut App, w: u16, h: u16) -> String {
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let mut screen = String::new();
    for y in 0..h {
        for x in 0..w {
            screen.push_str(term.backend().buffer()[(x, y)].symbol());
        }
        screen.push('\n');
    }
    screen
}

/// #347: what the language server says about a proposal's file while it is
/// checked goes to the popup, against the proposed row it names, and not to
/// the buffer's diagnostics; answering the proposal ends the check.
#[test]
fn a_proposals_diagnostics_show_in_the_popup_not_the_buffer() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("a.py");
    std::fs::write(&file, "x = 1\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.lsp = None;
    let _hook = queue_edit_proposal(&mut app, tmp.path(), &file, "1", "nope_undefined");
    app.sync_approval_check();
    assert!(
        app.approval_check.is_none(),
        "no server, nothing to check with"
    );
    // As a server would once the proposal's text was sent as the file's.
    app.approval_check = Some(crate::agent_approval::ProposalCheck {
        arrived: app.approvals[0].arrived,
        started: std::time::Instant::now(),
        path: file.clone(),
        opened: true,
        by_server: Default::default(),
    });
    let screen = popup_screen(&mut app, 110, 30);
    assert!(screen.contains("Checking the proposed file"), "{screen}");
    app.apply_diagnostics_updates(vec![crate::lsp::manager::DiagnosticsUpdate {
        path: file.clone(),
        server: "ruff".into(),
        diagnostics: vec![crate::lsp::manager::Diagnostic {
            start_line: 0,
            start_char: 4,
            end_line: 0,
            end_char: 18,
            severity: crate::lsp::manager::DiagnosticSeverity::Error,
            message: "Undefined name `nope_undefined`".into(),
            tags: Default::default(),
            code: None,
        }],
        pulled: false,
    }]);
    assert!(
        !app.lsp_diagnostics.contains_key(&file),
        "the buffer's diagnostics are the buffer's"
    );
    let screen = popup_screen(&mut app, 110, 30);
    assert!(
        screen.contains("1 error, 0 warnings in the proposed file"),
        "{screen}"
    );
    assert!(
        screen
            .lines()
            .any(|l| l.contains("+ x = nope_undefined") && l.contains("\u{25c0} Undefined name")),
        "the proposed row carries it: {screen}"
    );
    // Answered: the check ends with the proposal.
    app.approval_ui.as_mut().unwrap().shown_at -= crate::agent_approval::ARM_DELAY;
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert!(app.approvals.is_empty());
    app.sync_approval_check();
    assert!(app.approval_check.is_none());
}

/// #915: the servers keep a file open while any split group holds it, so a
/// checked proposal for a file held only in the other group hands the
/// servers back that buffer's text when the check ends, not the proposal's.
#[test]
fn a_checked_proposal_hands_back_a_buffer_held_in_another_split_group() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("a.py");
    std::fs::write(&file, "x = 1\n").unwrap();
    let notes = tmp.path().join("notes.txt");
    std::fs::write(&notes, "").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open_pinned(&file).unwrap();
    app.split_editor_dir(editor_layout::SplitDir::Horizontal, true);
    app.editor.open(&notes).unwrap();
    app.sync_lsp();
    let synced = |app: &App| app.lsp.as_ref().unwrap().synced_text_for_test(&file);
    let _hook = queue_edit_proposal(&mut app, tmp.path(), &file, "1", "nope_undefined");
    app.sync_approval_check();
    assert_eq!(synced(&app).as_deref(), Some("x = nope_undefined\n"));
    app.approvals.clear();
    app.sync_approval_check();
    app.sync_lsp();
    assert_eq!(synced(&app).as_deref(), Some("x = 1\n"));
}

/// #347's criterion against a real server: a proposal that uses an
/// undefined name shows the server's diagnostic in the popup before it is
/// approved. Ignored by default: it needs a Python language server that
/// publishes diagnostics for an open buffer (run it with `--ignored` where
/// one does); `a_proposals_diagnostics_show_in_the_popup_not_the_buffer`
/// covers the routing and rendering without one.
#[test]
#[ignore]
fn a_real_server_flags_the_proposal_before_approval() {
    if !["ruff", "pyright-langserver", "basedpyright-langserver"]
        .iter()
        .any(|b| crate::lsp::manager::is_on_path(b))
    {
        eprintln!("SKIPPED: no Python language server on PATH");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("a.py");
    std::fs::write(&file, "x = 1\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    if app.lsp.is_none() {
        eprintln!("SKIPPED: no language server manager");
        return;
    }
    let _hook = queue_edit_proposal(&mut app, tmp.path(), &file, "1", "nope_undefined");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        app.sync_approval_check();
        app.drain_lsp_diagnostics();
        let found = app.approval_check.as_ref().is_some_and(|c| {
            c.diagnostics()
                .iter()
                .any(|d| d.message.contains("nope_undefined"))
        });
        if found {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no server flagged the proposal: {:?}",
            app.approval_check
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(!app.lsp_diagnostics.contains_key(&file));
}

/// An agent's edit proposal, end to end through the App: the hook's
/// request opens the popup over everything, keys go to the popup and not
/// to the editor under it, and the answer reaches the hook's connection.
#[test]
fn an_agent_edit_proposal_opens_the_popup_and_enter_answers_the_hook() {
    use std::io::{BufRead, Write};
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("a.rs");
    std::fs::write(&file, "let x = 1;\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let sock = tmp.path().join("hook.sock");
    let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
    listener.set_nonblocking(true).unwrap();
    app.hook_listener = Some(listener);
    assert!(!app.drain_hook_requests());
    assert!(app.approval_ui.is_none());

    let mut hook = std::os::unix::net::UnixStream::connect(&sock).unwrap();
    let req = crate::agent_hook::EditRequest {
        agent: "claude-code".into(),
        tool: "Edit".into(),
        input: serde_json::json!({"file_path": file, "old_string": "1", "new_string": "2"}),
        cwd: tmp.path().into(),
    };
    writeln!(hook, "{}", serde_json::to_string(&req).unwrap()).unwrap();
    assert!(app.drain_hook_requests());
    assert!(app.modal_overlay_open());
    // A proposal the hook has stopped waiting for leaves with the popup.
    app.approvals[0].arrived -= crate::agent_hook::ANSWER_WINDOW;
    assert!(app.drain_hook_requests());
    assert!(app.approval_ui.is_none() && app.approvals.is_empty());
    let mut hook = std::os::unix::net::UnixStream::connect(&sock).unwrap();
    writeln!(hook, "{}", serde_json::to_string(&req).unwrap()).unwrap();
    assert!(app.drain_hook_requests());

    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let mut screen = String::new();
    for y in 0..30 {
        for x in 0..100 {
            screen.push_str(term.backend().buffer()[(x, y)].symbol());
        }
    }
    assert!(
        screen.contains("claude-code wants to edit a.rs"),
        "{screen}"
    );
    assert!(screen.contains("+ let x = 2;") && screen.contains("- let x = 1;"));

    // Too soon after it appeared: the Enter is swallowed, not an approval.
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert!(app.approval_ui.is_some());
    app.approval_ui.as_mut().unwrap().shown_at -= crate::agent_approval::ARM_DELAY;
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert!(app.approval_ui.is_none() && app.approvals.is_empty());
    let mut line = String::new();
    std::io::BufReader::new(hook).read_line(&mut line).unwrap();
    assert_eq!(
        serde_json::from_str::<crate::agent_hook::Decision>(line.trim()).unwrap(),
        crate::agent_hook::Decision::Allow
    );
    // Approving answers the agent; croft itself does not write the file.
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "let x = 1;\n");
}

/// #345: the AGENT LANE rows are each agent then its files, unreviewed
/// first, labelled relative to the workspace root.
#[test]
fn agent_lane_rows_list_each_agent_then_its_files_unreviewed_first() {
    use crate::widgets::agent_lane::LaneRow;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    let a = root.join("src/a.rs");
    let b = root.join("src/b.rs");
    std::fs::write(&a, "a").unwrap();
    std::fs::write(&b, "b").unwrap();
    let mut app = App::new(root.clone()).unwrap();
    assert!(
        app.agent_lane_panel_rows().is_empty(),
        "nothing to show before any agent wrote"
    );
    let working = vec![String::from("claude")];
    app.agent_ledger.record_write(&a, 1, &working);
    app.agent_ledger.record_write(&b, 2, &working);
    app.agent_ledger.mark_reviewed("claude", &a, 1, None);
    let rows = app.agent_lane_panel_rows();
    assert_eq!(
        rows[0],
        LaneRow::Agent {
            name: String::from("claude"),
            unreviewed: 1
        }
    );
    let files: Vec<(String, bool)> = rows[1..]
        .iter()
        .map(|r| match r {
            LaneRow::File {
                label, unreviewed, ..
            } => (label.clone(), *unreviewed),
            other => panic!("expected a file row, got {other:?}"),
        })
        .collect();
    assert_eq!(
        files,
        vec![
            (String::from("src/b.rs"), true),
            (String::from("src/a.rs"), false)
        ]
    );
}

/// #345: "Agents: Open Agent Lane" shows the section, open, even after it
/// was hidden from the ⋯ menu; with no agent activity it says so instead.
#[test]
fn agents_open_agent_lane_shows_the_section_open() {
    use crate::widgets::command_palette::Command;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let f = root.join("x.rs");
    std::fs::write(&f, "x").unwrap();
    let mut app = App::new(root).unwrap();
    app.run_command(Command::OpenAgentLaneSection);
    assert!(app.status.contains("no agent"), "status: {}", app.status);

    app.agent_ledger
        .record_write(&f, 1, &[String::from("claude")]);
    if app.explorer_views.is_visible(ExplorerView::AgentLane) {
        app.toggle_explorer_view(ExplorerView::AgentLane);
    }
    assert!(app.agent_lane_panel.collapsed);
    app.run_command(Command::OpenAgentLaneSection);
    assert!(app.explorer_views.is_visible(ExplorerView::AgentLane));
    assert!(!app.agent_lane_panel.collapsed);
    assert_eq!(app.sidebar_view, SidebarView::Explorer);
}

/// The phone loop (#359), end to end through the App: a proposal sends one
/// notification whose Approve link carries the proposal's token, and that
/// token, sent back the way `croft decide` sends it, answers the hook and
/// closes the popup without a key pressed.
#[test]
fn an_approval_notifies_once_and_its_approve_token_answers_the_hook() {
    use std::io::{BufRead, Write};
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("a.rs");
    std::fs::write(&file, "let x = 1;\n").unwrap();
    let out = tmp.path().join("notified");
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.notifier = crate::notifications::Notifier::new(&[crate::prefs::NotificationSink {
        kind: String::from("command"),
        argv: vec![
            String::from("/bin/sh"),
            String::from("-c"),
            format!(
                "printf '%s %s\\n' \"$CROFT_EVENT\" \"$CROFT_APPROVE_LINK\" >> '{}'",
                out.display()
            ),
        ],
        ..Default::default()
    }]);
    let sock = tmp.path().join("hook.sock");
    let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
    listener.set_nonblocking(true).unwrap();
    app.hook_listener = Some(listener);

    let mut hook = std::os::unix::net::UnixStream::connect(&sock).unwrap();
    let req = crate::agent_hook::EditRequest {
        agent: "claude-code".into(),
        tool: "Edit".into(),
        input: serde_json::json!({"file_path": file, "old_string": "1", "new_string": "2"}),
        cwd: tmp.path().into(),
    };
    writeln!(hook, "{}", serde_json::to_string(&req).unwrap()).unwrap();
    assert!(app.drain_hook_requests());
    // Later passes do not notify it again.
    app.drain_hook_requests();
    app.drain_hook_requests();
    let token = app.approvals[0].token.clone();
    // One worker delivers in order, so once this sentinel lands, anything
    // the passes above sent has landed too.
    app.notifier.emit(
        crate::notifications::Event::Osc9 {
            pane: String::from("p"),
            message: String::from("sentinel"),
        },
        tmp.path(),
        "h",
    );
    crate::test_budget::await_spawned(
        std::time::Duration::from_secs(5),
        "the notification",
        || std::fs::read_to_string(&out).is_ok_and(|s| s.contains("osc9")),
    );
    let sent = std::fs::read_to_string(&out).unwrap();
    let approvals: Vec<_> = sent
        .lines()
        .filter(|l| l.starts_with("approval_pending "))
        .collect();
    assert_eq!(approvals.len(), 1, "{sent}");
    let (event, link) = approvals[0].split_once(' ').unwrap();
    assert_eq!(event, "approval_pending");
    let link = crate::deep_link::parse(link).unwrap();
    assert_eq!(link.decide.as_ref().unwrap().token, token);
    assert!(link.decide.as_ref().unwrap().allow);
    assert_eq!(
        link.path.as_deref(),
        Some(tmp.path().to_str().unwrap()),
        "the link names the workspace whose croft holds the edit"
    );

    let mut phone = std::os::unix::net::UnixStream::connect(&sock).unwrap();
    writeln!(
        phone,
        "{}",
        serde_json::json!({"decide": {"token": token, "allow": true}})
    )
    .unwrap();
    assert!(app.drain_hook_requests());
    assert!(app.approval_ui.is_none() && app.approvals.is_empty());
    let mut reply = String::new();
    std::io::BufReader::new(phone)
        .read_line(&mut reply)
        .unwrap();
    assert!(reply.contains("\"ok\":true"), "{reply}");
    let mut line = String::new();
    std::io::BufReader::new(hook).read_line(&mut line).unwrap();
    assert_eq!(
        serde_json::from_str::<crate::agent_hook::Decision>(line.trim()).unwrap(),
        crate::agent_hook::Decision::Allow
    );
}

/// #345: a lane row counts the lines added and removed since the content
/// the user reviewed, follows later writes, and shows nothing for a row
/// with no review to count against.
#[test]
fn a_lane_row_counts_lines_added_and_removed_since_the_review() {
    use crate::widgets::agent_lane::LaneRow;
    let tmp = tempfile::tempdir().unwrap();
    let hist = tempfile::tempdir().unwrap();
    let f = tmp.path().join("f.rs");
    let g = tmp.path().join("g.rs");
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.history_root = hist.path().to_path_buf();
    let working = vec![String::from("claude")];
    let write = |app: &mut App, path: &Path, text: &str| {
        std::fs::write(path, text).unwrap();
        app.agent_ledger.record_write(
            path,
            crate::agent_lane::content_hash(text.as_bytes()),
            &working,
        );
    };
    let changes_of = |app: &mut App, path: &Path| {
        app.agent_lane_panel_rows()
            .into_iter()
            .find_map(|r| match r {
                LaneRow::File {
                    path: p, changes, ..
                } if p == path => Some(changes),
                _ => None,
            })
            .expect("the file has a row")
    };

    write(&mut app, &f, "a\nb\nc\n");
    write(&mut app, &g, "new\n");
    assert_eq!(
        changes_of(&mut app, &g),
        None,
        "never reviewed: no baseline"
    );
    assert!(app.mark_agent_file_reviewed("claude", &f));
    assert_eq!(changes_of(&mut app, &f), Some((0, 0)));

    // Replace one line and add two.
    write(&mut app, &f, "a\nB\nc\nd\ne\n");
    assert_eq!(changes_of(&mut app, &f), Some((3, 1)));
    // A later write is counted afresh, not served from the cache.
    write(&mut app, &f, "a\n");
    assert_eq!(changes_of(&mut app, &f), Some((0, 2)));

    // Painted beside the label.
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
    app.show_agent_lane_section();
    term.draw(|f| app.render(f)).unwrap();
    let buf = term.backend().buffer();
    let screen: String = (0..buf.area.height)
        .map(|y| {
            (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
                + "\n"
        })
        .collect();
    assert!(screen.contains("f.rs  +0 −2"), "{screen}");
}

/// #345: clicking an unreviewed lane row's ● marks that file reviewed;
/// clicking its name still opens the diff.
#[test]
fn clicking_a_lane_rows_dot_marks_the_file_reviewed() {
    use crate::widgets::agent_lane::LaneRow;
    let tmp = tempfile::tempdir().unwrap();
    let hist = tempfile::tempdir().unwrap();
    let f = tmp.path().join("f.rs");
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.history_root = hist.path().to_path_buf();
    std::fs::write(&f, "a\n").unwrap();
    app.agent_ledger.record_write(
        &f,
        crate::agent_lane::content_hash(b"a\n"),
        &[String::from("claude")],
    );
    app.show_agent_lane_section();
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let buf = term.backend().buffer().clone();
    let (x, y) = (0..buf.area.height)
        .find_map(|y| {
            let row: String = (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect();
            let at = row.find("● f.rs")?;
            Some((row[..at].chars().count() as u16, y))
        })
        .expect("the unreviewed row is painted");
    let unreviewed = |app: &mut App| {
        app.agent_lane_panel_rows().into_iter().any(|r| {
            matches!(
                r,
                LaneRow::File {
                    unreviewed: true,
                    ..
                }
            )
        })
    };
    assert!(unreviewed(&mut app));
    let click = |app: &mut App, column: u16| {
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(MouseButton::Left),
            column,
            row: y,
            modifiers: KeyModifiers::NONE,
        });
    };
    // The name opens the diff (refused here: never reviewed) and marks
    // nothing.
    click(&mut app, x + 3);
    assert!(unreviewed(&mut app), "{}", app.status);
    click(&mut app, x);
    assert!(!unreviewed(&mut app), "{}", app.status);
    assert!(app.status.contains("f.rs: reviewed"), "{}", app.status);
}

/// #347: `a` in the approval popup approves this edit and the same agent's
/// next ones for ten minutes, with a countdown chip; another agent still
/// asks, and Agents: Stop Auto-Approving (or the time running out) ends it.
#[test]
fn approve_all_lets_one_agent_through_until_stopped() {
    use std::io::{BufRead, Write};
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.rs"), "let x = 1;\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let sock = tmp.path().join("hook.sock");
    let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
    listener.set_nonblocking(true).unwrap();
    app.hook_listener = Some(listener);
    let propose = |app: &mut App, agent: &str| {
        let mut hook = std::os::unix::net::UnixStream::connect(&sock).unwrap();
        let req = crate::agent_hook::EditRequest {
            agent: agent.into(),
            tool: "Edit".into(),
            input: serde_json::json!({"file_path": tmp.path().join("a.rs"), "old_string": "1", "new_string": "2"}),
            cwd: tmp.path().into(),
        };
        writeln!(hook, "{}", serde_json::to_string(&req).unwrap()).unwrap();
        app.drain_hook_requests();
        hook
    };
    let answer = |hook: std::os::unix::net::UnixStream| {
        hook.set_read_timeout(Some(std::time::Duration::from_millis(200)))
            .unwrap();
        let mut line = String::new();
        let _ = std::io::BufReader::new(hook).read_line(&mut line);
        serde_json::from_str::<crate::agent_hook::Decision>(line.trim()).ok()
    };

    let first = propose(&mut app, "claude-code");
    let other = propose(&mut app, "other-agent");
    app.approval_ui.as_mut().unwrap().shown_at -= crate::agent_approval::ARM_DELAY;
    app.handle_key(key(KeyCode::Char('a'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(answer(first), Some(crate::agent_hook::Decision::Allow));
    assert!(app.status.contains("for 10 minutes"), "{}", app.status);
    assert_eq!(app.approvals.len(), 1, "the other agent still asks");
    assert_eq!(app.approvals[0].request.agent, "other-agent");

    // The same agent's next edit goes straight through.
    let next = propose(&mut app, "claude-code");
    assert_eq!(answer(next), Some(crate::agent_hook::Decision::Allow));
    assert_eq!(app.approvals.len(), 1);
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 30)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let buf = term.backend().buffer();
    let bottom: String = (0..160).map(|x| buf[(x, 29)].symbol()).collect();
    assert!(
        bottom.contains("auto-approving claude-code 9:")
            || bottom.contains("auto-approving claude-code 10:00"),
        "{bottom}"
    );

    app.run_command(crate::widgets::command_palette::Command::StopAutoApprove);
    let after = propose(&mut app, "claude-code");
    assert_eq!(app.approvals.len(), 2, "asks again once stopped");
    drop((after, other));

    // A window that has run out closes by itself.
    app.auto_approve = Some((String::from("x"), std::time::Instant::now()));
    app.drain_hook_requests();
    assert!(app.auto_approve.is_none());
}

/// #345: the agent review queue, and what was marked reviewed, survive a
/// restart and a re-root away and back; the Explorer's dots come back too.
#[test]
fn the_agent_review_queue_survives_a_restart_and_a_reroot() {
    let tmp = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap().keep().join("agent_lane.json");
    let a = tmp.path().join("a.rs");
    let b = tmp.path().join("b.rs");
    std::fs::write(&a, "a").unwrap();
    std::fs::write(&b, "b").unwrap();
    let working = [String::from("claude")];
    let root = tmp.path().canonicalize().unwrap();
    let (a, b) = (root.join("a.rs"), root.join("b.rs"));

    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.agent_ledger_store = Some(store.clone());
    app.agent_ledger.record_write(&a, 1, &working);
    app.agent_ledger.record_write(&b, 2, &working);
    app.agent_ledger.mark_reviewed("claude", &b, 2, None);
    app.persist_agent_ledger(true);
    drop(app);

    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.agent_ledger_store = Some(store.clone());
    app.agent_ledger = app.load_agent_ledger(&root);
    app.sync_agent_lane_decorations();
    assert!(app.agent_ledger.is_unreviewed(&a), "the queue came back");
    assert!(!app.agent_ledger.is_unreviewed(&b), "and b stays reviewed");
    assert!(app.tree.agent_touched.contains(&a), "the Explorer's dot");

    // Away and back: each workspace keeps its own queue.
    app.change_workspace_root(other.path().to_path_buf());
    assert!(app.agent_ledger.is_empty());
    app.change_workspace_root(tmp.path().to_path_buf());
    assert!(app.agent_ledger.is_unreviewed(&a));

    // Reviewing everything empties the queue and drops its saved entry.
    app.agent_ledger.mark_reviewed("claude", &a, 1, None);
    app.agent_ledger.forget("claude");
    app.persist_agent_ledger(true);
    let saved = std::fs::read_to_string(&store).unwrap();
    assert!(!saved.contains(&root.display().to_string()), "{saved}");
}

/// #345: the lane is reachable from the keyboard: "Agents: Review a Changed
/// File" lists every lane's files, and Enter opens the chosen one's diff.
#[test]
fn the_lane_file_picker_opens_the_chosen_files_diff() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let a = root.join("a.rs");
    std::fs::write(&a, "a\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.run_command(crate::widgets::command_palette::Command::PickAgentLaneFile);
    assert_eq!(app.status, "No agent has changed a file");
    assert!(app.list_picker.is_none());

    app.agent_ledger
        .record_write(&a, 1, &[String::from("claude")]);
    app.run_command(crate::widgets::command_palette::Command::PickAgentLaneFile);
    let picker = app.list_picker.as_ref().expect("a picker");
    assert_eq!(picker.rows.len(), 1);
    assert!(
        picker.rows[0].label.contains("a.rs"),
        "{}",
        picker.rows[0].label
    );
    assert!(picker.rows[0].label.contains("(claude)"));
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert!(app.list_picker.is_none());
    // Never reviewed, so the diff is refused with the reason, as a click on
    // the row is: the pick reached `diff_agent_lane_row`.
    assert!(app.status.contains("review"), "{}", app.status);
}

#[test]
fn edit_then_approve_sends_the_saved_text_as_the_tools_input() {
    // #347: `e` opens the proposal as a scratch file with the popup put
    // away; closing it unsaved brings the popup back; saving it approves
    // with input that makes the agent's own tool write the edited text.
    use std::io::{BufRead, Write};
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("a.rs");
        std::fs::write(&target, "let x = 1;\n").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        let sock = tmp.path().join("hook.sock");
        let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        listener.set_nonblocking(true).unwrap();
        app.hook_listener = Some(listener);
        let mut hook = std::os::unix::net::UnixStream::connect(&sock).unwrap();
        let input = serde_json::json!({"file_path": target, "old_string": "1", "new_string": "2"});
        let req = crate::agent_hook::EditRequest {
            agent: "claude-code".into(),
            tool: "Edit".into(),
            input,
            cwd: tmp.path().into(),
        };
        writeln!(hook, "{}", serde_json::to_string(&req).unwrap()).unwrap();
        // A tab with unsaved work, which opening the proposal must not
        // replace.
        let other = tmp.path().join("b.rs");
        std::fs::write(&other, "fn b() {}\n").unwrap();
        app.editor.open_pinned(&other).unwrap();
        app.editor.lines[0] = String::from("fn b() { unsaved }");
        app.editor.dirty = true;
        app.drain_hook_requests();
        let arm = |app: &mut App| {
            app.approval_ui.as_mut().unwrap().shown_at -= crate::agent_approval::ARM_DELAY;
        };

        arm(&mut app);
        app.handle_key(key(KeyCode::Char('e'), KeyModifiers::NONE))
            .unwrap();
        assert!(app.approval_ui.is_none(), "the popup is put away");
        let scratch = app.approval_edit.as_ref().unwrap().1.clone();
        assert_eq!(app.editor.path.as_deref(), Some(scratch.as_path()));
        assert_eq!(app.editor.lines[0], "let x = 2;", "the proposal's text");
        app.drain_hook_requests();
        assert!(app.approval_ui.is_none(), "and stays away while editing");

        // Closed unsaved: back to the popup, nothing answered.
        app.editor.close_active();
        app.drain_hook_requests();
        assert!(app.approval_edit.is_none() && app.approval_ui.is_some());
        assert_eq!(app.approvals.len(), 1);

        arm(&mut app);
        app.handle_key(key(KeyCode::Char('e'), KeyModifiers::NONE))
            .unwrap();
        app.editor.lines[0] = String::from("let x = 3;");
        app.editor.dirty = true;
        app.write_current_to_disk();
        assert!(app.status.contains("as you changed it"), "{}", app.status);
        assert!(app.approvals.is_empty() && app.approval_edit.is_none());

        hook.set_read_timeout(Some(std::time::Duration::from_millis(500)))
            .unwrap();
        let mut line = String::new();
        std::io::BufReader::new(hook).read_line(&mut line).unwrap();
        let crate::agent_hook::Decision::AllowEdited { input } =
            serde_json::from_str(line.trim()).unwrap()
        else {
            panic!("not an edited approval: {line}");
        };
        let read = |p: &std::path::Path| std::fs::read_to_string(p);
        let replay =
            crate::agent_approval::proposal_for("Edit", &input, tmp.path(), &read).unwrap();
        assert_eq!(
            replay.after, "let x = 3;\n",
            "the agent's Edit writes what was saved"
        );
        app.editor.open_pinned(&other).unwrap();
        assert!(app.editor.dirty, "the other tab kept its unsaved work");
        assert_eq!(app.editor.lines[0], "fn b() { unsaved }");
    });
}

#[test]
fn an_agent_edit_to_a_dirty_tab_goes_through_a_three_way_merge() {
    // #347: the proposal is computed from disk, so approving it over a tab
    // with unsaved edits would put the agent's write under them. Enter
    // opens a merge (disk, yours, the agent's) instead; auto-approve never
    // lets it through; saving with conflicts left approves nothing; the
    // resolved result is what the agent writes, and the tab holds it too.
    use std::io::{BufRead, Write};
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("a.rs");
        std::fs::write(&target, "one\ntwo\nthree\nfour\nfive\n").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.editor.open(&target).unwrap();
        app.editor.lines[0] = String::from("ONE"); // yours: line 1
        app.editor.lines[2] = String::from("mine"); // yours: line 3, in conflict
        app.editor.dirty = true;
        app.editor.pin_active(); // as any real edit does
        let sock = tmp.path().join("hook.sock");
        let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        listener.set_nonblocking(true).unwrap();
        app.hook_listener = Some(listener);
        // The agent's: lines 3 and 5 (Write, so the whole text).
        let mut hook = std::os::unix::net::UnixStream::connect(&sock).unwrap();
        let req = crate::agent_hook::EditRequest {
            agent: "claude-code".into(),
            tool: "Write".into(),
            input: serde_json::json!({"file_path": target, "content": "one\ntwo\ntheirs\nfour\nFIVE\n"}),
            cwd: tmp.path().into(),
        };
        writeln!(hook, "{}", serde_json::to_string(&req).unwrap()).unwrap();
        // Auto-approving this agent does not approve an edit under unsaved text.
        app.auto_approve = Some((
            String::from("claude-code"),
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        ));
        app.drain_hook_requests();
        assert_eq!(app.approvals.len(), 1, "auto-approved over unsaved edits");
        app.auto_approve = None;
        assert!(app.approval_ui.as_ref().unwrap().target_dirty);

        app.approval_ui.as_mut().unwrap().shown_at -= crate::agent_approval::ARM_DELAY;
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.approvals.len(), 1, "Enter approved over unsaved edits");
        let scratch = app.approval_edit.as_ref().unwrap().1.clone();
        assert_eq!(app.editor.path.as_deref(), Some(scratch.as_path()));
        let mv = app.editor.merge.as_ref().expect("a merge view");
        assert_eq!(mv.conflicts.len(), 1, "line 3 changed on both sides");
        let conflict_row = mv.conflicts[0].result_start;
        assert_eq!(
            app.editor.lines,
            ["ONE", "two", "three", "four", "FIVE"],
            "each side's own change applied, the conflict holding the base"
        );

        // Saving with the conflict open approves nothing.
        app.editor.dirty = true;
        app.write_current_to_disk();
        assert!(app.status.contains("1 conflict left"), "{}", app.status);
        assert_eq!(app.approvals.len(), 1);

        app.editor.cursor_row = conflict_row;
        app.merge_apply(crate::merge_editor::ConflictState::Incoming);
        app.write_current_to_disk();
        assert!(app.approvals.is_empty(), "{}", app.status);
        assert!(
            app.status.contains("your tab holds the same text"),
            "{}",
            app.status
        );

        hook.set_read_timeout(Some(std::time::Duration::from_millis(500)))
            .unwrap();
        let mut line = String::new();
        std::io::BufReader::new(hook).read_line(&mut line).unwrap();
        let crate::agent_hook::Decision::AllowEdited { input } =
            serde_json::from_str(line.trim()).unwrap()
        else {
            panic!("not an edited approval: {line}");
        };
        assert_eq!(input["content"], "ONE\ntwo\ntheirs\nfour\nFIVE\n");
        // The user's tab is still there, and holds what the agent is about
        // to write.
        app.editor.open_pinned(&target).unwrap();
        assert_eq!(app.editor.lines, ["ONE", "two", "theirs", "four", "FIVE"]);
    });
}

#[test]
fn completing_an_approval_merge_approves_it_and_stages_nothing() {
    // #1353: the merge of an agent's proposal with unsaved edits wore the
    // git merge editor's chrome ("git stages", "Complete Merge stages the
    // file"), and Complete Merge, after the save that approved it, ran
    // `git add` on croft's scratch copy: "Stage failed: … outside repository".
    use std::io::{BufRead, Write};
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(tmp.path())
                .args(args)
                .output()
                .unwrap()
        };
        git(&["init", "-q"]);
        let target = tmp.path().join("a.rs");
        std::fs::write(&target, "one\ntwo\nthree\n").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.editor.open(&target).unwrap();
        app.editor.lines[1] = String::from("mine");
        app.editor.dirty = true;
        app.editor.pin_active();
        let sock = tmp.path().join("hook.sock");
        let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        listener.set_nonblocking(true).unwrap();
        app.hook_listener = Some(listener);
        let mut hook = std::os::unix::net::UnixStream::connect(&sock).unwrap();
        let req = crate::agent_hook::EditRequest {
            agent: "claude-code".into(),
            tool: "Write".into(),
            input: serde_json::json!({"file_path": target, "content": "one\ntheirs\nthree\n"}),
            cwd: tmp.path().into(),
        };
        writeln!(hook, "{}", serde_json::to_string(&req).unwrap()).unwrap();
        app.drain_hook_requests();
        app.approval_ui.as_mut().unwrap().shown_at -= crate::agent_approval::ARM_DELAY;
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();
        assert!(app.editor.merge.is_some(), "precondition: the merge is up");

        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 40)).unwrap();
        term.draw(|f| app.render(f)).unwrap();
        let screen = screen_text(&term);
        assert!(
            screen.contains("MERGE (claude-code's proposal"),
            "the header names what is being merged: {screen}"
        );
        assert!(
            screen.contains("save to approve"),
            "the Result row says how to approve: {screen}"
        );
        assert!(!screen.contains("git stages"), "{screen}");
        assert!(!screen.contains("stages the file"), "{screen}");

        app.merge_apply(crate::merge_editor::ConflictState::Incoming);
        app.run_command(crate::widgets::command_palette::Command::MergeComplete);
        assert!(app.approvals.is_empty(), "approved: {}", app.status);
        assert!(!app.status.contains("Stage failed"), "{}", app.status);
        assert!(app.status.starts_with("Approved"), "{}", app.status);
        let staged = git(&["diff", "--cached", "--name-only"]);
        assert!(staged.stdout.is_empty(), "nothing staged");

        hook.set_read_timeout(Some(std::time::Duration::from_millis(500)))
            .unwrap();
        let mut line = String::new();
        std::io::BufReader::new(hook).read_line(&mut line).unwrap();
        let crate::agent_hook::Decision::AllowEdited { input } =
            serde_json::from_str(line.trim()).unwrap()
        else {
            panic!("not an edited approval: {line}");
        };
        assert_eq!(input["content"], "one\ntheirs\nthree\n");
    });
}
