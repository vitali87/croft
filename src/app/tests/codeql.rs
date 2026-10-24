//! CodeQL: databases, queries, query history, SARIF results, code scanning
//! and variant analysis.

use super::*;

/// #856: "CodeQL: Run Tests" put "Running CodeQL tests" in the status bar
/// and left it there after the run. It now says how the run went, as a
/// run-at-cursor does.
#[cfg(unix)]
#[test]
fn a_finished_codeql_test_run_replaces_its_running_status_with_the_outcome() {
    let mut s = CodeqlStandIn::new();
    s.will(STAND_IN_PASS, 0);
    s.app.run_codeql_tests();
    assert_eq!(s.app.status, "Running CodeQL tests");
    s.finish();
    assert!(
        s.app.status.starts_with("CodeQL tests passed ("),
        "{}",
        s.app.status
    );
}

fn screen_lower(term: &ratatui::Terminal<ratatui::backend::TestBackend>) -> String {
    let buf = term.backend().buffer();
    let mut all = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            all.push_str(buf[(x, y)].symbol());
        }
        all.push('\n');
    }
    all.to_lowercase()
}

#[test]
fn codeql_icon_sits_below_testing_and_opens_the_codeql_side_bar() {
    // #578: VS Code's CodeQL extension adds a QL entry to the activity bar;
    // croft's sits directly below Testing and opens the CodeQL side bar.
    // Opening the CodeQL view reads croft's cache, whose test override is
    // process-global: without the lock and a cache of its own, this test
    // reads whatever another CodeQL test has there at the time.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = codeql_app(tmp.path());
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
        term.draw(|f| app.render(f)).unwrap();
        let ql = app.sidebar_areas.codeql_icon;
        let testing = app.sidebar_areas.testing_icon;
        assert!(ql.width > 0, "the QL icon lays out on a tall bar");
        assert_eq!(ql.y, testing.y + testing.height, "directly below Testing");
        left_click(&mut app, ql.x, ql.y);
        assert_eq!(app.sidebar_view, SidebarView::CodeQL);
        term.draw(|f| app.render(f)).unwrap();
        let screen = screen_lower(&term);
        assert!(screen.contains(" codeql "), "the pane's frame:\n{screen}");
        for section in [
            "language",
            "queries",
            "variant analysis",
            "query history",
            "tools",
        ] {
            assert!(
                screen.contains(section),
                "section {section:?} listed:\n{screen}"
            );
        }
        // Without a database the first-run card stands in for the Databases
        // section: its button offers the four ways to add one in a picker.
        for text in [
            "query your code for bugs",
            "+  add database",
            "from a folder, archive,",
            "try quick query",
            "ast · log · model",
        ] {
            assert!(screen.contains(text), "{text:?} shown:\n{screen}");
        }
    });
}

#[test]
fn codeql_add_database_button_offers_the_four_sources_in_a_picker() {
    // #578: the first-run card's one button opens a picker of the places a
    // database comes from; each row asks for the source as its old side-bar
    // link did.
    use crate::widgets::codeql::{Action, Hit};
    use crate::widgets::input_prompt::{CodeqlDbSource, InputPurpose};
    use crate::widgets::list_picker::ListPurpose;
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = codeql_app(tmp.path());
        app.open_codeql_view();
        app.focus = Pane::Tree;
        app.codeql.select_action(Action::AddDatabase);
        assert_eq!(
            app.codeql.selected_hit(),
            Some(Hit::Action(Action::AddDatabase))
        );
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();
        let picker = app.list_picker.as_ref().expect("the sources picker");
        assert_eq!(picker.purpose, ListPurpose::CodeqlDbSource);
        let labels: Vec<&str> = picker.rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "From a folder",
                "From an archive",
                "From a URL (as a zip file)",
                "From GitHub"
            ]
        );
        for c in "GitHub".chars() {
            app.handle_key(key(KeyCode::Char(c), KeyModifiers::NONE))
                .unwrap();
        }
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();
        assert!(app.list_picker.is_none());
        let prompt = app.input_prompt.as_ref().expect("the GitHub prompt");
        assert_eq!(
            prompt.purpose,
            InputPurpose::CodeqlDatabase {
                source: CodeqlDbSource::Github
            }
        );
        assert_eq!(prompt.title, "Add CodeQL Database from GitHub");
    });
}

/// Paint `app` at 120 x 44 and find the first screen cell where `needle`
/// starts.
fn find_on_screen(app: &mut App, needle: &str) -> Option<(u16, u16)> {
    let backend = ratatui::backend::TestBackend::new(120, 44);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let buf = term.backend().buffer();
    (0..44).find_map(|y| {
        let text: String = (0..120).map(|x| buf[(x, y)].symbol()).collect();
        let at = text.find(needle)?;
        Some((text[..at].chars().count() as u16, y))
    })
}

#[test]
fn codeql_database_card_picker_switches_the_current_database() {
    // #578: the database card's name row offers the databases in a picker;
    // the one chosen becomes the current one, as a list row's Enter did.
    use crate::widgets::codeql::{Action, Hit};
    use crate::widgets::list_picker::ListPurpose;
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = codeql_app(tmp.path());
        for (name, lang) in [("flask-db", "python"), ("gin-db", "go")] {
            let db = make_codeql_db(tmp.path(), name, lang);
            app.submit_codeql_database(
                crate::widgets::input_prompt::CodeqlDbSource::Folder,
                &db.display().to_string(),
            );
        }
        assert_eq!(app.codeql.current_db, Some(1));
        app.run_command(crate::widgets::command_palette::Command::CodeqlFocusSideBar);
        assert!(find_on_screen(&mut app, "◆ gin-db").is_some(), "the card");
        let (x, y) = find_on_screen(&mut app, "◆ gin-db").unwrap();
        left_click(&mut app, x + 3, y);
        assert_eq!(
            app.codeql.selected_hit(),
            Some(Hit::Action(Action::PickDatabase)),
            "the click selects the name row"
        );
        let picker = app.list_picker.as_ref().expect("the database picker");
        assert_eq!(picker.purpose, ListPurpose::CodeqlDatabase);
        let labels: Vec<&str> = picker.rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, ["○ flask-db · Python", "● gin-db · Go"]);
        for c in "flask".chars() {
            app.handle_key(key(KeyCode::Char(c), KeyModifiers::NONE))
                .unwrap();
        }
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();
        assert!(app.list_picker.is_none());
        assert_eq!(app.codeql.current_db, Some(0), "{}", app.status);
        assert_eq!(app.status, "Current CodeQL database: flask-db");
        assert_eq!(
            crate::codeql_db::DatabaseStore::load(&App::codeql_db_store_path()).current,
            Some(0),
            "saved"
        );
        assert!(find_on_screen(&mut app, "◆ flask-db").is_some());
        assert!(
            find_on_screen(&mut app, "Python · ").is_some(),
            "the meta row names the language, then the size"
        );
        // Enter on the name row opens it too.
        app.codeql.select_database_card();
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(
            app.list_picker.as_ref().map(|p| &p.purpose),
            Some(&ListPurpose::CodeqlDatabase)
        );
    });
}

#[test]
fn codeql_database_sizes_are_summed_off_the_input_path() {
    // #578: the card's folder size is walked on a worker, so adding or
    // listing a large database never stalls a key; the size lands on a tick
    // and a cached one is not walked again.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        let db = make_codeql_db(tmp.path(), "flask-db", "python");
        std::fs::write(db.join("blob.bin"), vec![0u8; 4096]).unwrap();
        app.submit_codeql_database(
            crate::widgets::input_prompt::CodeqlDbSource::Folder,
            &db.display().to_string(),
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !app.codeql.db_sizes.contains_key(&db) && std::time::Instant::now() < deadline {
            app.sync_explorer_panels();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let size = *app.codeql.db_sizes.get(&db).expect("the size landed");
        assert!(size >= 4096, "{size}");
        assert!(app.codeql_size_pending.is_empty());
        app.refresh_codeql_databases();
        assert!(
            app.codeql_size_pending.is_empty(),
            "a cached size is not walked again"
        );
        assert_eq!(app.codeql.db_sizes.get(&db), Some(&size));
    });
}

#[test]
fn codeql_history_export_chip_reaches_the_export_prompt() {
    // #578: Space expands a history row into its details and action chips;
    // a click on Export asks where to copy that run's results.
    use crate::codeql_query::RunStatus;
    use crate::widgets::input_prompt::InputPurpose;
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let out = |n: &str| App::codeql_results_dir().join(format!("{n}/results.csv"));
        seed_codeql_history(
            tmp.path(),
            &[
                ("old.ql", 100, RunStatus::Succeeded, out("100-old")),
                ("new.ql", 200, RunStatus::Succeeded, out("200-new")),
            ],
        );
        let mut app = codeql_app(tmp.path());
        // A database, so the first-run card leaves the history in view.
        let db = make_codeql_db(tmp.path(), "app", "python");
        app.submit_codeql_database(
            crate::widgets::input_prompt::CodeqlDbSource::Folder,
            &db.display().to_string(),
        );
        app.run_command(crate::widgets::command_palette::Command::CodeqlFocusSideBar);
        // The older run, so the export is not just the newest one's.
        app.codeql.select_history(1);
        app.handle_key(key(KeyCode::Char(' '), KeyModifiers::NONE))
            .unwrap();
        assert!(app.codeql.is_expanded(1));
        assert!(app.input_prompt.is_none() && app.list_picker.is_none());
        app.codeql.select_history(0);
        let (x, y) = find_on_screen(&mut app, "↓ Export").expect("the chip");
        left_click(&mut app, x, y);
        let prompt = app.input_prompt.as_ref().expect("the export prompt");
        assert_eq!(
            prompt.purpose,
            InputPurpose::CodeqlExportResults {
                output: out("100-old")
            }
        );
        assert_eq!(prompt.title, "Export the results of old.ql");
        assert_eq!(app.codeql.selected_history(), Some(1), "its row selected");
        app.close_input_prompt();
        // Left folds the details away again.
        app.focus = Pane::Tree;
        app.handle_key(key(KeyCode::Left, KeyModifiers::NONE))
            .unwrap();
        assert!(!app.codeql.is_expanded(1));
    });
}

#[test]
fn codeql_view_label_round_trips_for_session_restore() {
    // Opening the CodeQL view reads croft's cache, whose test override is
    // process-global: without the lock and a cache of its own, this test
    // reads whatever another CodeQL test has there at the time.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        assert_eq!(sidebar_view_label(SidebarView::CodeQL), "CodeQL");
        assert_eq!(sidebar_view_from_label("CodeQL"), Some(SidebarView::CodeQL));
    });
}

#[test]
fn codeql_is_a_built_in_extensions_row_and_a_palette_command() {
    // Opening the CodeQL view reads croft's cache, whose test override is
    // process-global: without the lock and a cache of its own, this test
    // reads whatever another CodeQL test has there at the time.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let summaries = crate::lsp::manifest::summaries(crate::lsp::manifest::BUNDLED_MANIFESTS);
        assert!(
            summaries.iter().any(|s| s.id == "codeql"),
            "a built-in CodeQL row in the Extensions panel"
        );
        use crate::widgets::command_palette::Command;
        assert_eq!(Command::from_id("show_codeql"), Some(Command::ShowCodeQL));
        assert_eq!(Command::ShowCodeQL.title(), "View: Show CodeQL");
    });
}

#[test]
fn codeql_focus_side_bar_and_open_results_directory_are_palette_commands() {
    // #578: VS Code's "CodeQL: Focus Side Bar", and the results directory
    // of the selected run (else the newest) without the side bar's `o`.
    use crate::codeql_query::RunStatus;
    use crate::widgets::command_palette::Command;
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = codeql_app(tmp.path());
        app.config_dir = tmp.path().join("config");
        assert_eq!(
            Command::from_id("codeql_focus_side_bar"),
            Some(Command::CodeqlFocusSideBar)
        );
        assert_eq!(
            Command::CodeqlFocusSideBar.title(),
            "CodeQL: Focus Side Bar"
        );
        app.run_command(Command::CodeqlOpenResultsDirectory);
        assert_eq!(app.status, "No CodeQL query has run yet");

        let newer = App::codeql_results_dir().join("200-a/results.csv");
        let older = App::codeql_results_dir().join("100-b/results.csv");
        seed_codeql_history(
            tmp.path(),
            &[
                ("b.ql", 100, RunStatus::Succeeded, older.clone()),
                ("a.ql", 200, RunStatus::Succeeded, newer.clone()),
            ],
        );
        app.focus = Pane::Editor;
        app.run_command(Command::CodeqlFocusSideBar);
        assert_eq!(app.sidebar_view, SidebarView::CodeQL);
        assert!(app.focus == Pane::Tree, "the side bar takes the keys");

        // Nothing selected in Query History: the newest run.
        app.run_command(Command::CodeqlOpenResultsDirectory);
        assert_eq!(app.editor.path.as_deref(), Some(newer.as_path()));
        // A selected run: that one.
        app.codeql.select_history(1);
        app.run_command(Command::CodeqlOpenResultsDirectory);
        assert_eq!(
            app.editor.path.as_deref(),
            Some(older.as_path()),
            "{}",
            app.status
        );
    });
}

#[test]
fn disabling_codeql_hides_its_icon_and_leaves_its_view() {
    // Opening the CodeQL view reads croft's cache, whose test override is
    // process-global: without the lock and a cache of its own, this test
    // reads whatever another CodeQL test has there at the time.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = codeql_app(tmp.path());
        // Never write the developer's real ~/.config/croft from a test.
        app.config_dir = tmp.path().join("config");
        app.open_codeql_view();
        assert_eq!(app.sidebar_view, SidebarView::CodeQL);
        app.set_extension_enabled("codeql", false);
        assert_eq!(
            app.sidebar_view,
            SidebarView::Explorer,
            "a disabled feature's view does not stay open"
        );
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
        term.draw(|f| app.render(f)).unwrap();
        assert_eq!(app.sidebar_areas.codeql_icon.width, 0, "the icon is gone");
        assert!(
            app.sidebar_areas.settings_icon.width > 0,
            "the gear still lays out"
        );
        app.open_codeql_view();
        assert_ne!(
            app.sidebar_view,
            SidebarView::CodeQL,
            "cannot open while disabled"
        );
        assert!(app.status.contains("disabled"), "{}", app.status);
        app.set_extension_enabled("codeql", true);
        term.draw(|f| app.render(f)).unwrap();
        assert!(
            app.sidebar_areas.codeql_icon.width > 0,
            "re-enabling restores it"
        );
    });
}

/// #1905: CodeQL ships off. A fresh croft has no QL icon and no CodeQL
/// commands in the palette, and View: Show CodeQL says how to turn it on;
/// turning it on in the Extensions view brings both back and is saved.
#[test]
fn codeql_ships_off_until_turned_on_in_the_extensions_view() {
    // Opening the CodeQL view reads croft's cache, whose test override is
    // process-global: without the lock and a cache of its own, this test
    // reads whatever another CodeQL test has there at the time.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        // Never write the developer's real ~/.config/croft from a test.
        app.config_dir = tmp.path().join("config");
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
        term.draw(|f| app.render(f)).unwrap();
        assert_eq!(app.sidebar_areas.codeql_icon.width, 0, "no QL icon");
        assert!(
            !run_from_palette(&mut app, "CodeQL: Focus Side Bar"),
            "no CodeQL commands in the palette"
        );
        assert!(run_from_palette(&mut app, "View: Show CodeQL"));
        assert_ne!(app.sidebar_view, SidebarView::CodeQL);
        assert!(
            app.status.contains("enable it in the Extensions view"),
            "{}",
            app.status
        );

        app.refresh_extensions();
        let pos = app
            .extensions
            .visible_indices()
            .iter()
            .position(|&i| app.extensions.items()[i].id == "codeql")
            .expect("the CodeQL row is listed");
        app.extensions.select(pos);
        app.toggle_selected_extension();
        assert_eq!(app.status, "Enabled extension 'codeql'");
        term.draw(|f| app.render(f)).unwrap();
        assert!(app.sidebar_areas.codeql_icon.width > 0, "the icon is back");
        assert!(run_from_palette(&mut app, "CodeQL: Focus Side Bar"));
        assert_eq!(app.sidebar_view, SidebarView::CodeQL);
        let saved = crate::prefs::Prefs::load(&app.config_dir.join("config.json")).unwrap();
        assert!(saved.enabled_extensions.contains("codeql"));
    });
}

fn make_codeql_db(root: &std::path::Path, name: &str, lang: &str) -> std::path::PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("codeql-database.yml"),
        format!("primaryLanguage: \"{lang}\"\n"),
    )
    .unwrap();
    dir
}

/// Make database `index` the current one and select the side bar's
/// database card, which acts on it.
fn select_codeql_database(app: &mut App, index: usize) {
    app.activate_codeql(crate::widgets::codeql::Hit::Action(
        crate::widgets::codeql::Action::SelectDatabase(index),
    ));
    app.codeql.select_database_card();
}

#[test]
fn adding_a_codeql_database_from_a_folder_lists_and_persists_it() {
    // The cache-dir override is process-global; serialize with the
    // other tests that redirect it.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    // #578: Databases > From a folder.
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let db = make_codeql_db(tmp.path(), "flask-db", "python");
        let mut app = codeql_app(tmp.path());
        app.submit_codeql_database(
            crate::widgets::input_prompt::CodeqlDbSource::Folder,
            &db.display().to_string(),
        );
        assert!(
            app.status.contains("Added CodeQL database flask-db"),
            "{}",
            app.status
        );
        assert_eq!(app.codeql.databases.len(), 1);
        assert_eq!(app.codeql.current_db, Some(0));
        // A new session lists it too.
        let mut again = codeql_app(tmp.path());
        again.open_codeql_view();
        assert_eq!(again.codeql.databases.len(), 1, "the list persists");
        assert_eq!(
            again.codeql.databases[0].language.as_deref(),
            Some("python")
        );
    });
}

/// Drain the database download or extraction in flight until it lands.
fn wait_for_codeql_db_job(app: &mut App) {
    crate::test_budget::await_spawned(
        std::time::Duration::from_secs(20),
        "the CodeQL database job",
        || {
            app.drain_codeql_db_job();
            app.codeql_db_job.is_none()
        },
    );
}

/// A one-database CodeQL zip (Java) at `dir/<name>.zip`.
fn codeql_db_zip(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    use std::io::Write as _;
    let zp = dir.join(format!("{name}.zip"));
    let mut z = zip::ZipWriter::new(std::fs::File::create(&zp).unwrap());
    let opts =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    z.start_file("python/codeql-database.yml", opts).unwrap();
    z.write_all(b"primaryLanguage: \"python\"\n").unwrap();
    z.finish().unwrap();
    zp
}

/// A stand-in `gh`: lists one `python` database, and downloads `zip` once
/// a `go` file appears in `dir` (or after five seconds), as a large
/// database takes its time.
fn slow_fake_gh(dir: &std::path::Path, zip: &std::path::Path) -> std::path::PathBuf {
    let gh = dir.join("gh");
    std::fs::write(
        &gh,
        format!(
            "#!/bin/sh\ncase \"$*\" in *--jq*) echo python; exit 0;; esac\n\
             i=0; while [ ! -f '{go}' ] && [ $i -lt 100 ]; do sleep 0.05; i=$((i+1)); done\n\
             cat '{zip}'\n",
            go = dir.join("go").display(),
            zip = zip.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    gh
}

#[test]
fn adding_a_codeql_database_from_github_returns_at_once_and_lands_later() {
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let zip = codeql_db_zip(tmp.path(), "requests-db");
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.gh_program = slow_fake_gh(tmp.path(), &zip);
        let started = std::time::Instant::now();
        app.submit_codeql_database(
            crate::widgets::input_prompt::CodeqlDbSource::Github,
            "psf/requests",
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "the download held the UI for {:?}",
            started.elapsed()
        );
        assert_eq!(
            app.status,
            "Adding the CodeQL database psf/requests\u{2026}"
        );
        assert!(app.codeql.databases.is_empty(), "not there yet");
        app.submit_codeql_database(
            crate::widgets::input_prompt::CodeqlDbSource::Github,
            "pallets/flask",
        );
        assert_eq!(
            app.status, "Still adding the CodeQL database psf/requests",
            "one download at a time"
        );
        std::fs::write(tmp.path().join("go"), b"").unwrap();
        wait_for_codeql_db_job(&mut app);
        assert_eq!(app.status, "Added CodeQL database python (python)");
        assert_eq!(app.codeql.databases.len(), 1);
        let entry = &app.codeql.databases[0];
        assert!(
            entry.path.starts_with(home.path()),
            "extracted under croft's cache: {}",
            entry.path.display()
        );
        let leftovers: Vec<_> = std::fs::read_dir(App::codeql_db_cache_dir())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".download.zip"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "the download is removed once unpacked"
        );
    });
}

#[test]
fn a_failed_github_download_says_why_and_adds_nothing() {
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let gh = tmp.path().join("gh");
        std::fs::write(&gh, "#!/bin/sh\necho 'HTTP 404: Not Found' >&2\nexit 1\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.gh_program = gh;
        app.submit_codeql_database(
            crate::widgets::input_prompt::CodeqlDbSource::Github,
            "psf/requests python",
        );
        wait_for_codeql_db_job(&mut app);
        assert_eq!(app.status, "Download failed: HTTP 404: Not Found");
        assert!(app.codeql.databases.is_empty());
        let cache = App::codeql_db_cache_dir();
        let left = std::fs::read_dir(&cache).map(|d| d.count()).unwrap_or(0);
        assert_eq!(left, 0, "no partial download left in {}", cache.display());
    });
}

/// #917: adding a database from GitHub without gh says how to get it,
/// both when looking up the repository's languages and when downloading.
#[test]
fn a_github_database_without_gh_says_how_to_get_it() {
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.gh_program = tmp.path().join("gh");
        for (value, context) in [
            (
                "psf/requests",
                "Could not look up psf/requests's CodeQL databases",
            ),
            ("psf/requests python", "Download failed"),
        ] {
            app.submit_codeql_database(crate::widgets::input_prompt::CodeqlDbSource::Github, value);
            wait_for_codeql_db_job(&mut app);
            assert_eq!(app.status, format!("{context}: {}", crate::gh::MISSING));
        }
        let cache = App::codeql_db_cache_dir();
        let left = std::fs::read_dir(&cache).map(|d| d.count()).unwrap_or(0);
        assert_eq!(left, 0, "no partial download left in {}", cache.display());
    });
}

#[test]
fn adding_a_codeql_database_from_an_archive_extracts_it_first() {
    // The cache-dir override is process-global; serialize with the
    // other tests that redirect it.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    use std::io::Write as _;
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let zp = tmp.path().join("kafka-db.zip");
        let mut z = zip::ZipWriter::new(std::fs::File::create(&zp).unwrap());
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        z.start_file("java/codeql-database.yml", opts).unwrap();
        z.write_all(b"primaryLanguage: \"java\"\n").unwrap();
        z.finish().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.submit_codeql_database(
            crate::widgets::input_prompt::CodeqlDbSource::Archive,
            &zp.display().to_string(),
        );
        wait_for_codeql_db_job(&mut app);
        assert_eq!(app.codeql.databases.len(), 1, "{}", app.status);
        let entry = &app.codeql.databases[0];
        assert_eq!(entry.language.as_deref(), Some("java"));
        assert!(
            entry.path.starts_with(home.path()),
            "extracted under croft's cache, not beside the archive: {}",
            entry.path.display()
        );
    });
}

#[test]
fn a_folder_without_a_codeql_database_is_refused_and_selection_persists() {
    // The cache-dir override is process-global; serialize with the
    // other tests that redirect it.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = codeql_app(tmp.path());
        app.submit_codeql_database(
            crate::widgets::input_prompt::CodeqlDbSource::Folder,
            &tmp.path().display().to_string(),
        );
        assert!(
            app.status.contains("not a CodeQL database"),
            "{}",
            app.status
        );
        assert!(app.codeql.databases.is_empty());
        let a = make_codeql_db(tmp.path(), "a-db", "python");
        let b = make_codeql_db(tmp.path(), "b-db", "go");
        for db in [&a, &b] {
            app.submit_codeql_database(
                crate::widgets::input_prompt::CodeqlDbSource::Folder,
                &db.display().to_string(),
            );
        }
        assert_eq!(app.codeql.current_db, Some(1));
        app.activate_codeql(crate::widgets::codeql::Hit::Action(
            crate::widgets::codeql::Action::SelectDatabase(0),
        ));
        assert_eq!(app.codeql.current_db, Some(0));
        let mut again = codeql_app(tmp.path());
        again.open_codeql_view();
        assert_eq!(again.codeql.current_db, Some(0), "the selection persists");
    });
}

#[test]
fn codeql_databases_are_renamed_sorted_revealed_and_removed_from_the_side_bar() {
    // The cache-dir override is process-global; serialize with the
    // other tests that redirect it.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    // #578: VS Code's Databases view renames, sorts, shows the folder of
    // and removes a database; removing deletes only what croft downloaded.
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let mine = make_codeql_db(tmp.path(), "flask-db", "python");
        let cached_root = App::codeql_db_cache_dir().join("kafka");
        let cached = make_codeql_db(&cached_root, "java", "java");
        let mut app = codeql_app(tmp.path());
        for db in [&mine, &cached] {
            app.submit_codeql_database(
                crate::widgets::input_prompt::CodeqlDbSource::Folder,
                &db.display().to_string(),
            );
        }
        app.open_codeql_view();
        app.focus = Pane::Tree;
        let press = |app: &mut App, code: KeyCode| {
            app.handle_key(key(code, KeyModifiers::NONE)).unwrap();
        };

        // On the database card, F2 renames the current one, starting from
        // its name.
        app.activate_codeql(crate::widgets::codeql::Hit::Action(
            crate::widgets::codeql::Action::SelectDatabase(0),
        ));
        app.codeql.select_database_card();
        press(&mut app, KeyCode::F(2));
        assert_eq!(app.input_prompt.as_ref().unwrap().value, "flask-db");
        for _ in 0.."flask-db".len() {
            press(&mut app, KeyCode::Backspace);
        }
        for c in "web".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.codeql.databases[0].name, "web", "{}", app.status);
        assert_eq!(
            crate::codeql_db::DatabaseStore::load(&App::codeql_db_store_path()).databases[0].name,
            "web",
            "saved"
        );

        // `s` sorts by name; the current database and the selection follow.
        press(&mut app, KeyCode::Char('s'));
        let names: Vec<_> = app
            .codeql
            .databases
            .iter()
            .map(|d| d.name.as_str())
            .collect();
        assert_eq!(names, ["java", "web"]);
        assert_eq!(app.codeql.current_db, Some(1), "web is still current");
        assert_eq!(app.codeql.selected_database(), Some(1), "still on web");

        // `e` shows its folder in the Explorer.
        press(&mut app, KeyCode::Char('e'));
        assert_eq!(app.sidebar_view, SidebarView::Explorer);
        assert_eq!(app.tree.nodes[app.tree.selected].path, mine);
        app.open_codeql_view();
        app.focus = Pane::Tree;

        // Delete asks first; Esc keeps it.
        app.codeql.select_database_card();
        press(&mut app, KeyCode::Delete);
        assert!(
            app.input_prompt
                .as_ref()
                .unwrap()
                .title
                .contains("folder stays"),
            "the user's own folder is only forgotten"
        );
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.codeql.databases.len(), 2, "Esc keeps it");
        press(&mut app, KeyCode::Delete);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.codeql.databases.len(), 1, "{}", app.status);
        assert!(mine.is_dir(), "not croft's to delete");
        assert_eq!(
            app.codeql.selected_hit(),
            Some(crate::widgets::codeql::Hit::Action(
                crate::widgets::codeql::Action::PickDatabase
            )),
            "stays on the card"
        );
        assert_eq!(app.codeql.current_db, None, "the current one went");

        // The downloaded copy goes with its files, once current.
        app.activate_codeql(crate::widgets::codeql::Hit::Action(
            crate::widgets::codeql::Action::SelectDatabase(0),
        ));
        press(&mut app, KeyCode::Delete);
        assert!(
            app.input_prompt
                .as_ref()
                .unwrap()
                .title
                .contains("files are deleted"),
            "{}",
            app.input_prompt.as_ref().unwrap().title
        );
        press(&mut app, KeyCode::Enter);
        assert!(app.codeql.databases.is_empty(), "{}", app.status);
        assert!(!cached_root.exists(), "the whole cached copy is gone");
        assert_eq!(app.codeql.current_db, None);
    });
}

