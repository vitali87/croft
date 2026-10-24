//! Extensions: enabling, consent, uninstalling and recommendations.

use super::*;

/// One consent per extension, read from one place (#485 review): the sidecar
/// gate consults the same session set the viewer gate does, so allowing an
/// extension through either gate satisfies the other in the same session,
/// whatever became of the prefs write. And a prefs write that fails is
/// reported before the command's own argument prompt takes the screen.
#[test]
fn the_sidecar_gate_reads_the_session_consent_set() {
    use crate::widgets::input_prompt::InputPurpose;
    const EXT: &str = r#"
id = "tconsent"
name = "tconsent"
api_version = 1
[[mcp_servers]]
id = "srv"
command = "/bin/false"
[[commands]]
id = "tconsent.go"
title = "tconsent: go"
server = "srv"
tool = "go"
arg = "q"
prompt = "Query"
"#;
    let (_scratch, croft) = scratch_config(&[("tconsent", EXT)]);
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.config_dir = croft.clone();
    app.consented_extensions = consent_on_disk(&croft);
    // Allowed this session, not (yet) on disk: the gate must not re-prompt,
    // it must move on to the command's own argument prompt.
    app.consented_extensions.insert("tconsent".into());
    app.run_extension_command("tconsent.go");
    assert!(
        matches!(
            app.input_prompt.as_ref().map(|p| &p.purpose),
            Some(InputPurpose::McpArg { .. })
        ),
        "a consented extension goes straight to its argument prompt, not consent: {:?}",
        app.input_prompt.as_ref().map(|p| p.title.clone())
    );
    // Allowing through the sidecar prompt with the prefs file unwritable: the
    // failure is reported where the user can see it and the grant holds.
    app.close_input_prompt();
    app.consented_extensions.remove("tconsent");
    std::fs::create_dir_all(croft.join("config.json")).unwrap();
    app.run_extension_command("tconsent.go");
    assert!(
        matches!(
            app.input_prompt.as_ref().map(|p| &p.purpose),
            Some(InputPurpose::McpConsent { .. })
        ),
        "fixture: an unconsented extension is asked first"
    );
    app.submit_input_prompt();
    assert!(
        app.status.contains("could not be saved"),
        "a failed prefs write is reported: {:?}",
        app.status
    );
    assert!(
        matches!(
            app.input_prompt.as_ref().map(|p| &p.purpose),
            Some(InputPurpose::McpArg { .. })
        ),
        "and the command went on to its argument prompt"
    );
    assert!(
        app.consented_extensions.contains("tconsent"),
        "the session grant holds"
    );
}

/// Uninstalling an extension forgets its consent, in the session and on
/// disk: a later re-add is a fresh install of a program the user has not
/// approved this time, so the first-run gate asks again. The disabled state
/// it clears lands in the same config file.
#[test]
fn uninstalling_an_extension_forgets_its_consent() {
    let (_scratch, croft) = scratch_config(&[]);
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.config_dir = croft.clone();
    app.consented_extensions = consent_on_disk(&croft);
    crate::prefs::save_mcp_consent_in(&croft, "csvlens").unwrap();
    app.consented_extensions.insert("csvlens".into());
    app.disabled_extensions.insert("csvlens".into());
    crate::prefs::save_extension_enabled_in(&croft, "csvlens", false).unwrap();
    app.perform_extension_uninstall("csvlens");
    let disabled_on_disk = crate::prefs::Prefs::load(&croft.join("config.json"))
        .unwrap_or_default()
        .disabled_extensions;
    assert!(
        !disabled_on_disk.contains("csvlens"),
        "the disabled state is cleared in the app's own config file: {disabled_on_disk:?}"
    );
    assert!(
        app.status.starts_with("Uninstalled"),
        "fixture: the catalog entry uninstalls cleanly: {:?}",
        app.status
    );
    assert!(
        !app.consented_extensions.contains("csvlens"),
        "the session grant is gone"
    );
    let saved = consent_on_disk(&croft);
    assert!(
        !saved.contains("csvlens"),
        "and so is the one on disk: {saved:?}"
    );
}

#[test]
fn an_edit_between_refusal_and_consent_forces_a_fresh_prompt() {
    // The first Cmd+S arms the second — but consent named the characters the
    // buffer held at prompt time. Typing in between changes what the write
    // would destroy, so the next Cmd+S must refuse and re-prompt (now naming
    // the new character too), and only the press after that writes.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "a.txt", "plain");
    app.editor
        .reopen_with_encoding(encoding_rs::WINDOWS_1252)
        .unwrap();
    app.editor.lines = vec![String::from("日")];
    app.editor.dirty = true;
    app.save();
    assert!(
        app.editor.lossy_save_armed,
        "the refusal arms the second press"
    );
    app.editor.insert_char('語');
    app.save();
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("a.txt")).unwrap(),
        "plain",
        "the edit revoked the consent, so this press must refuse again"
    );
    assert!(
        app.status.contains('語'),
        "the fresh prompt must name the newly typed character; status: {:?}",
        app.status
    );
    app.save();
    assert_eq!(
        std::fs::read(tmp.path().join("a.txt")).unwrap(),
        b"&#35486;&#26085;",
        "consent given after the prompt that named both characters writes"
    );
}

