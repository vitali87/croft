//! Remote work: SSH, the relay, ports, fleet, devcontainers and drop to
//! local.

use super::*;

#[test]
fn persistence_advisory_only_for_nonpersistent_remote_sessions() {
    // Remote session without dtach: warn on the status line. Remote-with-dtach
    // and any local session: nothing.
    assert!(super::remote_persistence_status(true, false).is_some());
    assert!(super::remote_persistence_status(true, true).is_none());
    assert!(super::remote_persistence_status(false, false).is_none());
    assert!(super::remote_persistence_status(false, true).is_none());
}

/// #862: leaving a remote croft for the local one ends this process like a
/// quit, so it asks the same way.
#[test]
fn drop_to_local_with_unsaved_edits_asks_first() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, a) = app_with_unsaved_file(tmp.path());
    app.is_remote = true;
    let drop_key = key(
        KeyCode::Char('l'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    );
    app.handle_key(drop_key).unwrap();
    assert!(!app.quit && !app.drop_to_local);
    assert_eq!(app.pending_unsaved, Some(UnsavedExit::DropToLocal));
    app.handle_key(key(KeyCode::Char('s'), KeyModifiers::NONE))
        .unwrap();
    assert!(app.drop_to_local && app.quit);
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "xalpha\n");
}

/// The two install outcomes that hand this terminal to the remote croft:
/// one was already there (it launches while the reinstall runs on), or the
/// install just finished.
fn handoff_events() -> [crate::install_session::InstallEvent; 2] {
    use crate::install_session::InstallEvent;
    [InstallEvent::CanLaunch, InstallEvent::Done(Ok(()))]
}

/// #862: connecting to a remote croft ends this one once the remote side is
/// ready, which can be minutes after the connect was asked for. An edit
/// made meanwhile went with it: the handoff set `quit` without asking. It
/// asks now, and S saves before handing off.
#[test]
fn a_remote_handoff_asks_about_edits_made_during_the_install() {
    for event in handoff_events() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_open_file(tmp.path(), "a.txt", "alpha\n");
        // Nothing was unsaved when the connect was asked for; the edit
        // comes while the install runs.
        type_into_editor(&mut app, "x");
        ready_to_hand_off(&mut app, event);
        app.poll_install_session();
        assert!(!app.quit, "the unsaved a.txt must not go with the handoff");
        assert!(app.pending_unsaved.is_some(), "it asks");
        let screen = draw_screen(&mut app);
        assert!(screen.contains("ave all and connect"), "{screen}");
        app.handle_key(key(KeyCode::Char('s'), KeyModifiers::NONE))
            .unwrap();
        assert!(app.quit, "saved, so it hands off: {}", app.status);
        assert!(app.remote_launch.is_some(), "to the remote croft");
        let a = tmp.path().join("a.txt");
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "xalpha\n");
    }
}

/// #862 negative: with nothing unsaved the handoff goes at once, as before.
#[test]
fn a_remote_handoff_with_nothing_unsaved_goes_at_once() {
    for event in handoff_events() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_with_open_file(tmp.path(), "a.txt", "alpha\n");
        ready_to_hand_off(&mut app, event);
        app.poll_install_session();
        assert!(app.quit);
        assert!(app.pending_unsaved.is_none(), "nothing to ask about");
        assert!(app.remote_launch.is_some());
    }
}

/// #862: Esc at the handoff prompt keeps this croft and the edits, and
/// drops the launch rather than leaving it armed for a later quit.
#[test]
fn cancelling_the_remote_handoff_stays_here_with_the_edits() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, a) = app_with_unsaved_file(tmp.path());
    ready_to_hand_off(&mut app, crate::install_session::InstallEvent::CanLaunch);
    app.poll_install_session();
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert!(!app.quit);
    assert!(app.remote_launch.is_none(), "the launch is dropped");
    assert!(app.install_session.is_none());
    assert!(app.editor.dirty && app.editor.lines[0] == "xalpha");
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "alpha\n");
    assert!(app.status.contains("did not connect"), "{}", app.status);
}

/// #862 negative: S at the handoff prompt whose save is refused (the file
/// changed on disk) stays here, and drops the launch: left armed, the next
/// ordinary Ctrl+Q would have connected instead of quitting.
#[test]
fn a_refused_save_at_the_remote_handoff_drops_the_launch() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, a) = app_with_unsaved_file(tmp.path());
    std::fs::write(&a, "changed elsewhere\n").unwrap();
    ready_to_hand_off(&mut app, crate::install_session::InstallEvent::CanLaunch);
    app.poll_install_session();
    app.handle_key(key(KeyCode::Char('s'), KeyModifiers::NONE))
        .unwrap();
    assert!(!app.quit);
    assert!(app.status.contains("Did not connect"), "{}", app.status);
    assert!(app.remote_launch.is_none(), "not left armed");
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "changed elsewhere\n");
}

#[test]
fn ssh_empty_state_overlay_arms_image_clear_when_user_switches_off_remote_view() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // Simulate the post-emit state without actually writing OSC bytes.
    app.overlays.ssh.set_displayed(true);
    app.set_sidebar_view(SidebarView::Explorer);
    app.flush_ssh_empty_state_overlay();
    assert!(
        app.consume_ssh_empty_state_image_clear(),
        "leaving the Remote view after the illustration was painted must arm one terminal.clear() so iTerm2 evicts the cached image cells — otherwise the server-rack PNG ghosts onto the Explorer tree the user just clicked to"
    );
    assert!(!app.overlays.ssh.is_displayed());
}

#[test]
fn ssh_empty_state_overlay_arms_image_clear_when_modal_opens_on_top() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Remote);
    app.overlays.ssh.set_displayed(true);
    app.handle_key(key(KeyCode::F(1), KeyModifiers::NONE))
        .unwrap();
    app.flush_ssh_empty_state_overlay();
    assert!(
        app.consume_ssh_empty_state_image_clear(),
        "F1 opening the shortcuts modal over the SSH empty state must arm an image-clear so the illustration doesn't bleed through the modal text"
    );
}