#[test]
fn codeql_database_palette_commands_act_on_the_current_database() {
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    use crate::widgets::command_palette::Command;
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.run_command(Command::CodeqlRemoveDatabase);
        assert!(
            app.status.contains("Select a CodeQL database"),
            "{}",
            app.status
        );
        assert!(app.input_prompt.is_none());
        let db = make_codeql_db(tmp.path(), "flask-db", "python");
        app.submit_codeql_database(
            crate::widgets::input_prompt::CodeqlDbSource::Folder,
            &db.display().to_string(),
        );
        app.run_command(Command::CodeqlRenameDatabase);
        assert_eq!(
            app.input_prompt.as_ref().map(|p| &p.purpose),
            Some(
                &crate::widgets::input_prompt::InputPurpose::CodeqlRenameDatabase {
                    path: db.clone()
                }
            )
        );
        app.close_input_prompt();
        app.run_command(Command::CodeqlSortDatabases);
        assert_eq!(app.codeql.db_sort, Some(crate::codeql_db::DbSort::Name));
        assert_eq!(
            Command::from_id("codeql_reveal_database"),
            Some(Command::CodeqlRevealDatabase)
        );
        assert_eq!(
            Command::CodeqlRemoveDatabase.title(),
            "CodeQL: Remove Database"
        );
    });
}

/// Save a query history of `(query, started, status, output)` runs, with
/// each output file written, for the history management tests.
fn seed_codeql_history(
    tmp: &std::path::Path,
    runs: &[(&str, u64, crate::codeql_query::RunStatus, PathBuf)],
) {
    let mut history = crate::codeql_query::History::default();
    for (query, started, status, output) in runs {
        std::fs::write(tmp.join(query), "select 1").unwrap();
        std::fs::create_dir_all(output.parent().unwrap()).unwrap();
        std::fs::write(output, "col0\n1\n").unwrap();
        history.push(crate::codeql_query::HistoryEntry {
            query: tmp.join(query),
            database: String::from("app"),
            database_path: None,
            started: *started,
            seconds: 3,
            status: status.clone(),
            output: output.clone(),
            name: None,
            results: None,
        });
    }
    history.save(&App::codeql_history_path()).unwrap();
}

#[test]
fn codeql_query_history_is_renamed_sorted_opened_and_removed_from_the_side_bar() {
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    // #578: VS Code's Query History view renames, sorts, views the query
    // of, opens the results directory of and removes a run; removing
    // deletes only results in croft's own cache.
    use crate::codeql_query::RunStatus;
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let cached = App::codeql_results_dir().join("200-a");
        let elsewhere = tmp.path().join("elsewhere");
        seed_codeql_history(
            tmp.path(),
            &[
                (
                    "c.ql",
                    50,
                    RunStatus::Running,
                    App::codeql_results_dir().join("50-c/results.csv"),
                ),
                (
                    "b.ql",
                    100,
                    RunStatus::Succeeded,
                    elsewhere.join("results.csv"),
                ),
                (
                    "a.ql",
                    200,
                    RunStatus::Succeeded,
                    cached.join("results.csv"),
                ),
            ],
        );
        let mut app = codeql_app(tmp.path());
        app.open_codeql_view();
        app.focus = Pane::Tree;
        let press = |app: &mut App, code: KeyCode| {
            app.handle_key(key(code, KeyModifiers::NONE)).unwrap();
        };
        assert_eq!(app.codeql.history.len(), 3);
        assert_eq!(app.codeql.history[0].name, "a.ql", "newest first");

        // F2 renames, starting from the query's name.
        app.codeql.select_history(0);
        press(&mut app, KeyCode::F(2));
        assert_eq!(app.input_prompt.as_ref().unwrap().value, "a.ql");
        for _ in 0.."a.ql".len() {
            press(&mut app, KeyCode::Backspace);
        }
        for c in "zeta".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.codeql.history[0].name, "zeta", "{}", app.status);

        // `s` on a history row sorts the history, not the databases.
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.codeql.history_sort, crate::codeql_query::HistSort::Name);
        assert_eq!(app.codeql.db_sort, None, "databases untouched");
        assert_eq!(app.codeql.history[0].name, "b.ql");
        assert_eq!(app.codeql.selected_history(), Some(2), "still on zeta");
        assert_eq!(
            crate::codeql_query::History::load(&App::codeql_history_path()).sort_by,
            crate::codeql_query::HistSort::Name,
            "the order is saved"
        );

        // `v` opens the query; `o` the results in their cache folder.
        press(&mut app, KeyCode::Char('v'));
        assert_eq!(
            app.editor.path.as_deref(),
            Some(tmp.path().join("a.ql").as_path())
        );
        press(&mut app, KeyCode::Char('o'));
        assert_eq!(
            app.editor.path.as_deref(),
            Some(cached.join("results.csv").as_path())
        );
        assert!(app.status.contains("Results directory"), "{}", app.status);

        // A run in flight is not removed.
        app.codeql.select_history(1);
        press(&mut app, KeyCode::Delete);
        assert!(app.input_prompt.is_none());
        assert!(app.status.contains("still running"), "{}", app.status);

        // Delete asks first; Esc keeps it; results outside the cache stay.
        app.codeql.select_history(0);
        press(&mut app, KeyCode::Delete);
        assert!(
            app.input_prompt
                .as_ref()
                .unwrap()
                .title
                .contains("stay on disk"),
            "{}",
            app.input_prompt.as_ref().unwrap().title
        );
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.codeql.history.len(), 3, "Esc keeps it");
        press(&mut app, KeyCode::Delete);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.codeql.history.len(), 2, "{}", app.status);
        assert!(
            elsewhere.join("results.csv").is_file(),
            "not croft's to delete"
        );
        assert_eq!(app.codeql.selected_history(), Some(0), "stays in the list");

        // Croft's own results folder goes with its entry.
        app.codeql.select_history(1);
        press(&mut app, KeyCode::Delete);
        assert!(
            app.input_prompt
                .as_ref()
                .unwrap()
                .title
                .contains("results are deleted"),
            "{}",
            app.input_prompt.as_ref().unwrap().title
        );
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.codeql.history.len(), 1, "{}", app.status);
        assert!(!cached.exists(), "the run's folder is gone");
        assert!(
            App::codeql_results_dir().join("50-c").is_dir(),
            "only that run's folder"
        );
    });
}

#[cfg(unix)]
#[test]
fn a_codeql_runs_evaluator_log_opens_raw_or_summarised() {
    // #578: every run writes an evaluator log beside its results. "Show
    // Evaluator Log (Raw JSON)" opens it; "(Summary Text)" runs `codeql
    // generate log-summary` once and opens the text it wrote.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    use crate::codeql_query::RunStatus;
    use crate::widgets::command_palette::Command;
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        let out = App::codeql_results_dir().join("1-q").join("results.csv");
        seed_codeql_history(
            tmp.path(),
            &[("q.ql", 1, RunStatus::Succeeded, out.clone())],
        );
        app.run_command(Command::CodeqlShowEvaluatorLog);
        assert_eq!(app.status, "q.ql has no evaluator log");

        let log = crate::codeql_query::evaluator_log(&out);
        std::fs::create_dir_all(log.parent().unwrap()).unwrap();
        std::fs::write(&log, "{\"type\":\"LOG_HEADER\"}\n").unwrap();
        app.run_command(Command::CodeqlShowEvaluatorLog);
        assert_eq!(app.editor.path.as_deref(), Some(log.as_path()));

        // The fake CLI writes its reply to the path after the log.
        app.codeql_program = fake_codeql(bin.path(), "", 0, "");
        let script = std::fs::read_to_string(&app.codeql_program).unwrap();
        std::fs::write(
            &app.codeql_program,
            script.replace(
                "exit 0",
                "[ \"$1\" = generate ] && echo 'Summary' > \"$5\"\nexit 0",
            ),
        )
        .unwrap();
        app.run_command(Command::CodeqlShowEvaluatorLogSummary);
        assert_eq!(app.status, "Summarising the evaluator log\u{2026}");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !app.drain_codeql_log_summary() {
            assert!(
                std::time::Instant::now() < deadline,
                "the summary never finished"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let summary = crate::codeql_query::evaluator_log_summary(&out);
        assert_eq!(app.status, "Opened the evaluator log summary");
        assert_eq!(app.editor.path.as_deref(), Some(summary.as_path()));
        let calls = std::fs::read_to_string(bin.path().join("codeql-calls.log")).unwrap();
        assert_eq!(
            calls.trim(),
            format!(
                "generate log-summary --format=text {} {}",
                log.display(),
                summary.display()
            )
        );
        // Made once: the second request opens the file without the CLI.
        app.run_command(Command::CodeqlShowEvaluatorLogSummary);
        assert!(app.codeql_log_summary.is_none());
        assert_eq!(app.editor.path.as_deref(), Some(summary.as_path()));
        assert_eq!(
            Command::from_id("codeql_show_evaluator_log_summary"),
            Some(Command::CodeqlShowEvaluatorLogSummary)
        );
    });
}

#[cfg(unix)]
#[test]
fn a_codeql_runs_query_log_opens_the_newest_log_in_its_logs_folder() {
    // #578: VS Code's "Show Query Log". Every run passes `--logdir` so
    // `codeql` writes its log beside the results; the newest one opens.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    use crate::codeql_query::RunStatus;
    use crate::widgets::command_palette::Command;
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        let out = App::codeql_results_dir().join("1-q").join("results.csv");
        seed_codeql_history(
            tmp.path(),
            &[("q.ql", 1, RunStatus::Succeeded, out.clone())],
        );
        // A run from before croft kept query logs.
        app.run_command(Command::CodeqlShowQueryLog);
        assert_eq!(app.status, "q.ql has no query log");

        let logs = crate::codeql_query::query_log_dir(&out);
        std::fs::create_dir_all(&logs).unwrap();
        let at = |secs| std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs);
        for (name, secs) in [
            ("execute-new.log", 20),
            ("execute-old.log", 10),
            ("x.txt", 30),
        ] {
            let f = std::fs::File::create(logs.join(name)).unwrap();
            f.set_modified(at(secs)).unwrap();
        }
        app.run_command(Command::CodeqlShowQueryLog);
        assert_eq!(
            app.editor.path.as_deref(),
            Some(logs.join("execute-new.log").as_path())
        );
        assert_eq!(
            Command::from_id("codeql_show_query_log"),
            Some(Command::CodeqlShowQueryLog)
        );

        // A real run asks `codeql` for a log in its own folder.
        let bin = tempfile::tempdir().unwrap();
        let mut app = codeql_query_fixture(tmp.path(), "import rust\nselect 1");
        app.codeql_program = fake_codeql(bin.path(), "", 0, "");
        app.run_command(Command::CodeqlRunQuery);
        wait_for_codeql(&mut app);
        let history = crate::codeql_query::History::load(&App::codeql_history_path());
        let run = &history.entries[history.newest().unwrap()];
        let calls = std::fs::read_to_string(bin.path().join("codeql-calls.log")).unwrap();
        let logdir = format!(
            "--logdir={}",
            crate::codeql_query::query_log_dir(&run.output).display()
        );
        assert!(calls.starts_with("query run "), "{calls}");
        assert!(calls.contains(&logdir), "{calls}");
    });
}

#[cfg(unix)]
#[test]
fn a_codeql_runs_evaluator_log_viewer_shows_a_folded_predicate_tree() {
    // #578: "Show Evaluator Log (Viewer)" runs `codeql generate log-summary
    // --format=predicates` once, keeps the file beside the log, and shows
    // its predicates slowest first as a tree folded to one line each.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    use crate::codeql_query::RunStatus;
    use crate::widgets::command_palette::Command;
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        let out = App::codeql_results_dir().join("1-q").join("results.csv");
        seed_codeql_history(
            tmp.path(),
            &[("q.ql", 1, RunStatus::Succeeded, out.clone())],
        );
        app.run_command(Command::CodeqlShowEvaluatorLogViewer);
        assert_eq!(app.status, "q.ql has no evaluator log");
        assert!(app.codeql_log_viewer.is_none());

        let log = crate::codeql_query::evaluator_log(&out);
        std::fs::create_dir_all(log.parent().unwrap()).unwrap();
        std::fs::write(&log, "{\"type\":\"LOG_HEADER\"}\n").unwrap();
        let sample = concat!(
            r#"{"summaryLogVersion":"0.4.0"}"#,
            "\n",
            r#"{"predicateName":"Foo::fast#a","evaluationStrategy":"COMPUTE_SIMPLE","millis":7,"resultSize":2,"dependencies":{"files":"h"},"ra":{"pipeline":["{1} r1 = SCAN files","return r1"]},"pipelineRuns":[{"raReference":"pipeline"}]}"#,
            "\n",
            r#"{"predicateName":"Foo::slow#b","evaluationStrategy":"COMPUTE_RECURSIVE","millis":1500,"resultSize":1234,"predicateIterationMillis":[500,1000],"ra":{"base":["return r1"],"standard":["return r2"]}}"#,
            "\n",
        );
        let reply = tmp.path().join("reply.jsonl");
        std::fs::write(&reply, sample).unwrap();
        // The fake CLI copies its reply to the path after the log.
        app.codeql_program = fake_codeql(bin.path(), "", 0, "");
        let script = std::fs::read_to_string(&app.codeql_program).unwrap();
        std::fs::write(
            &app.codeql_program,
            script.replace(
                "exit 0",
                &format!(
                    "[ \"$1\" = generate ] && cp '{}' \"$5\"\nexit 0",
                    reply.display()
                ),
            ),
        )
        .unwrap();
        let wait = |app: &mut App| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while !app.drain_codeql_log_viewer() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the viewer never finished"
                );
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        };
        app.run_command(Command::CodeqlShowEvaluatorLogViewer);
        assert_eq!(app.status, "Reading the evaluator log\u{2026}");
        wait(&mut app);
        assert_eq!(app.status, "Opened the evaluator log viewer");
        assert_eq!(
            app.editor.path.as_deref(),
            Some(std::path::Path::new("Evaluator Log (q.ql)"))
        );
        assert_eq!(
            app.editor.lines[2],
            "\u{25b8} 1,500 ms  1,234 rows  Foo::slow#b (COMPUTE_RECURSIVE), 2 iterations"
        );
        assert!(
            app.editor
                .lines
                .iter()
                .any(|l| l.starts_with("\u{25b8} 7 ms  2 rows  Foo::fast#a")),
            "{:?}",
            app.editor.lines
        );
        assert!(app.editor.lines.iter().any(|l| l == "        files"));
        assert!(app.editor.is_line_hidden(3), "predicates start folded");
        // The side bar's Evaluator Log Viewer section holds the same tree.
        {
            use crate::widgets::codeql::{Action, Hit, Line};
            let rows = |app: &App| -> Vec<(Action, String)> {
                app.codeql
                    .lines()
                    .into_iter()
                    .filter_map(|l| match l {
                        Line::Action(
                            a @ (Action::EvalPredicate(_)
                            | Action::EvalDependency(_)
                            | Action::ClearEvalLog),
                            t,
                        ) => Some((a, t)),
                        _ => None,
                    })
                    .collect()
            };
            let folded = rows(&app);
            assert_eq!(folded.len(), 3, "{folded:?}");
            assert_eq!(folded[0].1, "Clear \u{b7} q.ql");
            assert!(folded[1].1.contains("Foo::slow#b"), "slowest first");
            assert!(folded[2].1.contains("Foo::fast#a"));
            app.activate_codeql(Hit::Action(Action::EvalPredicate(1)));
            let open = rows(&app);
            assert!(open[2].1.starts_with("\u{25be}"), "{open:?}");
            assert!(
                open.iter().any(|(_, t)| t == "  Pipeline pipeline"),
                "{open:?}"
            );
            let dep = open
                .iter()
                .find(|(a, _)| matches!(a, Action::EvalDependency(_)))
                .expect("the dependency row");
            assert_eq!(dep.1, "  \u{2192} files");
            app.activate_codeql(Hit::Action(dep.0));
            assert!(app.status.contains("not in this log"), "{}", app.status);
            app.activate_codeql(Hit::Action(Action::ClearEvalLog));
            assert!(app.codeql.evallog.is_none());
        }
        let predicates = crate::codeql_query::evaluator_log_predicates(&out);
        let calls = std::fs::read_to_string(bin.path().join("codeql-calls.log")).unwrap();
        assert_eq!(
            calls.trim(),
            format!(
                "generate log-summary --format=predicates {} {}",
                log.display(),
                predicates.display()
            )
        );

        // Made once: the second request reads the kept file without the CLI.
        app.run_command(Command::CodeqlShowEvaluatorLogViewer);
        wait(&mut app);
        assert_eq!(app.status, "Opened the evaluator log viewer");
        let calls = std::fs::read_to_string(bin.path().join("codeql-calls.log")).unwrap();
        assert_eq!(calls.trim().lines().count(), 1, "{calls}");
        assert_eq!(
            Command::from_id("codeql_show_evaluator_log_viewer"),
            Some(Command::CodeqlShowEvaluatorLogViewer)
        );

        // A CLI failure says why.
        std::fs::remove_file(&predicates).unwrap();
        app.codeql_program = fake_codeql(bin.path(), "", 2, "no such log");
        app.run_command(Command::CodeqlShowEvaluatorLogViewer);
        wait(&mut app);
        assert_eq!(app.status, "Could not read the evaluator log: no such log");
    });
}

#[cfg(unix)]
#[test]
fn comparing_codeql_performance_sets_two_runs_predicate_timings_side_by_side() {
    // #578: VS Code's "Compare Performance". The newest run's evaluator
    // log is set against the previous successful run of the same query,
    // whatever its kind of results; a predicate summary either run lacks
    // is made once by the CLI, as the log viewer makes it.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    use crate::codeql_query::RunStatus;
    use crate::widgets::command_palette::Command;
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        let old_out = App::codeql_results_dir().join("1-q").join("results.sarif");
        let new_out = App::codeql_results_dir().join("2-q").join("results.csv");
        seed_codeql_history(
            tmp.path(),
            &[("q.ql", 2, RunStatus::Succeeded, new_out.clone())],
        );
        app.run_command(Command::CodeqlComparePerformance);
        assert_eq!(app.status, "q.ql has no evaluator log");
        let new_log = crate::codeql_query::evaluator_log(&new_out);
        std::fs::write(&new_log, "{}\n").unwrap();
        app.run_command(Command::CodeqlComparePerformance);
        assert_eq!(
            app.status,
            "There is no earlier successful run of q.ql to compare with"
        );

        seed_codeql_history(
            tmp.path(),
            &[
                ("q.ql", 1, RunStatus::Succeeded, old_out.clone()),
                ("q.ql", 2, RunStatus::Succeeded, new_out.clone()),
            ],
        );
        app.run_command(Command::CodeqlComparePerformance);
        assert_eq!(app.status, "q.ql has no evaluator log", "the earlier run's");
        assert!(app.codeql_perf_compare.is_none());

        // The earlier run's summary is already kept; the later one's is not.
        std::fs::write(crate::codeql_query::evaluator_log(&old_out), "{}\n").unwrap();
        std::fs::write(
            crate::codeql_query::evaluator_log_predicates(&old_out),
            concat!(
                r#"{"predicateName":"Foo::bar#abc","evaluationStrategy":"COMPUTE_SIMPLE","millis":3400,"resultSize":12}"#,
                "\n",
                r#"{"predicateName":"Gone::p#1","evaluationStrategy":"COMPUTE_SIMPLE","millis":500,"resultSize":4}"#,
                "\n",
            ),
        )
        .unwrap();
        let reply = tmp.path().join("reply.jsonl");
        std::fs::write(
            &reply,
            concat!(
                r#"{"predicateName":"Foo::bar#def","evaluationStrategy":"COMPUTE_SIMPLE","millis":4600,"resultSize":15}"#,
                "\n",
                r#"{"predicateName":"Added::r#2","evaluationStrategy":"COMPUTE_SIMPLE","millis":90,"resultSize":7}"#,
                "\n",
            ),
        )
        .unwrap();
        app.codeql_program = fake_codeql(bin.path(), "", 0, "");
        let script = std::fs::read_to_string(&app.codeql_program).unwrap();
        std::fs::write(
            &app.codeql_program,
            script.replace(
                "exit 0",
                &format!(
                    "[ \"$1\" = generate ] && cp '{}' \"$5\"\nexit 0",
                    reply.display()
                ),
            ),
        )
        .unwrap();
        let wait = |app: &mut App| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while !app.drain_codeql_perf_compare() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the comparison never finished"
                );
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        };
        app.run_command(Command::CodeqlComparePerformance);
        assert_eq!(app.status, "Comparing the evaluator logs\u{2026}");
        wait(&mut app);
        assert_eq!(app.status, "Opened the performance comparison");
        assert_eq!(
            app.editor.path.as_deref(),
            Some(std::path::Path::new("Performance (q.ql)"))
        );
        assert_eq!(
            app.editor.lines[..6],
            [
                "Performance: earlier (app) \u{2192} later (app)",
                "3,900 \u{2192} 4,690 ms in all (+790 ms), 3 predicates: 1 in both, 1 only before, 1 only after",
                "",
                "+1,200 ms  3,400 \u{2192} 4,600 ms  12 \u{2192} 15 rows  Foo::bar#def",
                "  -500 ms  500 \u{2192} \u{2013} ms  4 \u{2192} \u{2013} rows  Gone::p#1",
                "   +90 ms  \u{2013} \u{2192} 90 ms  \u{2013} \u{2192} 7 rows  Added::r#2",
            ]
        );
        let calls = std::fs::read_to_string(bin.path().join("codeql-calls.log")).unwrap();
        assert_eq!(
            calls.trim(),
            format!(
                "generate log-summary --format=predicates {} {}",
                new_log.display(),
                crate::codeql_query::evaluator_log_predicates(&new_out).display()
            )
        );
        assert_eq!(
            Command::from_id("codeql_compare_performance"),
            Some(Command::CodeqlComparePerformance)
        );
    });
}

#[test]
fn codeql_results_are_exported_to_a_new_file_only() {
    // #578: VS Code's "Export Results" copies the newest successful run's
    // results (a failed run later on is passed over) to a file the user
    // names, suggesting one in the workspace, and never overwrites.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    use crate::codeql_query::RunStatus;
    use crate::widgets::command_palette::Command;
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.run_command(Command::CodeqlExportResults);
        assert_eq!(
            app.status,
            "There is no successful CodeQL query run to export"
        );
        assert!(app.input_prompt.is_none());

        seed_codeql_history(
            tmp.path(),
            &[
                (
                    "q.ql",
                    2,
                    RunStatus::Succeeded,
                    App::codeql_results_dir().join("2-q/results.csv"),
                ),
                (
                    "r.ql",
                    5,
                    RunStatus::Failed(String::from("x")),
                    App::codeql_results_dir().join("5-r/results.sarif"),
                ),
            ],
        );
        app.run_command(Command::CodeqlExportResults);
        let dest = app.workspace_root().join("q-results.csv");
        assert_eq!(
            app.input_prompt.as_ref().expect("the prompt opens").value,
            dest.display().to_string()
        );
        app.submit_input_prompt();
        assert_eq!(
            app.status,
            format!("Exported results to {}", dest.display())
        );
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "col0\n1\n");

        std::fs::write(&dest, "mine").unwrap();
        app.run_command(Command::CodeqlExportResults);
        app.submit_input_prompt();
        assert_eq!(
            app.status,
            format!(
                "{} already exists; export the results to a new file",
                dest.display()
            )
        );
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "mine");
        // A dangling symlink is refused too, not followed and written.
        #[cfg(unix)]
        {
            let link = app.workspace_root().join("link.csv");
            let target = app.workspace_root().join("nowhere.csv");
            std::os::unix::fs::symlink(&target, &link).unwrap();
            app.run_command(Command::CodeqlExportResults);
            app.input_prompt.as_mut().unwrap().value = link.display().to_string();
            app.submit_input_prompt();
            assert!(app.status.contains("already exists"), "{}", app.status);
            assert!(!target.exists(), "the link's target was not created");
        }
        // Results gone while the prompt was open: nothing is created at
        // the destination.
        let fresh = app.workspace_root().join("fresh.csv");
        app.run_command(Command::CodeqlExportResults);
        app.input_prompt.as_mut().unwrap().value = fresh.display().to_string();
        std::fs::remove_file(App::codeql_results_dir().join("2-q/results.csv")).unwrap();
        app.submit_input_prompt();
        assert!(
            app.status.starts_with("Could not read the results at "),
            "{}",
            app.status
        );
        assert!(!fresh.exists(), "no file was created for a failed export");
        assert_eq!(
            Command::from_id("codeql_export_results"),
            Some(Command::CodeqlExportResults)
        );
    });
}

#[cfg(unix)]
#[test]
fn codeql_query_help_is_previewed_as_markdown_or_reported_missing() {
    // #578: VS Code's "CodeQL: Preview Query Help" renders the open
    // query's help with `codeql generate query-help` off the UI thread and
    // shows it in the Markdown preview; with no .ql open it takes the
    // query of the newest history entry.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    use crate::widgets::command_palette::Command;
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let mut app = codeql_query_fixture(tmp.path(), "select 1");
        let wait = |app: &mut App| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while !app.drain_codeql_query_help() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the query help never landed"
                );
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        };
        app.codeql_program = fake_codeql(bin.path(), "# Q\n\nFinds things.\n", 0, "");
        app.run_command(Command::CodeqlPreviewQueryHelp);
        assert_eq!(app.status, "Rendering the help of q.ql\u{2026}");
        wait(&mut app);
        assert_eq!(app.status, "Query help for q.ql");
        let help = croft_cache_dir().join("codeql/help/q.md");
        assert_eq!(app.editor.path.as_deref(), Some(help.as_path()));
        assert!(app.editor.markdown_preview.is_some());
        let calls = std::fs::read_to_string(bin.path().join("codeql-calls.log")).unwrap();
        assert_eq!(
            calls.trim(),
            format!(
                "generate query-help --format=markdown --output={} {}",
                help.display(),
                tmp.path().join("q.ql").display()
            )
        );

        // No help beside the query: the CLI writes nothing.
        seed_codeql_history(
            tmp.path(),
            &[(
                "h.ql",
                5,
                crate::codeql_query::RunStatus::Succeeded,
                App::codeql_results_dir().join("5-h/results.csv"),
            )],
        );
        app.codeql_program = fake_codeql(bin.path(), "", 0, "");
        app.run_command(Command::CodeqlPreviewQueryHelp);
        assert_eq!(app.status, "Rendering the help of h.ql\u{2026}");
        wait(&mut app);
        assert_eq!(
            app.status,
            "h.ql has no query help (no .qhelp or .md beside it)"
        );

        app.editor.open(&tmp.path().join("q.ql")).unwrap();
        app.codeql_program = fake_codeql(bin.path(), "", 2, "ERROR: no help");
        app.run_command(Command::CodeqlPreviewQueryHelp);
        wait(&mut app);
        assert_eq!(app.status, "q.ql has no query help: ERROR: no help");
        assert_eq!(
            Command::from_id("codeql_preview_query_help"),
            Some(Command::CodeqlPreviewQueryHelp)
        );
    });
}

#[cfg(unix)]
#[test]
fn codeql_pack_commands_install_dependencies_and_download_packs() {
    // #578: "Install Pack Dependencies" runs `codeql pack install` on the
    // selected pack's folder; "Download Packs" asks which packs and runs
    // `codeql pack download` on them. Both off the UI thread.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    use crate::widgets::command_palette::Command;
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("pack")).unwrap();
        std::fs::write(tmp.path().join("pack/qlpack.yml"), "name: acme/rust\n").unwrap();
        std::fs::write(tmp.path().join("pack/a.ql"), "select 1").unwrap();
        let mut app = codeql_query_fixture(tmp.path(), "select 1");
        app.codeql_program = fake_codeql(bin.path(), "", 0, "");
        app.open_codeql_view();
        wait_for_codeql_queries(&mut app);
        app.run_command(Command::CodeqlInstallPackDependencies);
        assert_eq!(
            app.status,
            "Select a query pack or one of its queries in the CodeQL side bar first"
        );
        let row = app
            .codeql
            .lines()
            .iter()
            .position(|l| {
                matches!(
                    l,
                    crate::widgets::codeql::Line::Action(
                        crate::widgets::codeql::Action::RunQuery(0, 0),
                        _
                    )
                )
            })
            .expect("a.ql is listed");
        app.codeql.selected = row;
        app.run_command(Command::CodeqlInstallPackDependencies);
        assert_eq!(
            app.status,
            "Installing the dependencies of acme/rust\u{2026}"
        );
        app.run_command(Command::CodeqlInstallPackDependencies);
        assert_eq!(
            app.status,
            "A CodeQL pack install or download is already running"
        );
        let wait = |app: &mut App| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while !app.drain_codeql_pack_job() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the pack job never finished"
                );
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        };
        wait(&mut app);
        assert_eq!(app.status, "Installed the dependencies of acme/rust");

        app.run_command(Command::CodeqlDownloadPacks);
        for c in "codeql/java-queries codeql/python-all@1.0.0".chars() {
            app.input_prompt.as_mut().unwrap().push_char(c);
        }
        app.submit_input_prompt();
        wait(&mut app);
        assert_eq!(
            app.status,
            "Downloaded codeql/java-queries, codeql/python-all@1.0.0"
        );
        let calls = std::fs::read_to_string(bin.path().join("codeql-calls.log")).unwrap();
        assert_eq!(
            calls.lines().collect::<Vec<_>>(),
            vec![
                format!("pack install {}", tmp.path().join("pack").display()),
                String::from("pack download codeql/java-queries codeql/python-all@1.0.0"),
            ]
        );

        app.codeql_program = fake_codeql(bin.path(), "", 1, "ERROR: no such pack");
        app.run_command(Command::CodeqlDownloadPacks);
        for c in "acme/missing".chars() {
            app.input_prompt.as_mut().unwrap().push_char(c);
        }
        app.submit_input_prompt();
        wait(&mut app);
        assert_eq!(
            app.status,
            "Could not download acme/missing: ERROR: no such pack"
        );
    });
}

