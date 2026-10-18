//! Self-update: update checks, staged updates and relaunch.

use super::*;

#[test]
fn the_update_chord_relaunches_only_when_an_update_is_ready() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let chord = KeyModifiers::CONTROL | KeyModifiers::SHIFT;

    // Not ready: the chord must not arm the re-exec. With nothing on offer
    // it checks now, and says what it found (#1844).
    let _ = app.handle_key(key(KeyCode::F(9), chord));
    assert!(!app.pending_reexec);
    assert!(!app.quit);
    assert_eq!(app.status, "Checking for croft updates\u{2026}");
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(500),
        "the update check to answer",
        || app.poll_update_watch(),
    );
    assert_eq!(
        app.status,
        "Could not check for croft updates: tests never ask the network"
    );

    // Ready: the chord arms the re-exec and quits the loop so `run` can
    // exec. Cmd+Shift+F9 is the same chord.
    app.update_status = UpdateStatus::Ready;
    let _ = app.handle_key(key(
        KeyCode::F(9),
        KeyModifiers::SUPER | KeyModifiers::SHIFT,
    ));
    assert!(app.pending_reexec);
    assert!(app.quit);
}

/// #1844: Help: Update croft with a version on offer stages it, the way
/// the popup's Update does: it used to say no update was waiting.
#[test]
fn the_update_command_stages_the_offered_version() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.install_source = crate::update_check::InstallSource::SelfManaged;
    app.update_toast = Some(super::UpdateToast {
        version: String::from("9.9.9"),
        ready: false,
        failed: None,
        buttons: Vec::new(),
    });
    app.run_command(crate::widgets::command_palette::Command::UpdateCroft);
    assert_eq!(
        app.staged_install.as_ref().map(|s| s.version.as_str()),
        Some("9.9.9")
    );
    assert!(app.manual_update_check.is_none(), "{}", app.status);
}

/// #1844: an update that fails says why in the popup, the network above
/// all, and offers Retry; it used to come back as a fresh offer, with only
/// a "could not be built" on the status line.
#[test]
fn a_failed_update_says_why_in_the_popup_and_offers_retry() {
    use crate::update_watch::UpdateEvent;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.install_source = crate::update_check::InstallSource::SelfManaged;
    app.update_toast = Some(super::UpdateToast {
        version: String::from("9.9.9"),
        ready: false,
        failed: None,
        buttons: Vec::new(),
    });
    // A failure that lands with its start, as an offline one does.
    app.staged_install = Some(
        crate::update_check::StagedInstall::preloaded(
            &[UpdateEvent::InProgress, UpdateEvent::Failed],
            PathBuf::new(),
            "9.9.9",
        )
        .failing_with(crate::update_check::OFFLINE_REASON),
    );
    assert!(app.poll_update_watch());
    assert!(app.staged_install.is_none());
    assert!(
        app.status.contains("not installed: no internet connection"),
        "{}",
        app.status
    );
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let buf = term.backend().buffer();
    let text: String = (0..buf.area.height)
        .map(|y| {
            (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        text.contains("croft v9.9.9 was not installed: no internet connection"),
        "{text}"
    );
    let retry = app
        .update_toast
        .as_ref()
        .unwrap()
        .buttons
        .iter()
        .find(|(_, a)| *a == super::UpdateToastAction::Update)
        .map(|(r, _)| *r)
        .expect("a Retry button");
    assert_eq!(buf[(retry.x + 1, retry.y)].symbol(), "R", "labelled Retry");
    left_click(&mut app, retry.x, retry.y);
    assert!(app.staged_install.is_some(), "Retry stages again");
    assert_eq!(app.update_toast.as_ref().unwrap().failed, None);
}

/// #1844: with nothing on offer, Help: Update croft asks the network now
/// and says what it found: up to date, a newer version (the popup comes
/// up), or that it could not ask.
#[test]
fn the_update_command_checks_now_and_reports_the_answer() {
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        let ask = |app: &mut App, fetch: fn() -> Result<Option<String>, String>| {
            app.update_fetch = fetch;
            app.update_croft();
            assert_eq!(app.status, "Checking for croft updates\u{2026}");
            crate::test_budget::await_spawned(
                std::time::Duration::from_millis(500),
                "the update check to answer",
                || app.poll_update_watch(),
            );
            app.status.clone()
        };
        assert_eq!(
            ask(&mut app, || Err(String::from(
                crate::update_check::OFFLINE_REASON
            ))),
            "Could not check for croft updates: no internet connection"
        );
        assert_eq!(
            ask(&mut app, || Ok(None)),
            format!("croft v{} is up to date", env!("CARGO_PKG_VERSION"))
        );
        assert!(app.update_toast.is_none());
        assert_eq!(
            ask(&mut app, || Ok(Some(String::from("999.0.0")))),
            "croft v999.0.0 is available"
        );
        assert_eq!(
            app.update_toast.as_ref().map(|t| t.version.as_str()),
            Some("999.0.0")
        );
    });
}

