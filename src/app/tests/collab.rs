//! Live collaboration: owner and guest sessions and their shared edits.

use super::*;

/// #862 negative: edits the user already chose to leave behind at the
/// connect prompt (D records them; the answer itself would start a real
/// ssh here) go with the handoff without a second question. One more edit
/// after that answer is asked about.
#[test]
fn edits_left_at_the_connect_prompt_are_not_asked_about_again() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, a) = app_with_unsaved_file(tmp.path());
    app.remote_handoff_ok = app.unsaved_stamp();
    ready_to_hand_off(&mut app, crate::install_session::InstallEvent::CanLaunch);
    app.poll_install_session();
    assert!(app.quit, "already answered: {}", app.status);
    assert!(app.pending_unsaved.is_none());
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "alpha\n");

    let tmp = tempfile::tempdir().unwrap();
    let (mut app, _) = app_with_unsaved_file(tmp.path());
    app.remote_handoff_ok = app.unsaved_stamp();
    type_into_editor(&mut app, "y");
    ready_to_hand_off(&mut app, crate::install_session::InstallEvent::CanLaunch);
    app.poll_install_session();
    assert!(!app.quit, "the later edit was never agreed to");
    assert!(app.pending_unsaved.is_some());
}

#[test]
fn typist_change_stashes_and_restores_per_participant_carets() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("f.txt");
    std::fs::write(&file, "hello world\nsecond line\n").unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.open_at(&file, 0, 0).unwrap();

    // Participant 1 starts typing with the cursor at (1, 3): no stash to
    // restore, cursor untouched.
    app.editor.cursor_row = 1;
    app.editor.cursor_col = 3;
    app.apply_typist_change(1);
    assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (1, 3));

    // Participant 1 moves to (0, 2); then participant 2 takes over: 1's
    // caret is parked where they left it.
    app.editor.cursor_row = 0;
    app.editor.cursor_col = 2;
    app.apply_typist_change(2);
    let parked = app.session_carets.get(&1).expect("caret parked for 1");
    assert_eq!((parked.row, parked.col), (0, 2));

    // Participant 2 works at (1, 4); participant 1 returns: 2 is parked and
    // 1's caret is restored.
    app.editor.cursor_row = 1;
    app.editor.cursor_col = 4;
    app.apply_typist_change(1);
    assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (0, 2));
    let parked = app.session_carets.get(&2).expect("caret parked for 2");
    assert_eq!((parked.row, parked.col), (1, 4));

    // A parked caret past the end of a shrunken buffer restores clamped.
    app.session_carets.insert(
        3,
        SessionCaret {
            path: app.editor.path.clone().unwrap(),
            row: 99,
            col: 99,
        },
    );
    app.apply_typist_change(3);
    assert!(app.editor.cursor_row < app.editor.lines.len());
    let line_len = app.editor.lines[app.editor.cursor_row].chars().count();
    assert!(app.editor.cursor_col <= line_len);
}