#[cfg(unix)]
#[test]
fn a_codeql_quick_query_opens_for_the_databases_language() {
    // #578: VS Code's "CodeQL: Quick Query". The first time, croft writes
    // a scratch pack for the selected database's language and installs its
    // library; later it reopens the same file, edits and all.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    use crate::widgets::command_palette::Command;
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let mut app = codeql_query_fixture(tmp.path(), "select 1");
        app.codeql_program = fake_codeql(bin.path(), "", 0, "");
        app.run_command(Command::CodeqlQuickQuery);
        let query = app.editor.path.clone().expect("the quick query is open");
        assert!(
            query.ends_with("quick-query/rust/quick-query.ql"),
            "{}",
            query.display()
        );
        assert!(app.editor.lines.iter().any(|l| l == "import rust"));
        let dir = query.parent().unwrap().to_path_buf();
        assert!(
            std::fs::read_to_string(dir.join("qlpack.yml"))
                .unwrap()
                .contains("codeql/rust-all")
        );
        assert_eq!(
            app.status,
            "Installing codeql/rust-all for the quick query\u{2026}"
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !app.drain_codeql_pack_job() {
            assert!(
                std::time::Instant::now() < deadline,
                "the install never finished"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(
            app.status,
            "Quick query ready: codeql/rust-all is installed"
        );
        let calls = std::fs::read_to_string(bin.path().join("codeql-calls.log")).unwrap();
        assert_eq!(calls.trim(), format!("pack install {}", dir.display()));

        // A real install leaves a lock file, which says it is done.
        std::fs::write(dir.join("codeql-pack.lock.yml"), "lockVersion: 1.0.0\n").unwrap();
        std::fs::write(&query, "import rust\nselect 2\n").unwrap();
        app.run_command(Command::CodeqlQuickQuery);
        assert_eq!(app.status, "Opened the rust quick query");
        assert!(app.editor.lines.iter().any(|l| l == "select 2"));
        assert!(app.codeql_pack_job.is_none(), "no second install");
    });
}

#[test]
fn comparing_codeql_results_opens_the_rows_one_run_has_alone() {
    // #578: VS Code's "Compare Results". The newest run's table is set
    // against the previous run of the same query; the rows only one of
    // them has open as a CSV beside the newer results.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    use crate::codeql_query::RunStatus;
    use crate::widgets::command_palette::Command;
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        let out = |n: &str| App::codeql_results_dir().join(n).join("results.csv");
        seed_codeql_history(tmp.path(), &[("q.ql", 1, RunStatus::Succeeded, out("1-q"))]);
        app.run_command(Command::CodeqlCompareResults);
        assert_eq!(
            app.status,
            "There is no earlier successful run of q.ql to compare with"
        );
        seed_codeql_history(
            tmp.path(),
            &[
                ("q.ql", 1, RunStatus::Succeeded, out("1-q")),
                ("q.ql", 2, RunStatus::Succeeded, out("2-q")),
            ],
        );
        for (dir, body) in [("1-q", "f\na\nb\n"), ("2-q", "f\nb\nc\n")] {
            std::fs::create_dir_all(out(dir).parent().unwrap()).unwrap();
            std::fs::write(out(dir), body).unwrap();
        }
        app.run_command(Command::CodeqlCompareResults);
        assert_eq!(app.status, "2 rows differ between the runs");
        let compared = out("2-q").with_file_name("compare-1.csv");
        assert_eq!(app.editor.path.as_deref(), Some(compared.as_path()));
        let text = std::fs::read_to_string(&compared).unwrap();
        assert!(text.starts_with("run,f\n"), "{text}");
        assert!(text.contains(",a\n") && text.contains(",c\n"), "{text}");
    });
}

#[test]
fn codeql_query_history_palette_commands_act_on_the_selected_or_newest_run() {
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    use crate::codeql_query::RunStatus;
    use crate::widgets::command_palette::Command;
    use crate::widgets::input_prompt::InputPurpose;
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.run_command(Command::CodeqlRemoveHistory);
        assert!(
            app.status.contains("no CodeQL query history"),
            "{}",
            app.status
        );
        assert!(app.input_prompt.is_none());
        let out = |n: &str| App::codeql_results_dir().join(n).join("results.csv");
        seed_codeql_history(
            tmp.path(),
            &[
                ("a-old.ql", 1, RunStatus::Succeeded, out("1-old")),
                ("b-new.ql", 2, RunStatus::Succeeded, out("2-new")),
            ],
        );
        // Sorted by name, the newest run is no longer on top; it is still
        // the one the commands pick.
        app.run_command(Command::CodeqlSortHistory);
        assert_eq!(app.codeql.history_sort, crate::codeql_query::HistSort::Name);
        app.run_command(Command::CodeqlRenameHistory);
        assert_eq!(
            app.input_prompt.as_ref().map(|p| &p.purpose),
            Some(&InputPurpose::CodeqlRenameHistory {
                output: out("2-new")
            })
        );
        app.close_input_prompt();
        app.run_command(Command::CodeqlViewQuery);
        assert_eq!(
            app.editor.path.as_deref(),
            Some(tmp.path().join("b-new.ql").as_path())
        );
        // A selected row wins over the newest.
        app.open_codeql_view();
        app.codeql.select_history(0);
        app.run_command(Command::CodeqlViewQuery);
        assert_eq!(
            app.editor.path.as_deref(),
            Some(tmp.path().join("a-old.ql").as_path())
        );
        assert_eq!(
            Command::from_id("codeql_remove_history"),
            Some(Command::CodeqlRemoveHistory)
        );
        assert_eq!(Command::CodeqlViewQuery.title(), "CodeQL: View Query");
    });
}

/// Drain until the walk for the workspace's queries lands (#840): opening
/// the CodeQL view starts it off the UI thread.
fn wait_for_codeql_queries(app: &mut App) {
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(500),
        "the CodeQL query discovery to land",
        || {
            app.drain_codeql_queries();
            app.codeql_queries_job.is_none()
        },
    );
}

#[cfg(unix)]
#[test]
fn a_published_codeql_pack_is_downloaded_and_run_on_the_database() {
    // #578: "Run Queries in Published Pack": download the pack, analyze
    // the database with it (or the part after `:`), open its SARIF.
    use crate::widgets::command_palette::Command;
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let mut app = codeql_query_fixture(tmp.path(), "select 1");
        app.codeql_program = fake_codeql(bin.path(), r#"{"runs":[{"results":[{}]}]}"#, 0, "");

        app.run_command(Command::CodeqlRunPublishedPack);
        let prompt = app.input_prompt.as_mut().expect("which pack");
        prompt.value = String::from("python-queries");
        app.submit_input_prompt();
        assert_eq!(
            app.status,
            "\"python-queries\" is not a pack: write scope/name"
        );
        assert!(app.codeql_run.is_none());

        app.run_command(Command::CodeqlRunPublishedPack);
        app.input_prompt.as_mut().unwrap().value =
            String::from("codeql/python-queries@1.8.11:Security/CWE-078");
        app.submit_input_prompt();
        assert_eq!(
            app.status,
            "Running codeql/python-queries@1.8.11:Security/CWE-078 on app\u{2026}"
        );
        wait_for_codeql(&mut app);
        assert!(
            app.status
                .starts_with("codeql/python-queries@1.8.11:Security/CWE-078 finished in "),
            "{}",
            app.status
        );
        let calls = std::fs::read_to_string(bin.path().join("codeql-calls.log")).unwrap();
        let calls: Vec<&str> = calls.lines().collect();
        assert_eq!(calls[0], "pack download codeql/python-queries@1.8.11");
        assert!(
            calls[1].starts_with(&format!(
                "database analyze {} codeql/python-queries@1.8.11:Security/CWE-078 --format=sarif-latest",
                tmp.path().join("dbs/app").display()
            )),
            "{}",
            calls[1]
        );
        let history = crate::codeql_query::History::load(&App::codeql_history_path());
        let entry = &history.entries[history.newest().unwrap()];
        assert_eq!(entry.status, crate::codeql_query::RunStatus::Succeeded);
        assert_eq!(entry.results, Some(1));
        assert_eq!(
            app.editor.path.as_ref(),
            Some(&entry.output),
            "its SARIF is open"
        );

        // A failed download stops before the analysis.
        let bin = tempfile::tempdir().unwrap();
        app.codeql_program = fake_codeql(bin.path(), "", 2, "A fatal error occurred: no such pack");
        app.run_command(Command::CodeqlRunPublishedPack);
        app.input_prompt.as_mut().unwrap().value = String::from("acme/missing");
        app.submit_input_prompt();
        wait_for_codeql(&mut app);
        assert!(
            app.status.starts_with("acme/missing failed: "),
            "{}",
            app.status
        );
        let calls = std::fs::read_to_string(bin.path().join("codeql-calls.log")).unwrap();
        assert_eq!(calls, "pack download acme/missing\n");
    });
}

/// Run a published pack's command-injection query through croft against the
/// real CLI and registry (#578), on a Flask app that runs a request argument.
#[cfg(unix)]
#[test]
#[ignore = "needs the CodeQL CLI and the pack registry; set CROFT_TEST_CODEQL"]
fn real_published_pack_runs_on_the_selected_database() {
    let codeql = std::path::PathBuf::from(std::env::var("CROFT_TEST_CODEQL").unwrap());
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("app.py"),
            "import os\nfrom flask import Flask, request\napp = Flask(__name__)\n\n@app.route(\"/\")\ndef index():\n    os.system(request.args[\"cmd\"])\n    return \"\"\n",
        )
        .unwrap();
        let db = tmp.path().join("db");
        let made = std::process::Command::new(&codeql)
            .args(["database", "create", "--language=python", "--source-root"])
            .arg(&src)
            .arg(&db)
            .output()
            .unwrap();
        assert!(
            made.status.success(),
            "{}",
            String::from_utf8_lossy(&made.stderr)
        );
        crate::codeql_db::DatabaseStore {
            databases: vec![crate::codeql_db::DbEntry {
                name: String::from("flask"),
                path: db,
                language: Some(String::from("python")),
                added: 0,
                former_names: Vec::new(),
            }],
            current: Some(0),
            sort_by: None,
        }
        .save(&App::codeql_db_store_path())
        .unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.codeql_program = codeql;
        app.run_codeql_published_pack("codeql/python-queries:Security/CWE-078/CommandInjection.ql");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(900);
        while app.codeql_run.is_some() {
            app.drain_codeql_run();
            assert!(std::time::Instant::now() < deadline, "never finished");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let history = crate::codeql_query::History::load(&App::codeql_history_path());
        let entry = &history.entries[history.newest().unwrap()];
        assert_eq!(
            entry.status,
            crate::codeql_query::RunStatus::Succeeded,
            "{}",
            app.status
        );
        let sarif = std::fs::read_to_string(&entry.output).unwrap();
        assert!(
            sarif.contains("\"ruleId\":\"py/command-line-injection\""),
            "{sarif}"
        );
        assert_eq!(entry.results, Some(1));
    });
}

/// Wait for the AST Viewer's read to land, or fail after `secs`.
fn wait_for_codeql_ast(app: &mut App, secs: u64) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    while !app.drain_codeql_ast() {
        assert!(
            std::time::Instant::now() < deadline,
            "the AST read never finished"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[cfg(unix)]
#[test]
fn the_ast_viewer_shows_the_open_files_tree_and_goes_to_its_code() {
    // #578: the AST of a source file from the selected database, with
    // go-to-code and clear, through a stand-in for the CLI.
    use crate::widgets::codeql::{Action, Line};
    use std::os::unix::fs::PermissionsExt;
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let mut app = codeql_query_fixture(tmp.path(), "select 1");
        let src = tmp.path().join("app.py");
        std::fs::write(&src, "import os\n\ndef run(cmd):\n    os.system(cmd)\n").unwrap();
        let pack = bin.path().join("rust-all");
        std::fs::create_dir_all(&pack).unwrap();
        std::fs::write(pack.join("qlpack.yml"), "name: codeql/rust-all\n").unwrap();
        let query = pack.join("printAst.ql");
        std::fs::write(&query, "select 1").unwrap();
        let file = src.display().to_string();
        let graph = format!(
            r#"{{"nodes": {{"tuples": [
              [{{"id": 0, "label": "[Import] import os", "url": {{"uri": "file://{file}", "startLine": 1, "startColumn": 1}}}}, "semmle.order", "1"],
              [{{"id": 1, "label": "[FunctionDef] run", "url": {{"uri": "file://{file}", "startLine": 3, "startColumn": 1}}}}, "semmle.order", "2"],
              [{{"id": 2, "label": "[Call] os.system(cmd)", "url": {{"uri": "file://{file}", "startLine": 4, "startColumn": 5}}}}, "semmle.label", "[Call] os.system(cmd)"]]}},
             "edges": {{"tuples": [
              [{{"id": 1}}, {{"id": 2}}, "semmle.label", "body"]]}}}}"#
        );
        std::fs::write(bin.path().join("graph.json"), graph).unwrap();
        let script = format!(
            "#!/bin/sh\necho \"$*\" >> '{log}'\ncase \"$1 $2\" in 'resolve queries') echo '[\"{query}\"]'; exit 0 ;; esac\nfor a in \"$@\"; do case \"$a\" in --output=*) cp '{graph}' \"${{a#--output=}}\" ;; --external=selectedSourceFile=*) cp \"${{a#--external=selectedSourceFile=}}\" '{sel}' ;; esac; done\nexit 0\n",
            log = bin.path().join("calls.log").display(),
            query = query.display(),
            graph = bin.path().join("graph.json").display(),
            sel = bin.path().join("selected.csv").display(),
        );
        let program = bin.path().join("codeql");
        std::fs::write(&program, script).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        app.codeql_program = program;

        app.editor.open(&src).unwrap();
        app.run_command(crate::widgets::command_palette::Command::CodeqlViewAst);
        assert_eq!(app.status, "Reading the AST of app.py from app\u{2026}");
        wait_for_codeql_ast(&mut app, 10);
        assert_eq!(app.status, "AST of app.py: 3 nodes");
        let calls = std::fs::read_to_string(bin.path().join("calls.log")).unwrap();
        let calls: Vec<&str> = calls.lines().collect();
        assert!(
            calls[0].starts_with("resolve queries --format=json "),
            "{calls:?}"
        );
        assert_eq!(calls[1], format!("pack install {}", pack.display()));
        assert!(calls[2].starts_with("query run --database="), "{calls:?}");
        assert!(
            calls[3].starts_with("bqrs decode --format=json"),
            "{calls:?}"
        );
        assert_eq!(
            std::fs::read_to_string(bin.path().join("selected.csv")).unwrap(),
            format!("\"{file}\"\n"),
            "the file is named as the source archive names it"
        );

        app.open_codeql_view();
        let rows: Vec<String> = app
            .codeql
            .lines()
            .iter()
            .filter_map(|l| match l {
                Line::Action(Action::AstNode(_) | Action::ClearAst, text) => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            rows,
            [
                "Clear \u{b7} app.py in app",
                "  [Import] import os  1:1",
                "\u{25b8} [FunctionDef] run  3:1"
            ]
        );
        // Enter on the function unfolds it and goes to its code.
        let row = app
            .codeql
            .lines()
            .iter()
            .position(|l| matches!(l, Line::Action(Action::AstNode(1), _)))
            .unwrap();
        app.codeql.selected = row;
        app.focus = Pane::Tree;
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.editor.path.as_deref(), Some(src.as_path()));
        assert_eq!(app.editor.cursor_row, 2);
        assert!(app.codeql.lines().iter().any(|l| matches!(
            l,
            Line::Action(Action::AstNode(2), t) if t == "    body: [Call] os.system(cmd)  4:5"
        )));
        // Clear forgets it.
        app.activate_codeql(crate::widgets::codeql::Hit::Action(Action::ClearAst));
        assert!(app.codeql.ast.is_none());
    });
}

#[cfg(unix)]
#[test]
fn view_cfg_opens_the_control_flow_at_the_cursor_as_a_document() {
    // #578: View CFG runs the language pack's print-cfg query for the
    // cursor's position and opens the graph as a read-only document.
    use std::os::unix::fs::PermissionsExt;
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let mut app = codeql_query_fixture(tmp.path(), "select 1");
        let src = tmp.path().join("app.py");
        std::fs::write(&src, "import os\n\ndef run(cmd):\n    os.system(cmd)\n").unwrap();
        let query = bin.path().join("printCfg.ql");
        std::fs::write(&query, "select 1").unwrap();
        let reply = serde_json::json!({"mermaid": {"tuples": [[
            "flowchart TD\n1[\"Entry\"]\n2[\"Exit\"]\n3[\"os.system(cmd)\"]\n1 --> 3\n3 --> 2"
        ]]}});
        std::fs::write(bin.path().join("cfg.json"), reply.to_string()).unwrap();
        let script = format!(
            "#!/bin/sh\necho \"$*\" >> '{log}'\ncase \"$1 $2\" in 'resolve queries') echo '[\"{query}\"]'; exit 0 ;; esac\nfor a in \"$@\"; do case \"$a\" in --output=*) cp '{reply}' \"${{a#--output=}}\" ;; --external=selectedSourceLine=*) cp \"${{a#--external=selectedSourceLine=}}\" '{line}' ;; esac; done\nexit 0\n",
            log = bin.path().join("calls.log").display(),
            query = query.display(),
            reply = bin.path().join("cfg.json").display(),
            line = bin.path().join("line.csv").display(),
        );
        let program = bin.path().join("codeql");
        std::fs::write(&program, script).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        app.codeql_program = program;

        app.editor.open(&src).unwrap();
        app.editor.cursor_row = 2;
        app.run_command(crate::widgets::command_palette::Command::CodeqlViewCfg);
        assert_eq!(
            app.status,
            "Reading the control flow graph at app.py:3\u{2026}"
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !app.drain_codeql_doc() {
            assert!(std::time::Instant::now() < deadline, "never finished");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(app.status, "Control flow graph");
        let doc = app.editor.path.clone().unwrap();
        assert!(doc.starts_with(croft_cache_dir()), "{doc:?}");
        let text = app.editor.lines.join("\n");
        assert!(
            text.starts_with("# Control flow graph of app.py:3:1 in app"),
            "{text}"
        );
        assert!(text.contains("- Entry \u{2192} os.system(cmd)"), "{text}");
        assert!(text.contains("```mermaid"), "{text}");
        assert_eq!(
            std::fs::read_to_string(bin.path().join("line.csv")).unwrap(),
            "3\n"
        );
        let calls = std::fs::read_to_string(bin.path().join("calls.log")).unwrap();
        assert!(calls.contains("print-cfg.qls"), "{calls}");
    });
}

/// #578's View CFG against the real CLI: the control flow of a Python
/// function, read with the cursor on its definition.
#[cfg(unix)]
#[test]
#[ignore = "needs the CodeQL CLI and codeql/python-all; set CROFT_TEST_CODEQL"]
fn view_cfg_reads_a_real_functions_control_flow() {
    let codeql = std::path::PathBuf::from(std::env::var("CROFT_TEST_CODEQL").unwrap());
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        let file = src.join("app.py");
        std::fs::write(&file, "import os\n\ndef run(cmd):\n    os.system(cmd)\n").unwrap();
        let db = tmp.path().join("db");
        let made = std::process::Command::new(&codeql)
            .args(["database", "create", "--language=python", "--source-root"])
            .arg(&src)
            .arg(&db)
            .output()
            .unwrap();
        assert!(
            made.status.success(),
            "{}",
            String::from_utf8_lossy(&made.stderr)
        );
        crate::codeql_db::DatabaseStore {
            databases: vec![crate::codeql_db::DbEntry {
                name: String::from("py"),
                path: db,
                language: Some(String::from("python")),
                added: 0,
                former_names: Vec::new(),
            }],
            current: Some(0),
            sort_by: None,
        }
        .save(&App::codeql_db_store_path())
        .unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.codeql_program = codeql;
        app.editor.open(&file).unwrap();
        app.editor.cursor_row = 2;
        app.run_command(crate::widgets::command_palette::Command::CodeqlViewCfg);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
        while !app.drain_codeql_doc() {
            assert!(std::time::Instant::now() < deadline, "never finished");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert_eq!(app.status, "Control flow graph");
        let text = app.editor.lines.join("\n");
        assert!(text.contains("- Entry \u{2192} "), "{text}");
        assert!(text.contains("Exit"), "{text}");
    });
}

#[test]
fn walking_a_codeql_results_table_opens_each_results_code() {
    // #578: `codeQLQueryResults` up/down/left/right. The walk moves the
    // table's cursor and opens the cell's entity; it goes on after the
    // code has the focus, and a sorted table still leads to the right row.
    use crate::codeql_query::{CellLoc, RowLocs};
    use crate::widgets::command_palette::Command;
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("app.py");
        std::fs::write(&src, "a = 1\nb = 2\ndef f():\n    pass\n").unwrap();
        let csv = tmp.path().join("results.csv");
        std::fs::write(
            &csv,
            "\"x\",\"n\"\n\"Name b\",\"2\"\n\"Function f\",\"3\"\n\"plain\",\"9\"\n",
        )
        .unwrap();
        let at = |line: u32, column: u32| {
            Some(CellLoc {
                path: src.clone(),
                line,
                column,
            })
        };
        let rows = vec![
            RowLocs {
                cells: vec!["Name b".into(), "2".into()],
                locs: vec![at(2, 1), None],
            },
            RowLocs {
                cells: vec!["Function f".into(), "3".into()],
                locs: vec![at(3, 5), None],
            },
            RowLocs {
                cells: vec!["plain".into(), "9".into()],
                locs: vec![None, None],
            },
        ];
        std::fs::write(
            crate::codeql_query::locations_path(&csv),
            serde_json::to_string(&rows).unwrap(),
        )
        .unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.run_command(Command::CodeqlResultsDown);
        assert_eq!(app.status, "Open a CodeQL results table first");
        app.editor.open(&csv).unwrap();

        // Down from the first row: row 2, "Function f" at 3:5.
        app.run_command(Command::CodeqlResultsDown);
        assert_eq!(
            app.editor.path.as_deref(),
            Some(src.as_path()),
            "{}",
            app.status
        );
        assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (2, 4));
        // The code has the focus; the walk goes on in the table: back up.
        app.run_command(Command::CodeqlResultsUp);
        assert_eq!(app.editor.cursor_row, 1, "{}", app.status);
        // Right onto the number column: no location there, so the row's
        // entity.
        app.run_command(Command::CodeqlResultsRight);
        assert_eq!(app.editor.cursor_row, 1, "{}", app.status);
        // A row with no entity says so.
        app.run_command(Command::CodeqlResultsDown);
        app.run_command(Command::CodeqlResultsDown);
        assert_eq!(app.status, "Result 3 has no location");

        // Sorted by the name column (descending: plain, Name b, Function
        // f), the first row still leads to its own code.
        app.editor.open(&csv).unwrap();
        let data = &mut app.editor.sheet.as_mut().unwrap().sheets[0];
        data.cur_col = 0;
        data.sort_by_column(0);
        data.sort_by_column(0);
        data.cur_row = 0;
        app.run_command(Command::CodeqlResultsDown);
        assert_eq!(
            app.editor.path.as_deref(),
            Some(src.as_path()),
            "{}",
            app.status
        );
        assert_eq!(app.editor.cursor_row, 1, "Name b is on line 2");
    });
}

#[test]
fn a_runs_result_sets_are_picked_from_a_list() {
    // #578: raw results with a result-set picker. Each set is its own CSV.
    use crate::codeql_query::RunStatus;
    use crate::widgets::command_palette::Command;
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.run_command(Command::CodeqlShowResultSet);
        assert_eq!(app.status, "No CodeQL query has run yet");
        let out = App::codeql_results_dir().join("5-q/results.csv");
        seed_codeql_history(
            tmp.path(),
            &[("q.ql", 5, RunStatus::Succeeded, out.clone())],
        );
        app.run_command(Command::CodeqlShowResultSet);
        assert_eq!(app.status, "q.ql has one result set");

        let calls = out.with_file_name("results-calls.csv");
        std::fs::write(&calls, "c,s\nx,y\n").unwrap();
        app.run_command(Command::CodeqlShowResultSet);
        let picker = app.list_picker.as_ref().expect("the picker opens");
        assert_eq!(picker.title, "Result sets of q.ql");
        let labels: Vec<&str> = picker.rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, ["#select", "calls"]);
        app.list_picker.as_mut().unwrap().selected = 1;
        app.confirm_list_picker();
        assert_eq!(app.editor.path.as_deref(), Some(calls.as_path()));
        assert_eq!(app.status, "Result set calls");
    });
}

/// #578's result sets against the real CLI: a query with a `query
/// predicate` beside its `select` gets one CSV per set, not both tables
/// run together in one file.
#[cfg(unix)]
#[test]
#[ignore = "needs the CodeQL CLI and codeql/python-all; set CROFT_TEST_CODEQL"]
fn a_real_query_with_two_result_sets_decodes_each_to_its_own_csv() {
    let codeql = std::path::PathBuf::from(std::env::var("CROFT_TEST_CODEQL").unwrap());
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("app.py"),
            "def run(cmd):\n    print(cmd)\n\nrun(input())\n",
        )
        .unwrap();
        let db = tmp.path().join("db");
        let made = std::process::Command::new(&codeql)
            .args(["database", "create", "--language=python", "--source-root"])
            .arg(&src)
            .arg(&db)
            .output()
            .unwrap();
        assert!(
            made.status.success(),
            "{}",
            String::from_utf8_lossy(&made.stderr)
        );
        crate::codeql_db::DatabaseStore {
            databases: vec![crate::codeql_db::DbEntry {
                name: String::from("py"),
                path: db,
                language: Some(String::from("python")),
                added: 0,
                former_names: Vec::new(),
            }],
            current: Some(0),
            sort_by: None,
        }
        .save(&App::codeql_db_store_path())
        .unwrap();
        let pack = tmp.path().join("q");
        std::fs::create_dir_all(&pack).unwrap();
        std::fs::write(
            pack.join("qlpack.yml"),
            "name: me/q\nversion: 0.0.1\ndependencies:\n  codeql/python-all: \"*\"\n",
        )
        .unwrap();
        let installed = std::process::Command::new(&codeql)
            .args(["pack", "install"])
            .arg(&pack)
            .output()
            .unwrap();
        assert!(
            installed.status.success(),
            "{}",
            String::from_utf8_lossy(&installed.stderr)
        );
        let query = pack.join("Two.ql");
        std::fs::write(
            &query,
            "import python\nquery predicate calls(Call c, string s) { s = c.toString() }\nfrom Function f\nselect f, f.getName()\n",
        )
        .unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.codeql_program = codeql;
        app.editor.open(&query).unwrap();
        app.run_command(crate::widgets::command_palette::Command::CodeqlRunQuery);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
        while app.codeql_run.is_some() {
            app.drain_codeql_run();
            assert!(std::time::Instant::now() < deadline, "never finished");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(app.status.contains("2 result sets"), "{}", app.status);
        let output = app.editor.path.clone().unwrap();
        let main = std::fs::read_to_string(&output).unwrap();
        assert_eq!(main.lines().next(), Some("\"f\",\"col1\""), "{main}");
        assert!(
            !main.contains("\"c\",\"s\""),
            "only #select in the main CSV: {main}"
        );
        let calls = std::fs::read_to_string(output.with_file_name("results-calls.csv")).unwrap();
        assert_eq!(calls.lines().next(), Some("\"c\",\"s\""), "{calls}");
        assert_eq!(calls.lines().count(), 4, "{calls}");
    });
}