/// #1844: Help: Update croft checks for a release only where the startup
/// check would: never on a remote-launched croft, which the croft that
/// launched it updates, nor with `CROFT_NO_UPDATE_CHECK` set.
#[test]
fn a_manual_update_check_keeps_to_the_startup_checks_rules() {
    assert_eq!(super::manual_update_check_refusal(false, false), None);
    for (remote, disabled) in [(true, false), (false, true), (true, true)] {
        let why = super::manual_update_check_refusal(remote, disabled)
            .expect("no check when the startup check is off");
        assert!(why.starts_with("No croft update is waiting"), "{why}");
    }
}

// #865: the updater's own chord takes the drift hint's offer, and a second
// press while that rebuild runs does not start another.
#[test]
fn the_update_chord_starts_the_offered_rebuild_once() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.local_drift = Some(String::from("def456"));
    let chord = key(KeyCode::F(9), KeyModifiers::CONTROL | KeyModifiers::SHIFT);
    let _ = app.handle_key(chord);
    assert!(app.self_install.is_some(), "the chord starts the rebuild");
    assert!(
        app.editor.breakpoints.is_empty(),
        "and toggles no breakpoint"
    );
    assert!(!app.update_armed(), "a started rebuild latches the updater");
    let _ = app.handle_key(chord);
    assert_eq!(app.status, "croft is already updating in the background");
}

// #865 guard: only Ctrl/Cmd+Shift+F9 reaches the updater. With a rebuild on
// offer, Shift+F9 still opens the conditional-breakpoint editor and
// Ctrl+Shift+Alt+F9 falls to the Shift+Alt logpoint arm; neither starts the
// rebuild or relaunches.
#[test]
fn only_ctrl_or_cmd_shift_f9_reaches_the_updater() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.local_drift = Some(String::from("def456"));
    let _ = app.handle_key(key(KeyCode::F(9), KeyModifiers::SHIFT));
    assert_eq!(app.status, "Open a file to set a conditional breakpoint");
    let _ = app.handle_key(key(
        KeyCode::F(9),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT | KeyModifiers::ALT,
    ));
    assert_eq!(app.status, "Open a file to set a logpoint");
    assert!(app.self_install.is_none(), "no rebuild started");
    assert!(!app.pending_reexec && !app.quit, "and no relaunch");
}

#[test]
fn the_update_chord_reexecs_a_ready_update_even_when_the_terminal_is_focused() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.focus_pane(Pane::Terminal);
    app.update_status = UpdateStatus::Ready;
    let _ = app.handle_key(key(
        KeyCode::F(9),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ));
    assert!(
        app.pending_reexec && app.quit,
        "a landed update's chord must win over PTY forwarding"
    );
}