/// Headless end-to-end of the Phase D live wiring: two Apps on the same
/// workspace (an owner and a solo guest) connected through a real relay
/// socket. The guest bootstraps the owner's buffer, edits flow both ways and
/// converge, the guest never echoes an applied edit back, and the save-owner
/// gates hold (guest Cmd+S is a no-op with a hint; the owner writes).
#[test]
fn collab_solo_apps_bootstrap_edit_and_gate_saves() {
    use std::time::{Duration, Instant};
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("f.txt");
    std::fs::write(&file, "hello world").unwrap();
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
    let mut guest = App::new(tmp.path().to_path_buf()).unwrap();
    guest.collab_config = Some((socket.clone(), crate::collab::CollabRole::Guest));
    owner.open_file_at_launch(&file);
    guest.open_file_at_launch(&file);

    let pump_until =
        |owner: &mut App, guest: &mut App, what: &str, done: &dyn Fn(&App, &App) -> bool| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !done(owner, guest) {
                assert!(Instant::now() < deadline, "never settled: {what}");
                owner.poll_collab();
                guest.poll_collab();
                std::thread::sleep(Duration::from_millis(5));
            }
        };

    // Guest opens the shared file and bootstraps it from the owner's buffer.
    pump_until(&mut owner, &mut guest, "bootstrap", &|_, g| {
        g.collab.as_ref().is_some_and(|s| s.is_live("f.txt"))
    });

    // Guest types; the owner's buffer converges (through poll_collab's
    // extract, the relay, and apply_collab_spans on the owner side).
    guest.editor.insert_str("GUEST ");
    pump_until(
        &mut owner,
        &mut guest,
        "guest edit reaches owner",
        &|o, _| o.editor.lines[0].contains("GUEST"),
    );

    // Concurrent edits from both sides converge to identical buffers.
    owner.editor.cursor_row = 0;
    owner.editor.cursor_col = 0;
    owner.editor.insert_str("OWNER ");
    pump_until(
        &mut owner,
        &mut guest,
        "concurrent edits converge",
        &|o, g| o.editor.lines == g.editor.lines && o.editor.lines[0].contains("OWNER"),
    );

    // Settle: buffer convergence does not imply the WIRE is drained (#56).
    // Typing sends caret frames besides the edits, every drained event —
    // a caret included — reports redraw-worthy from poll_collab, and the
    // relay thread can deliver a trailing caret after the convergence
    // predicate already passed. Drain those, bounded so a genuine echo
    // storm still reaches the silence check below and fails there.
    let settle = Instant::now() + Duration::from_secs(5);
    loop {
        let owner_drained = owner.poll_collab();
        let guest_drained = guest.poll_collab();
        if !owner_drained && !guest_drained {
            break;
        }
        assert!(
            Instant::now() < settle,
            "the wire never went quiet: an echo storm, not a trailing caret"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    // No echo: with the wire drained and both sides idle, it stays quiet.
    let settled = owner.editor.lines.clone();
    for _ in 0..20 {
        assert!(!owner.poll_collab(), "owner must be quiescent");
        assert!(!guest.poll_collab(), "guest must be quiescent");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(owner.editor.lines, settled);
    assert_eq!(guest.editor.lines, settled);

    // Save-owner gate: the guest's Cmd+S never writes shared files.
    let disk_before = std::fs::read_to_string(&file).unwrap();
    guest.save();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), disk_before);
    assert!(
        guest.status.contains("owner saves"),
        "guest gets a hint, got: {}",
        guest.status
    );
    // Nor does File: Save with Sudo (#1379).
    let panes = guest.terminals.len();
    guest.run_command(crate::widgets::command_palette::Command::SaveWithSudo);
    assert_eq!(guest.terminals.len(), panes);
    assert!(guest.pending_sudo_saves.is_empty());
    assert!(guest.status.contains("owner saves"), "{}", guest.status);

    // The owner writes the converged text.
    owner.save();
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        settled.join("\n"),
        "owner save must persist the converged buffer"
    );
}

/// A relay on a temp socket with `plan.txt` from #1147 beside it, plus an
/// owner and a `--solo` guest that have both opened it and bootstrapped.
fn collab_owner_and_guest_on_plan() -> (tempfile::TempDir, std::path::PathBuf, App, App) {
    use std::time::{Duration, Instant};
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("plan.txt");
    std::fs::write(&file, "Owner: alice\nDue: Friday").unwrap();
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
    let mut guest = App::new(tmp.path().to_path_buf()).unwrap();
    guest.collab_config = Some((socket, crate::collab::CollabRole::Guest));
    owner.open_file_at_launch(&file);
    guest.open_file_at_launch(&file);
    pump_apps_until(&mut [&mut owner, &mut guest], "bootstrap", |apps| {
        apps[1]
            .collab
            .as_ref()
            .is_some_and(|s| s.is_live("plan.txt"))
    });
    (tmp, file, owner, guest)
}