#[test]
fn cmd_shift_r_jumps_to_remote_from_any_pane() {
    let mut app = editor_app_with_lines(&["x"]);
    app.focus_pane(Pane::Editor);
    app.handle_key(key(
        KeyCode::Char('r'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert_eq!(app.sidebar_view, SidebarView::Remote);
}

#[test]
fn is_drop_to_local_key_matches_only_cmd_shift_l() {
    assert!(is_drop_to_local_key(key(
        KeyCode::Char('l'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT
    )));
    assert!(is_drop_to_local_key(key(
        KeyCode::Char('L'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT
    )));
    assert!(!is_drop_to_local_key(key(
        KeyCode::Char('l'),
        KeyModifiers::SUPER
    )));
    assert!(!is_drop_to_local_key(key(
        KeyCode::Char('q'),
        KeyModifiers::CONTROL
    )));
    assert!(!is_drop_to_local_key(key(
        KeyCode::Char('r'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT
    )));
}

#[test]
fn cmd_shift_l_on_remote_requests_drop_to_local() {
    let mut app = editor_app_with_lines(&["x"]);
    app.is_remote = true;
    app.handle_key(key(
        KeyCode::Char('l'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert!(
        app.drop_to_local && app.quit,
        "Cmd+Shift+L on a remote-launched croft must request a clean hand-back to the local croft"
    );
}

#[test]
fn cmd_shift_l_on_local_is_inert() {
    let mut app = editor_app_with_lines(&["x"]);
    app.is_remote = false;
    app.handle_key(key(
        KeyCode::Char('l'),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ))
    .unwrap();
    assert!(
        !app.drop_to_local && !app.quit,
        "Cmd+Shift+L on a local croft must do nothing - there is no remote session to leave"
    );
}

#[test]
fn remote_launched_drop_queues_pull_request_via_relay_log() {
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    // The user dragged a Finder file onto a remote-launched croft. The Mac path
    // doesn't exist on the remote box, but the session is a `croft remote` child
    // (CROFT_REMOTE_AUTOUPDATE set) whose local parent runs a drop pump, so the
    // drop is recorded in the identity-derived relay log (no env path needed)
    // instead of being pasted into the editor.
    let workspace = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let mut app = App::new(workspace.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Explorer);
    app.focus_pane(Pane::Tree);
    let (log, _) = with_relay_home(home.path(), || {
        let log = app.relay_log_path().expect("relay log path derivable");
        app.handle_paste("/Users/vitali/Documents/foo.txt\n");
        (log, ())
    });
    let written = std::fs::read_to_string(&log).expect("relay log was written");
    assert!(
        written.starts_with("pull\t"),
        "relay log entry must be a pull request, got: {written:?}",
    );
    assert!(
        written.contains("/Users/vitali/Documents/foo.txt"),
        "relay log must carry the foreign path, got: {written:?}",
    );
    assert_eq!(app.pending_remote_pulls.len(), 1);
}

/// #538, half two: what "the clipboard" means when croft runs on a remote box.
///
/// The user's clipboard is on the machine they are typing at, not on the Linux
/// box croft runs on. A copy there wrote the REMOTE box's clipboard (usually no
/// tool at all in an SSH session) and fell back to OSC 52, which the host
/// terminal may silently refuse — while the status line claimed success either
/// way. The drop relay already pulls the local clipboard for paste; this is the
/// same road in the other direction.
#[test]
fn copying_on_a_relay_session_pushes_the_text_to_the_local_clipboard() {
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // A silent child, so the grid holds exactly what the test feeds it.
    app.terminals[0] = crate::widgets::terminal::PtyTerminal::new_running(
        "/bin/sh",
        &[String::from("-c"), String::from("sleep 60")],
        tmp.path(),
    )
    .unwrap();
    app.show_terminal = true;
    app.focus_pane(Pane::Terminal);
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 30)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let inner = app.terminals[0].last_inner;
    app.terminals[0].feed_bytes_for_test(b"copy-me-538\r\n");
    let (lines, top) = app.terminals[0].grid_lines();
    let vrow = (top
        + lines
            .iter()
            .position(|l| l.starts_with("copy-me-538"))
            .expect("the fed text reached the grid") as i32) as u16;
    app.terminals[0].start_selection_at(inner.x, inner.y + vrow);
    app.terminals[0].extend_selection_to(inner.x + 20, inner.y + vrow);
    let expected = app.terminals[0].selection_text();
    assert!(
        expected.contains("copy-me-538"),
        "precondition: the selection covers the fed text, got {expected:?}"
    );

    // Opt in explicitly. The flag is inert under cfg(test) precisely so that
    // this test's CROFT_RELAY_KEY cannot reroute a concurrent test's copy.
    app.is_relay_session = true;
    let (log, inbox_for_assert) = with_relay_home(home.path(), || {
        let log = app.relay_log_path().expect("relay log path derivable");
        let inbox = app.relay_inbox_path().expect("relay inbox path derivable");
        app.handle_key(key(KeyCode::Char('c'), KeyModifiers::SUPER))
            .unwrap();
        (log, inbox)
    });

    let written = std::fs::read_to_string(&log).expect("relay log was written");
    assert!(
        written.starts_with("copy\t"),
        "a copy on a relay session must queue a copy request, got {written:?}"
    );
    // The line carries an ID ONLY: a path on the wire would let anything that
    // can write this log point the pump at a FIFO or outside the relay.
    let request_id = written
        .trim_end()
        .split('\t')
        .nth(1)
        .expect("the request carries an id");
    assert_eq!(
        written.trim_end().split('\t').count(),
        2,
        "no path belongs on the wire, got {written:?}"
    );
    let payload_path = inbox_for_assert.join(format!("{request_id}.txt"));
    let payload = std::fs::read_to_string(&payload_path).expect("the payload file was written");
    assert_eq!(
        payload, expected,
        "the payload must be exactly what was selected"
    );
    assert!(
        app.status.contains("local"),
        "and the status must say WHERE it went, got {:?}",
        app.status
    );
}

#[test]
fn remote_finder_drag_queues_pull_without_tree_focus() {
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    // A drop does not move keyboard focus to the Explorer tree, so a focus-only
    // gate dropped drags whenever the user had last clicked the terminal or
    // editor. A pure-path payload must be recognised regardless of focus.
    let workspace = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let mut app = App::new(workspace.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Explorer);
    app.focus_pane(Pane::Editor); // focus is NOT on the tree
    let log = with_relay_home(home.path(), || {
        let log = app.relay_log_path().expect("relay log path derivable");
        app.handle_paste("/Users/vitali/Documents/dragged.txt\n");
        log
    });
    let written = std::fs::read_to_string(&log).expect("relay log was written");
    assert!(
        written.starts_with("pull\t") && written.contains("/Users/vitali/Documents/dragged.txt"),
        "a pure-path drag must queue a pull even without tree focus, got: {written:?}",
    );
    assert_eq!(app.pending_remote_pulls.len(), 1);
}

#[test]
fn drain_remote_pulls_imports_file_when_relay_signals_ok() {
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let workspace = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let mut app = App::new(workspace.path().to_path_buf()).unwrap();
    app.set_sidebar_view(SidebarView::Explorer);
    app.focus_pane(Pane::Tree);
    let request_dir = with_relay_home(home.path(), || {
        let inbox = app.relay_inbox_path().expect("relay inbox derivable");
        let request_dir = inbox.join("test-req-1");
        std::fs::create_dir_all(&request_dir).unwrap();
        std::fs::write(request_dir.join("foo.txt"), "payload").unwrap();
        std::fs::write(request_dir.join(".ok"), b"").unwrap();
        app.pending_remote_pulls.push(PendingRemotePull {
            request_id: String::from("test-req-1"),
            src_display: String::from("/Users/v/Docs/foo.txt"),
            basename: String::from("foo.txt"),
            dest_dir: workspace.path().to_path_buf(),
            started_at: std::time::Instant::now(),
            kind: RemotePullKind::File,
        });
        let changed = app.drain_remote_pulls();
        assert!(changed);
        request_dir
    });
    assert!(app.pending_remote_pulls.is_empty());
    let landed = workspace.path().join("foo.txt");
    assert!(landed.exists(), "file should land in workspace");
    assert_eq!(std::fs::read_to_string(&landed).unwrap(), "payload");
    assert!(
        !request_dir.exists(),
        "relay request dir must be cleaned up after import",
    );
    assert!(app.status.contains("Pulled"));
}

#[test]
fn drain_remote_pulls_surfaces_relay_error_message() {
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let workspace = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let mut app = App::new(workspace.path().to_path_buf()).unwrap();
    with_relay_home(home.path(), || {
        let inbox = app.relay_inbox_path().expect("relay inbox derivable");
        let request_dir = inbox.join("req-2");
        std::fs::create_dir_all(&request_dir).unwrap();
        std::fs::write(request_dir.join(".err"), b"scp exited with 1").unwrap();
        app.pending_remote_pulls.push(PendingRemotePull {
            request_id: String::from("req-2"),
            src_display: String::from("/Users/v/missing.txt"),
            basename: String::from("missing.txt"),
            dest_dir: workspace.path().to_path_buf(),
            started_at: std::time::Instant::now(),
            kind: RemotePullKind::File,
        });
        let changed = app.drain_remote_pulls();
        assert!(changed);
    });
    assert!(app.status.contains("scp exited with 1"));
}

#[test]
fn clicking_the_ports_tab_switches_the_panel_view() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let backend = ratatui::backend::TestBackend::new(140, 50);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let r = app.ports_tab_rect;
    assert!(r.width > 0, "the PORTS tab must have a hit rect");
    left_click(&mut app, r.x, r.y);
    assert_eq!(
        app.bottom_panel_tab,
        BottomPanelTab::Ports,
        "a click on the PORTS tab activates that view",
    );
}

#[test]
fn a_detected_port_renders_in_the_ports_panel() {
    use crate::widgets::ports::PortOrigin;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.bottom_panel_tab = BottomPanelTab::Ports;
    app.ports
        .upsert(3000, None, Some("node".into()), PortOrigin::Local);
    let backend = ratatui::backend::TestBackend::new(140, 50);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let buf = term.backend().buffer().clone();
    let text: String = buf.content().iter().map(|c| c.symbol()).collect();
    assert!(text.contains("3000"), "the detected port is listed");
    assert!(text.contains("node"), "its process is listed");
}

#[test]
fn opening_a_forwarded_remote_port_routes_to_the_local_browser_via_relay() {
    use crate::widgets::ports::{PortOrigin, PortsPanel};
    // On a remote session the local opener would run on the headless remote, so
    // a forwarded port must re-open through the relay (lands on the local Mac).
    let mut p = PortsPanel::new();
    p.upsert(3000, None, None, PortOrigin::Remote("box".into()));
    p.mark_forwarded(3000, 3001);
    let entry = p.selected().unwrap();
    match super::plan_port_open(entry, true) {
        super::PortOpenAction::OpenViaRelay(url) => assert_eq!(url, "http://127.0.0.1:3001/"),
        other => panic!("expected relay open, got {other:?}"),
    }
}

#[test]
fn opening_an_unforwarded_remote_port_forwards_first() {
    use crate::widgets::ports::{PortOrigin, PortsPanel};
    let mut p = PortsPanel::new();
    p.upsert(3000, None, None, PortOrigin::Remote("box".into()));
    let entry = p.selected().unwrap();
    assert!(matches!(
        super::plan_port_open(entry, true),
        super::PortOpenAction::Forward(3000)
    ));
}

#[test]
fn opening_a_local_port_opens_directly_not_via_relay() {
    use crate::widgets::ports::{PortOrigin, PortsPanel};
    let mut p = PortsPanel::new();
    p.upsert(8000, None, None, PortOrigin::Local);
    let entry = p.selected().unwrap();
    match super::plan_port_open(entry, false) {
        super::PortOpenAction::OpenDirect(url) => assert!(url.contains("8000"), "{url}"),
        other => panic!("expected direct open, got {other:?}"),
    }
}

#[test]
fn double_clicking_a_port_row_opens_it() {
    use crate::widgets::ports::PortOrigin;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.bottom_panel_tab = BottomPanelTab::Ports;
    // A remote, not-yet-forwarded port: opening it routes through the forward
    // path, which sets an observable status without spawning a real browser.
    app.ports
        .upsert(3000, None, None, PortOrigin::Remote("box".into()));
    draw(&mut app, 140, 50);
    let x = app.ports.last_area.x + 2;
    let y = app.ports.last_area.y + 1; // first data row
    // A single click selects the row but takes no open action.
    left_click(&mut app, x, y);
    assert!(
        !app.status.contains("orward"),
        "a single click must not open the port: {}",
        app.status
    );
    // A second click on the same row within the double-click window opens it.
    left_click(&mut app, x, y);
    assert!(
        app.status.contains("orward"),
        "a double click opens (forwards) the port: {}",
        app.status
    );
}

/// #317 beyond the terminal: the guard lives in `ClickTracker`, so every
/// surface that pairs clicks inherits it. The ports table is the cheapest one
/// to drive end to end, and it has a visible consequence - a double click
/// forwards the port - so a modified click that wrongly counted as half a
/// double would open something the user never double-clicked.
#[test]
fn a_modified_click_does_not_pair_with_a_plain_one_in_the_ports_table() {
    use crate::widgets::ports::PortOrigin;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.bottom_panel_tab = BottomPanelTab::Ports;
    app.ports
        .upsert(3000, None, None, PortOrigin::Remote("box".into()));
    draw(&mut app, 140, 50);
    let x = app.ports.last_area.x + 2;
    let y = app.ports.last_area.y + 1;

    let click_with = |app: &mut App, modifiers| {
        app.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: x,
            row: y,
            modifiers,
        });
    };

    // A modified click must not ARM the pair: the plain click after it is the
    // user's first ordinary click on that row, not the second half of a
    // gesture they never made.
    click_with(&mut app, KeyModifiers::CONTROL);
    click_with(&mut app, KeyModifiers::empty());
    assert!(
        !app.status.contains("orward"),
        "a plain click after a ctrl+click must not open the port: {}",
        app.status
    );

    // And a modified click must not COMPLETE one either.
    click_with(&mut app, KeyModifiers::empty());
    app.status.clear();
    click_with(&mut app, KeyModifiers::CONTROL);
    assert!(
        !app.status.contains("orward"),
        "a ctrl+click must not complete a double-click: {}",
        app.status
    );

    // The guard must not have cost the real gesture.
    app.status.clear();
    click_with(&mut app, KeyModifiers::empty());
    click_with(&mut app, KeyModifiers::empty());
    assert!(
        app.status.contains("orward"),
        "two ordinary clicks must still open the port: {}",
        app.status
    );
}

#[test]
fn clicking_the_port_toast_dismiss_button_clears_it() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.show_port_toast(3000, Some("node".into()));
    let backend = ratatui::backend::TestBackend::new(140, 50);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    // The render records the toast's clickable button rects; find Dismiss.
    let dismiss = app
        .port_toast
        .as_ref()
        .expect("toast is showing")
        .buttons
        .iter()
        .find(|(_, a)| matches!(a, super::PortToastAction::Dismiss))
        .map(|(r, _)| *r)
        .expect("the toast has a dismiss button");
    left_click(&mut app, dismiss.x, dismiss.y);
    assert!(
        app.port_toast.is_none(),
        "clicking dismiss clears the toast"
    );
}

#[test]
fn relay_request_ids_are_unique_per_request() {
    // `Instant::now().elapsed()` measures the gap between an instant and
    // itself, so the id was `<kind>-<pid>-0` almost every time. The relay keys
    // its remote inbox directory by request id, so two requests raised in the
    // same second collided: the first reply satisfied both pending pulls and
    // deleted the directory out from under the second, which then timed out
    // 120s later - with its file already destroyed or its tunnel already up.
    let ids: Vec<String> = (0..64).map(|_| App::relay_request_id("forward")).collect();
    let unique: std::collections::HashSet<&String> = ids.iter().collect();
    assert_eq!(unique.len(), ids.len(), "every request id must be distinct");
    assert!(ids[0].starts_with("forward-"), "kind stays in the id");
}

/// Losing the relay link must not be silent: the app detects the dead
/// channel, drops the session, and reconnects through the normal backoff
/// path. Before detection existed, `is_live` stayed true forever and the
/// guest typed into a buffer whose ops reached nobody.
#[test]
fn losing_the_relay_reconnects_instead_of_typing_into_the_void() {
    use std::time::{Duration, Instant};
    let tmp = tempfile::tempdir().unwrap();
    let socket = tmp.path().join("collab.sock");
    // A hand-rolled "relay": accepts, then hangs up on command.
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let mut guest = App::new(tmp.path().to_path_buf()).unwrap();
    guest.collab_config = Some((socket.clone(), crate::collab::CollabRole::Guest));
    guest.poll_collab();
    assert!(guest.collab.is_some(), "first connect");
    let (peer, _) = listener.accept().unwrap();
    drop(peer); // the relay dies

    // Detection: the dead session is dropped.
    let deadline = Instant::now() + Duration::from_secs(3);
    while guest.collab.is_some() {
        assert!(
            Instant::now() < deadline,
            "the dead session was never dropped"
        );
        guest.poll_collab();
        std::thread::sleep(Duration::from_millis(10));
    }
    // Recovery: a fresh session connects (the listener's backlog accepts).
    let deadline = Instant::now() + Duration::from_secs(5);
    while guest.collab.is_none() {
        assert!(Instant::now() < deadline, "never reconnected");
        guest.poll_collab();
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Relay startup failure is the spawn failure's sibling: the window has
/// already acquired the workspace host lock, cannot host without a relay,
/// and must release the lock on the way out so another window can try.
/// Holding it made every other window report "hosted by another croft
/// window" while nobody hosted anything.
#[test]
fn a_relay_failure_releases_the_host_lock() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.pair_record_path = tmp.path().join("x.pair.json");
    // A socket inside a directory that does not exist: the relay can never
    // bind it, so ensure_relay fails after its aliveness poll.
    app.pair_socket = tmp.path().join("missing-dir").join("collab.sock");
    app.pair_host_lock_path = tmp.path().join("x.host.lock");
    app.pair_spawn_override = Some(Box::new(|_| panic!("must not reach the spawn")));
    crate::session::write_pair_record(
        &app.pair_record_path,
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
    app.last_pair_check = None;
    app.maybe_seat_navigator();
    assert!(app.pair_host.is_none());
    assert!(
        app.status.contains("relay failed"),
        "the failure is announced: {}",
        app.status
    );
    assert!(
        app.pair_host_lock.is_none(),
        "a window without a relay cannot host; the lock must be released"
    );
}

/// #363: a fleet run does not block the event loop, and its results arrive
/// through the channel like every other background answer.
///
/// The blocking version is the tempting one — `run_on_hosts` returns a
/// `Vec`, so calling it inline reads naturally — and it freezes the editor
/// for up to `FLEET_TIMEOUT` while the network answers. The user could not
/// scroll, type, or cancel. This asserts the split: the command returns at
/// once, and a later drain reports.
#[test]
fn a_fleet_run_reports_through_the_channel_rather_than_blocking() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();

    // Nothing in flight: a drain finds nothing and says so, rather than
    // blocking or inventing an empty result.
    assert!(!app.drain_fleet_results(), "no run, nothing to report");

    // Feed a result as the worker thread would.
    let results = vec![
        crate::fleet::HostResult {
            host: String::from("a"),
            output: String::from("5.15.0"),
            exit: Some(0),
            elapsed: std::time::Duration::ZERO,
        },
        crate::fleet::HostResult {
            host: String::from("b"),
            output: String::from("5.15.0"),
            exit: Some(0),
            elapsed: std::time::Duration::ZERO,
        },
        crate::fleet::HostResult {
            host: String::from("odd"),
            output: String::from("6.1.0"),
            exit: Some(0),
            elapsed: std::time::Duration::ZERO,
        },
        crate::fleet::HostResult {
            host: String::from("down"),
            output: String::from("timed out"),
            exit: None,
            elapsed: std::time::Duration::ZERO,
        },
    ];
    app.fleet_running = true;
    app.fleet_tx.send(results).unwrap();

    assert!(app.drain_fleet_results(), "a delivered result must report");
    assert!(!app.fleet_running, "the run is no longer in flight");
    assert!(
        app.status.contains("2 identical") && app.status.contains("1 differ"),
        "the summary must reach the user: {}",
        app.status
    );
    assert!(app.status.contains("1 failed"), "status: {}", app.status);

    // And the channel is empty again, so a second drain is a no-op rather
    // than replaying the same run.
    assert!(!app.drain_fleet_results(), "results are consumed once");
}

#[test]
fn an_ssh_session_raises_the_offer_once_and_drops_it_when_the_session_ends() {
    // #364: the offer appears when a pane's foreground BECOMES `ssh <host>`
    // for a known host, not on every sample of the same session, and it
    // leaves with the session.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // The refusal memory is read from the real cache directory at startup;
    // this machine's must not decide the test.
    app.remote_offer_refused.clear();
    let pane = app.terminals[0].uid();
    app.consider_ssh_offer(pane, Some(String::from("db-1")));
    assert!(
        app.ssh_offer
            .as_ref()
            .is_some_and(|o| o.pane == pane && o.host == "db-1"),
        "a new session offers"
    );
    assert!(
        app.status.starts_with("Connected to db-1"),
        "{:?}",
        app.status
    );
    app.status = String::from("something else happened");
    app.consider_ssh_offer(pane, Some(String::from("db-1")));
    assert_eq!(
        app.status, "something else happened",
        "the same session sampled again does not re-prompt"
    );
    app.status = String::from("Connected to db-1 · Open workspace here?");
    app.consider_ssh_offer(pane, None);
    assert!(
        app.ssh_offer.is_none(),
        "the session ended: the offer is gone"
    );
    assert!(app.status.is_empty(), "and its status line with it");
    // A NEW session to the same host offers again.
    app.consider_ssh_offer(pane, Some(String::from("db-1")));
    assert!(app.ssh_offer.is_some());
}

#[test]
fn the_offer_respects_the_global_switch_the_host_list_and_learned_refusals() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.remote_offer_refused.clear();
    let pane = app.terminals[0].uid();
    app.remote_offer_excluded = vec![String::from("DB-1")];
    app.consider_ssh_offer(pane, Some(String::from("db-1")));
    assert!(
        app.ssh_offer.is_none(),
        "per-host opt-out, case-insensitive"
    );
    app.remote_offer_excluded.clear();
    app.remote_offer_refused
        .insert(String::from("db-1"), std::time::SystemTime::now());
    app.consider_ssh_offer(pane, None);
    app.consider_ssh_offer(pane, Some(String::from("db-1")));
    assert!(
        app.ssh_offer.is_none(),
        "a host that refused provisioning is not offered"
    );
    app.remote_offer_refused.clear();
    app.remote_offer_disabled = true;
    app.consider_ssh_offer(pane, None);
    app.consider_ssh_offer(pane, Some(String::from("db-1")));
    assert!(app.ssh_offer.is_none(), "the global switch");
    app.remote_offer_disabled = false;
    app.consider_ssh_offer(pane, None);
    app.consider_ssh_offer(pane, Some(String::from("db-1")));
    assert!(
        app.ssh_offer.is_some(),
        "and with every gate open it offers"
    );
}

#[test]
fn accepting_or_dismissing_the_offer_clears_it() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    // The refusal memory is read from the real cache directory at startup;
    // this machine's must not decide the test.
    app.remote_offer_refused.clear();
    let pane = app.terminals[0].uid();
    app.consider_ssh_offer(pane, Some(String::from("db-1")));
    assert_eq!(
        app.accept_ssh_offer().map(|(host, _)| host).as_deref(),
        Some("db-1")
    );
    assert!(app.ssh_offer.is_none() && app.status.is_empty());
    assert_eq!(app.accept_ssh_offer(), None, "nothing to accept twice");

    app.consider_ssh_offer(pane, None);
    app.consider_ssh_offer(pane, Some(String::from("db-1")));
    assert!(app.ssh_offer.is_some());
    // Esc dismisses through the ordinary key path, whatever pane has focus.
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert!(app.ssh_offer.is_none(), "Esc dismissed the offer");
    assert!(app.status.is_empty());
}

#[test]
fn taking_a_host_off_the_exclusion_list_offers_the_session_already_on_it() {
    // A pane sampled on db-1 while db-1 was excluded is remembered as seen
    // there; the same host on the next sample would read as the same
    // session and never prompt. Un-excluding the host forgets that, so the
    // next sample offers it.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.remote_offer_refused.clear();
    let pane = app.terminals[0].uid();
    let mut prefs = crate::prefs::Prefs {
        remote_offer_excluded_hosts: vec![String::from("DB-1")],
        ..Default::default()
    };
    app.apply_merged_settings(&prefs);
    app.consider_ssh_offer(pane, Some(String::from("db-1")));
    assert!(app.ssh_offer.is_none(), "excluded: no offer");
    app.consider_ssh_offer(pane, Some(String::from("db-1")));
    assert!(
        app.ssh_offer.is_none(),
        "still the same session, still excluded"
    );
    prefs.remote_offer_excluded_hosts.clear();
    app.apply_merged_settings(&prefs);
    app.consider_ssh_offer(pane, Some(String::from("db-1")));
    assert!(
        app.ssh_offer
            .as_ref()
            .is_some_and(|o| o.pane == pane && o.host == "db-1"),
        "the live session is offered once its host is allowed again"
    );
    // A pane on a host that stayed excluded is untouched by the change.
    prefs.remote_offer_excluded_hosts = vec![String::from("jump"), String::from("db-1")];
    app.apply_merged_settings(&prefs);
    assert!(
        app.ssh_offer.is_none(),
        "re-excluding db-1 takes its offer down"
    );
    app.consider_ssh_offer(pane, Some(String::from("jump")));
    prefs.remote_offer_excluded_hosts = vec![String::from("jump")];
    app.apply_merged_settings(&prefs);
    app.consider_ssh_offer(pane, Some(String::from("jump")));
    assert!(
        app.ssh_offer.is_none(),
        "jump stayed excluded and stayed quiet"
    );
}

#[test]
fn cmd_k_g_with_no_offer_open_says_so_instead_of_connecting() {
    // The chord's guidance branch: nothing to accept, nothing launched, a
    // status that says where the offer comes from. (The accept branch runs
    // `request_remote_launch`, which spawns a real ssh; `accept_ssh_offer`
    // is covered directly instead of reaching the network from a test.)
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    assert!(app.ssh_offer.is_none());
    assert!(app.handle_cmd_k_chord(key(KeyCode::Char('g'), KeyModifiers::NONE)));
    assert!(
        app.status.starts_with("No ssh workspace offer is open"),
        "status: {}",
        app.status
    );
    assert!(app.connect_dialog.is_none(), "nothing was launched");
    assert!(app.pending_remote_launch_host.is_none());
}

#[test]
fn ssh_samples_from_the_label_thread_drive_the_offer_by_shell_pid() {
    // The label thread ships `(shell pid, host)`; the app maps the shell pid
    // to the pane's uid and feeds the same transition logic the direct tests
    // cover. An unknown shell pid is ignored.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.remote_offer_refused.clear();
    let shell = app.terminals[0]
        .shell_pid()
        .expect("the pane has a shell pid");
    let uid = app.terminals[0].uid();
    app.apply_ssh_samples(vec![
        (shell, Some(String::from("db-1"))),
        (-42, Some(String::from("x"))),
    ]);
    assert!(
        app.ssh_offer
            .as_ref()
            .is_some_and(|o| o.pane == uid && o.host == "db-1")
    );
    app.apply_ssh_samples(vec![(shell, None)]);
    assert!(
        app.ssh_offer.is_none(),
        "the session ended through the same path"
    );
}

#[test]
fn a_failed_provisioning_is_remembered_and_a_later_success_forgets_it() {
    // Drives the two notes the install session calls, against the cache-dir
    // override so nothing touches this machine's real memory.
    let _serial = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            *crate::app::CACHE_DIR_OVERRIDE_FOR_TEST.lock().unwrap() = None;
        }
    }
    let _restore = Restore;
    *crate::app::CACHE_DIR_OVERRIDE_FOR_TEST.lock().unwrap() = Some(tmp.path().join("cache"));
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    assert!(
        app.remote_offer_refused.is_empty(),
        "a fresh cache dir remembers nothing"
    );
    let pane = app.terminals[0].uid();

    app.note_provisioning_failed("DB-1", "refused");
    assert!(
        app.remote_offer_refused.contains_key("db-1"),
        "remembered in memory, lower-cased"
    );
    let refused_path = crate::remote::refused_hosts_path(&tmp.path().join("cache"));
    let on_disk = crate::remote::load_refused_hosts(&refused_path, std::time::SystemTime::now());
    assert!(on_disk.contains_key("db-1"), "and on disk: {on_disk:?}");
    app.consider_ssh_offer(pane, Some(String::from("db-1")));
    assert!(app.ssh_offer.is_none(), "a refused host is not offered");

    app.note_provisioning_succeeded("db-1");
    assert!(
        !app.remote_offer_refused.contains_key("db-1"),
        "a success forgets it in memory"
    );
    let on_disk = crate::remote::load_refused_hosts(&refused_path, std::time::SystemTime::now());
    assert!(!on_disk.contains_key("db-1"), "and on disk");
    app.consider_ssh_offer(pane, None);
    app.consider_ssh_offer(pane, Some(String::from("db-1")));
    assert!(app.ssh_offer.is_some(), "and the host is offered again");
}

#[test]
fn a_success_forgets_a_refusal_another_croft_recorded() {
    // The refusal file is shared by every croft on the machine. A refusal
    // this instance never loaded (recorded by another one after this one
    // started) must still be erased by a success here, or it outlives the
    // host that demonstrably works for its whole seven-day TTL.
    let _serial = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            *crate::app::CACHE_DIR_OVERRIDE_FOR_TEST.lock().unwrap() = None;
        }
    }
    let _restore = Restore;
    let cache = tmp.path().join("cache");
    *crate::app::CACHE_DIR_OVERRIDE_FOR_TEST.lock().unwrap() = Some(cache.clone());
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.remote_offer_refused.clear();
    let refused_path = crate::remote::refused_hosts_path(&cache);
    // "Another instance" writes the refusal behind this one's back.
    crate::remote::remember_refused_host(&refused_path, "db-1", std::time::SystemTime::now())
        .unwrap();
    assert!(
        crate::remote::load_refused_hosts(&refused_path, std::time::SystemTime::now())
            .contains_key("db-1")
    );
    app.note_provisioning_succeeded("DB-1");
    assert!(
        !crate::remote::load_refused_hosts(&refused_path, std::time::SystemTime::now())
            .contains_key("db-1"),
        "the disk refusal is gone even though this instance never held it"
    );
}