#[test]
fn update_spinner_frame_cycles_through_all_frames_and_wraps() {
    // Frame 0 at the start, advances one frame per UPDATE_SPINNER_FRAME_MS.
    assert_eq!(update_spinner_frame(0), UPDATE_SPINNER_FRAMES[0]);
    assert_eq!(
        update_spinner_frame(UPDATE_SPINNER_FRAME_MS),
        UPDATE_SPINNER_FRAMES[1]
    );
    assert_eq!(
        update_spinner_frame(UPDATE_SPINNER_FRAME_MS * 5 + 50),
        UPDATE_SPINNER_FRAMES[5],
        "mid-frame elapsed still maps to the floor frame"
    );
    // Wraps back to frame 0 after a full cycle of all six frames.
    let cycle = UPDATE_SPINNER_FRAME_MS * UPDATE_SPINNER_FRAMES.len() as u128;
    assert_eq!(update_spinner_frame(cycle), UPDATE_SPINNER_FRAMES[0]);
    assert_eq!(
        update_spinner_frame(cycle + UPDATE_SPINNER_FRAME_MS),
        UPDATE_SPINNER_FRAMES[1]
    );
}

/// #333: a newer release raises a bottom-LEFT popup that never captures
/// keys; Later dismisses it and remembers the version, so the same release
/// is not offered on the next launch.
#[test]
fn an_update_offer_raises_the_bottom_left_toast_and_later_dismisses_it() {
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.update_check = Some(crate::update_check::UpdateCheck::preloaded(Some(
            "9.9.9".into(),
        )));
        assert!(app.poll_update_watch(), "the offer landing is a redraw");
        let toast = app
            .update_toast
            .as_ref()
            .expect("the offer raises the toast");
        assert_eq!(toast.version, "9.9.9");
        let backend = ratatui::backend::TestBackend::new(140, 50);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| app.render(f)).unwrap();
        let buttons = app.update_toast.as_ref().unwrap().buttons.clone();
        let update = buttons
            .iter()
            .find(|(_, a)| matches!(a, super::UpdateToastAction::Update))
            .map(|(r, _)| *r)
            .expect("an Update button");
        assert!(update.x < 70, "the popup sits bottom-left: {update:?}");
        assert!(update.y > 40, "the popup sits bottom-left: {update:?}");
        let later = buttons
            .iter()
            .find(|(_, a)| matches!(a, super::UpdateToastAction::Later))
            .map(|(r, _)| *r)
            .expect("a Later button");
        left_click(&mut app, later.x, later.y);
        assert!(app.update_toast.is_none(), "Later clears the toast");
        assert!(
            app.staged_install.is_none(),
            "Later stages nothing: the installed binary is untouched"
        );
        let cache =
            crate::update_check::CheckCache::load(&home.path().join(".cache").join("croft"));
        assert_eq!(
            cache.dismissed.as_deref(),
            Some("9.9.9"),
            "Later is remembered"
        );
    });
}

/// #333: the three update producers share one event type; a drift
/// rebuild's Failed must not be charged to the staged release build, and a
/// drift Ready must not hand Relaunch a staged binary that was never built.
#[test]
fn update_events_are_charged_to_their_own_producer() {
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        update_events_are_charged_to_their_own_producer_body();
    });
}