/// Poll every app's collab session until `done` holds (5s bound).
fn pump_apps_until(apps: &mut [&mut App], what: &str, done: impl Fn(&[&mut App]) -> bool) {
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done(apps) {
        assert!(Instant::now() < deadline, "never settled: {what}");
        for app in apps.iter_mut() {
            app.poll_collab();
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The guest's edit to line 2 of `plan.txt`, typed and synced to the owner.
fn guest_moves_the_due_date(owner: &mut App, guest: &mut App) {
    guest.editor.cursor_row = 1;
    guest.editor.cursor_col = "Due: Friday".len();
    guest.editor.insert_str(" -> moved to Monday");
    pump_apps_until(&mut [owner, guest], "the edit reaches the owner", |apps| {
        apps[0]
            .editor
            .lines
            .get(1)
            .is_some_and(|l| l.ends_with("Monday"))
    });
}

/// #1147: once the owner quits, the guest's Cmd+S writes the file instead
/// of deferring to an owner that is gone.
#[test]
fn a_guest_saves_to_disk_once_the_owner_quits() {
    use std::time::{Duration, Instant};
    let (_tmp, file, mut owner, mut guest) = collab_owner_and_guest_on_plan();
    guest_moves_the_due_date(&mut owner, &mut guest);
    drop(owner);
    let until = Instant::now() + Duration::from_secs(3);
    while Instant::now() < until && !guest.status.contains("owner left") {
        guest.poll_collab();
        std::thread::sleep(Duration::from_millis(5));
    }
    guest.save();
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "Owner: alice\nDue: Friday -> moved to Monday",
        "{}",
        guest.status
    );
}

/// #1147: a new owner on the same relay gets the edits the stranded guest
/// made after the first owner left, instead of the two forking.
#[test]
fn a_new_owner_receives_the_stranded_guests_edits() {
    use std::time::{Duration, Instant};
    let (tmp, file, mut owner, mut guest) = collab_owner_and_guest_on_plan();
    guest_moves_the_due_date(&mut owner, &mut guest);
    let socket = owner.collab_config.clone().unwrap().0;
    drop(owner);
    let until = Instant::now() + Duration::from_secs(3);
    while Instant::now() < until && !guest.status.contains("owner left") {
        guest.poll_collab();
        std::thread::sleep(Duration::from_millis(5));
    }
    guest.editor.cursor_row = 0;
    guest.editor.cursor_col = "Owner: alice".len();
    guest.editor.insert_str(" and bob");
    let mut owner2 = App::new(tmp.path().to_path_buf()).unwrap();
    owner2.collab_config = Some((socket, crate::collab::CollabRole::Owner));
    owner2.open_file_at_launch(&file);
    pump_apps_until(
        &mut [&mut owner2, &mut guest],
        "the new owner converges",
        |apps| {
            apps[0].editor.lines == apps[1].editor.lines
                && apps[0].editor.lines[0].ends_with("and bob")
        },
    );
    assert_eq!(
        guest.editor.lines,
        vec!["Owner: alice and bob", "Due: Friday -> moved to Monday"]
    );
}

/// While the owner is still there, a guest's save stays with the owner,
/// however long the session runs between saves.
#[test]
fn a_guest_save_still_defers_to_an_owner_that_is_present() {
    use std::time::{Duration, Instant};
    let (_tmp, file, mut owner, mut guest) = collab_owner_and_guest_on_plan();
    guest_moves_the_due_date(&mut owner, &mut guest);
    let until = Instant::now() + Duration::from_millis(2500);
    while Instant::now() < until {
        owner.poll_collab();
        guest.poll_collab();
        std::thread::sleep(Duration::from_millis(5));
    }
    guest.save();
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "Owner: alice\nDue: Friday"
    );
    assert!(guest.status.contains("owner saves"), "{}", guest.status);
}

/// Splitting (or reopening) a live shared file loads the DISK copy, which
/// is stale by construction: a guest never writes shared files, so an
/// unsaved session's work exists only in buffers and replicas. The stale
/// duplicate used to become the active buffer, catch incoming spans at
/// nonsense offsets, and — on the first keystroke — broadcast a diff of
/// the live doc against the old disk text, reverting every peer to the
/// pre-session file. A fresh buffer of a live file must be seeded from the
/// replicated text, and every pane of the file must mirror the session.
#[test]
fn splitting_a_live_file_does_not_revert_the_session() {
    use std::time::{Duration, Instant};
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("f.txt");
    std::fs::write(&file, "hello world").unwrap();
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
    let mut guest = App::new(tmp.path().to_path_buf()).unwrap();
    guest.collab_config = Some((socket.clone(), crate::collab::CollabRole::Guest));
    owner.open_file_at_launch(&file);
    guest.open_file_at_launch(&file);
    let pump_until =
        |owner: &mut App, guest: &mut App, what: &str, done: &dyn Fn(&App, &App) -> bool| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !done(owner, guest) {
                assert!(Instant::now() < deadline, "never settled: {what}");
                owner.poll_collab();
                guest.poll_collab();
                std::thread::sleep(Duration::from_millis(5));
            }
        };
    pump_until(&mut owner, &mut guest, "bootstrap", &|_, g| {
        g.collab.as_ref().is_some_and(|s| s.is_live("f.txt"))
    });

    // An hour of unsaved session work, in miniature.
    guest.editor.insert_str("SESSION ");
    pump_until(&mut owner, &mut guest, "session edit converges", &|o, g| {
        o.editor.lines == g.editor.lines && o.editor.lines[0].contains("SESSION")
    });
    let session_text = owner.editor.lines.clone();

    // The guest splits: the duplicate is read from the stale disk copy.
    guest.split_editor_dir(crate::app::editor_layout::SplitDir::Horizontal, true);
    pump_until(
        &mut owner,
        &mut guest,
        "the split pane seeds from the live doc, not the disk",
        &|_, g| g.editor.lines == session_text,
    );

    // Typing in the split must extend the session, never revert it.
    guest.editor.insert_str("MORE ");
    pump_until(
        &mut owner,
        &mut guest,
        "the split edit reaches the owner",
        &|o, _| o.editor.lines[0].contains("MORE"),
    );
    assert!(
        owner.editor.lines[0].contains("SESSION"),
        "the pre-split session work survived, got: {:?}",
        owner.editor.lines
    );

    // Both guest panes mirror the live document.
    pump_until(&mut owner, &mut guest, "both panes mirror", &|o, g| {
        g.editor.lines == o.editor.lines
            && g.editor_layout
                .inactive_groups()
                .iter()
                .all(|grp| grp.editors.iter().all(|ed| ed.lines == o.editor.lines))
    });
}