#[test]
fn provisioning_outcomes_are_logged_to_the_remote_output_channel() {
    // The cache-dir override is process-global; serialize with the
    // other tests that redirect it.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    // #364: a host that gets croft through provisioning says so in OUTPUT >
    // Remote, success and failure alike, with the reason on failure.
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.note_provisioning_failed("zz364-a.example", "no space left on device");
        app.note_provisioning_succeeded("zz364-b.example");
        let lines = crate::output::snapshot(crate::output::CHANNEL_REMOTE)
            .expect("the Remote channel exists");
        let failed = lines
            .iter()
            .find(|l| l.text.contains("zz364-a.example"))
            .expect("the failure is logged");
        assert!(
            failed.text.contains("no space left on device"),
            "{}",
            failed.text
        );
        assert_eq!(failed.level, crate::output::OutputLevel::Error);
        assert!(
            lines.iter().any(|l| l.text.contains("zz364-b.example")
                && l.level != crate::output::OutputLevel::Error),
            "the success is logged"
        );
    });
}

#[test]
fn fleet_output_shows_each_hosts_time_and_the_lines_that_differ() {
    // #363: a DIFFERS row says what differs, line by line, not just that
    // something did; every row says how long the host took.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let r = |host: &str, out: &str, ms: u64| crate::fleet::HostResult {
        host: String::from(host),
        output: String::from(out),
        exit: Some(0),
        elapsed: std::time::Duration::from_millis(ms),
    };
    app.fleet_running = true;
    app.fleet_tx
        .send(vec![
            r("zz363-a", "Linux\n6.8.0-45", 120),
            r("zz363-b", "Linux\n6.8.0-45", 80),
            r("zz363-odd", "Linux\n6.8.0-31", 1500),
        ])
        .unwrap();
    assert!(app.drain_fleet_results());
    let lines: Vec<String> = crate::output::snapshot(crate::output::CHANNEL_FLEET)
        .unwrap()
        .into_iter()
        .map(|l| l.text)
        .collect();
    let odd = lines
        .iter()
        .position(|l| l.starts_with("zz363-odd"))
        .expect("the odd host has a row");
    assert!(
        lines[odd].contains("[DIFFERS]") && lines[odd].contains("1.5s"),
        "{}",
        lines[odd]
    );
    assert_eq!(
        lines[odd + 1..odd + 3],
        ["    - 6.8.0-45", "    + 6.8.0-31"],
        "the reference's line and the host's, alone"
    );
    let a = lines.iter().find(|l| l.starts_with("zz363-a")).unwrap();
    assert!(a.contains("0.1s"), "{a}");
    assert!(
        !lines
            .iter()
            .any(|l| l.ends_with(" Linux") && l.starts_with("    ")),
        "an equal line is not marked"
    );
}