fn update_events_are_charged_to_their_own_producer_body() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let staged = tmp.path().join("never-built");
    app.staged_install = Some(crate::update_check::StagedInstall::preloaded(
        &[],
        staged.clone(),
        "9.9.9",
    ));
    app.self_install = Some(crate::update_watch::SelfInstall::preloaded(&[
        crate::update_watch::UpdateEvent::Failed,
    ]));
    assert!(app.poll_update_watch());
    assert!(
        app.staged_install.is_some(),
        "the drift rebuild's failure leaves the staged build running"
    );
    assert!(
        app.self_install.is_none(),
        "the drift rebuild is the one that ended"
    );
    assert!(
        app.status.contains("rebuild failed") && !app.status.contains("9.9.9"),
        "the failure names the rebuild, not the release: {}",
        app.status
    );

    app.self_install = Some(crate::update_watch::SelfInstall::preloaded(&[
        crate::update_watch::UpdateEvent::Ready,
    ]));
    assert!(app.poll_update_watch());
    assert_eq!(
        app.update_status,
        UpdateStatus::Ready,
        "the updater is armed for the rebuild"
    );
    assert_eq!(
        app.staged_update_binary(),
        None,
        "Relaunch must not swap in a staged binary that never landed"
    );
    assert!(
        !app.update_toast.as_ref().is_some_and(|t| t.ready),
        "the popup does not claim the release is ready"
    );

    // And the updater stays out of the rebuild path while the staged build
    // holds cargo's lock (#245's latch, extended).
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.local_drift = Some(String::from("abc123"));
    assert!(app.update_armed(), "a drift hint arms the updater");
    app.staged_install = Some(crate::update_check::StagedInstall::preloaded(
        &[],
        staged,
        "9.9.9",
    ));
    assert!(!app.update_armed(), "not while a staged build runs");

    // A staged build that LANDED keeps its Relaunch through an unrelated
    // rebuild, whatever that rebuild does: the staged channel delivers one
    // Ready only, so the two lifecycles must never write each other's cell.
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let landed = tmp.path().join("landed");
    std::fs::write(&landed, b"bin").unwrap();
    app.staged_install = Some(crate::update_check::StagedInstall::preloaded(
        &[
            crate::update_watch::UpdateEvent::InProgress,
            crate::update_watch::UpdateEvent::Ready,
        ],
        landed.clone(),
        "9.9.9",
    ));
    assert!(app.poll_update_watch());
    assert_eq!(
        app.staged_update_binary().as_deref(),
        Some(landed.as_path())
    );
    // The real sequence a drift rebuild emits: InProgress first, then its
    // outcome. The staged binary stays swappable throughout.
    app.self_install = Some(crate::update_watch::SelfInstall::preloaded(&[
        crate::update_watch::UpdateEvent::InProgress,
    ]));
    assert!(app.poll_update_watch());
    assert_eq!(
        app.staged_update_binary().as_deref(),
        Some(landed.as_path())
    );
    assert!(
        app.update_armed(),
        "the updater still relaunches mid-rebuild"
    );
    app.self_install = Some(crate::update_watch::SelfInstall::preloaded(&[
        crate::update_watch::UpdateEvent::Failed,
    ]));
    assert!(app.poll_update_watch());
    assert_eq!(
        app.staged_update_binary().as_deref(),
        Some(landed.as_path())
    );
    assert!(app.update_armed(), "and after the rebuild fails");
    assert!(app.status.contains("rebuild failed"), "{}", app.status);
    let backend = ratatui::backend::TestBackend::new(140, 50);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let relaunch = app
        .update_toast
        .as_ref()
        .expect("the ready toast")
        .buttons
        .iter()
        .find(|(_, a)| matches!(a, super::UpdateToastAction::Relaunch))
        .map(|(r, _)| *r)
        .expect("a Relaunch button");
    left_click(&mut app, relaunch.x, relaunch.y);
    assert!(app.pending_reexec && app.quit, "Relaunch still fires");

    // The symmetric case: a drift rebuild that landed survives a staged
    // build failing.
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.self_install = Some(crate::update_watch::SelfInstall::preloaded(&[
        crate::update_watch::UpdateEvent::InProgress,
        crate::update_watch::UpdateEvent::Ready,
    ]));
    assert!(app.poll_update_watch());
    app.staged_install = Some(crate::update_check::StagedInstall::preloaded(
        &[
            crate::update_watch::UpdateEvent::InProgress,
            crate::update_watch::UpdateEvent::Failed,
        ],
        tmp.path().join("nope"),
        "9.9.9",
    ));
    assert!(app.poll_update_watch());
    assert_eq!(
        app.update_status,
        UpdateStatus::Ready,
        "the rebuild's relaunch is alive"
    );
    assert!(app.update_armed());
    assert_eq!(app.staged_update_binary(), None);

    // Relaunch on the popup fires only for the STAGED readiness: with a
    // drift Ready and no staged one, the click does nothing.
    app.update_toast = Some(super::UpdateToast {
        version: String::from("9.9.9"),
        ready: true,
        failed: None,
        buttons: Vec::new(),
    });
    term.draw(|f| app.render(f)).unwrap();
    let relaunch = app
        .update_toast
        .as_ref()
        .unwrap()
        .buttons
        .iter()
        .find(|(_, a)| matches!(a, super::UpdateToastAction::Relaunch))
        .map(|(r, _)| *r)
        .expect("a Relaunch button");
    left_click(&mut app, relaunch.x, relaunch.y);
    assert!(
        !app.pending_reexec,
        "no staged binary: Relaunch does not re-exec a rebuild"
    );
}