/// A guest whose bootstrap nobody answered degrades to plain local editing,
/// as the timeout promises: the input gate lifts, the file stays local-only
/// (no re-request re-arming the gate every 3 seconds), and the guest's save
/// writes to disk — with no owner there is no other author to defer to, so
/// a refused save meant the work could never persist anywhere.
#[test]
fn a_guest_without_an_owner_degrades_to_local_editing_and_saves() {
    use std::time::{Duration, Instant};
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("f.txt");
    std::fs::write(&file, "hello").unwrap();
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

    // A guest alone on the relay: nobody answers its snapshot request.
    let mut guest = App::new(tmp.path().to_path_buf()).unwrap();
    guest.collab_config = Some((socket.clone(), crate::collab::CollabRole::Guest));
    guest.open_file_at_launch(&file);
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        guest.poll_collab();
        if let Some(s) = guest.collab.as_ref()
            && s.is_local_only("f.txt")
        {
            break;
        }
        assert!(Instant::now() < deadline, "bootstrap never gave up");
        std::thread::sleep(Duration::from_millis(20));
    }
    // Later ticks must not re-arm the bootstrap gate.
    for _ in 0..5 {
        guest.poll_collab();
    }
    assert!(
        !guest.collab.as_ref().unwrap().is_bootstrapping("f.txt"),
        "the tick re-armed the input gate after the give-up"
    );
    // The guest is the only author: its save persists.
    guest.editor.insert_str("LOCAL ");
    guest.save();
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        guest.editor.lines.join("\n"),
        "the degraded file's save must reach disk, got status: {}",
        guest.status
    );
}