/// #363 end to end: a fleet run's results open as tiles in the Fleet tab,
/// `s` saves the whole run to a file, `d` switches to full output, Enter
/// opens a shell pane on the selected host, and Esc closes the tab.
#[test]
fn a_fleet_run_opens_tiles_that_save_and_open_a_shell() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.run_fleet_command("localhost: echo fleet-ok");
    crate::test_budget::await_spawned(std::time::Duration::from_secs(10), "the fleet run", || {
        app.drain_fleet_results();
        app.editor.fleet.is_some()
    });
    let view = app.editor.fleet.as_ref().unwrap();
    assert_eq!(view.command, "echo fleet-ok");
    assert_eq!(view.results.len(), 1);
    assert_eq!(view.results[0].output, "fleet-ok");
    assert_eq!(app.editor.path.as_deref(), Some(Path::new("Fleet")));
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let screen: String = (0..30)
        .map(|y| {
            (0..120)
                .map(|x| term.backend().buffer()[(x, y)].symbol())
                .collect::<String>()
                + "\n"
        })
        .collect();
    assert!(screen.contains("localhost"), "{screen}");

    app.handle_key(key(KeyCode::Char('d'), KeyModifiers::NONE))
        .unwrap();
    assert!(!app.editor.fleet.as_ref().unwrap().diff_mode);
    app.handle_key(key(KeyCode::Char('s'), KeyModifiers::NONE))
        .unwrap();
    let saved = std::fs::read_dir(tmp.path())
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("fleet-"))
        })
        .expect("a capture file");
    let text = std::fs::read_to_string(saved).unwrap();
    assert!(text.starts_with("$ echo fleet-ok\n"), "{text}");
    assert!(text.contains("== localhost (exit 0,"), "{text}");

    let panes = app.terminals.len();
    app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(
        app.terminals.len(),
        panes + 1,
        "a shell pane opened: {}",
        app.status
    );
    app.focus_pane(Pane::Editor);
    app.handle_key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert!(app.editor.fleet.is_none());
}