/// #333: both corner popups can be up at once; they must not overlap.
#[test]
fn the_update_toast_stacks_above_the_port_toast() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.update_toast = Some(super::UpdateToast {
        version: String::from("9.9.9"),
        ready: false,
        failed: None,
        buttons: Vec::new(),
    });
    app.show_port_toast(3000, Some("node".into()));
    let backend = ratatui::backend::TestBackend::new(60, 40);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let update_row = app.update_toast.as_ref().unwrap().buttons[0].0.y;
    let port_row = app.port_toast.as_ref().unwrap().buttons[0].0.y;
    assert!(
        update_row + 4 <= port_row,
        "the update popup sits a full box above the port toast: {update_row} vs {port_row}"
    );
}

/// #333: a staged build that lands turns the popup into a Relaunch offer,
/// and Relaunch is the same re-exec the updater chord arms - never automatic.
#[test]
fn a_staged_update_offers_relaunch_and_only_relaunch_re_execs() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    let staged = tmp.path().join("staged-croft");
    app.staged_install = Some(crate::update_check::StagedInstall::preloaded(
        &[
            crate::update_watch::UpdateEvent::InProgress,
            crate::update_watch::UpdateEvent::Ready,
        ],
        staged.clone(),
        "9.9.9",
    ));
    assert!(app.poll_update_watch());
    assert_eq!(app.staged_status, UpdateStatus::Ready);
    assert!(app.any_update_ready());
    assert!(!app.pending_reexec, "landing never relaunches by itself");
    assert_eq!(
        app.staged_update_binary().as_deref(),
        Some(staged.as_path())
    );
    let backend = ratatui::backend::TestBackend::new(140, 50);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let relaunch = app
        .update_toast
        .as_ref()
        .expect("the ready toast")
        .buttons
        .iter()
        .find(|(_, a)| matches!(a, super::UpdateToastAction::Relaunch))
        .map(|(r, _)| *r)
        .expect("a Relaunch button");
    left_click(&mut app, relaunch.x, relaunch.y);
    assert!(app.pending_reexec && app.quit, "Relaunch arms the re-exec");
}

// The 2026-08-09 gitcgr report: the installer slid into the slow
// compile-on-remote fallback without asking. `answer_install_fallback` is the
// decision point behind both the Enter and the click handler, so the y/yes
// parsing and the decline teardown are pinned here rather than at each caller.
#[test]
fn install_fallback_proceeds_only_on_an_explicit_yes() {
    let tmp = tempfile::tempdir().unwrap();
    for reply in ["y", "Y", "yes", "YES", "  yes  "] {
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.connect_dialog = Some(crate::widgets::connect_dialog::ConnectDialog::new(
            "gitcgr".to_string(),
        ));
        app.answer_install_fallback(reply);
        let dialog = app.connect_dialog.as_ref().unwrap_or_else(|| {
            panic!("{reply:?} accepts the slow compile, so the dialog stays up")
        });
        assert_eq!(
            dialog.phase,
            crate::widgets::connect_dialog::DialogPhase::Installing,
            "{reply:?} should hand over to the installing phase"
        );
    }
}