/// An edit the guest made while the relay link was down exists nowhere but
/// its buffer: the op never reached the owner. Reconnection bootstraps from
/// the owner's older snapshot, and replacing the buffer with it silently
/// deleted the offline work. The divergence must instead be re-injected as
/// local edits, so the guest keeps its work and the owner receives it.
#[test]
fn reconnecting_replays_offline_guest_edits_instead_of_deleting_them() {
    use std::time::{Duration, Instant};
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("f.txt");
    std::fs::write(&file, "hello world").unwrap();
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
    let mut guest = App::new(tmp.path().to_path_buf()).unwrap();
    guest.collab_config = Some((socket.clone(), crate::collab::CollabRole::Guest));
    owner.open_file_at_launch(&file);
    guest.open_file_at_launch(&file);
    let pump_until =
        |owner: &mut App, guest: &mut App, what: &str, done: &dyn Fn(&App, &App) -> bool| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !done(owner, guest) {
                assert!(Instant::now() < deadline, "never settled: {what}");
                owner.poll_collab();
                guest.poll_collab();
                std::thread::sleep(Duration::from_millis(5));
            }
        };
    pump_until(&mut owner, &mut guest, "bootstrap", &|_, g| {
        g.collab.as_ref().is_some_and(|s| s.is_live("f.txt"))
    });
    guest.editor.insert_str("SESSION ");
    pump_until(&mut owner, &mut guest, "session edit converges", &|o, g| {
        o.editor.lines == g.editor.lines && o.editor.lines[0].contains("SESSION")
    });

    // The link dies: drive the same teardown poll_collab runs after
    // detecting a dead channel (session dropped, config kept). Hold the
    // reconnect backoff while the guest keeps typing offline.
    let dead = guest.collab.take().unwrap();
    guest.drop_lost_collab_session(dead);
    guest.last_collab_connect = Some(Instant::now());
    guest.editor.insert_str("OFFLINE ");
    let offline_text = guest.editor.lines.clone();

    // Reconnect: the file re-bootstraps from the owner's older snapshot.
    guest.last_collab_connect = None;
    pump_until(&mut owner, &mut guest, "reconnect goes live", &|_, g| {
        g.collab.as_ref().is_some_and(|s| s.is_live("f.txt"))
    });
    assert!(
        guest.editor.lines[0].contains("OFFLINE"),
        "the bootstrap deleted the guest's offline edit, got: {:?}",
        guest.editor.lines
    );
    assert_eq!(
        guest.editor.lines, offline_text,
        "the guest's buffer must survive the reconnect intact"
    );
    // The offline edit reaches the owner: nothing was lost.
    pump_until(
        &mut owner,
        &mut guest,
        "the offline edit reaches the owner",
        &|o, g| o.editor.lines == g.editor.lines && o.editor.lines[0].contains("OFFLINE"),
    );
}

/// The converse of the replay test above: while the guest was offline, the
/// OWNER also edited the file. The guest's whole diverged buffer replayed
/// over the reconnect bootstrap used to read the owner's unseen edit as a
/// deletion and strip it from every replica. The reconnect must merge the
/// guest's offline delta with the owner's snapshot (three-way, against the
/// last shared text harvested when the link died).
#[test]
fn reconnect_replay_preserves_owner_edits_made_during_the_outage() {
    use std::time::{Duration, Instant};
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("f.txt");
    std::fs::write(&file, "alpha\nbeta\ngamma").unwrap();
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
    let mut guest = App::new(tmp.path().to_path_buf()).unwrap();
    guest.collab_config = Some((socket.clone(), crate::collab::CollabRole::Guest));
    owner.open_file_at_launch(&file);
    guest.open_file_at_launch(&file);
    let pump_until =
        |owner: &mut App, guest: &mut App, what: &str, done: &dyn Fn(&App, &App) -> bool| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !done(owner, guest) {
                assert!(Instant::now() < deadline, "never settled: {what}");
                owner.poll_collab();
                guest.poll_collab();
                std::thread::sleep(Duration::from_millis(5));
            }
        };
    pump_until(&mut owner, &mut guest, "bootstrap", &|_, g| {
        g.collab.as_ref().is_some_and(|s| s.is_live("f.txt"))
    });

    // The guest's link dies through the real teardown (which harvests the
    // merge base); it edits line 1 offline while the owner edits line 3.
    let dead = guest.collab.take().unwrap();
    guest.drop_lost_collab_session(dead);
    guest.last_collab_connect = Some(Instant::now());
    guest.editor.insert_str("OFFLINE ");
    let owner_pump = |owner: &mut App| {
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            owner.poll_collab();
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    owner.editor.cursor_row = 2;
    owner.editor.cursor_col = 0;
    owner.editor.insert_str("OWNER ");
    owner_pump(&mut owner);

    // Reconnect: both offline-era edits must survive, everywhere.
    guest.last_collab_connect = None;
    pump_until(&mut owner, &mut guest, "reconnect goes live", &|_, g| {
        g.collab.as_ref().is_some_and(|s| s.is_live("f.txt"))
    });
    pump_until(
        &mut owner,
        &mut guest,
        "both offline-era edits converge on every replica",
        &|o, g| {
            // `get`, not indexing: if the merge dropped a line, the test must
            // fail at the deadline with "never settled", not panic here.
            o.editor.lines == g.editor.lines
                && o.editor
                    .lines
                    .first()
                    .is_some_and(|l| l.contains("OFFLINE"))
                && o.editor.lines.get(2).is_some_and(|l| l.contains("OWNER"))
        },
    );
}