#[cfg(unix)]
#[test]
fn an_alert_runs_alerts_and_raw_results_open_as_csv_or_sarif_text() {
    // #578: Query History's "View Alerts (CSV)", "View Alerts (SARIF)" and
    // "View Results (CSV)".
    use crate::codeql_query::RunStatus;
    use crate::widgets::command_palette::Command;
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let mut app = codeql_query_fixture(tmp.path(), "select 1");
        app.codeql_program = fake_codeql(
            bin.path(),
            "\"Calls\",,\"warning\",\"A call.\",\"/app.py\",\"4\",\"5\",\"4\",\"18\"\n",
            0,
            "",
        );
        let wait = |app: &mut App| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while !app.drain_codeql_doc() {
                assert!(std::time::Instant::now() < deadline, "never finished");
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        };
        let table = App::codeql_results_dir().join("1-t/results.csv");
        let alerts = App::codeql_results_dir().join("2-a/results.sarif");
        seed_codeql_history(
            tmp.path(),
            &[
                ("t.ql", 1, RunStatus::Succeeded, table.clone()),
                ("Alert.ql", 2, RunStatus::Succeeded, alerts.clone()),
            ],
        );
        std::fs::write(&alerts, r#"{"version":"2.1.0","runs":[]}"#).unwrap();

        // The newest run is the alert one.
        app.run_command(Command::CodeqlViewAlertsSarif);
        assert_eq!(app.status, "Alerts of Alert.ql (SARIF)");
        assert_eq!(app.editor.path.as_deref(), Some(alerts.as_path()));
        assert!(app.editor.sarif.is_none(), "the JSON, not the viewer");
        assert_eq!(app.editor.lines[0], r#"{"version":"2.1.0","runs":[]}"#);

        app.run_command(Command::CodeqlViewAlertsCsv);
        wait(&mut app);
        assert_eq!(app.status, "Alerts of Alert.ql (CSV)");
        let csv = alerts.with_file_name("alerts.csv");
        assert_eq!(app.editor.path.as_deref(), Some(csv.as_path()));
        let calls = std::fs::read_to_string(bin.path().join("codeql-calls.log")).unwrap();
        assert_eq!(
            calls.lines().last().unwrap(),
            format!(
                "database interpret-results --format=csv --output={} {} {}",
                csv.display(),
                tmp.path().join("dbs/app").display(),
                tmp.path().join("Alert.ql").display()
            )
        );

        // Raw results come from the BQRS the database keeps.
        app.run_command(Command::CodeqlViewResultsCsv);
        assert_eq!(
            app.status,
            "The database no longer keeps the results of Alert.ql"
        );
        let kept = tmp.path().join("dbs/app/results/me/q/Alert.bqrs");
        std::fs::create_dir_all(kept.parent().unwrap()).unwrap();
        std::fs::write(&kept, "").unwrap();
        app.run_command(Command::CodeqlViewResultsCsv);
        wait(&mut app);
        assert_eq!(app.status, "Results of Alert.ql (CSV)");
        assert_eq!(
            app.editor.path.as_deref(),
            Some(alerts.with_file_name("raw-results.csv").as_path())
        );

        // A table run has no alerts; its results are its table.
        app.open_codeql_view();
        app.codeql.select_history(1);
        app.run_command(Command::CodeqlViewAlertsCsv);
        assert_eq!(
            app.status,
            "t.ql did not produce alerts; its results are a table"
        );
        app.run_command(Command::CodeqlViewResultsCsv);
        assert_eq!(app.editor.path.as_deref(), Some(table.as_path()));
    });
}

/// #578's alert views against the real CLI: after an alert query runs,
/// its alerts are written as CSV and its raw results decoded, both from
/// what the database keeps.
#[cfg(unix)]
#[test]
#[ignore = "needs the CodeQL CLI and codeql/python-all; set CROFT_TEST_CODEQL"]
fn a_real_alert_runs_alerts_and_raw_results_are_viewed_as_csv() {
    use crate::widgets::command_palette::Command;
    let codeql = std::path::PathBuf::from(std::env::var("CROFT_TEST_CODEQL").unwrap());
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("app.py"),
            "def run(cmd):\n    print(cmd)\n\nrun(input())\n",
        )
        .unwrap();
        let db = tmp.path().join("db");
        let made = std::process::Command::new(&codeql)
            .args(["database", "create", "--language=python", "--source-root"])
            .arg(&src)
            .arg(&db)
            .output()
            .unwrap();
        assert!(
            made.status.success(),
            "{}",
            String::from_utf8_lossy(&made.stderr)
        );
        crate::codeql_db::DatabaseStore {
            databases: vec![crate::codeql_db::DbEntry {
                name: String::from("py"),
                path: db,
                language: Some(String::from("python")),
                added: 0,
                former_names: Vec::new(),
            }],
            current: Some(0),
            sort_by: None,
        }
        .save(&App::codeql_db_store_path())
        .unwrap();
        let pack = tmp.path().join("q");
        std::fs::create_dir_all(&pack).unwrap();
        std::fs::write(
            pack.join("qlpack.yml"),
            "name: me/q\nversion: 0.0.1\ndependencies:\n  codeql/python-all: \"*\"\n",
        )
        .unwrap();
        let installed = std::process::Command::new(&codeql)
            .args(["pack", "install"])
            .arg(&pack)
            .output()
            .unwrap();
        assert!(
            installed.status.success(),
            "{}",
            String::from_utf8_lossy(&installed.stderr)
        );
        let query = pack.join("Alert.ql");
        std::fs::write(
            &query,
            "/**\n * @name Calls\n * @kind problem\n * @id me/calls\n * @problem.severity warning\n */\nimport python\nfrom Call c\nselect c, \"A call.\"\n",
        )
        .unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.codeql_program = codeql;
        app.editor.open(&query).unwrap();
        app.run_command(Command::CodeqlRunQuery);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(900);
        while app.codeql_run.is_some() {
            app.drain_codeql_run();
            assert!(std::time::Instant::now() < deadline, "never finished");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(app.status.contains("finished"), "{}", app.status);
        let wait = |app: &mut App| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
            while !app.drain_codeql_doc() {
                assert!(std::time::Instant::now() < deadline, "never finished");
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        };
        app.run_command(Command::CodeqlViewAlertsCsv);
        wait(&mut app);
        assert_eq!(app.status, "Alerts of Alert.ql (CSV)");
        // A CSV opens in the table view; its text is the file's.
        let text = std::fs::read_to_string(app.editor.path.as_ref().unwrap()).unwrap();
        assert_eq!(text.matches("\"A call.\"").count(), 3, "{text}");
        app.run_command(Command::CodeqlViewResultsCsv);
        wait(&mut app);
        assert_eq!(app.status, "Results of Alert.ql (CSV)");
        let text = std::fs::read_to_string(app.editor.path.as_ref().unwrap()).unwrap();
        assert!(text.contains("A call."), "{text}");
        assert_eq!(text.lines().count(), 4, "a header and three calls: {text}");
    });
}

#[cfg(unix)]
#[test]
fn a_qlref_opens_the_query_it_references() {
    // #578: "open a referenced file" from a .qlref test file.
    use crate::widgets::command_palette::Command;
    use std::os::unix::fs::PermissionsExt;
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let query = tmp.path().join("src/Alert.ql");
        std::fs::create_dir_all(query.parent().unwrap()).unwrap();
        std::fs::write(&query, "select 1").unwrap();
        let qlref = tmp.path().join("test/A.qlref");
        std::fs::create_dir_all(qlref.parent().unwrap()).unwrap();
        std::fs::write(&qlref, "Alert.ql\n").unwrap();
        let program = bin.path().join("codeql");
        std::fs::write(
            &program,
            format!(
                "#!/bin/sh\necho \"$*\" >> '{log}'\necho '{{ \"resolvedPath\" : \"{q}\", \"resolvedPostprocessingPaths\" : [ ] }}'\n",
                log = bin.path().join("calls.log").display(),
                q = query.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.codeql_program = program;
        app.run_command(Command::CodeqlOpenReferencedFile);
        assert_eq!(app.status, "Open a .qlref file first");
        app.editor.open(&qlref).unwrap();
        app.run_command(Command::CodeqlOpenReferencedFile);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !app.drain_codeql_doc() {
            assert!(std::time::Instant::now() < deadline, "never finished");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(app.status, "The query A.qlref references");
        assert_eq!(app.editor.path.as_deref(), Some(query.as_path()));
        let calls = std::fs::read_to_string(bin.path().join("calls.log")).unwrap();
        assert_eq!(calls.trim(), format!("resolve qlref {}", qlref.display()));
    });
}

/// #578's "open a referenced file" against the real CLI: a test pack's
/// `.qlref` names a query in the query pack it depends on.
#[cfg(unix)]
#[test]
#[ignore = "needs the CodeQL CLI and codeql/python-all; set CROFT_TEST_CODEQL"]
fn a_real_qlref_opens_the_query_in_the_pack_it_depends_on() {
    let codeql = std::path::PathBuf::from(std::env::var("CROFT_TEST_CODEQL").unwrap());
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        std::fs::write(
            ws.join("codeql-workspace.yml"),
            "provide:\n  - \"*/qlpack.yml\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(ws.join("src")).unwrap();
        std::fs::create_dir_all(ws.join("test")).unwrap();
        std::fs::write(
            ws.join("src/qlpack.yml"),
            "name: me/src\nversion: 0.0.1\ndependencies:\n  codeql/python-all: \"*\"\n",
        )
        .unwrap();
        let query = ws.join("src/Alert.ql");
        std::fs::write(&query, "import python\nselect 1\n").unwrap();
        std::fs::write(
            ws.join("test/qlpack.yml"),
            "name: me/test\nversion: 0.0.1\ndependencies:\n  me/src: \"*\"\nextractor: python\ntests: .\n",
        )
        .unwrap();
        let qlref = ws.join("test/A.qlref");
        std::fs::write(&qlref, "Alert.ql\n").unwrap();
        for dir in ["src", "test"] {
            let out = std::process::Command::new(&codeql)
                .args(["pack", "install"])
                .current_dir(ws.join(dir))
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        let mut app = App::new(ws.to_path_buf()).unwrap();
        app.codeql_program = codeql;
        app.editor.open(&qlref).unwrap();
        app.run_command(crate::widgets::command_palette::Command::CodeqlOpenReferencedFile);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
        while !app.drain_codeql_doc() {
            assert!(std::time::Instant::now() < deadline, "never finished");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert_eq!(app.status, "The query A.qlref references");
        assert_eq!(
            app.editor.path.as_ref().map(|p| p.canonicalize().unwrap()),
            Some(query.canonicalize().unwrap())
        );
    });
}

#[cfg(unix)]
#[test]
fn quick_evaluation_runs_the_predicate_at_the_cursor_through_the_query_server() {
    // #578: Quick Evaluation (and its count) of the predicate under the
    // cursor, recorded in the query history like a run.
    use crate::widgets::command_palette::Command;
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let mut app = codeql_query_fixture(
            tmp.path(),
            "import python\n\npredicate isCall(Call c) { exists(c) }\n\nfrom Call c\nwhere isCall(c)\nselect c\n",
        );
        app.codeql_program = fake_query_server(
            bin.path(),
            r#"{"resultType": 0, "message": "", "evaluationTime": 384}"#,
            "\"c\"\n\"run()\"\n",
        );
        let query = tmp.path().join("q.ql");
        app.editor.cursor_row = 2;
        app.editor.cursor_col = 13;
        app.run_command(Command::CodeqlQuickEval);
        assert_eq!(
            app.status,
            "Running Quick evaluation of isCall on app\u{2026}"
        );
        wait_for_codeql(&mut app);
        assert!(app.status.contains("finished"), "{}", app.status);
        assert_eq!(
            app.codeql.history[0].name, "Quick evaluation of isCall",
            "{:?}",
            app.codeql.history[0]
        );
        let requests: Vec<serde_json::Value> =
            std::fs::read_to_string(bin.path().join("requests.jsonl"))
                .unwrap()
                .lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect();
        assert_eq!(requests[0]["method"], "evaluation/registerDatabases");
        assert_eq!(requests[1]["method"], "evaluation/runQuery");
        let body = &requests[1]["params"]["body"];
        assert_eq!(body["queryPath"], query.display().to_string());
        assert_eq!(
            body["target"]["quickEval"]["quickEvalPos"],
            serde_json::json!({"fileName": query.display().to_string(), "line": 3, "column": 11, "endLine": 3, "endColumn": 16})
        );
        assert!(body["target"]["quickEval"].get("countOnly").is_none());

        // The count variant says so, in the request and the history.
        app.editor.open(&query).unwrap();
        app.editor.cursor_row = 2;
        app.editor.cursor_col = 13;
        app.run_command(Command::CodeqlQuickEvalCount);
        wait_for_codeql(&mut app);
        assert!(
            app.codeql.history[0]
                .name
                .contains("Quick evaluation count of isCall")
        );
        let last: serde_json::Value = serde_json::from_str(
            std::fs::read_to_string(bin.path().join("requests.jsonl"))
                .unwrap()
                .lines()
                .last()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            last["params"]["body"]["target"]["quickEval"]["countOnly"],
            true
        );

        // The server's refusal is the run's reason.
        std::fs::write(
            bin.path().join("result.json"),
            r#"{"resultType": 2, "message": "ERROR: The selection is not a valid quick-eval target. (q.ql:1,1-6)\n", "evaluationTime": -1}"#,
        )
        .unwrap();
        app.editor.open(&query).unwrap();
        app.editor.cursor_row = 0;
        app.editor.cursor_col = 2;
        app.run_command(Command::CodeqlQuickEval);
        wait_for_codeql(&mut app);
        assert!(
            app.status.ends_with(
                "failed: ERROR: The selection is not a valid quick-eval target. (q.ql:1,1-6)"
            ),
            "{}",
            app.status
        );

        // Unsaved edits, and nothing under the cursor, are refused first.
        app.editor.open(&query).unwrap();
        app.editor.cursor_row = 1;
        app.editor.cursor_col = 0;
        app.run_command(Command::CodeqlQuickEval);
        assert_eq!(
            app.status,
            "Put the cursor on a predicate, or select an expression"
        );
        app.editor.dirty = true;
        app.run_command(Command::CodeqlQuickEval);
        assert!(
            app.status.starts_with("Save the query first"),
            "{}",
            app.status
        );
    });
}

/// #578's quick evaluation against the real CLI's query server: a
/// predicate's tuples, and their count.
#[cfg(unix)]
#[test]
#[ignore = "needs the CodeQL CLI and codeql/python-all; set CROFT_TEST_CODEQL"]
fn quick_evaluation_against_a_real_query_server() {
    use crate::widgets::command_palette::Command;
    let codeql = std::path::PathBuf::from(std::env::var("CROFT_TEST_CODEQL").unwrap());
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("app.py"),
            "def run(cmd):\n    print(cmd)\n\nrun(input())\n",
        )
        .unwrap();
        let db = tmp.path().join("db");
        let made = std::process::Command::new(&codeql)
            .args(["database", "create", "--language=python", "--source-root"])
            .arg(&src)
            .arg(&db)
            .output()
            .unwrap();
        assert!(
            made.status.success(),
            "{}",
            String::from_utf8_lossy(&made.stderr)
        );
        crate::codeql_db::DatabaseStore {
            databases: vec![crate::codeql_db::DbEntry {
                name: String::from("py"),
                path: db,
                language: Some(String::from("python")),
                added: 0,
                former_names: Vec::new(),
            }],
            current: Some(0),
            sort_by: None,
        }
        .save(&App::codeql_db_store_path())
        .unwrap();
        let pack = tmp.path().join("q");
        std::fs::create_dir_all(&pack).unwrap();
        std::fs::write(
            pack.join("qlpack.yml"),
            "name: me/q\nversion: 0.0.1\ndependencies:\n  codeql/python-all: \"*\"\n",
        )
        .unwrap();
        let installed = std::process::Command::new(&codeql)
            .args(["pack", "install"])
            .arg(&pack)
            .output()
            .unwrap();
        assert!(
            installed.status.success(),
            "{}",
            String::from_utf8_lossy(&installed.stderr)
        );
        let query = pack.join("Quick.ql");
        std::fs::write(
            &query,
            "import python\n\npredicate isCall(Call c) { exists(c) }\n\nfrom Call c\nwhere isCall(c)\nselect c\n",
        )
        .unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.codeql_program = codeql;
        let run = |app: &mut App, command: Command| {
            app.editor.open(&query).unwrap();
            app.editor.cursor_row = 2;
            app.editor.cursor_col = 13;
            app.run_command(command);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(900);
            while app.codeql_run.is_some() {
                app.drain_codeql_run();
                assert!(std::time::Instant::now() < deadline, "never finished");
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            assert!(app.status.contains("finished"), "{}", app.status);
            std::fs::read_to_string(app.editor.path.as_ref().unwrap()).unwrap()
        };
        let tuples = run(&mut app, Command::CodeqlQuickEval);
        assert_eq!(
            tuples.lines().count(),
            4,
            "a header and three calls: {tuples}"
        );
        let counts = run(&mut app, Command::CodeqlQuickEvalCount);
        assert!(counts.contains("\"Total tuples\",3"), "{counts}");
    });
}

/// #578's results walk against the real CLI: a table run keeps where its
/// entities are, and walking the table opens their code.
#[cfg(unix)]
#[test]
#[ignore = "needs the CodeQL CLI and codeql/python-all; set CROFT_TEST_CODEQL"]
fn a_real_results_table_is_walked_into_the_code() {
    let codeql = std::path::PathBuf::from(std::env::var("CROFT_TEST_CODEQL").unwrap());
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        let file = src.join("app.py");
        std::fs::write(
            &file,
            "import os\n\ndef run(cmd):\n    print(cmd)\n\ndef go():\n    run(input())\n",
        )
        .unwrap();
        let db = tmp.path().join("db");
        let made = std::process::Command::new(&codeql)
            .args(["database", "create", "--language=python", "--source-root"])
            .arg(&src)
            .arg(&db)
            .output()
            .unwrap();
        assert!(
            made.status.success(),
            "{}",
            String::from_utf8_lossy(&made.stderr)
        );
        crate::codeql_db::DatabaseStore {
            databases: vec![crate::codeql_db::DbEntry {
                name: String::from("py"),
                path: db,
                language: Some(String::from("python")),
                added: 0,
                former_names: Vec::new(),
            }],
            current: Some(0),
            sort_by: None,
        }
        .save(&App::codeql_db_store_path())
        .unwrap();
        let pack = tmp.path().join("q");
        std::fs::create_dir_all(&pack).unwrap();
        std::fs::write(
            pack.join("qlpack.yml"),
            "name: me/q\nversion: 0.0.1\ndependencies:\n  codeql/python-all: \"*\"\n",
        )
        .unwrap();
        let installed = std::process::Command::new(&codeql)
            .args(["pack", "install"])
            .arg(&pack)
            .output()
            .unwrap();
        assert!(
            installed.status.success(),
            "{}",
            String::from_utf8_lossy(&installed.stderr)
        );
        let query = pack.join("Functions.ql");
        std::fs::write(
            &query,
            "import python\nfrom Function f\nselect f, f.getName()\n",
        )
        .unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.codeql_program = codeql;
        app.editor.open(&query).unwrap();
        app.run_command(crate::widgets::command_palette::Command::CodeqlRunQuery);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(900);
        while app.codeql_run.is_some() {
            app.drain_codeql_run();
            assert!(std::time::Instant::now() < deadline, "never finished");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let table = app.editor.path.clone().unwrap();
        assert!(
            crate::codeql_query::locations_path(&table).is_file(),
            "{}",
            app.status
        );
        let data = &mut app.editor.sheet.as_mut().expect("the table").sheets[0];
        data.cur_col = 0;
        data.sort_by_column(1); // by name: go, run
        data.cur_row = 0;
        // Right, then Left: back on the first row's entity, `def go`.
        app.run_command(crate::widgets::command_palette::Command::CodeqlResultsRight);
        app.run_command(crate::widgets::command_palette::Command::CodeqlResultsLeft);
        assert_eq!(
            app.editor.path.as_ref().map(|p| p.canonicalize().unwrap()),
            Some(file.canonicalize().unwrap()),
            "{}",
            app.status
        );
        assert_eq!(app.editor.cursor_row, 5, "def go is on line 6");
        app.run_command(crate::widgets::command_palette::Command::CodeqlResultsDown);
        assert_eq!(app.editor.cursor_row, 2, "def run is on line 3");
    });
}

#[cfg(unix)]
#[test]
fn the_model_editor_lists_a_databases_endpoints_by_group() {
    // #578: Method Modeling. The language pack's model-editor endpoints
    // query runs on the current database, and its endpoints are listed by
    // module or class with whether CodeQL models them; Enter goes to one.
    use crate::widgets::codeql::{Action, Hit, Line};
    use std::os::unix::fs::PermissionsExt;
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let mut app = codeql_query_fixture(tmp.path(), "select 1");
        let src = tmp.path().join("core.py");
        std::fs::write(
            &src,
            "import os\n\ndef run(cmd, shell=True):\n    return os.system(cmd)\n",
        )
        .unwrap();
        let query = bin.path().join("FrameworkModeEndpoints.ql");
        std::fs::write(&query, "select 1").unwrap();
        let rows = format!(
            r#"{{"columns":[],"tuples":[
              [{{"label":"Function run","url":{{"uri":"file://{f}","startLine":3,"startColumn":1}}}},"mylib","core","run","(cmd,shell)",false,"core.py","",{{"label":"Function"}}],
              [{{"label":"Function go","url":{{"uri":"file://{f}","startLine":3,"startColumn":1}}}},"mylib","core.Runner","go","(self,what)",true,"core.py","",{{"label":"InstanceMethod"}}]]}}"#,
            f = src.display()
        );
        std::fs::write(bin.path().join("rows.json"), rows).unwrap();
        let script = format!(
            "#!/bin/sh\necho \"$*\" >> '{log}'\ncase \"$1 $2\" in 'resolve queries') echo '[\"{query}\"]'; exit 0 ;; esac\nfor a in \"$@\"; do case \"$a\" in --output=*) cp '{rows}' \"${{a#--output=}}\" ;; esac; done\nexit 0\n",
            log = bin.path().join("calls.log").display(),
            query = query.display(),
            rows = bin.path().join("rows.json").display(),
        );
        let program = bin.path().join("codeql");
        std::fs::write(&program, script).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        app.codeql_program = program;

        app.run_command(crate::widgets::command_palette::Command::CodeqlOpenModelEditor);
        assert_eq!(app.status, "Reading the endpoints of app\u{2026}");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !app.drain_codeql_model() {
            assert!(std::time::Instant::now() < deadline, "never finished");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(app.status, "2 endpoints of app, 1 modeled by CodeQL");
        let calls = std::fs::read_to_string(bin.path().join("calls.log")).unwrap();
        let calls: Vec<&str> = calls.lines().collect();
        assert!(
            calls[0].starts_with("resolve queries --format=json "),
            "{calls:?}"
        );
        assert!(calls[1].starts_with("query run --database="), "{calls:?}");
        assert!(calls[2].contains("--result-set=#select"), "{calls:?}");

        let rows: Vec<(Action, String)> = app
            .codeql
            .lines()
            .into_iter()
            .filter_map(|l| match l {
                Line::Action(
                    a
                    @ (Action::OpenModelEditor | Action::ModelGroup(_) | Action::ModelEndpoint(_)),
                    t,
                ) => Some((a, t)),
                _ => None,
            })
            .collect();
        let texts: Vec<&str> = rows.iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(
            texts,
            [
                "Refresh \u{b7} app (rust)",
                "\u{25be} mylib.core  0/1 modeled",
                "  \u{25cb} run(cmd,shell)",
                "\u{25be} mylib.core.Runner  1/1 modeled",
                "  \u{2713} go(self,what)",
            ]
        );
        // Enter on an endpoint goes to its code.
        app.activate_codeql(Hit::Action(rows[2].0));
        assert_eq!(app.editor.path.as_deref(), Some(src.as_path()));
        assert_eq!(app.editor.cursor_row, 2);
        assert_eq!(app.status, "run(cmd,shell) (Function): not modeled");
        // A group folds.
        app.activate_codeql(Hit::Action(rows[3].0));
        assert!(
            !app.codeql
                .lines()
                .iter()
                .any(|l| matches!(l, Line::Action(_, t) if t.contains("go(self")))
        );
    });
}

#[cfg(unix)]
#[test]
fn modeling_an_endpoint_writes_a_model_pack_that_runs_use() {
    // #578: the Model Editor models sources, sinks and summaries. A model
    // lands in a model pack under .github/codeql/extensions, and query
    // runs pass it as a model pack.
    use crate::widgets::codeql::{Action, Line};
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let mut app = codeql_query_fixture(tmp.path(), "select 1");
        app.codeql_program = fake_codeql(bin.path(), "col0\n1\n", 0, "");
        app.codeql.model = Some(crate::codeql_model::ModelView {
            database: String::from("lib"),
            language: String::from("python"),
            endpoints: crate::codeql_model::parse_endpoints(
                r#"{"tuples":[[{"label":"Function run"},"mylib","core","run","(cmd,shell)",false,"core.py","",{"label":"Function"}]]}"#,
            ),
            ..Default::default()
        });
        app.open_codeql_view();
        app.focus = Pane::Tree;
        let row = app
            .codeql
            .lines()
            .iter()
            .position(|l| matches!(l, Line::Action(Action::ModelEndpoint(0), _)))
            .unwrap();
        app.codeql.selected = row;
        app.handle_key(key(KeyCode::Char('m'), KeyModifiers::NONE))
            .unwrap();
        let picker = app.list_picker.as_ref().expect("what to model it as");
        assert_eq!(picker.title, "Model run(cmd,shell)");
        let labels: Vec<&str> = picker.rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "source ReturnValue (remote)",
                "sink Argument[0,cmd:] (command-injection)",
                "sink Argument[1,shell:] (command-injection)",
                "summary Argument[0,cmd:] \u{2192} ReturnValue (taint)",
                "summary Argument[1,shell:] \u{2192} ReturnValue (taint)",
            ]
        );
        app.list_picker.as_mut().unwrap().selected = 1;
        app.confirm_list_picker();
        let prompt = app.input_prompt.as_ref().expect("its kind");
        assert_eq!(prompt.value, "command-injection");
        app.submit_input_prompt();
        let pack = tmp.path().join(".github/codeql/extensions/lib-python");
        assert!(
            app.status
                .starts_with("Modeled run(cmd,shell) as a sink Argument[0,cmd:]"),
            "{}",
            app.status
        );
        assert_eq!(
            std::fs::read_to_string(pack.join("models/lib-python.model.yml")).unwrap(),
            "extensions:\n  - addsTo:\n      pack: codeql/python-all\n      extensible: sinkModel\n    data:\n      - ['mylib', 'Member[core].Member[run].Argument[0,cmd:]', 'command-injection']\n"
        );
        assert!(
            std::fs::read_to_string(pack.join("codeql-pack.yml"))
                .unwrap()
                .starts_with("name: croft/lib-python\n")
        );
        assert!(
            app.codeql.lines().iter().any(|l| matches!(
                l,
                Line::Action(Action::ModelEndpoint(0), t) if t == "  \u{25cf} run(cmd,shell)"
            )),
            "marked as modeled by you"
        );

        // A query run now uses the pack.
        app.editor.open(&tmp.path().join("q.ql")).unwrap();
        app.run_command(crate::widgets::command_palette::Command::CodeqlRunQuery);
        wait_for_codeql(&mut app);
        let calls = std::fs::read_to_string(bin.path().join("codeql-calls.log")).unwrap();
        assert!(calls.contains("--model-packs=croft/lib-python"), "{calls}");

        // Removing its models empties the extension.
        app.open_codeql_view();
        app.focus = Pane::Tree;
        app.codeql.selected = app
            .codeql
            .lines()
            .iter()
            .position(|l| matches!(l, Line::Action(Action::ModelEndpoint(0), _)))
            .unwrap();
        app.handle_key(key(KeyCode::Char('m'), KeyModifiers::NONE))
            .unwrap();
        let rows = &app.list_picker.as_ref().unwrap().rows;
        let remove = rows
            .iter()
            .position(|r| r.id == "0:remove")
            .expect("remove is offered");
        app.list_picker.as_mut().unwrap().selected = remove;
        app.confirm_list_picker();
        assert!(
            app.status
                .starts_with("Removed the models of run(cmd,shell)"),
            "{}",
            app.status
        );
        assert_eq!(
            std::fs::read_to_string(pack.join("models/lib-python.model.yml")).unwrap(),
            "extensions:\n  []\n"
        );
    });
}