#[test]
fn install_fallback_declines_and_tears_down_on_anything_else() {
    let tmp = tempfile::tempdir().unwrap();
    // An empty submit (bare Enter) is the important one: the footer offers it
    // as Cancel, so it must never read as consent.
    for reply in ["", "   ", "n", "no", "yep", "later"] {
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.connect_dialog = Some(crate::widgets::connect_dialog::ConnectDialog::new(
            "gitcgr".to_string(),
        ));
        app.pending_remote_launch_host = Some("gitcgr".to_string());
        app.answer_install_fallback(reply);
        assert!(
            app.connect_dialog.is_none(),
            "{reply:?} declines, which closes the dialog"
        );
        assert!(
            app.pending_remote_launch_host.is_none(),
            "{reply:?} declines, so the pending launch is torn down too"
        );
        assert!(
            app.status.contains("setup-cross"),
            "the decline must point at the fast-path fix, got {:?}",
            app.status
        );
    }
}

#[test]
fn install_fallback_without_a_dialog_does_not_panic() {
    // The install thread can answer after the dialog is gone (user hit Esc
    // while the prompt was in flight); both arms must tolerate the absence.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.answer_install_fallback("y");
    assert!(app.connect_dialog.is_none());
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.answer_install_fallback("");
    assert!(app.connect_dialog.is_none());
}

// 2026-08-22 (#242): a Mac croft shipped a remote main @ 84a31a0 while itself
// running an hour-old binary — locally, nothing ever compares the installed
// croft against the repo it came from. The drift hint arms the updater
// (Ctrl+Shift+F9 since #865) only while no update is otherwise in flight.
#[test]
fn drift_hint_arms_the_updater_only_while_idle() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    assert!(
        !app.update_armed(),
        "no drift, no landed update: nothing to do"
    );
    app.local_drift = Some(String::from("def456"));
    assert!(app.update_armed(), "an armed drift hint offers a rebuild");
    app.self_install = Some(crate::update_watch::SelfInstall::preloaded(&[]));
    assert!(
        !app.update_armed(),
        "a spawned install must latch the updater even before its InProgress \
         event is drained - two presses in one input batch must not double-spawn"
    );
    app.self_install = None;
    app.update_status = UpdateStatus::InProgress;
    assert!(!app.update_armed(), "a running rebuild must not re-trigger");
    app.local_drift = None;
    app.update_status = UpdateStatus::Ready;
    assert!(app.update_armed(), "a landed update offers the re-exec");
}

// The probe end-to-end against a real repo: a baked hash that differs from
// the repo's HEAD must arm the hint and say how to act on it.
#[test]
fn drift_probe_verdict_arms_the_rebuild_hint() {
    let repo = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?} failed");
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };
    git(&["init", "-q"]);
    git(&[
        "-c",
        "user.email=t@t",
        "-c",
        "user.name=t",
        "commit",
        "-q",
        "--allow-empty",
        "-m",
        "one",
    ]);
    let head = git(&["rev-parse", "--short", "HEAD"]);

    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.drift_probe = Some(crate::update_watch::DriftProbe::start(
        repo.path().to_string_lossy().into_owned(),
        String::from("0000000"),
    ));
    let mut armed = false;
    for _ in 0..50 {
        if app.poll_drift_probe() {
            armed = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(armed, "the probe's verdict must arrive and report drift");
    assert!(app.drift_probe.is_none(), "the probe answers exactly once");
    assert_eq!(app.local_drift.as_deref(), Some(head.as_str()));
    assert!(
        app.status.contains(super::UPDATE_CROFT_CHORD),
        "the status must say how to act on the drift: {:?}",
        app.status
    );
}

// CodeRabbit on #243: a failed rebuild used to strand the user — the drift
// marker was cleared when the install started and nothing restored it, so
// after a failure the offer silently went away and only a full relaunch
// re-probed. A failure must leave the hint armed for a retry.
#[test]
fn a_failed_reinstall_rearms_the_drift_hint_for_retry() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.local_drift = Some(String::from("def456"));
    app.self_install = Some(crate::update_watch::SelfInstall::preloaded(&[
        crate::update_watch::UpdateEvent::InProgress,
        crate::update_watch::UpdateEvent::Failed,
    ]));
    assert!(app.poll_update_watch());
    assert_eq!(app.update_status, UpdateStatus::Idle);
    assert!(
        app.status
            .contains(&format!("self-install-{}.log", std::process::id())),
        "a failure must say where THIS process's build log is: {:?}",
        app.status
    );
    assert!(
        app.update_armed(),
        "the drift hint must survive a failed rebuild so the updater retries"
    );
}