/// Diff, image, and sheet tabs can carry a shared file's path (a diff tab
/// stores it so close-by-path works), but their `lines` are not documents:
/// the collab passes must never seed, span-splice, or mark them — and a
/// diff tab sitting earlier in the tab strip must not shadow the real text
/// tab out of receiving the session's updates.
#[test]
fn collab_never_touches_diff_image_or_sheet_tabs() {
    use std::time::{Duration, Instant};
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("f.txt");
    std::fs::write(&file, "hello world").unwrap();
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
    let mut guest = App::new(tmp.path().to_path_buf()).unwrap();
    guest.collab_config = Some((socket.clone(), crate::collab::CollabRole::Guest));
    owner.open_file_at_launch(&file);
    // The guest's diff tab lands at index 0, AHEAD of the text tab: a
    // first-found-by-path lookup would hand it the session's updates.
    guest.editor.open_diff(&file, &file).unwrap();
    guest.open_file_at_launch(&file);
    let pump_until =
        |owner: &mut App, guest: &mut App, what: &str, done: &dyn Fn(&App, &App) -> bool| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !done(owner, guest) {
                assert!(Instant::now() < deadline, "never settled: {what}");
                owner.poll_collab();
                guest.poll_collab();
                std::thread::sleep(Duration::from_millis(5));
            }
        };
    pump_until(&mut owner, &mut guest, "bootstrap", &|_, g| {
        g.collab.as_ref().is_some_and(|s| s.is_live("f.txt"))
    });
    owner.editor.insert_str("OWNER ");
    pump_until(
        &mut owner,
        &mut guest,
        "the owner edit reaches the guest text tab",
        &|o, g| g.editor.lines == o.editor.lines && g.editor.lines[0].contains("OWNER"),
    );
    let diff_tab = &guest.editor.editors[0];
    assert!(diff_tab.diff.is_some(), "the diff tab survived");
    assert_eq!(
        diff_tab.collab_doc_gen, 0,
        "a diff tab must never be marked as an attached collab buffer"
    );
    assert!(
        !diff_tab.lines.join("\n").contains("hello"),
        "the collab passes seeded document text into a diff tab"
    );
}

/// While the relay link is down there is no session to defer to: the guest
/// editing offline must be able to save its shared file, or a permanent
/// relay outage strands the work in RAM forever (the save gate used to
/// treat a missing session as "shared, owner saves").
#[test]
fn a_disconnected_guest_can_still_save_its_shared_file() {
    use std::time::{Duration, Instant};
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("f.txt");
    std::fs::write(&file, "hello world").unwrap();
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
    let mut guest = App::new(tmp.path().to_path_buf()).unwrap();
    guest.collab_config = Some((socket.clone(), crate::collab::CollabRole::Guest));
    owner.open_file_at_launch(&file);
    guest.open_file_at_launch(&file);
    let pump_until =
        |owner: &mut App, guest: &mut App, what: &str, done: &dyn Fn(&App, &App) -> bool| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !done(owner, guest) {
                assert!(Instant::now() < deadline, "never settled: {what}");
                owner.poll_collab();
                guest.poll_collab();
                std::thread::sleep(Duration::from_millis(5));
            }
        };
    pump_until(&mut owner, &mut guest, "bootstrap", &|_, g| {
        g.collab.as_ref().is_some_and(|s| s.is_live("f.txt"))
    });

    // The link dies; the guest keeps editing offline and saves.
    guest.collab = None;
    guest.last_collab_connect = Some(Instant::now());
    guest.editor.insert_str("OFF ");
    guest.save();
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        guest.editor.lines.join("\n"),
        "a disconnected guest's save must reach disk, got status: {}",
        guest.status
    );
}