/// #578's Model Editor end to end against the real CLI: model a library
/// function as a source and another as a sink, and the command-injection
/// query, run through croft, finds the flow it misses without them.
#[cfg(unix)]
#[test]
#[ignore = "needs the CodeQL CLI and codeql/python-queries; set CROFT_TEST_CODEQL"]
fn real_models_made_in_the_model_editor_change_what_a_query_finds() {
    let codeql = std::path::PathBuf::from(std::env::var("CROFT_TEST_CODEQL").unwrap());
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(src.join("mylib")).unwrap();
        std::fs::write(src.join("mylib/__init__.py"), "").unwrap();
        std::fs::write(
            src.join("mylib/core.py"),
            "import os\n\ndef run(cmd, shell=True):\n    return os.system(cmd)\n\ndef fetch():\n    return \"x\"\n",
        )
        .unwrap();
        std::fs::write(
            src.join("app.py"),
            "from mylib.core import run, fetch\nrun(fetch())\n",
        )
        .unwrap();
        let db = tmp.path().join("db");
        let made = std::process::Command::new(&codeql)
            .args(["database", "create", "--language=python", "--source-root"])
            .arg(&src)
            .arg(&db)
            .output()
            .unwrap();
        assert!(
            made.status.success(),
            "{}",
            String::from_utf8_lossy(&made.stderr)
        );
        crate::codeql_db::DatabaseStore {
            databases: vec![crate::codeql_db::DbEntry {
                name: String::from("lib"),
                path: db,
                language: Some(String::from("python")),
                added: 0,
                former_names: Vec::new(),
            }],
            current: Some(0),
            sort_by: None,
        }
        .save(&App::codeql_db_store_path())
        .unwrap();
        let resolved = std::process::Command::new(&codeql)
            .args([
                "resolve",
                "queries",
                "--format=json",
                "codeql/python-queries:Security/CWE-078/CommandInjection.ql",
            ])
            .output()
            .unwrap();
        let query = crate::codeql_ast::first_query(&String::from_utf8_lossy(&resolved.stdout))
            .expect("the command-injection query");
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.codeql_program = codeql;
        let run_query = |app: &mut App| -> String {
            app.editor.open(&query).unwrap();
            app.run_command(crate::widgets::command_palette::Command::CodeqlRunQuery);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(900);
            while app.codeql_run.is_some() {
                app.drain_codeql_run();
                assert!(std::time::Instant::now() < deadline, "never finished");
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            let history = crate::codeql_query::History::load(&App::codeql_history_path());
            std::fs::read_to_string(&history.entries[history.newest().unwrap()].output)
                .unwrap_or_else(|e| panic!("{e}: {}", app.status))
        };
        let before = run_query(&mut app);
        assert!(before.contains("\"results\":[]"), "no flow yet: {before}");

        app.run_command(crate::widgets::command_palette::Command::CodeqlOpenModelEditor);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(900);
        while !app.drain_codeql_model() {
            assert!(std::time::Instant::now() < deadline, "never finished");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let find = |app: &App, f: &str| {
            app.codeql
                .model
                .as_ref()
                .unwrap_or_else(|| panic!("{}", app.status))
                .endpoints
                .iter()
                .position(|e| e.function == f)
                .unwrap()
        };
        let (fetch, run) = (find(&app, "fetch"), find(&app, "run"));
        app.choose_codeql_model(&format!("{fetch}:0"));
        app.submit_input_prompt(); // remote source at ReturnValue
        app.choose_codeql_model(&format!("{run}:1"));
        app.submit_input_prompt(); // command-injection sink at Argument[0,cmd:]
        let after = run_query(&mut app);
        assert!(
            after.contains("\"ruleId\":\"py/command-line-injection\""),
            "{after}"
        );
    });
}

/// #578's Model Editor against the real CLI: a Python library's
/// framework-mode endpoints, read from a database built in the test.
#[cfg(unix)]
#[test]
#[ignore = "needs the CodeQL CLI and codeql/python-queries; set CROFT_TEST_CODEQL"]
fn the_model_editor_reads_a_real_librarys_endpoints() {
    let codeql = std::path::PathBuf::from(std::env::var("CROFT_TEST_CODEQL").unwrap());
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(src.join("mylib")).unwrap();
        std::fs::write(
            src.join("mylib/__init__.py"),
            "from .core import run, Runner\n",
        )
        .unwrap();
        std::fs::write(
            src.join("mylib/core.py"),
            "import os\n\ndef run(cmd, shell=True):\n    return os.system(cmd)\n\nclass Runner:\n    def go(self, what):\n        return run(what)\n",
        )
        .unwrap();
        std::fs::write(
            src.join("pyproject.toml"),
            "[project]\nname = \"mylib\"\nversion = \"0.1\"\n",
        )
        .unwrap();
        let db = tmp.path().join("db");
        let made = std::process::Command::new(&codeql)
            .args(["database", "create", "--language=python", "--source-root"])
            .arg(&src)
            .arg(&db)
            .output()
            .unwrap();
        assert!(
            made.status.success(),
            "{}",
            String::from_utf8_lossy(&made.stderr)
        );
        crate::codeql_db::DatabaseStore {
            databases: vec![crate::codeql_db::DbEntry {
                name: String::from("lib"),
                path: db,
                language: Some(String::from("python")),
                added: 0,
                former_names: Vec::new(),
            }],
            current: Some(0),
            sort_by: None,
        }
        .save(&App::codeql_db_store_path())
        .unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.codeql_program = codeql;
        app.run_command(crate::widgets::command_palette::Command::CodeqlOpenModelEditor);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(900);
        while !app.drain_codeql_model() {
            assert!(std::time::Instant::now() < deadline, "never finished");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let view = app
            .codeql
            .model
            .as_ref()
            .unwrap_or_else(|| panic!("{}", app.status));
        let labels: Vec<String> = view.endpoints.iter().map(|e| e.label()).collect();
        assert!(
            labels.contains(&String::from("run(cmd,shell)")),
            "{labels:?}"
        );
        assert!(
            labels.contains(&String::from("go(self,what)")),
            "{labels:?}"
        );
        let run = view.endpoints.iter().find(|e| e.function == "run").unwrap();
        assert_eq!(
            (run.namespace.as_str(), run.class.as_str()),
            ("mylib", "core")
        );
        assert_eq!(run.location.as_ref().map(|l| l.line), Some(3));
    });
}

/// #578's AST Viewer against the real CLI: build a Python database, read
/// the AST of its source file, and find the function in it. Needs a
/// `codeql` with the codeql/python-all pack downloaded; set
/// `CROFT_TEST_CODEQL` to it.
#[cfg(unix)]
#[test]
#[ignore = "needs the CodeQL CLI and codeql/python-all; set CROFT_TEST_CODEQL"]
fn the_ast_viewer_reads_a_real_databases_ast() {
    let codeql = std::path::PathBuf::from(std::env::var("CROFT_TEST_CODEQL").unwrap());
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        let file = src.join("app.py");
        std::fs::write(&file, "import os\n\ndef run(cmd):\n    os.system(cmd)\n").unwrap();
        let db = tmp.path().join("db");
        let made = std::process::Command::new(&codeql)
            .args(["database", "create", "--language=python", "--source-root"])
            .arg(&src)
            .arg(&db)
            .output()
            .unwrap();
        assert!(
            made.status.success(),
            "{}",
            String::from_utf8_lossy(&made.stderr)
        );
        crate::codeql_db::DatabaseStore {
            databases: vec![crate::codeql_db::DbEntry {
                name: String::from("py"),
                path: db,
                language: Some(String::from("python")),
                added: 0,
                former_names: Vec::new(),
            }],
            current: Some(0),
            sort_by: None,
        }
        .save(&App::codeql_db_store_path())
        .unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.codeql_program = codeql;
        app.editor.open(&file).unwrap();
        app.run_command(crate::widgets::command_palette::Command::CodeqlViewAst);
        wait_for_codeql_ast(&mut app, 600);
        let view = app
            .codeql
            .ast
            .as_ref()
            .unwrap_or_else(|| panic!("{}", app.status));
        let def = view
            .tree
            .roots
            .iter()
            .copied()
            .find(|&i| view.tree.nodes[i].label.starts_with("[FunctionDef] run"))
            .expect("the function is a top-level node");
        assert_eq!(
            view.target(def).map(|t| (t.0, t.1)),
            Some((file.clone(), 3))
        );
        assert!(!view.tree.nodes[def].children.is_empty());
    });
}

#[cfg(unix)]
#[test]
fn a_problem_query_runs_on_the_current_database_and_opens_as_sarif() {
    // #578: VS Code's "Run Query on Selected Database"; alerts open in the
    // SARIF viewer and the run is kept in the query history.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let mut app = codeql_query_fixture(
            tmp.path(),
            "/**\n * @kind problem\n */\nimport rust\nselect 1",
        );
        app.codeql_program = fake_codeql(
            bin.path(),
            r#"{"version":"2.1.0","runs":[{"tool":{"driver":{"name":"CodeQL"}},"results":[{"ruleId":"q","message":{"text":"From codeql."}}]}]}"#,
            0,
            "",
        );
        app.run_command(crate::widgets::command_palette::Command::CodeqlRunQuery);
        assert!(app.status.contains("Running q.ql on app"), "{}", app.status);
        assert_eq!(app.codeql.history.len(), 1);
        assert_eq!(
            app.codeql.history[0].status,
            crate::codeql_query::RunStatus::Running,
            "{:?}",
            app.codeql.history
        );
        wait_for_codeql(&mut app);
        // The alerts open as a .sarif file, which the SARIF viewer claims.
        let opened = app.editor.path.clone().expect("the results open");
        assert_eq!(opened.extension().and_then(|e| e.to_str()), Some("sarif"));
        assert!(
            std::fs::read_to_string(&opened)
                .unwrap()
                .contains("From codeql.")
        );
        let run = &app.codeql.history[0];
        assert_eq!(
            (&run.status, run.name.as_str(), run.database.as_str()),
            (&crate::codeql_query::RunStatus::Succeeded, "q.ql", "app"),
            "{:?}",
            app.codeql.history
        );
        let calls = std::fs::read_to_string(bin.path().join("codeql-calls.log")).unwrap();
        assert!(calls.starts_with("database analyze "), "{calls}");
    });
}

#[cfg(unix)]
#[test]
fn a_failed_query_run_is_recorded_with_codeqls_own_words() {
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let mut app = codeql_query_fixture(tmp.path(), "import rust\nselect 1");
        app.codeql_program = fake_codeql(bin.path(), "", 2, "ERROR: could not resolve module rust");
        app.run_command(crate::widgets::command_palette::Command::CodeqlRunQuery);
        wait_for_codeql(&mut app);
        assert_eq!(
            app.codeql.history[0].status,
            crate::codeql_query::RunStatus::Failed(String::from(
                "ERROR: could not resolve module rust"
            ))
        );
        assert_eq!(app.codeql.history[0].name, "q.ql");
        assert!(
            app.status.contains("could not resolve module"),
            "{}",
            app.status
        );
        let calls = std::fs::read_to_string(bin.path().join("codeql-calls.log")).unwrap();
        assert!(
            calls.starts_with("query run "),
            "a table query runs as a query: {calls}"
        );
    });
}

#[cfg(unix)]
#[test]
fn a_successful_query_run_records_its_result_count() {
    // #578: Query History shows how many results a run produced, and can
    // sort by it.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let mut app = codeql_query_fixture(tmp.path(), "import rust\nselect 1");
        app.codeql_program = fake_codeql(bin.path(), "col0\n1\n2\n", 0, "");
        app.run_command(crate::widgets::command_palette::Command::CodeqlRunQuery);
        wait_for_codeql(&mut app);
        let history = crate::codeql_query::History::load(&App::codeql_history_path());
        assert_eq!(history.entries[0].results, Some(2), "{}", app.status);
        assert_eq!(
            app.codeql.history[0].results,
            Some(2),
            "{:?}",
            app.codeql.history[0]
        );
    });
}

#[cfg(unix)]
#[test]
fn a_runs_query_text_is_kept_as_it_was_when_it_ran() {
    // #578: "View Query Text" shows the query a run used, even after the
    // file has changed.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let mut app = codeql_query_fixture(tmp.path(), "import rust\nselect 1");
        app.codeql_program = fake_codeql(bin.path(), "col0\n1\n", 0, "");
        app.run_command(crate::widgets::command_palette::Command::CodeqlRunQuery);
        wait_for_codeql(&mut app);
        std::fs::write(tmp.path().join("q.ql"), "import rust\nselect 2").unwrap();

        app.open_codeql_view();
        app.focus = Pane::Tree;
        app.codeql.select_history(0);
        app.handle_key(key(KeyCode::Char('t'), KeyModifiers::NONE))
            .unwrap();
        let opened = app.editor.path.clone().expect("the query text opened");
        assert!(
            opened.starts_with(App::codeql_results_dir()),
            "{opened:?}: {}",
            app.status
        );
        assert_eq!(opened.file_name().unwrap(), "q.ql");
        assert_eq!(app.editor.lines, ["import rust", "select 1"]);
        assert!(
            app.status.contains("as it was when it ran"),
            "{}",
            app.status
        );
    });
}

#[cfg(unix)]
#[test]
fn codeql_accept_test_output_accepts_the_open_test_or_the_failed_ones() {
    // #578: accept a failing CodeQL test's actual output as expected,
    // through `codeql test accept`.
    use crate::widgets::command_palette::Command;
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("test/Find");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(tmp.path().join("test/qlpack.yml"), "name: t\ntests: .\n").unwrap();
        std::fs::write(dir.join("Find.qlref"), "Find.ql\n").unwrap();
        std::fs::write(dir.join("Find.expected"), "old\n").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.set_codeql_program(fake_codeql(bin.path(), "", 0, ""));
        let wait = |app: &mut App| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while !app.drain_codeql_pack_job() {
                assert!(std::time::Instant::now() < deadline, "never finished");
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        };

        assert_eq!(
            Command::from_id("codeql_accept_test_output"),
            Some(Command::CodeqlAcceptTestOutput)
        );
        app.run_command(Command::CodeqlAcceptTestOutput);
        assert!(
            app.status.starts_with("No CodeQL test to accept"),
            "{}",
            app.status
        );

        // The open file's test, but no run has left output yet.
        app.editor.open(&dir.join("Find.expected")).unwrap();
        app.run_command(Command::CodeqlAcceptTestOutput);
        assert!(app.status.starts_with("No actual output"), "{}", app.status);

        std::fs::write(dir.join("Find.actual"), "new\n").unwrap();
        app.run_command(Command::CodeqlAcceptTestOutput);
        wait(&mut app);
        assert_eq!(app.status, "Accepted the output of Find.qlref as expected");
        let calls = std::fs::read_to_string(bin.path().join("codeql-calls.log")).unwrap();
        assert_eq!(
            calls.trim(),
            format!("test accept {}", dir.join("Find.qlref").display())
        );

        // With no test file open: the tests the last run failed.
        app.editor
            .open(&tmp.path().join("test/qlpack.yml"))
            .unwrap();
        app.testing.apply_case(crate::testing::model::TestCase {
            name: String::from("test/Find::Find.qlref"),
            status: crate::testing::model::TestStatus::Failed,
        });
        app.run_command(Command::CodeqlAcceptTestOutput);
        wait(&mut app);
        assert_eq!(app.status, "Accepted the output of Find.qlref as expected");
    });
}

#[test]
fn a_variant_runs_repository_list_is_copied_and_its_logs_opened() {
    // #578: for a variant analysis run, copy the repository list and view
    // its logs, which live on the controller's GitHub Actions run.
    use crate::codeql_submit::{parse_progress, parse_submission, record_submitted};
    use crate::widgets::command_palette::Command;
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.run_command(Command::CodeqlCopyVariantRepoList);
        assert_eq!(app.status, "No variant analysis has been submitted yet");

        let run = parse_submission(
            r#"{"id": 9}"#,
            "me/ctl",
            std::path::Path::new("/w/Find.ql"),
            "python",
            0,
        )
        .unwrap();
        record_submitted(&App::codeql_variant_runs_path(), run.clone()).unwrap();
        app.run_command(Command::CodeqlCopyVariantRepoList);
        assert_eq!(
            app.status,
            "Variant analysis 9 has not reported its repositories yet"
        );
        app.run_command(Command::CodeqlViewVariantLogs);
        assert!(
            app.status.contains("has no Actions run yet"),
            "{}",
            app.status
        );

        let mut later = run;
        later.id = 10;
        later.workflow_run = Some(777);
        later.progress = parse_progress(
            r#"{"status": "in_progress", "scanned_repositories": [
                {"repository": {"full_name": "a/b"}, "analysis_status": "succeeded", "result_count": 1},
                {"repository": {"full_name": "c/d"}, "analysis_status": "pending"}]}"#,
        )
        .ok();
        record_submitted(&App::codeql_variant_runs_path(), later).unwrap();
        // With no run selected in the side bar: the newest.
        app.run_command(Command::CodeqlCopyVariantRepoList);
        assert_eq!(
            app.status,
            "Copied the 2 repositories of variant analysis 10"
        );
        assert_eq!(crate::clipboard::read_string().unwrap(), "a/b\nc/d");
        app.run_command(Command::CodeqlViewVariantLogs);
        assert_eq!(
            app.status,
            "Open https://github.com/me/ctl/actions/runs/777"
        );
    });
}

#[cfg(unix)]
#[test]
fn a_running_codeql_query_shows_in_the_status_bar_and_a_click_cancels_it() {
    // #578: progress and cancellation in the status bar. The run is killed
    // mid-flight and recorded as cancelled, not failed.
    use crate::widgets::command_palette::Command;
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let mut app = codeql_query_fixture(tmp.path(), "import rust\nselect 1");
        app.run_command(Command::CodeqlCancelRunningQuery);
        assert_eq!(app.status, "No CodeQL query is running");

        app.codeql_program = fake_codeql(bin.path(), "col0\n1\n", 0, "");
        let script = std::fs::read_to_string(&app.codeql_program).unwrap();
        std::fs::write(
            &app.codeql_program,
            script.replace("exit 0", "exec sleep 30"),
        )
        .unwrap();
        app.run_command(Command::CodeqlRunQuery);
        let started = std::time::Instant::now();
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 30)).unwrap();
        term.draw(|f| app.render(f)).unwrap();
        let screen = screen_text(&term);
        assert!(screen.contains("CodeQL q.ql 0:0"), "{screen}");
        let chip = app.status_codeql_rect;
        assert!(chip.width > 0, "the chip is a click target");
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: chip.x + 1,
            row: chip.y,
            modifiers: KeyModifiers::NONE,
        });
        assert!(app.status.starts_with("Cancelling"), "{}", app.status);
        wait_for_codeql(&mut app);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "killed, not waited out"
        );
        assert_eq!(app.status, "q.ql cancelled");
        let history = crate::codeql_query::History::load(&App::codeql_history_path());
        assert_eq!(
            history.entries[0].status,
            crate::codeql_query::RunStatus::Cancelled
        );
        assert_eq!(
            app.codeql.history[0].status,
            crate::codeql_query::RunStatus::Cancelled,
            "{:?}",
            app.codeql.history[0]
        );
        term.draw(|f| app.render(f)).unwrap();
        assert!(!screen_text(&term).contains("CodeQL q.ql"), "the chip goes");
        assert_eq!(app.status_codeql_rect, ratatui::layout::Rect::default());
    });
}

#[test]
fn running_a_query_needs_an_open_ql_file_and_a_database() {
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.rs"), "fn main() {}\n").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.editor.open(&tmp.path().join("a.rs")).unwrap();
        app.run_command(crate::widgets::command_palette::Command::CodeqlRunQuery);
        assert!(app.status.contains("Open a .ql query"), "{}", app.status);
        std::fs::write(tmp.path().join("q.ql"), "select 1").unwrap();
        app.editor.open(&tmp.path().join("q.ql")).unwrap();
        app.run_command(crate::widgets::command_palette::Command::CodeqlRunQuery);
        assert!(
            app.status.contains("Add a CodeQL database"),
            "{}",
            app.status
        );
        assert!(app.codeql_run.is_none());
    });
}

#[cfg(unix)]
#[test]
fn a_query_history_entry_reopens_its_results() {
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let mut app = codeql_query_fixture(tmp.path(), "select 1");
        app.codeql_program = fake_codeql(bin.path(), "col0\n1\n", 0, "");
        app.run_command(crate::widgets::command_palette::Command::CodeqlRunQuery);
        wait_for_codeql(&mut app);
        let results = app.editor.path.clone().unwrap();
        assert_eq!(results.extension().and_then(|e| e.to_str()), Some("csv"));
        app.editor.open(&tmp.path().join("q.ql")).unwrap();
        app.activate_codeql(crate::widgets::codeql::Hit::Action(
            crate::widgets::codeql::Action::OpenHistory(0),
        ));
        assert_eq!(app.editor.path.as_deref(), Some(results.as_path()));
    });
}

#[cfg(unix)]
#[test]
fn a_query_row_in_the_side_bar_runs_that_file_and_records_it() {
    // #578: the Queries section lists the workspace's queries by pack, and
    // Enter on one runs it from disk on the current database, whatever the
    // editor has open.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("pack")).unwrap();
        std::fs::write(
            tmp.path().join("pack/qlpack.yml"),
            "name: acme/rust\nextractor: rust\n",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("pack/r.ql"),
            "/** @kind problem */ select 1",
        )
        .unwrap();
        let mut app = codeql_query_fixture(tmp.path(), "select 1");
        app.codeql_program = fake_codeql(bin.path(), r#"{"version":"2.1.0","runs":[]}"#, 0, "");
        app.open_codeql_view();
        wait_for_codeql_queries(&mut app);
        let names: Vec<&str> = app.codeql.queries.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["acme/rust", crate::codeql_query::NO_PACK]);
        let row = app
            .codeql
            .lines()
            .iter()
            .position(|l| {
                matches!(l, crate::widgets::codeql::Line::Action(
                    crate::widgets::codeql::Action::RunQuery(0, 0),
                    label,
                ) if label.trim() == "r.ql")
            })
            .expect("the pack's query is listed");
        app.codeql.selected = row;
        app.focus = Pane::Tree;
        app.handle_codeql_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.status.contains("Running r.ql on app"), "{}", app.status);
        wait_for_codeql(&mut app);
        let run = &app.codeql.history[0];
        assert_eq!(
            (&run.status, run.name.as_str(), run.database.as_str()),
            (&crate::codeql_query::RunStatus::Succeeded, "r.ql", "app"),
            "{:?}",
            app.codeql.history
        );
        let calls = std::fs::read_to_string(bin.path().join("codeql-calls.log")).unwrap();
        assert!(
            calls.starts_with("database analyze ") && calls.contains("pack/r.ql"),
            "the saved file's @kind picks SARIF: {calls}"
        );
    });
}

/// A workspace whose pack `acme/rust` holds `a.ql`, `b.ql` and `c.ql`,
/// database "app" current, the CodeQL view open on the pack's line, and a
/// stand-in `codeql` in `bin` that fails whatever runs `b.ql`.
#[cfg(unix)]
fn codeql_pack_fixture(tmp: &std::path::Path, bin: &std::path::Path) -> App {
    use std::os::unix::fs::PermissionsExt;
    let pack = tmp.join("pack");
    std::fs::create_dir_all(&pack).unwrap();
    std::fs::write(
        pack.join("qlpack.yml"),
        "name: acme/rust\nextractor: rust\n",
    )
    .unwrap();
    for q in ["a", "b", "c"] {
        std::fs::write(pack.join(format!("{q}.ql")), "select 1").unwrap();
    }
    let mut app = codeql_query_fixture(tmp, "select 1");
    let reply = bin.join("codeql-reply");
    std::fs::write(&reply, "col0\n1\n").unwrap();
    let script = format!(
        "#!/bin/sh\necho \"$*\" >> '{log}'\ncase \"$*\" in *b.ql*) echo 'ERROR: b is broken' >&2; exit 1 ;; esac\nfor a in \"$@\"; do case \"$a\" in --output=*) cp '{reply}' \"${{a#--output=}}\" ;; esac; done\nexit 0\n",
        log = bin.join("codeql-calls.log").display(),
        reply = reply.display(),
    );
    let program = bin.join("codeql");
    std::fs::write(&program, script).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    app.codeql_program = program;
    app.open_codeql_view();
    wait_for_codeql_queries(&mut app);
    app.focus = Pane::Tree;
    let row = app
        .codeql
        .lines()
        .iter()
        .position(|l| {
            matches!(
                l,
                crate::widgets::codeql::Line::Action(
                    crate::widgets::codeql::Action::TogglePack(0),
                    _
                )
            )
        })
        .expect("the pack is listed");
    app.codeql.selected = row;
    app
}

/// Drain until one query run lands (the next of a pack run may start).
fn drain_one_codeql_run(app: &mut App) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !app.drain_codeql_run() {
        assert!(
            std::time::Instant::now() < deadline,
            "the query run never finished"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[cfg(unix)]
#[test]
fn codeql_runs_every_workspace_query_or_the_ones_selected_in_the_explorer() {
    // #578: "run one or all" queries, and run on selected files.
    use crate::widgets::command_palette::Command;
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let mut app = codeql_pack_fixture(tmp.path(), bin.path());
        let pack = tmp.path().join("pack");
        let listed: usize = app.codeql.queries.iter().map(|p| p.queries.len()).sum();
        assert!(listed >= 3);

        app.run_command(Command::CodeqlRunAllQueries);
        assert!(
            app.status
                .starts_with(&format!("Running CodeQL queries 1/{listed}: ")),
            "{}",
            app.status
        );
        while app.codeql_run.is_some() {
            drain_one_codeql_run(&mut app);
        }
        assert_eq!(app.status, format!("Ran {listed} CodeQL queries, 1 failed"));

        // Nothing selected in the Explorer that holds a query.
        app.tree.marked.clear();
        app.tree.marked.insert(tmp.path().join("pack/qlpack.yml"));
        app.run_command(Command::CodeqlRunSelectedQueries);
        assert!(app.status.starts_with("Select .ql files"), "{}", app.status);

        // Two files, listed out of order.
        app.tree.marked.clear();
        app.tree.marked.insert(pack.join("c.ql"));
        app.tree.marked.insert(pack.join("a.ql"));
        app.run_command(Command::CodeqlRunSelectedQueries);
        assert_eq!(
            app.status,
            "Running CodeQL queries 1/2: a.ql on app\u{2026}"
        );
        drain_one_codeql_run(&mut app);
        assert_eq!(
            app.status,
            "Running CodeQL queries 2/2: c.ql on app\u{2026}"
        );
        drain_one_codeql_run(&mut app);
        assert_eq!(app.status, "Ran 2 CodeQL queries");
    });
}

#[cfg(unix)]
#[test]
fn a_codeql_query_runs_on_each_database_checked_in_the_list() {
    // #578: "Run Query on Multiple Databases": check databases in a list,
    // and the open query, as edited, runs on each in the list's order.
    use crate::widgets::command_palette::Command;
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let mut app = codeql_query_fixture(tmp.path(), "select 1");
        app.codeql_program = fake_codeql(bin.path(), "col0\n1\n", 0, "");
        let mut store = crate::codeql_db::DatabaseStore::load(&App::codeql_db_store_path());
        for name in ["lib", "gone"] {
            let path = tmp.path().join("dbs").join(name);
            std::fs::create_dir_all(&path).unwrap();
            store.databases.push(crate::codeql_db::DbEntry {
                name: name.to_string(),
                path,
                language: None,
                added: 0,
                former_names: Vec::new(),
            });
        }
        store.save(&App::codeql_db_store_path()).unwrap();
        app.editor.lines[0] = String::from("select 2");

        app.run_command(Command::CodeqlRunQueryOnDatabases);
        let labels = |app: &App| -> Vec<String> {
            let picker = app.list_picker.as_ref().expect("the databases");
            picker.rows.iter().map(|r| r.label.clone()).collect()
        };
        assert_eq!(
            app.list_picker.as_ref().unwrap().title,
            "Run q.ql on databases"
        );
        assert_eq!(
            labels(&app),
            [
                "\u{25b6} Run on the checked database",
                "[x] app (rust)",
                "[ ] lib",
                "[ ] gone",
            ]
        );
        // Check lib and gone, the cursor staying on the row picked.
        for row in [2, 3] {
            app.list_picker.as_mut().unwrap().selected = row;
            app.confirm_list_picker();
            assert_eq!(app.list_picker.as_ref().unwrap().selected, row);
        }
        assert_eq!(labels(&app)[0], "\u{25b6} Run on the 3 checked databases");
        assert_eq!(labels(&app)[3], "[x] gone");
        // Unchecking all of them refuses to run.
        for row in [1, 2, 3] {
            app.list_picker.as_mut().unwrap().selected = row;
            app.confirm_list_picker();
        }
        app.list_picker.as_mut().unwrap().selected = 0;
        app.confirm_list_picker();
        assert_eq!(app.status, "Check at least one database to run on");
        for row in [3, 2, 1] {
            app.list_picker.as_mut().unwrap().selected = row;
            app.confirm_list_picker();
        }

        app.list_picker.as_mut().unwrap().selected = 0;
        app.confirm_list_picker();
        assert!(app.list_picker.is_none());
        assert_eq!(
            app.status,
            "Running CodeQL queries 1/3: q.ql on app\u{2026}"
        );
        // gone disappears before its turn: skipped, the rest still run.
        let mut store = crate::codeql_db::DatabaseStore::load(&App::codeql_db_store_path());
        store.databases.retain(|db| db.name != "gone");
        store.save(&App::codeql_db_store_path()).unwrap();
        drain_one_codeql_run(&mut app);
        assert_eq!(
            app.status,
            "Running CodeQL queries 2/3: q.ql on lib\u{2026}"
        );
        drain_one_codeql_run(&mut app);
        assert!(app.codeql_run.is_none());
        assert_eq!(app.status, "Ran 3 CodeQL queries, 1 failed");

        let history = crate::codeql_query::History::load(&App::codeql_history_path());
        let ran: Vec<&str> = history
            .entries
            .iter()
            .map(|e| e.database.as_str())
            .collect();
        assert_eq!(ran.len(), 2);
        assert!(ran.contains(&"app") && ran.contains(&"lib"), "{ran:?}");
        for e in &history.entries {
            let text =
                std::fs::read_to_string(crate::codeql_query::query_text_path(&e.output, &e.query))
                    .unwrap();
            assert_eq!(text, "select 2", "the buffer, not the file");
        }
    });
}