#[test]
fn fleet_shell_commands_quote_the_target() {
    assert_eq!(fleet_shell_command("localhost"), None);
    assert_eq!(fleet_shell_command("db-1").as_deref(), Some("ssh 'db-1'"));
    assert_eq!(
        fleet_shell_command("docker:web").as_deref(),
        Some("docker exec -it 'web' sh")
    );
}

/// A second click on the same forwarded loopback link reuses the tunnel
/// instead of asking for another (each one was a new tunnel on a random port
/// and a new browser tab, #648); a click while one is in flight asks nothing.
#[test]
fn a_forwarded_port_is_reused_and_a_pending_forward_not_repeated() {
    use crate::widgets::ports::PortOrigin;
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let workspace = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let mut app = App::new(workspace.path().to_path_buf()).unwrap();
    let log = with_relay_home(home.path(), || {
        let log = app.relay_log_path().expect("relay log path derivable");
        app.request_remote_forward(3000, true);
        app.request_remote_forward(3000, true);
        let first = std::fs::read_to_string(&log).unwrap_or_default();
        assert_eq!(first.matches("forward\t").count(), 1, "{first:?}");
        // Once forwarded, a click opens the existing tunnel.
        app.pending_remote_pulls.clear();
        app.ports
            .upsert(3000, None, None, PortOrigin::Remote("box".into()));
        app.ports.mark_forwarded(3000, 3001);
        app.request_remote_forward(3000, true);
        log
    });
    let all = std::fs::read_to_string(&log).unwrap();
    assert_eq!(all.matches("forward\t").count(), 1, "{all:?}");
    assert!(all.contains("http://127.0.0.1:3001/"), "{all:?}");
}