/// Two split panes of a shared file edited while the link is down must both
/// keep their work through the reconnect. Panes cannot converge through the
/// doc while there is no session, so the active pane (the only one input
/// reaches) mirrors onto its siblings — the reconnect then replays exactly
/// one offline text per file, holding every edit.
#[test]
fn offline_edits_in_split_panes_all_survive_a_reconnect() {
    use std::time::{Duration, Instant};
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("f.txt");
    std::fs::write(&file, "hello world").unwrap();
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
    let mut guest = App::new(tmp.path().to_path_buf()).unwrap();
    guest.collab_config = Some((socket.clone(), crate::collab::CollabRole::Guest));
    owner.open_file_at_launch(&file);
    guest.open_file_at_launch(&file);
    let pump_until =
        |owner: &mut App, guest: &mut App, what: &str, done: &dyn Fn(&App, &App) -> bool| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !done(owner, guest) {
                assert!(Instant::now() < deadline, "never settled: {what}");
                owner.poll_collab();
                guest.poll_collab();
                std::thread::sleep(Duration::from_millis(5));
            }
        };
    pump_until(&mut owner, &mut guest, "bootstrap", &|_, g| {
        g.collab.as_ref().is_some_and(|s| s.is_live("f.txt"))
    });
    guest.editor.insert_str("SESSION ");
    pump_until(&mut owner, &mut guest, "session edit converges", &|o, g| {
        o.editor.lines == g.editor.lines && o.editor.lines[0].contains("SESSION")
    });
    guest.split_editor_dir(crate::app::editor_layout::SplitDir::Horizontal, true);
    let session_text = owner.editor.lines.clone();
    pump_until(&mut owner, &mut guest, "both panes attach", &|_, g| {
        g.editor.lines == session_text
            && g.editor_layout
                .inactive_groups()
                .iter()
                .all(|grp| grp.editors.iter().all(|ed| ed.lines == session_text))
    });

    // The link dies. Both panes are edited while offline.
    guest.collab = None;
    guest.last_collab_connect = Some(Instant::now());
    guest.editor.insert_str("RIGHT ");
    guest.poll_collab();
    guest.focus_editor_group(true);
    guest.editor.insert_str("LEFT ");
    guest.poll_collab();

    // Reconnect: every offline edit must reach the owner.
    guest.last_collab_connect = None;
    pump_until(
        &mut owner,
        &mut guest,
        "both offline edits reach the owner",
        &|o, _| {
            let text = o.editor.lines.join("\n");
            text.contains("RIGHT") && text.contains("LEFT")
        },
    );
}

/// Collab carets are keyed by root-relative file paths: after a re-root, a
/// same-named file under the new root would wear a departed peer's caret
/// from the old workspace. Re-rooting clears them; a live session
/// repopulates its own on the next poll.
#[test]
fn changing_the_workspace_root_drops_stale_collab_carets() {
    let tmp = tempfile::tempdir().unwrap();
    let inner = tmp.path().join("inner");
    std::fs::create_dir_all(&inner).unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.collab_carets.insert(
        7,
        CollabCaret {
            file: String::from("src/main.rs"),
            row: 3,
            col: 1,
            name: String::from("guest"),
            last_moved: std::time::Instant::now(),
        },
    );
    app.change_workspace_root(inner);
    assert!(
        app.collab_carets.is_empty(),
        "old-workspace carets must not survive a re-root"
    );
}

/// A solo guest viewport never hosts the navigator: the owner's croft does,
/// or two hosts would fight over one seat.
#[test]
fn solo_guest_never_hosts_the_navigator() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = tmp.path().join("collab.sock");
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.pair_record_path = tmp.path().join("x.pair.json");
    app.pair_socket = socket.clone();
    app.pair_host_lock_path = tmp.path().join("x.pair-host.lock");
    app.collab_config = Some((socket, crate::collab::CollabRole::Guest));
    app.pair_spawn_override = Some(Box::new(|_| {
        panic!("a solo guest must never spawn the pilot");
    }));
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
    app.last_pair_check = None;
    app.maybe_seat_navigator();
    assert!(app.pair_host.is_none());
}