#[cfg(unix)]
#[test]
fn running_a_codeql_pack_runs_each_query_in_turn_past_a_failure() {
    // #578: `r` on a pack line queues every query in it; each run lands in
    // the query history in order and a failing one doesn't stop the rest.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let mut app = codeql_pack_fixture(tmp.path(), bin.path());
        let pack = tmp.path().join("pack");
        assert_eq!(
            app.codeql.queries[0].queries,
            vec![pack.join("a.ql"), pack.join("b.ql"), pack.join("c.ql")]
        );
        app.handle_codeql_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
        assert_eq!(
            app.status,
            "Running CodeQL queries 1/3: a.ql on app\u{2026}"
        );
        drain_one_codeql_run(&mut app);
        assert_eq!(
            app.status,
            "Running CodeQL queries 2/3: b.ql on app\u{2026}"
        );
        drain_one_codeql_run(&mut app);
        assert_eq!(
            app.status,
            "Running CodeQL queries 3/3: c.ql on app\u{2026}"
        );
        drain_one_codeql_run(&mut app);
        assert_eq!(app.status, "Ran 3 CodeQL queries, 1 failed");
        assert!(app.codeql_run.is_none());
        assert!(app.codeql_run_queue.is_empty());
        // Newest first: one entry per query, in the order they ran.
        assert_eq!(app.codeql.history.len(), 3, "{:?}", app.codeql.history);
        use crate::codeql_query::RunStatus;
        let runs: Vec<(&RunStatus, &str, &str)> = app
            .codeql
            .history
            .iter()
            .map(|r| (&r.status, r.name.as_str(), r.database.as_str()))
            .collect();
        let failed = RunStatus::Failed(String::from("ERROR: b is broken"));
        assert_eq!(
            runs,
            [
                (&RunStatus::Succeeded, "c.ql", "app"),
                (&failed, "b.ql", "app"),
                (&RunStatus::Succeeded, "a.ql", "app"),
            ]
        );
        let calls = std::fs::read_to_string(bin.path().join("codeql-calls.log")).unwrap();
        let at = |q: &str| calls.find(&format!("pack/{q}.ql")).unwrap();
        assert!(at("a") < at("b") && at("b") < at("c"), "{calls}");
    });
}

#[cfg(unix)]
#[test]
fn queued_codeql_queries_are_cancelled_from_the_palette_or_with_esc() {
    // #578: cancelling drops the queries still waiting; the one running
    // finishes and is recorded.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    use crate::widgets::command_palette::Command;
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let mut app = codeql_pack_fixture(tmp.path(), bin.path());
        app.run_command(Command::CodeqlRunPack);
        assert!(app.status.contains("1/3"), "{}", app.status);
        app.run_command(Command::CodeqlCancelQueue);
        assert_eq!(app.status, "Cancelled 2 queued CodeQL queries");
        wait_for_codeql(&mut app);
        assert_eq!(app.codeql.history.len(), 1, "{:?}", app.codeql.history);
        assert_eq!(
            (
                &app.codeql.history[0].status,
                app.codeql.history[0].name.as_str()
            ),
            (&crate::codeql_query::RunStatus::Succeeded, "a.ql")
        );

        // Esc in the side bar cancels first and leaves the view only after.
        app.set_sidebar_view(SidebarView::CodeQL);
        app.focus = Pane::Tree;
        app.handle_codeql_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
        app.handle_codeql_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.status, "Cancelled 2 queued CodeQL queries");
        assert!(app.sidebar_view == SidebarView::CodeQL);
        wait_for_codeql(&mut app);
        assert_eq!(app.codeql.history.len(), 2, "{:?}", app.codeql.history);
        app.run_command(Command::CodeqlCancelQueue);
        assert_eq!(app.status, "No CodeQL queries are queued");
        app.handle_codeql_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.sidebar_view == SidebarView::Explorer);
        assert_eq!(
            Command::from_id("codeql_run_pack"),
            Some(Command::CodeqlRunPack)
        );
        assert_eq!(
            Command::CodeqlRunPack.title(),
            "CodeQL: Run Queries in Pack"
        );
        assert_eq!(
            Command::CodeqlCancelQueue.title(),
            "CodeQL: Cancel Queued Queries"
        );
    });
}

#[cfg(unix)]
#[test]
fn running_a_codeql_pack_is_refused_while_a_query_runs_or_without_a_database() {
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    use crate::widgets::command_palette::Command;
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let mut app = codeql_pack_fixture(tmp.path(), bin.path());
        // A single query is running: the pack run is refused, not mixed in.
        app.run_command(Command::CodeqlRunQuery);
        assert!(app.codeql_run.is_some());
        app.handle_codeql_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
        assert_eq!(app.status, "A CodeQL query is already running");
        assert!(app.codeql_run_queue.is_empty());
        wait_for_codeql(&mut app);
        assert_eq!(app.codeql.history.len(), 1);

        // Nothing selected in the Queries section.
        app.codeql.selected = 0;
        app.run_command(Command::CodeqlRunPack);
        assert!(app.status.contains("Select a query pack"), "{}", app.status);

        // No current database: nothing is queued or recorded.
        let mut store = crate::codeql_db::DatabaseStore::load(&App::codeql_db_store_path());
        store.current = None;
        store.save(&App::codeql_db_store_path()).unwrap();
        app.open_codeql_view();
        let row = app
            .codeql
            .lines()
            .iter()
            .position(|l| {
                matches!(
                    l,
                    crate::widgets::codeql::Line::Action(
                        crate::widgets::codeql::Action::RunQuery(0, 1),
                        _
                    )
                )
            })
            .expect("b.ql is listed");
        app.codeql.selected = row;
        app.run_command(Command::CodeqlRunPack);
        assert_eq!(app.status, "Add a CodeQL database and select it first");
        assert!(app.codeql_run.is_none());
        assert!(app.codeql_run_queue.is_empty());
        assert_eq!(app.codeql.history.len(), 1);
    });
}

#[cfg(unix)]
#[test]
fn codeql_run_tests_runs_the_test_pack_through_the_testing_view() {
    // #578: "CodeQL: Run Tests" runs every CodeQL test with the app's
    // `codeql`, and the results land in the Testing view; outside a
    // CodeQL test workspace it says why nothing ran.
    // Opening the CodeQL view reads croft's cache, whose test override is
    // process-global: without the lock and a cache of its own, this test
    // reads whatever another CodeQL test has there at the time.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        use crate::testing::model::TestStatus;
        use crate::widgets::command_palette::Command;
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            Command::from_id("codeql_run_tests"),
            Some(Command::CodeqlRunTests)
        );
        assert_eq!(Command::CodeqlRunTests.title(), "CodeQL: Run Tests");

        let plain = tempfile::tempdir().unwrap();
        let mut app = App::new(plain.path().to_path_buf()).unwrap();
        app.run_command(Command::CodeqlRunTests);
        assert!(
            app.status.starts_with("No CodeQL tests here"),
            "{}",
            app.status
        );

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("test/Bad")).unwrap();
        std::fs::write(
            root.join("test/qlpack.yml"),
            "name: acme/tests\nextractor: rust\ntests: .\n",
        )
        .unwrap();
        std::fs::write(root.join("test/Bad/Bad.ql"), "select 1").unwrap();
        std::fs::write(root.join("test/Bad/Bad.expected"), "").unwrap();
        let bin = tempfile::tempdir().unwrap();
        let log = bin.path().join("calls.log");
        let program = bin.path().join("codeql");
        std::fs::write(
            &program,
            format!(
                "#!/bin/sh\necho \"$*\" >> '{log}'\n\
             echo '[1/1 comp 1s eval 9ms] FAILED(RESULT) {root}/test/Bad/Bad.ql'\n\
             echo '--- expected'\necho '+++ actual'\necho '+| 1 |'\n\
             echo '0 tests passed; 1 tests failed:'\nexit 1\n",
                log = log.display(),
                root = root.display(),
            ),
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut app = App::new(root.clone()).unwrap();
        app.set_codeql_program(program.clone());
        app.run_command(Command::CodeqlRunTests);
        assert_eq!(app.status, "Running CodeQL tests");
        assert!(app.sidebar_view == SidebarView::Testing);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let _ = app.test_worker.drain(&mut app.testing);
            if !app.testing.is_busy()
                && app
                    .testing
                    .cases_for_test()
                    .contains(&(String::from("test/Bad::Bad.ql"), TestStatus::Failed))
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the CodeQL run never reported; cases: {:?}",
                app.testing.cases_for_test()
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(app.testing.failed_count(), 1);
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "test run test\n");
    });
}

#[cfg(unix)]
#[test]
fn a_codeql_test_clicked_in_the_tree_runs_with_the_configured_codeql() {
    // #578: the test worker learns the app's `codeql` when it starts and
    // whenever it changes, so a tree click runs the configured program
    // (never PATH's) without "CodeQL: Run Tests" having run first.
    use crate::testing::model::TestStatus;
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("test/Find")).unwrap();
    std::fs::write(root.join("test/qlpack.yml"), "name: acme/tests\ntests: .\n").unwrap();
    std::fs::write(root.join("test/Find/Find.qlref"), "Find.ql\n").unwrap();
    std::fs::write(root.join("test/Find/Find.expected"), "").unwrap();
    let bin = tempfile::tempdir().unwrap();
    let log = bin.path().join("calls.log");
    let program = bin.path().join("codeql");
    std::fs::write(
        &program,
        format!(
            "#!/bin/sh\necho \"$*\" >> '{log}'\n\
             echo '[1/1 comp 1s eval 9ms] PASSED {root}/test/Find/Find.qlref'\nexit 0\n",
            log = log.display(),
            root = root.display(),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut app = App::new(root.clone()).unwrap();
    app.set_codeql_program(program);
    app.run_test(String::from("test/Find::Find.qlref"));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let _ = app.test_worker.drain(&mut app.testing);
        if app.testing.status_of("test/Find::Find.qlref") == Some(TestStatus::Passed) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the clicked test never ran with the configured codeql; cases: {:?}",
            app.testing.cases_for_test()
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(
        std::fs::read_to_string(&log).unwrap(),
        "test run test/Find/Find.qlref\n"
    );
}

#[cfg(unix)]
#[test]
fn copy_codeql_version_copies_croft_the_cli_and_the_platform() {
    // Opening the CodeQL view reads croft's cache, whose test override is
    // process-global: without the lock and a cache of its own, this test
    // reads whatever another CodeQL test has there at the time.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.codeql_program = fake_codeql(bin.path(), "", 0, "");
        // The fake prints nothing on stdout; give it a version to report.
        let script = std::fs::read_to_string(&app.codeql_program).unwrap();
        std::fs::write(
            &app.codeql_program,
            script.replace("exit 0", "echo 2.19.3\nexit 0"),
        )
        .unwrap();
        app.run_command(crate::widgets::command_palette::Command::CodeqlCopyVersion);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !app.drain_codeql_version() {
            assert!(
                std::time::Instant::now() < deadline,
                "the version check never finished"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(app.status, "Copied CodeQL version information");
        let clip = crate::clipboard::read_string().unwrap();
        assert!(clip.contains(concat!("croft version: ", env!("CARGO_PKG_VERSION"))));
        assert!(clip.contains("CodeQL CLI version: 2.19.3"));
        let calls = std::fs::read_to_string(bin.path().join("codeql-calls.log")).unwrap();
        assert_eq!(calls.trim(), "version --format=terse");
    });
}

/// Offline stand-ins for the CodeQL CLI's network calls (#578): the
/// latest release is `latest`, and a download copies `zip` (or fails with
/// `offline` when there is none), noting each URL asked for in `urls`.
fn offline_codeql_net(
    latest: &str,
    zip: Option<std::path::PathBuf>,
    urls: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
) -> crate::codeql_cli::Net {
    let latest = latest.to_string();
    crate::codeql_cli::Net {
        latest: std::sync::Arc::new(move || Ok(latest.clone())),
        download: std::sync::Arc::new(move |url, dest, progress| {
            urls.lock().unwrap().push(url.to_string());
            let zip = zip.as_ref().ok_or_else(|| String::from("offline"))?;
            let n = std::fs::copy(zip, dest).map_err(|e| e.to_string())?;
            progress(n);
            Ok(())
        }),
    }
}

/// Drain the CLI download until it lands, or fail after a few seconds.
fn wait_for_codeql_cli_download(app: &mut App) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while app.codeql_cli_download.is_some() {
        app.drain_codeql_cli_download();
        assert!(
            std::time::Instant::now() < deadline,
            "the CLI download never finished"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Drain the CLI update check until it lands, or fail after a few seconds.
fn wait_for_codeql_cli_check(app: &mut App) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !app.drain_codeql_cli_check() {
        assert!(
            std::time::Instant::now() < deadline,
            "the CLI update check never finished"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[cfg(unix)]
#[test]
fn codeql_cli_update_check_then_download_switches_to_the_managed_cli() {
    // #578: "Check for CLI Updates" compares the latest release with the
    // CLI in use; "Download CLI" then installs that release into the cache
    // and runs it from then on, for the Testing view's CodeQL tests too.
    let _guard = relay_test_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let home = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let zip = bin.path().join("release.zip");
    crate::codeql_cli::write_test_archive(&zip, "#!/bin/sh\necho 2.30.0\n");
    let urls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    with_relay_home(home.path(), || {
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.codeql_cli_net = offline_codeql_net("2.30.0", Some(zip.clone()), urls.clone());
        let on_path = fake_codeql(bin.path(), "", 0, "");
        let script = std::fs::read_to_string(&on_path).unwrap();
        std::fs::write(&on_path, script.replace("exit 0", "echo 2.19.3\nexit 0")).unwrap();
        app.set_codeql_program(on_path);
        app.codeql_cli_source = crate::codeql_cli::Source::Path;

        app.run_command(crate::widgets::command_palette::Command::CodeqlCheckCliUpdates);
        wait_for_codeql_cli_check(&mut app);
        assert_eq!(
            app.status,
            "CodeQL CLI v2.30.0 is available \u{2014} run CodeQL: Download CLI to install it"
        );

        app.start_codeql_cli_download("codeql-linux64.zip");
        wait_for_codeql_cli_download(&mut app);
        assert_eq!(
            app.status,
            "Installed CodeQL CLI v2.30.0 and switched to it"
        );
        let root = crate::codeql_cli::cli_root(&home.path().join(".cache").join("croft"));
        assert_eq!(
            urls.lock().unwrap().as_slice(),
            [crate::codeql_cli::download_url(
                "2.30.0",
                "codeql-linux64.zip"
            )]
        );
        assert_eq!(
            app.codeql_program,
            crate::codeql_cli::managed_program(&root, "2.30.0")
        );
        assert!(!root.join("2.30.0.download.zip").exists());

        // The managed copy's folder names its version, so the next check
        // does not need to run it.
        app.run_command(crate::widgets::command_palette::Command::CodeqlCheckCliUpdates);
        wait_for_codeql_cli_check(&mut app);
        assert_eq!(app.status, "The CodeQL CLI is up to date (v2.30.0)");

        // Downloading an installed release reuses it.
        app.start_codeql_cli_download("codeql-linux64.zip");
        wait_for_codeql_cli_download(&mut app);
        assert_eq!(
            app.status,
            "Installed CodeQL CLI v2.30.0 and switched to it"
        );
        assert_eq!(urls.lock().unwrap().len(), 1);
    });
}

#[cfg(unix)]
#[test]
fn codeql_cli_download_keeps_the_settings_cli_and_reports_failures() {
    // #578: a CLI named in settings always wins over the downloaded one,
    // and a failed download says why on the status line.
    let _guard = relay_test_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let home = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let urls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    with_relay_home(home.path(), || {
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.codeql_cli_net = offline_codeql_net("2.30.0", None, urls.clone());
        app.start_codeql_cli_download("codeql-linux64.zip");
        wait_for_codeql_cli_download(&mut app);
        assert_eq!(
            app.status,
            "Could not install the CodeQL CLI: download failed: offline"
        );
        assert_eq!(app.codeql_program, std::path::PathBuf::from("codeql"));

        let zip = bin.path().join("release.zip");
        crate::codeql_cli::write_test_archive(&zip, "#!/bin/sh\necho 2.30.0\n");
        app.codeql_cli_net = offline_codeql_net("2.30.0", Some(zip), urls.clone());
        let mine = fake_codeql(bin.path(), "", 0, "");
        app.codeql_cli_setting = Some(mine.display().to_string());
        app.resolve_codeql_cli();
        assert_eq!(app.codeql_cli_source, crate::codeql_cli::Source::Settings);
        // No check has run, so the pinned release is the one installed.
        app.start_codeql_cli_download("codeql-linux64.zip");
        wait_for_codeql_cli_download(&mut app);
        assert_eq!(
            app.status,
            format!(
                "Installed CodeQL CLI v{}; codeql_cli_path in settings still takes precedence",
                crate::codeql_cli::PINNED_VERSION
            )
        );
        assert_eq!(app.codeql_program, mine);

        // Cleared from settings, the managed copy is found at the next
        // resolution (PATH aside).
        app.codeql_cli_setting = None;
        let resolved = crate::codeql_cli::resolve(None, None, None, &crate::app::croft_cache_dir());
        assert_eq!(
            resolved.source,
            crate::codeql_cli::Source::Managed(crate::codeql_cli::PINNED_VERSION.into())
        );
    });
}

/// Drain the database upgrade until it lands, or fail after a few seconds.
fn wait_for_codeql_upgrade(app: &mut App) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !app.drain_codeql_upgrade() {
        assert!(
            std::time::Instant::now() < deadline,
            "the upgrade never finished"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[cfg(unix)]
#[test]
fn upgrading_a_codeql_database_runs_the_cli_off_the_ui_thread() {
    // #578: VS Code's "CodeQL: Upgrade Database". `u` on a database row
    // runs `codeql database upgrade` on a worker; the status line says how
    // it went, with the CLI's first error line when it failed. An upgrade
    // and a query run never overlap.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    use crate::widgets::command_palette::Command;
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let mut app = codeql_query_fixture(tmp.path(), "select 1");
        app.codeql_program = fake_codeql(bin.path(), "", 0, "");
        app.open_codeql_view();
        app.focus = Pane::Tree;
        select_codeql_database(&mut app, 0);
        let u = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE);
        app.handle_codeql_key(u);
        assert_eq!(app.status, "Upgrading CodeQL database app\u{2026}");
        app.handle_codeql_key(u);
        assert_eq!(app.status, "A CodeQL database upgrade is already running");
        app.run_command(Command::CodeqlRunQuery);
        assert_eq!(app.status, "Wait for the CodeQL database upgrade to finish");
        assert!(app.codeql_run.is_none());
        wait_for_codeql_upgrade(&mut app);
        assert_eq!(app.status, "Upgraded CodeQL database app");
        let calls = std::fs::read_to_string(bin.path().join("codeql-calls.log")).unwrap();
        assert_eq!(
            calls.trim(),
            format!("database upgrade {}", tmp.path().join("dbs/app").display())
        );

        app.codeql_program = fake_codeql(bin.path(), "", 1, "ERROR: the database is too new");
        app.run_command(Command::CodeqlUpgradeDatabase);
        wait_for_codeql_upgrade(&mut app);
        assert_eq!(
            app.status,
            "Could not upgrade CodeQL database app: ERROR: the database is too new"
        );

        // A query is running: the upgrade waits its turn.
        app.codeql_program = fake_codeql(bin.path(), "col0\n1\n", 0, "");
        app.run_command(Command::CodeqlRunQuery);
        assert!(app.codeql_run.is_some());
        app.run_command(Command::CodeqlUpgradeDatabase);
        assert_eq!(
            app.status,
            "Wait for the CodeQL query run to finish before upgrading"
        );
        assert!(app.codeql_upgrade.is_none());
        wait_for_codeql(&mut app);
        assert_eq!(
            Command::from_id("codeql_upgrade_database"),
            Some(Command::CodeqlUpgradeDatabase)
        );
        assert_eq!(
            Command::CodeqlUpgradeDatabase.title(),
            "CodeQL: Upgrade Database"
        );
    });
}

#[cfg(unix)]
#[test]
fn the_codeql_cache_commands_clean_up_the_selected_database() {
    // #578: VS Code's "CodeQL: Clear Cache", "Trim Cache" and "Trim Cache
    // to Overlay Base" run `codeql database cleanup` on the selected
    // database, one job at a time and never beside a query run.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    use crate::widgets::command_palette::Command;
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let mut app = codeql_query_fixture(tmp.path(), "select 1");
        app.codeql_program = fake_codeql(bin.path(), "", 0, "");
        app.open_codeql_view();
        select_codeql_database(&mut app, 0);
        let db = tmp.path().join("dbs/app");
        app.run_command(Command::CodeqlClearCache);
        assert_eq!(
            app.status,
            "Clearing the cache of CodeQL database app\u{2026}"
        );
        app.run_command(Command::CodeqlUpgradeDatabase);
        assert_eq!(
            app.status,
            "A CodeQL database cache cleanup is already running"
        );
        app.run_command(Command::CodeqlRunQuery);
        assert_eq!(
            app.status,
            "Wait for the CodeQL database cache cleanup to finish"
        );
        wait_for_codeql_upgrade(&mut app);
        assert_eq!(app.status, "Cleared the cache of CodeQL database app");
        app.run_command(Command::CodeqlTrimCache);
        wait_for_codeql_upgrade(&mut app);
        assert_eq!(app.status, "Trimmed the cache of CodeQL database app");
        app.run_command(Command::CodeqlTrimCacheToOverlay);
        wait_for_codeql_upgrade(&mut app);
        assert_eq!(
            app.status,
            "Trimmed the cache of CodeQL database app to its overlay base"
        );
        let calls = std::fs::read_to_string(bin.path().join("codeql-calls.log")).unwrap();
        let expected: Vec<String> = ["clear", "trim", "overlay"]
            .iter()
            .map(|m| format!("database cleanup --cache-cleanup={m} {}", db.display()))
            .collect();
        assert_eq!(calls.lines().collect::<Vec<_>>(), expected);

        app.codeql_program = fake_codeql(bin.path(), "", 1, "ERROR: the cache is locked");
        app.run_command(Command::CodeqlTrimCache);
        wait_for_codeql_upgrade(&mut app);
        assert_eq!(
            app.status,
            "Could not clean up the cache of CodeQL database app: ERROR: the cache is locked"
        );

        app.codeql_program = fake_codeql(bin.path(), "col0\n1\n", 0, "");
        app.run_command(Command::CodeqlRunQuery);
        app.run_command(Command::CodeqlClearCache);
        assert_eq!(
            app.status,
            "Wait for the CodeQL query run to finish before cleaning up the cache"
        );
        assert!(app.codeql_upgrade.is_none());
        wait_for_codeql(&mut app);
        assert_eq!(
            Command::from_id("codeql_trim_cache_to_overlay_base"),
            Some(Command::CodeqlTrimCacheToOverlay)
        );
    });
}

#[test]
fn a_codeql_databases_source_is_added_to_the_workspace() {
    // #578: VS Code's "CodeQL: Add Database Source to Workspace". A `src`
    // folder is added as it is; a `src.zip` is extracted once into croft's
    // cache and that folder added.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    use crate::widgets::command_palette::Command;
    use std::io::Write as _;
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let folder_db = make_codeql_db(&tmp.path().join("dbs"), "folder", "go");
        std::fs::create_dir_all(folder_db.join("src/proj")).unwrap();
        std::fs::write(folder_db.join("src/proj/main.go"), "package main\n").unwrap();
        let zip_db = make_codeql_db(&tmp.path().join("dbs"), "zipped", "go");
        let mut z = zip::ZipWriter::new(std::fs::File::create(zip_db.join("src.zip")).unwrap());
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        z.start_file("proj/main.go", opts).unwrap();
        z.write_all(b"package main\n").unwrap();
        z.finish().unwrap();
        let bare_db = make_codeql_db(&tmp.path().join("dbs"), "bare", "go");
        let mut store = crate::codeql_db::DatabaseStore::default();
        for db in [&folder_db, &zip_db, &bare_db] {
            store.add(db).unwrap();
        }
        store.current = Some(0);
        store.save(&App::codeql_db_store_path()).unwrap();
        let ws = tmp.path().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let mut app = App::new(ws).unwrap();
        app.workspace_folders_path = tmp.path().join("folders.json");
        let has_root = |app: &App, p: &std::path::Path| {
            app.roots.iter().any(|r| r == p.canonicalize().unwrap())
        };

        app.run_command(Command::CodeqlAddDatabaseSource);
        assert_eq!(
            app.status,
            "Added the source of CodeQL database folder to the workspace"
        );
        assert!(has_root(&app, &folder_db.join("src")));
        app.run_command(Command::CodeqlAddDatabaseSource);
        assert_eq!(
            app.status,
            "The source of CodeQL database folder is already in the workspace"
        );

        app.open_codeql_view();
        app.focus = Pane::Tree;
        select_codeql_database(&mut app, 1);
        let w = KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE);
        app.handle_codeql_key(w);
        assert_eq!(
            app.status,
            "Added the source of CodeQL database zipped to the workspace"
        );
        let extracted = App::codeql_source_cache_dir().join(crate::codeql_db::source_cache_name(
            &zip_db,
            &zip_db.join("src.zip"),
        ));
        assert_eq!(
            std::fs::read_to_string(extracted.join("proj/main.go")).unwrap(),
            "package main\n"
        );
        assert!(has_root(&app, &extracted));

        // Extracted once: adding it again reuses the folder as it is.
        std::fs::write(extracted.join("mine.txt"), "kept").unwrap();
        app.remove_workspace_folder(extracted.canonicalize().unwrap());
        assert!(!has_root(&app, &extracted));
        app.set_sidebar_view(SidebarView::CodeQL);
        app.focus = Pane::Tree;
        select_codeql_database(&mut app, 1);
        app.handle_codeql_key(w);
        assert!(has_root(&app, &extracted));
        assert!(extracted.join("mine.txt").exists());

        // A database replaced at the same path has a new src.zip: it is
        // extracted afresh and the out-of-date copy dropped.
        app.remove_workspace_folder(extracted.canonicalize().unwrap());
        let mut z = zip::ZipWriter::new(std::fs::File::create(zip_db.join("src.zip")).unwrap());
        z.start_file("proj/main.go", opts).unwrap();
        z.write_all(b"package main // v2\n").unwrap();
        z.finish().unwrap();
        app.set_sidebar_view(SidebarView::CodeQL);
        app.focus = Pane::Tree;
        select_codeql_database(&mut app, 1);
        app.handle_codeql_key(w);
        let fresh = App::codeql_source_cache_dir().join(crate::codeql_db::source_cache_name(
            &zip_db,
            &zip_db.join("src.zip"),
        ));
        assert_ne!(fresh, extracted);
        assert_eq!(
            std::fs::read_to_string(fresh.join("proj/main.go")).unwrap(),
            "package main // v2\n"
        );
        assert!(has_root(&app, &fresh));
        assert!(!extracted.exists(), "the stale extraction is removed");

        select_codeql_database(&mut app, 2);
        app.handle_codeql_key(w);
        assert_eq!(
            app.status,
            "CodeQL database bare has no src folder or src.zip"
        );
        assert_eq!(
            Command::CodeqlAddDatabaseSource.title(),
            "CodeQL: Add Database Source to Workspace"
        );
    });
}