#[test]
fn a_homebrew_install_is_offered_brew_upgrade_not_a_self_update() {
    // The cache-dir override is process-global; serialize with the
    // other tests that redirect it.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    // #375: a binary inside Homebrew's Cellar must not be rebuilt or swapped
    // by croft: the popup names `brew upgrade croft` and has no Update.
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.install_source = crate::update_check::InstallSource::Homebrew;
        app.update_check = Some(crate::update_check::UpdateCheck::preloaded(Some(
            "9.9.9".into(),
        )));
        assert!(app.poll_update_watch());
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 50)).unwrap();
        term.draw(|f| app.render(f)).unwrap();
        let buttons = app.update_toast.as_ref().unwrap().buttons.clone();
        assert!(
            !buttons
                .iter()
                .any(|(_, a)| matches!(a, super::UpdateToastAction::Update)),
            "no Update button on a Homebrew install"
        );
        assert!(
            buttons
                .iter()
                .any(|(_, a)| matches!(a, super::UpdateToastAction::Later)),
            "Later still dismisses the offer"
        );
        let buf = term.backend().buffer();
        let mut screen = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                screen.push_str(buf[(x, y)].symbol());
            }
        }
        assert!(
            screen.contains("brew upgrade croft"),
            "the popup names the command to run"
        );
        // Even a direct request cannot stage a build over the Cellar.
        app.start_staged_update("9.9.9".into());
        assert!(app.staged_install.is_none(), "nothing staged");
        assert!(app.status.contains("brew upgrade croft"), "{}", app.status);
    });
}

#[test]
fn a_self_managed_install_still_offers_update() {
    // The cache-dir override is process-global; serialize with the
    // other tests that redirect it.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let app = App::new(tmp.path().to_path_buf()).unwrap();
        assert_eq!(
            app.install_source,
            crate::update_check::InstallSource::SelfManaged,
            "a test binary under target/ is not Homebrew-owned"
        );
    });
}

/// #1588: an update relaunch, unlike hot exit, carries the merge editor's
/// unsaved Result to the new process: it exits at once, and the Result
/// exists nowhere else.
#[test]
fn an_update_relaunch_keeps_a_dirty_merge_result() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut app, file) = app_with_merge_editor_open(tmp.path());
    let disk = std::fs::read_to_string(&file).unwrap();
    let result = app.editor.lines.clone();
    assert_eq!(result, vec!["top", "mine", "bottom"]);
    app.arm_reexec();
    assert!(app.pending_reexec, "the relaunch is armed");
    let state = app.capture_session_state();
    let tab = state
        .tabs
        .iter()
        .find(|t| t.path.as_deref() == Some(file.as_path()))
        .expect("the tab is handed over");
    assert!(tab.dirty);
    assert_eq!(tab.unsaved_text.as_deref(), Some("top\nmine\nbottom"));

    let mut next = App::new(tmp.path().to_path_buf()).unwrap();
    next.apply_session_state(&state);
    let ed = next
        .editor
        .editors
        .iter()
        .find(|e| e.path.as_deref() == Some(file.as_path()))
        .expect("the file is reopened");
    assert!(ed.dirty);
    assert_eq!(ed.lines, result);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), disk);
}