/// The AI-stream badge lifecycle: a pilot's StreamState(active) sets
/// `collab_stream` on a dirty tick, the cancel action broadcasts
/// StreamCancel to the pilot, and StreamState(inactive) clears the badge.
/// With no active stream, cancel is a hint, not a broadcast.
#[test]
fn collab_stream_state_drives_the_badge_and_cancel_broadcasts() {
    use std::time::{Duration, Instant};
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("f.txt");
    std::fs::write(&file, "hello world").unwrap();
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

    // Cancel with no stream: a hint, nothing on the wire required yet.
    owner.collab_cancel_stream();
    assert!(
        owner.status.to_lowercase().contains("no ai stream"),
        "inactive cancel hints, got: {}",
        owner.status
    );

    // The relay is a dumb fan-out with no replay, so the owner must be
    // connected BEFORE the pilot broadcasts or the message is simply lost
    // (poll_collab's lazy connect is throttled and can lose that race
    // under a contended parallel suite).
    let deadline = Instant::now() + Duration::from_secs(5);
    while owner.collab.is_none() {
        assert!(Instant::now() < deadline, "owner never joined the relay");
        owner.poll_collab();
        std::thread::sleep(Duration::from_millis(5));
    }

    // A pilot stand-in seat on the same relay.
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

    // Stream starts: the owner's badge state fills on a dirty tick.
    pilot.send_stream_state("f.txt", true);
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut dirty_on_arrival = false;
    while owner.collab_stream.is_none() {
        assert!(Instant::now() < deadline, "badge never appeared");
        dirty_on_arrival = owner.poll_collab();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(dirty_on_arrival, "the badge tick must request a redraw");
    let stream = owner.collab_stream.as_ref().unwrap();
    assert_eq!(
        (stream.name.as_str(), stream.file.as_str()),
        ("pilot", "f.txt")
    );

    // The owner cancels: the pilot's seat receives StreamCancel.
    owner.collab_cancel_stream();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let events = pilot.poll(|_| None);
        if events
            .iter()
            .any(|e| matches!(e, crate::collab::CollabEvent::StreamCancel))
        {
            break;
        }
        assert!(Instant::now() < deadline, "pilot never saw the cancel");
        std::thread::sleep(Duration::from_millis(5));
    }

    // Stream ends: the badge clears.
    pilot.send_stream_state("f.txt", false);
    let deadline = Instant::now() + Duration::from_secs(5);
    while owner.collab_stream.is_some() {
        assert!(Instant::now() < deadline, "badge never cleared");
        owner.poll_collab();
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// #919: a peer's edit is no step of this user's undo. The owner types on
/// line 1 and a peer on line 3; the owner's Ctrl+Z removes only the owner's
/// text, the peer's stays, and the peer sees the undo as an ordinary edit.
#[test]
fn undo_reverts_only_your_own_edit_and_keeps_the_peers() {
    use std::time::{Duration, Instant};
    let tmp = tempfile::tempdir().unwrap();
    let (mut owner, mut pilot) =
        owner_and_pilot_on(&tmp, "def a():\n    return 1\ndef b():\n    return 2");
    owner.editor.cursor_row = 0;
    owner.editor.cursor_col = "def a():".len();
    owner.editor.insert_str("  # A-edit");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !pilot
        .doc_text("f.txt")
        .is_some_and(|t| t.contains("A-edit"))
    {
        assert!(
            Instant::now() < deadline,
            "the peer never saw the owner's edit"
        );
        owner.poll_collab();
        pilot.poll(|_| None);
        std::thread::sleep(Duration::from_millis(5));
    }
    pilot_writes(
        &mut owner,
        &mut pilot,
        "def a():  # A-edit\n    return 1\ndef b():  # B-edit\n    return 2",
    );
    assert!(owner.editor.undo());
    let undone = "def a():\n    return 1\ndef b():  # B-edit\n    return 2";
    assert_eq!(
        owner.editor.lines.join("\n"),
        undone,
        "the owner's undo takes back the owner's edit, not the peer's"
    );
    assert!(
        !owner.editor.undo(),
        "the peer's edit is not the owner's step"
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while pilot.doc_text("f.txt") != Some(undone) {
        assert!(Instant::now() < deadline, "the peer never saw the undo");
        owner.poll_collab();
        pilot.poll(|_| None);
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A caret name tag is visible while inside its 2s fade window and signals
/// exactly one redraw when it appears and one when it expires — no redraw
/// churn in between (the tick loop repaints only on `poll_collab() == true`).
#[test]
fn collab_caret_label_fade_signals_one_redraw_per_transition() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    assert!(
        !app.collab_labels_dirty(),
        "no carets, no labels, no redraw"
    );

    app.collab_carets.insert(
        2,
        CollabCaret {
            file: "f.txt".into(),
            row: 0,
            col: 3,
            name: "claude".into(),
            last_moved: std::time::Instant::now(),
        },
    );
    assert!(app.collab_labels_dirty(), "fresh tag: one redraw");
    assert!(!app.collab_labels_dirty(), "still visible: quiet");

    app.collab_carets.get_mut(&2).unwrap().last_moved =
        std::time::Instant::now() - std::time::Duration::from_secs(3);
    assert!(app.collab_labels_dirty(), "expired tag: one redraw");
    assert!(!app.collab_labels_dirty(), "stays hidden: quiet");
}