#[test]
fn deleting_unused_codeql_databases_keeps_used_current_and_outside_ones() {
    // #578: VS Code's "CodeQL: Delete Unused Databases". Only copies in
    // croft's cache that no query history entry ran on go, after a
    // confirmation; the current database and ones elsewhere stay.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    use crate::widgets::command_palette::Command;
    use crate::widgets::input_prompt::InputPurpose;
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let cache = App::codeql_db_cache_dir();
        // An archive's extraction keeps the database one level down.
        let cached = |name: &str| make_codeql_db(&cache.join(name), name, "go");
        let used = cached("used");
        let legacy = cached("legacy");
        let current = cached("current");
        let stale_a = cached("stale-a");
        let stale_b = cached("stale-b");
        let outside = make_codeql_db(tmp.path(), "outside", "go");
        let mut store = crate::codeql_db::DatabaseStore::default();
        for db in [&used, &legacy, &current, &stale_a, &stale_b, &outside] {
            store.add(db).unwrap();
        }
        // Renamed since its run: the history still finds it by path.
        store.rename(0, "used, renamed").unwrap();
        store.current = Some(2);
        store.save(&App::codeql_db_store_path()).unwrap();
        let mut history = crate::codeql_query::History::default();
        for (db, name, path) in [
            (&used, "used", Some(used.clone())),
            // Saved before runs recorded a path: matched by name.
            (&legacy, "legacy", None),
        ] {
            history.push(crate::codeql_query::HistoryEntry {
                query: tmp.path().join("q.ql"),
                database: name.to_string(),
                database_path: path,
                started: 1,
                seconds: 1,
                status: crate::codeql_query::RunStatus::Succeeded,
                output: db.join("unused-output.csv"),
                name: None,
                results: None,
            });
        }
        history.save(&App::codeql_history_path()).unwrap();

        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        // Renaming the database a pre-path run named pins that run to its
        // path first, so the rename does not make it look unused.
        app.submit_rename_codeql_database(&legacy, "legacy, renamed");
        let history = crate::codeql_query::History::load(&App::codeql_history_path());
        assert!(history.entries.iter().any(
            |e| e.database == "legacy" && e.database_path.as_deref() == Some(legacy.as_path())
        ));
        app.run_command(Command::CodeqlDeleteUnusedDatabases);
        let prompt = app.input_prompt.as_ref().expect("asks first");
        assert_eq!(prompt.title, "Delete 2 unused CodeQL databases?");
        assert_eq!(
            prompt.purpose,
            InputPurpose::CodeqlDeleteUnusedDatabases {
                paths: vec![stale_a.clone(), stale_b.clone()]
            }
        );
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.status, "Deleted 2 unused CodeQL databases");
        let store = crate::codeql_db::DatabaseStore::load(&App::codeql_db_store_path());
        let names: Vec<&str> = store.databases.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(
            names,
            ["used, renamed", "legacy, renamed", "current", "outside"]
        );
        assert_eq!(store.current, Some(2), "still the current one");
        assert!(!cache.join("stale-a").exists() && !cache.join("stale-b").exists());
        for kept in [&used, &legacy, &current, &outside] {
            assert!(kept.exists(), "{}", kept.display());
        }

        app.run_command(Command::CodeqlDeleteUnusedDatabases);
        assert!(app.input_prompt.is_none());
        assert_eq!(app.status, "There are no unused CodeQL databases to delete");
        assert_eq!(
            Command::from_id("codeql_delete_unused_databases"),
            Some(Command::CodeqlDeleteUnusedDatabases)
        );
    });
}

#[test]
fn renaming_one_of_two_same_named_databases_keeps_both_protected() {
    // #578: an old run that names "dup" could have been on either of two
    // databases called that. After one is renamed, Delete Unused must
    // still leave both alone: the renamed one remembers its former name.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    use crate::widgets::command_palette::Command;
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let cache = App::codeql_db_cache_dir();
        let first = make_codeql_db(&cache.join("one"), "dup", "go");
        let second = make_codeql_db(&cache.join("two"), "dup", "go");
        let current = make_codeql_db(&cache.join("cur"), "current", "go");
        let mut store = crate::codeql_db::DatabaseStore::default();
        for db in [&first, &second, &current] {
            store.add(db).unwrap();
        }
        store.current = Some(2);
        store.save(&App::codeql_db_store_path()).unwrap();
        let mut history = crate::codeql_query::History::default();
        history.push(crate::codeql_query::HistoryEntry {
            query: tmp.path().join("q.ql"),
            database: String::from("dup"),
            database_path: None,
            started: 1,
            seconds: 1,
            status: crate::codeql_query::RunStatus::Succeeded,
            output: tmp.path().join("r.csv"),
            name: None,
            results: None,
        });
        history.save(&App::codeql_history_path()).unwrap();

        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.submit_rename_codeql_database(&first, "renamed");
        let history = crate::codeql_query::History::load(&App::codeql_history_path());
        assert_eq!(
            history.entries[0].database_path, None,
            "ambiguous: not pinned to either"
        );
        app.run_command(Command::CodeqlDeleteUnusedDatabases);
        assert!(app.input_prompt.is_none());
        assert_eq!(app.status, "There are no unused CodeQL databases to delete");
        assert!(first.exists() && second.exists());
    });
}

#[test]
fn creating_a_codeql_query_writes_opens_and_lists_it_in_the_selected_pack() {
    // #578: "CodeQL: Create Query" asks for a name and writes a starter
    // query into the selected pack, in its language; from the empty
    // Queries section it goes to the workspace root, with a pack file in
    // the Language section's language.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    use crate::widgets::codeql::{Action, Hit, Section};
    use crate::widgets::command_palette::Command;
    use crate::widgets::input_prompt::InputPurpose;
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let press = |app: &mut App, code: KeyCode| {
            app.handle_key(key(code, KeyModifiers::NONE)).unwrap();
        };
        let type_str = |app: &mut App, text: &str| {
            for c in text.chars() {
                press(app, KeyCode::Char(c));
            }
        };
        let tmp = tempfile::tempdir().unwrap();
        let pack = tmp.path().join("pack");
        std::fs::create_dir_all(&pack).unwrap();
        std::fs::write(pack.join("qlpack.yml"), "name: acme/go\nextractor: go\n").unwrap();
        std::fs::write(pack.join("a.ql"), "select 1").unwrap();
        let mut app = codeql_app(tmp.path());
        app.open_codeql_view();
        wait_for_codeql_queries(&mut app);
        app.focus = Pane::Tree;
        let row = app
            .codeql
            .lines()
            .iter()
            .position(|l| {
                matches!(
                    l,
                    crate::widgets::codeql::Line::Action(Action::TogglePack(0), _)
                )
            })
            .expect("the pack is listed");
        app.codeql.selected = row;
        app.run_command(Command::CodeqlCreateQuery);
        let prompt = app.input_prompt.as_ref().expect("a name is asked for");
        assert_eq!(
            prompt.purpose,
            InputPurpose::CodeqlCreateQuery {
                dir: pack.clone(),
                language: Some(String::from("go")),
            }
        );
        assert!(prompt.title.contains("acme/go"), "{}", prompt.title);
        type_str(&mut app, "find-calls");
        press(&mut app, KeyCode::Enter);
        let created = pack.join("find-calls.ql");
        let text = std::fs::read_to_string(&created).expect("written");
        assert!(text.contains("\nimport go\n"), "{text}");
        assert!(text.contains("@id go/find-calls"), "{text}");
        assert_eq!(app.editor.path.as_deref(), Some(created.as_path()));
        assert!(app.focus == Pane::Editor);
        assert!(app.codeql.queries[0].queries.contains(&created), "listed");
        assert!(app.status.contains("pack/find-calls.ql"), "{}", app.status);
        assert!(!pack.join("sub").exists());

        // The same name again is refused on the status line.
        app.set_sidebar_view(SidebarView::CodeQL);
        app.focus = Pane::Tree;
        press(&mut app, KeyCode::Char('n'));
        type_str(&mut app, "find-calls.ql");
        press(&mut app, KeyCode::Enter);
        assert!(app.status.contains("already exists"), "{}", app.status);
        assert_eq!(std::fs::read_to_string(&created).unwrap(), text);

        // An empty workspace: the welcome row, in the chosen language.
        let empty = tempfile::tempdir().unwrap();
        let mut app = codeql_app(empty.path());
        app.open_codeql_view();
        wait_for_codeql_queries(&mut app);
        app.focus = Pane::Tree;
        app.codeql.language = Some(6);
        app.codeql.collapsed.insert(Section::Databases);
        // An empty Queries section starts folded: unfold it for its row.
        app.codeql.toggle(Section::Queries);
        let row = app
            .codeql
            .lines()
            .iter()
            .position(|l| {
                matches!(
                    l,
                    crate::widgets::codeql::Line::Action(Action::CreateQuery, _)
                )
            })
            .expect("the welcome offers to create one");
        app.codeql.selected = row;
        assert_eq!(
            app.codeql.selected_hit(),
            Some(Hit::Action(Action::CreateQuery))
        );
        press(&mut app, KeyCode::Enter);
        type_str(&mut app, "hello");
        press(&mut app, KeyCode::Enter);
        let created = empty.path().join("hello.ql");
        assert!(
            std::fs::read_to_string(&created)
                .unwrap()
                .contains("import python"),
            "{}",
            app.status
        );
        let qlpack = std::fs::read_to_string(empty.path().join("qlpack.yml")).unwrap();
        assert!(qlpack.contains("codeql/python-all"), "{qlpack}");
        assert_eq!(app.editor.path.as_deref(), Some(created.as_path()));
        assert!(
            app.codeql
                .queries
                .iter()
                .any(|p| p.queries.contains(&created)),
            "{:?}",
            app.codeql.queries
        );
        assert_eq!(
            Command::from_id("codeql_create_query"),
            Some(Command::CodeqlCreateQuery)
        );
        assert_eq!(Command::CodeqlCreateQuery.title(), "CodeQL: Create Query");
    });
}

#[test]
fn opening_the_codeql_view_does_not_wait_for_the_query_walk() {
    // #840: the QL icon walked the whole workspace for queries before the
    // view switched. It switches at once and the queries land on a later
    // tick; a query created meanwhile is listed at once all the same.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    use crate::widgets::codeql::Line;
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let pack = tmp.path().join("pack");
        std::fs::create_dir_all(&pack).unwrap();
        std::fs::write(pack.join("qlpack.yml"), "name: acme/go\nextractor: go\n").unwrap();
        std::fs::write(pack.join("a.ql"), "select 1").unwrap();
        let mut app = codeql_app(tmp.path());
        app.open_codeql_view();
        assert_eq!(app.sidebar_view, SidebarView::CodeQL);
        assert!(
            app.codeql_queries_job.is_some(),
            "the walk is off the UI thread"
        );
        assert!(app.codeql.queries.is_empty());
        assert!(
            app.codeql
                .lines()
                .contains(&Line::Text("Discovering queries\u{2026}"))
        );
        wait_for_codeql_queries(&mut app);
        let names: Vec<&str> = app.codeql.queries.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["acme/go"]);

        app.open_codeql_view();
        assert!(app.codeql_queries_job.is_some());
        assert_eq!(app.codeql.queries[0].queries, vec![pack.join("a.ql")]);
        app.submit_create_codeql_query(&pack, Some("go"), "later");
        let created = pack.join("later.ql");
        assert!(
            app.codeql.queries[0].queries.contains(&created),
            "{:?}",
            app.codeql.queries
        );
        assert!(
            !app.drain_codeql_queries(),
            "the older walk is dropped, never listed over the new query"
        );
    });
}

#[test]
fn moving_the_cursor_onto_a_result_selects_it_in_the_viewer() {
    let (tmp, log) = sarif_nav_fixture();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&log).unwrap();
    app.editor.pin_active();
    app.open_at(&tmp.path().join("a.rs"), 4, 0).unwrap();
    app.sync_sarif_selection_to_cursor();
    let viewer = (0..app.editor.tab_count())
        .find(|&i| app.editor.tab_path(i).as_deref() == Some(log.as_path()))
        .unwrap();
    let view = app.editor.editors[viewer].sarif.as_ref().unwrap();
    assert_eq!(view.selected_entry().unwrap().message, "second");
}

/// #1369: a result of a database created from a sub-folder of the
/// workspace opens that folder's file, not a Locate prompt.
#[test]
fn a_codeql_result_opens_from_the_folder_its_database_was_created_from() {
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let app_dir = tmp.path().join("cq/app");
        std::fs::create_dir_all(&app_dir).unwrap();
        std::fs::write(app_dir.join("web.py"), WEB_PY_1369).unwrap();
        let db = tmp.path().join("db");
        std::fs::create_dir_all(&db).unwrap();
        std::fs::write(
            db.join(crate::codeql_db::DB_MARKER),
            format!("sourceLocationPrefix: {}\n", app_dir.display()),
        )
        .unwrap();
        let log = codeql_run_on(tmp.path(), &db);
        let mut app = App::new(tmp.path().join("cq")).unwrap();
        app.editor.open_preview(&log).unwrap();
        app.editor.sarif.as_mut().unwrap().selected = 1;
        app.open_selected_sarif_result();
        assert!(
            app.input_prompt.is_none(),
            "no Locate prompt: {}",
            app.status
        );
        assert_eq!(
            app.editor.path.as_deref(),
            Some(app_dir.join("web.py").as_path())
        );
        assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (3, 4));
    });
}

/// #1369: with the source not on this machine, the result opens read-only
/// from the database's `src.zip`, as VS Code's CodeQL extension does.
#[test]
fn a_codeql_result_opens_from_the_databases_source_archive() {
    use std::io::Write as _;
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("cq")).unwrap();
        let db = tmp.path().join("db");
        std::fs::create_dir_all(&db).unwrap();
        std::fs::write(
            db.join(crate::codeql_db::DB_MARKER),
            "sourceLocationPrefix: /home/someone/app\n",
        )
        .unwrap();
        let mut z = zip::ZipWriter::new(std::fs::File::create(db.join("src.zip")).unwrap());
        z.start_file(
            "home/someone/app/web.py",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        z.write_all(WEB_PY_1369.as_bytes()).unwrap();
        z.finish().unwrap();
        let log = codeql_run_on(tmp.path(), &db);
        let mut app = App::new(tmp.path().join("cq")).unwrap();
        app.editor.open_preview(&log).unwrap();
        app.editor.sarif.as_mut().unwrap().selected = 1;
        app.open_selected_sarif_result();
        assert!(
            app.input_prompt.is_none(),
            "no Locate prompt: {}",
            app.status
        );
        let opened = app.editor.path.clone().expect("the archived copy opened");
        assert!(opened.starts_with(croft_cache_dir()), "{opened:?}");
        assert_eq!(opened.file_name().unwrap(), "web.py");
        assert_eq!(app.editor.lines.join("\n"), WEB_PY_1369.trim_end());
        assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (3, 4));
        assert_eq!(
            app.status,
            "Opened web.py from the CodeQL database's source archive (read-only)"
        );
    });
}

/// #1369: a result of a CodeQL log added to a viewer whose own log is not
/// a CodeQL run's is looked up in the added log's database.
#[test]
fn a_codeql_result_of_an_added_log_opens_from_its_own_database() {
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let app_dir = tmp.path().join("cq/app");
        std::fs::create_dir_all(&app_dir).unwrap();
        std::fs::write(app_dir.join("web.py"), WEB_PY_1369).unwrap();
        let db = tmp.path().join("db");
        std::fs::create_dir_all(&db).unwrap();
        std::fs::write(
            db.join(crate::codeql_db::DB_MARKER),
            format!("sourceLocationPrefix: {}\n", app_dir.display()),
        )
        .unwrap();
        let log = codeql_run_on(tmp.path(), &db);
        let other = tmp.path().join("ci.sarif");
        std::fs::write(
            &other,
            r#"{"version":"2.1.0","runs":[{"tool":{"driver":{"name":"lint"}},"results":[
                {"message":{"text":"elsewhere"},"locations":[{"physicalLocation":{"artifactLocation":{"uri":"gone.py"},"region":{"startLine":1}}}]}
            ]}]}"#,
        )
        .unwrap();
        let mut app = App::new(tmp.path().join("cq")).unwrap();
        app.editor.open_preview(&other).unwrap();
        {
            let view = app.editor.sarif.as_mut().unwrap();
            let parsed =
                crate::sarif::load::parse_log(&std::fs::read_to_string(&log).unwrap()).unwrap();
            assert!(view.add_log(&log, parsed));
            let rows = view.rows();
            view.selected = rows
                .iter()
                .position(|r| matches!(r, crate::sarif::view::Row::Item { entry } if view.entries[*entry].log == 1))
                .unwrap();
        }
        app.open_selected_sarif_result();
        assert!(
            app.input_prompt.is_none(),
            "no Locate prompt: {}",
            app.status
        );
        assert_eq!(
            app.editor.path.as_deref(),
            Some(app_dir.join("web.py").as_path())
        );
    });
}

/// #1369: an archived line with a character outside the BMP before the
/// result opens at the result, its UTF-16 column turned into characters.
#[test]
fn a_codeql_result_from_the_source_archive_converts_utf16_columns() {
    use std::io::Write as _;
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("cq")).unwrap();
        let db = tmp.path().join("db");
        std::fs::create_dir_all(&db).unwrap();
        std::fs::write(
            db.join(crate::codeql_db::DB_MARKER),
            "sourceLocationPrefix: /home/someone/app\n",
        )
        .unwrap();
        let source = "s = '\u{1F600}'; run(s)\n";
        let mut z = zip::ZipWriter::new(std::fs::File::create(db.join("src.zip")).unwrap());
        z.start_file(
            "home/someone/app/web.py",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        z.write_all(source.as_bytes()).unwrap();
        z.finish().unwrap();
        let log = codeql_run_on(tmp.path(), &db);
        // `run` starts at character 9, UTF-16 unit 10 (1-based 11).
        std::fs::write(
            &log,
            r#"{"version":"2.1.0","runs":[{"tool":{"driver":{"name":"CodeQL"}},"results":[
                {"message":{"text":"run call"},"locations":[{"physicalLocation":{"artifactLocation":{"uri":"web.py","uriBaseId":"%SRCROOT%"},"region":{"startLine":1,"startColumn":11}}}]}
            ]}]}"#,
        )
        .unwrap();
        let mut app = App::new(tmp.path().join("cq")).unwrap();
        app.editor.open_preview(&log).unwrap();
        app.editor.sarif.as_mut().unwrap().selected = 1;
        app.open_selected_sarif_result();
        let opened = app.editor.path.clone().expect("the archived copy opened");
        assert!(opened.starts_with(croft_cache_dir()), "{opened:?}");
        assert_eq!((app.editor.cursor_row, app.editor.cursor_col), (0, 9));
    });
}

#[test]
fn a_second_log_joins_the_viewer_and_delete_closes_it_from_the_logs_tab() {
    // #577: VS Code merges several logs in one panel; its Logs tab closes one.
    let (tmp, log) = sarif_fixture();
    let second = tmp.path().join("second.sarif");
    std::fs::write(
        &second,
        r#"{"version":"2.1.0","runs":[{"tool":{"driver":{"name":"other"}},"results":[
            {"ruleId":"Z1","message":{"text":"From the second log."}}]}]}"#,
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&log).unwrap();
    app.handle_sarif_key(key(KeyCode::Char('o'), KeyModifiers::NONE));
    assert!(
        matches!(
            app.input_prompt.as_ref().map(|p| &p.purpose),
            Some(crate::widgets::input_prompt::InputPurpose::SarifAddLog)
        ),
        "o asks for a log"
    );
    app.submit_sarif_add_log(&second.display().to_string());
    let view = app.editor.sarif.as_ref().unwrap();
    assert_eq!(view.logs.len(), 2);
    assert_eq!(view.entries.len(), 2);
    assert!(
        view.entries
            .iter()
            .any(|e| e.message == "From the second log.")
    );
    // Adding the same log again does nothing.
    app.submit_sarif_add_log(&second.display().to_string());
    assert_eq!(app.editor.sarif.as_ref().unwrap().logs.len(), 2);
    // Logs tab: select the second log's group and close it.
    app.handle_sarif_key(key(KeyCode::Tab, KeyModifiers::NONE));
    app.handle_sarif_key(key(KeyCode::Tab, KeyModifiers::NONE));
    let view = app.editor.sarif.as_mut().unwrap();
    assert_eq!(view.tab, crate::sarif::view::Tab::Logs);
    let rows = view.rows();
    let second_header = rows
        .iter()
        .position(|r| matches!(r, crate::sarif::view::Row::Group { label, .. } if label == "second.sarif"))
        .unwrap();
    view.selected = second_header;
    app.handle_sarif_key(key(KeyCode::Delete, KeyModifiers::NONE));
    let view = app.editor.sarif.as_ref().unwrap();
    assert_eq!(view.logs.len(), 1);
    assert!(
        view.entries
            .iter()
            .all(|e| e.message != "From the second log.")
    );
    // The last log is not closed this way: that is closing the tab.
    app.handle_sarif_key(key(KeyCode::Delete, KeyModifiers::NONE));
    assert_eq!(
        app.editor.sarif.as_ref().unwrap().logs.len(),
        1,
        "{}",
        app.status
    );
}

#[test]
fn a_baseline_log_marks_results_new_or_unchanged_and_lists_what_disappeared() {
    // #577: logs without baselineState are compared against a chosen baseline.
    let tmp = tempfile::tempdir().unwrap();
    let result = |rule: &str, file: &str, line: i64| {
        format!(
            r#"{{"ruleId":"{rule}","message":{{"text":"{rule} here"}},"locations":[{{"physicalLocation":{{"artifactLocation":{{"uri":"{file}"}},"region":{{"startLine":{line}}}}}}}]}}"#
        )
    };
    let write_log = |name: &str, results: Vec<String>| {
        let p = tmp.path().join(name);
        std::fs::write(
            &p,
            format!(
                r#"{{"version":"2.1.0","runs":[{{"tool":{{"driver":{{"name":"T"}}}},"results":[{}]}}]}}"#,
                results.join(",")
            ),
        )
        .unwrap();
        p
    };
    for f in ["a.js", "c.js", "d.js"] {
        std::fs::write(tmp.path().join(f), "one\ntwo\nthree\n").unwrap();
    }
    let now = write_log(
        "now.sarif",
        vec![result("kept", "a.js", 3), result("fresh", "d.js", 2)],
    );
    let before = write_log(
        "before.sarif",
        vec![result("kept", "a.js", 3), result("gone", "c.js", 1)],
    );
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.editor.open(&now).unwrap();
    app.handle_sarif_key(key(KeyCode::Char('b'), KeyModifiers::NONE));
    assert!(
        matches!(
            app.input_prompt.as_ref().map(|p| &p.purpose),
            Some(crate::widgets::input_prompt::InputPurpose::SarifBaseline)
        ),
        "b asks for a baseline"
    );
    app.submit_sarif_baseline(&before.display().to_string());
    let view = app.editor.sarif.as_ref().unwrap();
    assert_eq!(view.baseline, Some(1));
    let state = |rule: &str| {
        view.entries
            .iter()
            .find(|e| e.rule_id == rule)
            .map(|e| e.baseline)
    };
    use crate::sarif::semantics::BaselineState;
    assert_eq!(state("kept"), Some(BaselineState::Unchanged));
    assert_eq!(state("fresh"), Some(BaselineState::New));
    assert_eq!(state("gone"), Some(BaselineState::Absent));
    // The baseline contributes only what disappeared, never a second "kept".
    assert_eq!(view.entries.len(), 3);
    // What disappeared is not a problem in today's code.
    app.sync_sarif_diagnostics();
    let c = tmp.path().join("c.js");
    assert!(
        app.lsp_diagnostics
            .get(&c)
            .is_none_or(|m| m.values().all(Vec::is_empty)),
        "absent results publish no diagnostics"
    );
    assert!(
        app.lsp_diagnostics
            .get(&tmp.path().join("d.js"))
            .is_some_and(|m| m.values().any(|v| !v.is_empty()))
    );
}

#[test]
fn codeql_variant_analysis_repositories_are_set_up_from_the_side_bar_and_palette() {
    // The cache-dir override is process-global; serialize with the
    // other tests that redirect it.
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    // #578: VS Code's Variant Analysis Repositories view: a controller
    // repository, then lists, repositories and owners to run against.
    use crate::codeql_variant::{Item, Selection, VariantConfig};
    use crate::widgets::codeql::{Action, Line, Section};
    use crate::widgets::command_palette::Command;
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = codeql_app(tmp.path());
        app.open_codeql_view();
        app.focus = Pane::Tree;
        let press = |app: &mut App, code: KeyCode| {
            app.handle_key(key(code, KeyModifiers::NONE)).unwrap();
        };
        let type_in = |app: &mut App, text: &str| {
            for c in text.chars() {
                app.handle_key(key(KeyCode::Char(c), KeyModifiers::NONE))
                    .unwrap();
            }
            app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
                .unwrap();
        };
        let saved = || VariantConfig::load(&App::codeql_variant_path()).unwrap();

        // The welcome row asks for the controller and refuses a bad one. The
        // empty section starts folded to its header's "set up".
        assert!(app.codeql.folded(Section::VariantAnalysis));
        app.codeql.toggle(Section::VariantAnalysis);
        app.codeql.select_action(Action::SetUpControllerRepository);
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            app.input_prompt.as_ref().map(|p| &p.purpose),
            Some(&crate::widgets::input_prompt::InputPurpose::CodeqlControllerRepository)
        );
        type_in(&mut app, "not a repo");
        assert!(
            app.status.contains("is not a GitHub repository"),
            "{}",
            app.status
        );
        assert!(!App::codeql_variant_path().exists(), "nothing saved");
        press(&mut app, KeyCode::Enter);
        type_in(&mut app, "https://github.com/me/ctl.git");
        assert_eq!(saved().controller_repo.as_deref(), Some("me/ctl"));
        assert!(app.codeql.lines().contains(&Line::Action(
            Action::SetUpControllerRepository,
            "Controller: me/ctl".into()
        )));

        // `l` adds a list, `a` a repository into the selected list.
        press(&mut app, KeyCode::Char('l'));
        type_in(&mut app, "top");
        assert_eq!(app.codeql.selected_variant_item(), Some(Item::List(0)));
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(
            app.input_prompt.as_ref().unwrap().title,
            "Add Repository to top"
        );
        type_in(&mut app, "github/codeql");
        assert_eq!(saved().lists[0].repos, ["github/codeql"]);
        assert_eq!(
            app.codeql.selected_variant_item(),
            Some(Item::Repo(Some(0), 0))
        );
        press(&mut app, KeyCode::Char('a'));
        type_in(&mut app, "GitHub/CodeQL");
        assert!(app.status.contains("already in list top"), "{}", app.status);

        // `o` adds an owner; the palette adds a single repository.
        press(&mut app, KeyCode::Char('o'));
        type_in(&mut app, "octo-org");
        assert_eq!(saved().owners, ["octo-org"]);
        assert_eq!(app.codeql.selected_variant_item(), Some(Item::Owner(0)));
        app.run_command(Command::CodeqlAddVariantRepo);
        type_in(&mut app, "https://github.com/e/f/tree/main");
        assert_eq!(saved().repos, ["e/f"]);

        // Enter selects what a run targets, marked in the side bar.
        assert_eq!(
            app.codeql.selected_variant_item(),
            Some(Item::Repo(None, 0))
        );
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            saved().selected,
            Some(Selection::Repo {
                nwo: "e/f".into(),
                list: None
            })
        );
        assert!(
            app.codeql
                .lines()
                .contains(&Line::Action(Action::VariantRepo(None, 0), "● e/f".into()))
        );
        press(&mut app, KeyCode::Char('g'));
        assert_eq!(app.status, "Open https://github.com/e/f");

        // It persists for the next session.
        let mut again = codeql_app(tmp.path());
        again.open_codeql_view();
        assert_eq!(again.codeql.variant, saved());

        // F2 renames a list; Space folds it.
        app.codeql.select_variant_item(Item::List(0));
        press(&mut app, KeyCode::F(2));
        assert_eq!(app.input_prompt.as_ref().unwrap().value, "top");
        for _ in 0.."top".len() {
            press(&mut app, KeyCode::Backspace);
        }
        type_in(&mut app, "best");
        assert_eq!(saved().lists[0].name, "best", "{}", app.status);
        press(&mut app, KeyCode::Char(' '));
        assert!(
            !app.codeql
                .lines()
                .iter()
                .any(|l| matches!(l, Line::Action(Action::VariantRepo(Some(_), _), _)))
        );
        assert_eq!(app.codeql.selected_variant_item(), Some(Item::List(0)));

        // Delete removes an owner at once and asks before a list.
        app.codeql.select_variant_item(Item::Owner(0));
        press(&mut app, KeyCode::Delete);
        assert!(saved().owners.is_empty(), "{}", app.status);
        app.codeql.select_variant_item(Item::List(0));
        press(&mut app, KeyCode::Delete);
        assert!(
            app.input_prompt
                .as_ref()
                .unwrap()
                .title
                .contains("and its 1 repository"),
            "{}",
            app.input_prompt.as_ref().unwrap().title
        );
        press(&mut app, KeyCode::Esc);
        assert_eq!(saved().lists.len(), 1, "Esc keeps it");
        press(&mut app, KeyCode::Delete);
        press(&mut app, KeyCode::Enter);
        assert!(saved().lists.is_empty(), "{}", app.status);
        assert!(app.codeql.selected_hit().is_some(), "still on a row");

        // A run needs a query open.
        app.run_command(Command::CodeqlRunVariantAnalysis);
        assert!(app.status.contains("Open a .ql query"), "{}", app.status);

        // The config file opens in a tab.
        app.run_command(Command::CodeqlOpenVariantConfig);
        assert_eq!(
            app.editor.path.as_deref(),
            Some(App::codeql_variant_path().as_path())
        );

        // A corrupt file is reported and never saved over.
        std::fs::write(App::codeql_variant_path(), "{ broken").unwrap();
        app.run_command(Command::CodeqlAddVariantList);
        type_in(&mut app, "x");
        assert!(app.status.contains("unchanged"), "{}", app.status);
        assert_eq!(
            std::fs::read_to_string(App::codeql_variant_path()).unwrap(),
            "{ broken"
        );
        app.open_codeql_view();
        assert!(app.codeql.variant_error);
        assert_eq!(
            Command::from_id("codeql_set_up_controller_repository"),
            Some(Command::CodeqlSetUpController)
        );
    });
}