/// #352: a workspace's `.vscode/extensions.json` recommendations appear in the
/// Extensions panel under FROM VS CODE with what croft has for each.
#[test]
fn workspace_extension_recommendations_show_under_from_vscode() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join(".vscode")).unwrap();
    std::fs::write(
        tmp.path().join(".vscode").join("extensions.json"),
        r#"{ "recommendations": ["rust-lang.rust-analyzer", "nobody.nothing"] }"#,
    )
    .unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.refresh_extensions();
    let ids: Vec<String> = app
        .extensions
        .items()
        .iter()
        .filter(|i| i.vscode)
        .map(|i| i.id.clone())
        .collect();
    assert_eq!(ids, ["rust-lang.rust-analyzer", "nobody.nothing"]);

    // Toggling a built-in row is an answer, not an install: the status names
    // the extension and what croft has for it, and nothing is disabled.
    let visible = app.extensions.visible_indices();
    let pos = visible
        .iter()
        .position(|&i| app.extensions.items()[i].id == "rust-lang.rust-analyzer")
        .expect("the row is visible");
    app.extensions.select(pos);
    let before = app.disabled_extensions.clone();
    app.toggle_selected_extension();
    assert!(
        app.status
            .starts_with("rust-lang.rust-analyzer: built in: lsp-rust"),
        "the status restates the row: {}",
        app.status
    );
    assert_eq!(app.disabled_extensions, before, "nothing was toggled");
}

/// #624 review: toggling an extension off and on re-approves its tools, as
/// the refusal says: turning it off forgets the fingerprints of the
/// commands it declares, and only those, so the next run trusts afresh.
#[test]
fn toggling_an_extension_off_forgets_its_tool_fingerprints() {
    const EXT: &str = r#"
id = "tfp"
name = "tfp"
api_version = 1
[[mcp_servers]]
id = "srv"
command = "/bin/false"
[[commands]]
id = "tfp.go"
title = "tfp: go"
server = "srv"
tool = "go"
"#;
    let other = EXT.replace("tfp", "oth");
    let (_scratch, croft) = scratch_config(&[("tfp", EXT), ("oth", &other)]);
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.config_dir = croft.clone();
    // The panel lists the real user dir; seat the scratch extension's row.
    // A toggle refreshes the panel from that dir, so re-seat before each.
    let toggle_tfp = |app: &mut App| {
        app.extensions
            .set_items(crate::widgets::extensions::items_from_summaries(
                crate::lsp::manifest::summaries(&[EXT]),
                &app.disabled_extensions,
            ));
        let visible = app.extensions.visible_indices();
        let pos = visible
            .iter()
            .position(|&i| app.extensions.items()[i].id == "tfp")
            .expect("the tfp row is visible");
        app.extensions.select(pos);
        app.toggle_selected_extension();
    };
    assert!(crate::prefs::trust_mcp_tool_in(&croft, "tfp.go", "fp1"));
    assert!(crate::prefs::trust_mcp_tool_in(&croft, "oth.go", "o1"));
    toggle_tfp(&mut app);
    assert!(app.disabled_extensions.contains("tfp"), "{}", app.status);
    assert!(
        crate::prefs::trust_mcp_tool_in(&croft, "tfp.go", "fp2"),
        "the changed tool is approved afresh"
    );
    assert!(
        !crate::prefs::trust_mcp_tool_in(&croft, "oth.go", "o2"),
        "another extension's record stays"
    );
    // Turning it back on keeps the new record: a further change is refused.
    toggle_tfp(&mut app);
    assert!(!app.disabled_extensions.contains("tfp"), "{}", app.status);
    assert!(!crate::prefs::trust_mcp_tool_in(&croft, "tfp.go", "fp3"));
}

/// Uninstalling an extension forgets its tools' fingerprints, so a re-added
/// newer version approves its tools afresh instead of being refused.
#[test]
fn uninstalling_an_extension_forgets_its_tool_fingerprints() {
    // Only a catalog entry uninstalls, so the fixture is the bundled Time
    // sidecar beside a hand-added extension whose record must stay.
    const TIME: &str = include_str!("../../../assets/catalog/mcp-time/extension.toml");
    const OTH: &str = r#"
id = "oth"
name = "oth"
api_version = 1
[[mcp_servers]]
id = "srv"
command = "/bin/false"
[[commands]]
id = "oth.go"
title = "oth: go"
server = "srv"
tool = "go"
"#;
    let (_scratch, croft) = scratch_config(&[("mcp-time", TIME), ("oth", OTH)]);
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(tmp.path().to_path_buf()).unwrap();
    app.config_dir = croft.clone();
    assert!(crate::prefs::trust_mcp_tool_in(&croft, "time.now", "fp1"));
    assert!(crate::prefs::trust_mcp_tool_in(&croft, "oth.go", "o1"));
    app.perform_extension_uninstall("mcp-time");
    assert!(app.status.starts_with("Uninstalled"), "{}", app.status);
    assert!(
        crate::prefs::trust_mcp_tool_in(&croft, "time.now", "fp2"),
        "the re-added tool is approved afresh"
    );
    assert!(
        !crate::prefs::trust_mcp_tool_in(&croft, "oth.go", "o2"),
        "another extension's record stays"
    );
}