/// A local croft (no update watcher) never pauses its servers, and the
/// marker is read at most once a second.
#[test]
fn only_a_remote_launched_croft_polls_the_build_marker() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    assert!(app.update_watch.is_none());
    assert!(!app.poll_source_build_marker());
    assert!(
        app.build_marker_checked.is_none(),
        "a local croft read the marker"
    );
    assert!(app.lsp.is_some());

    app.update_watch = Some(crate::update_watch::UpdateWatch::start(
        tmp.path().to_path_buf(),
        String::new(),
    ));
    let just_now = std::time::Instant::now();
    app.build_marker_checked = Some(just_now);
    assert!(!app.poll_source_build_marker());
    assert_eq!(
        app.build_marker_checked,
        Some(just_now),
        "the marker was read again within the second"
    );
}

#[test]
fn the_output_ports_and_captures_tabs_are_announced_by_name_and_row() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus = Pane::Terminal;

    let channel = "a11y-1295-channel";
    crate::output::push(channel, crate::output::OutputLevel::Info, "hello");
    app.output.sync();
    assert!(app.output.select_by_name(channel));
    app.bottom_panel_tab = BottomPanelTab::Output;
    assert_eq!(app.a11y_snapshot().focus, format!("Output, {channel}"));

    app.bottom_panel_tab = BottomPanelTab::Ports;
    let snap = app.a11y_snapshot();
    assert_eq!((snap.focus.as_str(), snap.item), ("Ports, none", None));
    app.ports.upsert(
        3000,
        Some("http://localhost:3000".into()),
        Some("node".into()),
        crate::widgets::ports::PortOrigin::Local,
    );
    let snap = app.a11y_snapshot();
    assert_eq!(snap.focus, "Ports, 1 port");
    assert_eq!(
        snap.item.as_deref(),
        Some("Port 3000, node, http://localhost:3000")
    );

    app.bottom_panel_tab = BottomPanelTab::Captures;
    assert_eq!(app.a11y_snapshot().focus, "Captures, none");
    app.captures.push(crate::widgets::captures::CapturedLine {
        pane: "zsh".into(),
        shell_pid: None,
        message: "Build failed".into(),
        line: "error: build failed".into(),
    });
    let snap = app.a11y_snapshot();
    assert_eq!(snap.focus, "Captures, 1 capture");
    assert_eq!(snap.item.as_deref(), Some("Build failed, from zsh"));
}