#[test]
fn codeql_run_variant_analysis_bundles_the_query_and_posts_it_to_the_controller() {
    // #578: VS Code's "CodeQL: Run Variant Analysis". The open query is
    // bundled with its pack (default suite pointed at it) and posted with
    // gh to the controller, against what the side bar selects.
    use crate::codeql_submit::load_submitted;
    use crate::codeql_variant::{Item, VariantConfig};
    use crate::widgets::command_palette::Command;
    use base64::Engine;
    use std::os::unix::fs::PermissionsExt;
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        let pack = ws.join("mypack");
        std::fs::create_dir_all(pack.join("src")).unwrap();
        std::fs::write(
            pack.join("qlpack.yml"),
            "name: me/q\nversion: 0.0.1\ndependencies:\n  codeql/python-all: \"*\"\ndefaultSuite:\n  - queries: .\n",
        )
        .unwrap();
        std::fs::write(pack.join("src/Lib.qll"), "predicate p() { any() }").unwrap();
        let query = pack.join("src/Find.ql");
        std::fs::write(&query, "import python\nselect 1").unwrap();

        // `codeql pack bundle` writes the pack file and its file list as the
        // "archive", so the test can read what was bundled.
        let log = tmp.path().join("log");
        let stub = |name: &str, body: String| {
            let path = tmp.path().join(name);
            std::fs::write(&path, body).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        };
        let codeql = stub(
            "codeql",
            format!(
                r#"#!/bin/sh
echo "codeql $*" >> '{log}'
case "$1 $2" in
  "pack install") exit 0 ;;
  "pack bundle") out="${{3#--output=}}"; dir="$5"
     {{ cat "$dir/qlpack.yml"; (cd "$dir" && find . -type f | sort); }} > "$out" ;;
esac
"#,
                log = log.display()
            ),
        );
        let body = tmp.path().join("body.json");
        let gh = stub(
            "gh",
            format!(
                r#"#!/bin/sh
echo "gh $*" >> '{log}'
case "$2" in
  */variant-analyses/4242)
    # The first read finds the run going, every later one finished.
    if [ -e '{polls}' ]; then
      echo '{{"status": "succeeded", "scanned_repositories": [{{"repository": {{"full_name": "a/b"}}, "analysis_status": "succeeded", "result_count": 3}}, {{"repository": {{"full_name": "c/d"}}, "analysis_status": "succeeded", "result_count": 2}}]}}'
    else
      touch '{polls}'
      echo '{{"status": "in_progress", "scanned_repositories": [{{"repository": {{"full_name": "a/b"}}, "analysis_status": "succeeded", "result_count": 3}}, {{"repository": {{"full_name": "c/d"}}, "analysis_status": "in_progress"}}]}}'
    fi
    exit 0 ;;
esac
while [ $# -gt 0 ]; do [ "$1" = --input ] && cp "$2" '{body}'; shift; done
echo '{{"id": 4242, "actions_workflow_run_id": 99, "skipped_repositories": {{"no_codeql_db_repos": {{"repository_count": 1}}}}}}'
"#,
                log = log.display(),
                body = body.display(),
                polls = tmp.path().join("polled").display()
            ),
        );

        let mut app = codeql_app(&ws);
        app.gh_program = gh;
        app.set_codeql_program(codeql);
        let finish = |app: &mut App| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
            while !app.drain_codeql_variant_submit() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the submission never finished"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        };

        app.editor.open(&query).unwrap();
        app.run_command(Command::CodeqlRunVariantAnalysis);
        assert!(
            app.status.contains("controller repository"),
            "{}",
            app.status
        );

        let mut config = VariantConfig::default();
        config.set_controller("me/ctl").unwrap();
        config.add_list("top").unwrap();
        config.add_repo(Some(0), "a/b").unwrap();
        config.add_repo(Some(0), "c/d").unwrap();
        config.save(&App::codeql_variant_path()).unwrap();
        app.run_command(Command::CodeqlRunVariantAnalysis);
        assert!(
            app.status.contains("Select a repository list"),
            "{}",
            app.status
        );

        config.select(Item::List(0)).unwrap();
        config.save(&App::codeql_variant_path()).unwrap();
        app.run_command(Command::CodeqlRunVariantAnalysis);
        assert!(
            app.status
                .contains("Submitting Find.ql as a variant analysis against 2 repositories"),
            "{}",
            app.status
        );
        finish(&mut app);
        assert_eq!(
            app.status,
            "Variant analysis 4242 of Find.ql submitted, 1 repository skipped: https://github.com/me/ctl/actions/runs/99"
        );

        let asked = std::fs::read_to_string(&log).unwrap();
        assert!(asked.contains("codeql pack install"), "{asked}");
        assert!(
            asked.contains(
                "gh api --method POST repos/me/ctl/code-scanning/codeql/variant-analyses --input"
            ),
            "{asked}"
        );
        let sent: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&body).unwrap()).unwrap();
        assert_eq!(sent["language"], "python", "from the pack's dependency");
        assert_eq!(sent["repositories"], serde_json::json!(["a/b", "c/d"]));
        let bundled = base64::engine::general_purpose::STANDARD
            .decode(sent["query_pack"].as_str().unwrap())
            .unwrap();
        let bundled = String::from_utf8(bundled).unwrap();
        assert!(
            bundled.contains("defaultSuite:\n  - query: \"src/Find.ql\"\n"),
            "{bundled}"
        );
        assert!(
            !bundled.contains("queries: ."),
            "the pack's own suite is replaced: {bundled}"
        );
        assert!(bundled.contains("./src/Lib.qll"), "{bundled}");

        let runs = load_submitted(&App::codeql_variant_runs_path());
        assert_eq!(runs.len(), 1);
        assert_eq!((runs[0].id, runs[0].controller.as_str()), (4242, "me/ctl"));
        assert!(
            !croft_cache_dir()
                .join("codeql/variant")
                .read_dir()
                .is_ok_and(|mut d| d.next().is_some()),
            "the work folder is cleaned up"
        );

        // The run shows in the side bar and is followed until it finishes.
        assert_eq!(app.codeql.variant_runs, ["Find.ql · submitted"]);
        let poll = |app: &mut App| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
            app.poll_codeql_variant_runs();
            assert!(app.codeql_variant_poll.is_some(), "a read started");
            while !app.poll_codeql_variant_runs() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the read never finished"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        };
        poll(&mut app);
        assert_eq!(
            app.codeql.variant_runs,
            ["Find.ql · running · 1/2 repos · 3 results"]
        );
        app.poll_codeql_variant_runs();
        assert!(
            app.codeql_variant_poll.is_none(),
            "not again before the interval"
        );
        app.codeql_variant_polled = None;
        poll(&mut app);
        assert_eq!(
            app.status,
            "Variant analysis 4242 of Find.ql succeeded: 2/2 repositories, 5 results"
        );
        assert_eq!(
            app.codeql.variant_runs,
            ["Find.ql · succeeded · 2/2 repos · 5 results"]
        );
        let reads = |log: &std::path::Path| {
            std::fs::read_to_string(log)
                .unwrap()
                .lines()
                .filter(|l| l.ends_with("variant-analyses/4242"))
                .count()
        };
        assert_eq!(reads(&log), 2);
        app.codeql_variant_polled = None;
        app.poll_codeql_variant_runs();
        assert!(
            app.codeql_variant_poll.is_none(),
            "a finished run is not read"
        );
        assert_eq!(reads(&log), 2);

        // Enter on the run's row opens its report.
        app.open_codeql_view();
        app.codeql
            .select_action(crate::widgets::codeql::Action::VariantRun(0));
        app.focus = Pane::Tree;
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();
        let report = app.editor.lines.join("\n");
        assert!(
            report.starts_with("# Variant analysis 4242 of Find.ql"),
            "{report}"
        );
        assert!(
            report.contains("| [c/d](https://github.com/c/d) | succeeded | 2 |  |"),
            "{report}"
        );

        // An unsaved query is not sent as its last saved version.
        app.editor.open(&query).unwrap();
        app.editor.dirty = true;
        app.run_command(Command::CodeqlRunVariantAnalysis);
        assert!(app.status.contains("Save the query"), "{}", app.status);
    });
}

#[test]
fn codeql_variant_results_are_fetched_once_and_opened_per_repository() {
    // #578: a finished variant analysis's results. Each repository with
    // results has its artifact downloaded (once), its alerts gathered into
    // one SARIF log with a run per repository, its tables into one CSV.
    use crate::codeql_submit::{parse_progress, parse_submission, record_submitted};
    use crate::widgets::command_palette::Command;
    use std::io::{BufRead, Write};
    use std::os::unix::fs::PermissionsExt;
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let zip_of = |name: &str, body: &str| {
            let mut out = std::io::Cursor::new(Vec::new());
            let mut z = zip::ZipWriter::new(&mut out);
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            z.start_file(name, opts).unwrap();
            z.write_all(body.as_bytes()).unwrap();
            z.finish().unwrap();
            out.into_inner()
        };
        let sarif = r#"{"version": "2.1.0", "runs": [{"tool": {"driver": {"name": "CodeQL"}}, "results": [{"ruleId": "r", "message": {"text": "found it"}, "locations": [{"physicalLocation": {"artifactLocation": {"uri": "src/x.py"}, "region": {"startLine": 4}}}]}]}]}"#;
        let files = std::sync::Arc::new(vec![
            ("/ab.zip", zip_of("results/results.sarif", sarif)),
            ("/cd.zip", zip_of("results.bqrs", "binary")),
        ]);
        let served = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        {
            let (files, served) = (files.clone(), served.clone());
            std::thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
                    loop {
                        let mut h = String::new();
                        if reader.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
                            break;
                        }
                    }
                    served.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let body = files
                        .iter()
                        .find(|(p, _)| *p == path)
                        .map(|(_, b)| b.clone());
                    let mut stream = stream;
                    match body {
                        Some(b) => {
                            let _ = write!(
                                stream,
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                b.len()
                            );
                            let _ = stream.write_all(&b);
                        }
                        None => {
                            let _ = write!(
                                stream,
                                "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            );
                        }
                    }
                }
            });
        }
        let log = tmp.path().join("log");
        let stub = |name: &str, body: String| {
            let path = tmp.path().join(name);
            std::fs::write(&path, body).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        };
        let gh = stub(
            "gh",
            format!(
                r#"#!/bin/sh
echo "gh $*" >> '{log}'
case "$2" in
  */repos/a/b) echo '{{"analysis_status": "succeeded", "artifact_url": "http://127.0.0.1:{port}/ab.zip", "database_commit_sha": "abc"}}' ;;
  */repos/c/d) echo '{{"analysis_status": "succeeded", "artifact_url": "http://127.0.0.1:{port}/cd.zip"}}' ;;
  create) echo "- Creating gist"; echo "https://gist.github.com/me/0123" ;;
  *) echo 'HTTP 404: Not Found' >&2; exit 1 ;;
esac
"#,
                log = log.display()
            ),
        );
        let codeql = stub(
            "codeql",
            format!(
                r#"#!/bin/sh
echo "codeql $*" >> '{log}'
out="${{4#--output=}}"; printf '"name","count"\n"x","1"\n' > "$out"
"#,
                log = log.display()
            ),
        );

        let mut run = parse_submission(
            r#"{"id": 9}"#,
            "me/ctl",
            std::path::Path::new("/w/Find.ql"),
            "python",
            0,
        )
        .unwrap();
        run.progress = parse_progress(
            r#"{"status": "succeeded", "scanned_repositories": [
                {"repository": {"full_name": "a/b"}, "analysis_status": "succeeded", "result_count": 1},
                {"repository": {"full_name": "c/d"}, "analysis_status": "succeeded", "result_count": 1},
                {"repository": {"full_name": "e/f"}, "analysis_status": "succeeded", "result_count": 4},
                {"repository": {"full_name": "g/h"}, "analysis_status": "succeeded", "result_count": 0}]}"#,
        )
        .ok();
        record_submitted(&App::codeql_variant_runs_path(), run).unwrap();

        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.gh_program = gh;
        app.set_codeql_program(codeql);
        let finish = |app: &mut App| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
            while !app.drain_codeql_variant_results() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the fetch never finished"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        };

        app.run_command(Command::CodeqlOpenVariantResults);
        assert!(
            app.status
                .starts_with("Fetching the results of 3 repositories"),
            "{}",
            app.status
        );
        finish(&mut app);
        let dir = croft_cache_dir().join("codeql/variant-results/me-ctl-9");
        assert!(
            app.status
                .starts_with("Results of 2 repositories; tables in "),
            "{}",
            app.status
        );
        assert!(
            app.status.ends_with("; e/f failed: HTTP 404: Not Found"),
            "{}",
            app.status
        );
        assert_eq!(
            app.editor.path.as_deref(),
            Some(dir.join("results.sarif").as_path())
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("results.csv")).unwrap(),
            "\"repository\",\"name\",\"count\"\n\"c/d\",\"x\",\"1\"\n"
        );
        assert_eq!(served.load(std::sync::atomic::Ordering::SeqCst), 2);

        // A location in another repository opens on GitHub at its commit.
        let view = app.editor.sarif.as_mut().expect("the SARIF viewer is open");
        assert_eq!(view.entries.len(), 1);
        view.selected = view
            .rows()
            .iter()
            .position(|r| matches!(r, crate::sarif::view::Row::Item { .. }))
            .unwrap();
        app.open_sarif_loc(&crate::sarif::details::LocRef {
            label: "src/x.py".into(),
            uri: "src/x.py".into(),
            line: 4,
            column: 1,
            ..Default::default()
        });
        assert_eq!(
            app.status,
            "Open https://github.com/a/b/blob/abc/src/x.py#L4"
        );

        // Opening again uses the downloaded copies.
        let gh_calls = std::fs::read_to_string(&log)
            .unwrap()
            .matches("gh api")
            .count();
        app.run_command(Command::CodeqlOpenVariantResults);
        finish(&mut app);
        assert_eq!(
            served.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "nothing downloaded twice"
        );
        let again = std::fs::read_to_string(&log)
            .unwrap()
            .matches("gh api")
            .count();
        assert_eq!(
            again,
            gh_calls + 1,
            "only the failed repository is asked again"
        );

        // Export: Markdown into a folder, offered in the workspace.
        app.run_command(Command::CodeqlExportVariantResults);
        let offered = tmp.path().join("codeql-variant-analysis-9");
        assert_eq!(
            app.input_prompt.as_ref().map(|p| p.value.clone()),
            Some(offered.display().to_string())
        );
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();
        let export = |app: &mut App| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
            while !app.drain_codeql_variant_export() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the export never finished"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        };
        export(&mut app);
        assert!(
            app.status
                .starts_with(&format!("Exported 2 repositories to {}", offered.display())),
            "{}",
            app.status
        );
        assert_eq!(
            app.editor.path.as_deref(),
            Some(offered.join("_summary.md").as_path())
        );
        let summary = std::fs::read_to_string(offered.join("_summary.md")).unwrap();
        assert!(summary.contains("| [a/b](a-b.md) | 1 |"), "{summary}");
        assert!(summary.contains("| [c/d](c-d.md) | 1 |"), "{summary}");
        let ab = std::fs::read_to_string(offered.join("a-b.md")).unwrap();
        assert!(
            ab.contains("- found it ([src/x.py:4](https://github.com/a/b/blob/abc/src/x.py#L4))"),
            "{ab}"
        );
        assert!(
            std::fs::read_to_string(offered.join("c-d.md"))
                .unwrap()
                .contains("| x | 1 |")
        );
        assert_eq!(
            served.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "export reuses the downloads"
        );

        // Or a secret gist, `_summary.md` first.
        app.run_command(Command::CodeqlExportVariantResults);
        if let Some(p) = app.input_prompt.as_mut() {
            p.value = String::from("gist");
        }
        let _ = std::fs::write(&log, "");
        app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();
        export(&mut app);
        assert!(
            app.status
                .starts_with("Exported 2 repositories to https://gist.github.com/me/0123"),
            "{}",
            app.status
        );
        let asked = std::fs::read_to_string(&log).unwrap();
        let gist = asked
            .lines()
            .find(|l| l.starts_with("gh gist create"))
            .unwrap_or_default();
        assert!(
            gist.contains("--desc Find.ql (variant analysis 9 of 2 repositories)"),
            "{gist}"
        );
        let first_file = gist
            .split_whitespace()
            .find(|w| w.ends_with(".md"))
            .unwrap_or_default();
        assert!(first_file.ends_with("/_summary.md"), "{gist}");
    });
}

#[test]
fn codeql_code_search_adds_the_repositories_it_finds_to_the_selected_list() {
    // #578: VS Code's "Add repositories with GitHub Code Search". The search
    // runs off the UI thread, page by page, scoped to the side bar's
    // language, and its distinct repositories join the selected list.
    use crate::codeql_variant::{Item, VariantConfig};
    use crate::widgets::command_palette::Command;
    use std::os::unix::fs::PermissionsExt;
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("gh-args");
        let gh = tmp.path().join("gh");
        // Page 1 is full (100 results over 60 repositories, one already
        // listed in another case); page 2 is short, so page 3 is never
        // asked for. A query with "broken" in it fails; one with "cut"
        // fills page 1 and then hits the rate limit.
        std::fs::write(
            &gh,
            format!(
                r#"#!/bin/sh
echo "$@" >> '{log}'
case "$*" in
  *broken*) echo 'gh: Validation Failed (HTTP 422)' >&2; exit 1 ;;
  *cut*" page=1 "*) i=0; while [ $i -lt 100 ]; do echo "c/r$i"; i=$((i+1)); done ;;
  *cut*) echo 'API rate limit exceeded' >&2; exit 1 ;;
  *" page=1 "*) i=0; while [ $i -lt 100 ]; do echo "o/r$((i % 60))"; i=$((i+1)); done; echo "A/B" ;;
  *" page=2 "*) echo o/extra ;;
  *) echo "asked for a page past the short one" >&2; exit 1 ;;
esac
"#,
                log = log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut app = codeql_app(tmp.path());
        app.gh_program = gh.clone();
        app.open_codeql_view();
        app.focus = Pane::Tree;
        let press = |app: &mut App, code: KeyCode| {
            app.handle_key(key(code, KeyModifiers::NONE)).unwrap();
        };
        let type_in = |app: &mut App, text: &str| {
            for c in text.chars() {
                app.handle_key(key(KeyCode::Char(c), KeyModifiers::NONE))
                    .unwrap();
            }
            app.handle_key(key(KeyCode::Enter, KeyModifiers::NONE))
                .unwrap();
        };
        let finish = |app: &mut App| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
            while !app.drain_codeql_code_search() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the search never finished"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        };
        let saved = || VariantConfig::load(&App::codeql_variant_path()).unwrap();

        // With no list selected there is nowhere to put the results.
        app.run_command(Command::CodeqlVariantCodeSearch);
        assert!(app.input_prompt.is_none());
        assert!(
            app.status.contains("Select a repository list"),
            "{}",
            app.status
        );

        // The empty section starts folded; unfold it to reach its row.
        app.codeql
            .toggle(crate::widgets::codeql::Section::VariantAnalysis);
        app.codeql
            .select_action(crate::widgets::codeql::Action::SetUpControllerRepository);
        press(&mut app, KeyCode::Char('l'));
        type_in(&mut app, "found");
        press(&mut app, KeyCode::Char('a'));
        type_in(&mut app, "a/b");
        // The language first: it filters rows, which moves the selection.
        app.codeql.language = crate::widgets::codeql::LANGUAGE_IDS
            .iter()
            .position(|l| *l == "python");
        app.codeql.select_variant_item(Item::List(0));

        press(&mut app, KeyCode::Char('s'));
        assert_eq!(
            app.input_prompt.as_ref().unwrap().title,
            "Add Repositories to found with GitHub Code Search"
        );
        type_in(&mut app, "import torch");
        assert!(
            app.status.starts_with("Searching GitHub code"),
            "{}",
            app.status
        );
        finish(&mut app);

        let repos = saved().lists[0].repos.clone();
        assert_eq!(repos.len(), 1 + 60 + 1, "{repos:?}");
        assert_eq!(repos[..2], ["a/b", "o/r0"]);
        assert_eq!(repos.last().map(String::as_str), Some("o/extra"));
        assert_eq!(
            app.status, "Added 61 repositories to found (1 already there)",
            "{}",
            app.status
        );
        let asked = std::fs::read_to_string(&log).unwrap();
        assert_eq!(asked.lines().count(), 2, "stops at the short page: {asked}");
        assert!(asked.contains("q=import torch language:python"), "{asked}");

        // A failed search says why and changes nothing.
        app.codeql.select_variant_item(Item::List(0));
        app.run_command(Command::CodeqlVariantCodeSearch);
        type_in(&mut app, "broken");
        finish(&mut app);
        assert!(
            app.status.contains("GitHub Code Search failed"),
            "{}",
            app.status
        );
        assert!(app.status.contains("Validation Failed"), "{}", app.status);
        assert_eq!(saved().lists[0].repos, repos);

        // A later page failing keeps the earlier pages and says so.
        app.codeql.select_variant_item(Item::List(0));
        app.run_command(Command::CodeqlVariantCodeSearch);
        type_in(&mut app, "cut");
        finish(&mut app);
        assert_eq!(saved().lists[0].repos.len(), repos.len() + 100);
        assert!(
            app.status.starts_with("Added 100 repositories to found; the search stopped early, page 2 failed: API rate limit exceeded"),
            "{}",
            app.status
        );
    });
}

/// #840 regression, through base APIs only: clicking the QL icon walked the
/// whole workspace for queries on the UI thread before the view switched,
/// so the list was already filled when `open_codeql_view` returned. The
/// view now switches at once and the walk's packs land on a later tick.
#[test]
fn opening_the_codeql_view_returns_before_the_query_walk_lands() {
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let pack = tmp.path().join("pack");
        std::fs::create_dir_all(&pack).unwrap();
        std::fs::write(pack.join("qlpack.yml"), "name: acme/go\nextractor: go\n").unwrap();
        std::fs::write(pack.join("a.ql"), "select 1").unwrap();
        let mut app = codeql_app(tmp.path());
        app.open_codeql_view();
        assert_eq!(app.sidebar_view, SidebarView::CodeQL);
        let names: Vec<&str> = app.codeql.queries.iter().map(|p| p.name.as_str()).collect();
        assert!(
            names.is_empty(),
            "the walk ran on the UI thread before the view opened: {names:?}"
        );
    });
}

/// #840 negative: reopening the view while a walk is in flight keeps that
/// walk rather than starting a second over the same tree: what it sends is
/// what lands.
#[test]
fn reopening_the_codeql_view_keeps_the_walk_in_flight() {
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        app.codeql_queries_job = Some((app.workspace_root().to_path_buf(), rx));
        app.open_codeql_view();
        let pack = crate::codeql_query::QueryPack {
            name: String::from("acme/in-flight"),
            language: None,
            dir: tmp.path().to_path_buf(),
            queries: Vec::new(),
        };
        tx.send(vec![pack]).unwrap();
        assert!(app.drain_codeql_queries());
        let names: Vec<&str> = app.codeql.queries.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["acme/in-flight"]);
    });
}

/// #840 negative: a walk whose thread died without sending stops the
/// "Discovering queries" line and leaves the list alone; with no walk in
/// flight a drain changes nothing.
#[test]
fn a_query_walk_that_dies_stops_saying_it_is_discovering() {
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        assert!(
            !app.drain_codeql_queries(),
            "nothing in flight, nothing changes"
        );
        let (tx, rx) = std::sync::mpsc::channel::<Vec<crate::codeql_query::QueryPack>>();
        drop(tx);
        app.codeql_queries_job = Some((app.workspace_root().to_path_buf(), rx));
        app.codeql.discovering_queries = true;
        assert!(app.drain_codeql_queries());
        assert!(!app.codeql.discovering_queries);
        assert!(app.codeql_queries_job.is_none());
        assert!(app.codeql.queries.is_empty());
    });
}

/// A workspace folder holding one CodeQL pack named `name`.
fn codeql_pack_workspace_840(name: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let pack = tmp.path().join("pack");
    std::fs::create_dir_all(&pack).unwrap();
    std::fs::write(
        pack.join("qlpack.yml"),
        format!("name: {name}\nextractor: go\n"),
    )
    .unwrap();
    std::fs::write(pack.join("a.ql"), "select 1").unwrap();
    tmp
}

/// #840: the walk runs off the UI thread, so the workspace can move to
/// another root before it lands. Its packs are the old root's: they must
/// not be listed for the new one, and the view walks the new root.
#[test]
fn a_query_walk_of_a_root_the_workspace_left_never_lands() {
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let a = codeql_pack_workspace_840("acme/a");
        let b = codeql_pack_workspace_840("acme/b");
        let mut app = codeql_app(a.path());
        app.open_codeql_view();
        assert!(
            app.codeql_queries_job.is_some(),
            "fixture: a's walk in flight"
        );
        app.change_workspace_root(b.path().to_path_buf());
        wait_for_codeql_queries(&mut app);
        let names: Vec<&str> = app.codeql.queries.iter().map(|p| p.name.as_str()).collect();
        assert!(
            !names.contains(&"acme/a"),
            "a's queries landed on b: {names:?}"
        );
        app.open_codeql_view();
        wait_for_codeql_queries(&mut app);
        let names: Vec<&str> = app.codeql.queries.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["acme/b"]);
    });
}

/// #840: reopening the view after a workspace switch, with the old root's
/// walk still in flight, walks the new root instead of waiting on the old.
#[test]
fn reopening_codeql_after_a_workspace_switch_walks_the_new_root() {
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let a = codeql_pack_workspace_840("acme/a");
        let b = codeql_pack_workspace_840("acme/b");
        let mut app = codeql_app(a.path());
        app.open_codeql_view();
        app.change_workspace_root(b.path().to_path_buf());
        // Undrained, a's walk is still in flight here, landed or not.
        app.open_codeql_view();
        wait_for_codeql_queries(&mut app);
        let names: Vec<&str> = app.codeql.queries.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["acme/b"]);
    });
}

/// #840 negative: a walk of the root the workspace still has lands as
/// before, and one the test holds for that root keeps its place when the
/// view is reopened (no second walk of the same tree).
#[test]
fn a_query_walk_of_the_current_root_still_lands() {
    let _guard = relay_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    with_relay_home(home.path(), || {
        let a = codeql_pack_workspace_840("acme/a");
        let mut app = codeql_app(a.path());
        app.open_codeql_view();
        app.open_codeql_view();
        wait_for_codeql_queries(&mut app);
        let names: Vec<&str> = app.codeql.queries.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["acme/a"]);
        assert!(!app.codeql.discovering_queries);
    });
}

/// A stand-in `codeql` still open for writing, the way a test's freshly
/// written script is while another test thread forks: exec fails with
/// "Text file busy" until the writer lets go. The handle is dropped after
/// `hold`.
#[cfg(unix)]
fn busy_codeql(dir: &std::path::Path, hold: std::time::Duration) -> std::path::PathBuf {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt;
    let bin = dir.join("codeql");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o755)
        .open(&bin)
        .unwrap();
    f.write_all(b"#!/bin/sh\necho ran\n").unwrap();
    f.flush().unwrap();
    std::thread::spawn(move || {
        std::thread::sleep(hold);
        drop(f);
    });
    bin
}

#[cfg(unix)]
#[test]
fn a_codeql_briefly_busy_after_writing_still_runs() {
    // CI failed "Compare Performance" with "could not run codeql: Text file
    // busy (os error 26)" when another test forked while the stand-in was
    // open for writing; a CLI croft has just downloaded can be busy the same
    // way. The run waits out the moment instead of failing.
    let dir = tempfile::tempdir().unwrap();
    let bin = busy_codeql(dir.path(), std::time::Duration::from_millis(100));
    assert_eq!(App::codeql_stdout(&bin, &[]), Ok(String::from("ran\n")));
}

#[cfg(unix)]
#[test]
fn a_briefly_busy_codeql_command_and_version_still_run() {
    let dir = tempfile::tempdir().unwrap();
    let bin = busy_codeql(dir.path(), std::time::Duration::from_millis(100));
    assert_eq!(App::codeql_command(&bin, &[]), Ok(()));
    let dir = tempfile::tempdir().unwrap();
    let bin = busy_codeql(dir.path(), std::time::Duration::from_millis(100));
    assert_eq!(App::read_codeql_version(&bin), Ok(String::from("ran")));
}

#[cfg(unix)]
#[test]
fn a_codeql_that_stays_busy_still_fails_with_the_reason() {
    let dir = tempfile::tempdir().unwrap();
    let bin = busy_codeql(dir.path(), std::time::Duration::from_secs(3));
    let started = std::time::Instant::now();
    let err = App::codeql_stdout(&bin, &[]).unwrap_err();
    assert!(
        err.starts_with("could not run codeql: Text file busy"),
        "{err}"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "the wait is bounded, took {:?}",
        started.elapsed()
    );
}

#[test]
fn a_missing_codeql_fails_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let started = std::time::Instant::now();
    let err = App::codeql_stdout(&dir.path().join("codeql"), &[]).unwrap_err();
    assert!(
        err.starts_with("could not run codeql: No such file"),
        "{err}"
    );
    assert!(started.elapsed() < std::time::Duration::from_millis(200));
}
