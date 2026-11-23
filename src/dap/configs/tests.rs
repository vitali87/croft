//! The launch configurations' own tests, moved out of configs.rs (#2062).

use super::*;

fn ctx() -> SubstCtx {
    SubstCtx {
        workspace_folder: PathBuf::from("/work/proj"),
        file: Some(PathBuf::from("/work/proj/src/main.py")),
        inputs: Default::default(),
        folders: Vec::new(),
    }
}

fn config(json_text: &str) -> DebugConfig {
    parse_launch_json(json_text, ".vscode/launch.json")
        .into_iter()
        .next()
        .expect("one config")
}

#[test]
fn parses_vscode_shape_bare_arrays_and_jsonc() {
    let vscode = r#"{
  // launch.json allows comments
  "version": "0.2.0",
  "configurations": [
    { "name": "Run API", "type": "python", "request": "launch", "program": "api.py", },
  ],
}"#;
    let cfgs = parse_launch_json(vscode, ".vscode/launch.json");
    assert_eq!(cfgs.len(), 1);
    assert_eq!(cfgs[0].name, "Run API");
    assert_eq!(cfgs[0].request, RequestKind::Launch);

    let bare = r#"[ { "name": "A", "type": "lldb", "request": "attach", "pid": 1 } ]"#;
    let cfgs = parse_launch_json(bare, ".croft/launch.json");
    assert_eq!(cfgs.len(), 1);
    assert_eq!(cfgs[0].request, RequestKind::Attach);
}

#[test]
fn entries_without_name_or_type_are_skipped_and_request_defaults_to_launch() {
    let text = r#"{ "configurations": [
            { "type": "python", "program": "a.py" },
            { "name": "no type" },
            { "name": "ok", "type": "node", "program": "s.js" }
        ]}"#;
    let cfgs = parse_launch_json(text, ".vscode/launch.json");
    assert_eq!(cfgs.len(), 1);
    assert_eq!(cfgs[0].name, "ok");
    assert_eq!(cfgs[0].request, RequestKind::Launch);
}

#[test]
fn croft_file_wins_duplicate_names_and_both_files_are_read() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".croft")).unwrap();
    std::fs::create_dir_all(tmp.path().join(".vscode")).unwrap();
    std::fs::write(
        tmp.path().join(".croft/launch.json"),
        r#"[ { "name": "Serve", "type": "python", "program": "croft.py" } ]"#,
    )
    .unwrap();
    std::fs::write(
        tmp.path().join(".vscode/launch.json"),
        r#"{ "configurations": [
                { "name": "Serve", "type": "python", "program": "vscode.py" },
                { "name": "Worker", "type": "node", "program": "w.js" }
            ]}"#,
    )
    .unwrap();
    let cfgs = discover_configs(tmp.path());
    assert_eq!(cfgs.len(), 2);
    assert_eq!(cfgs[0].name, "Serve");
    assert_eq!(cfgs[0].source, ".croft/launch.json");
    assert_eq!(cfgs[0].raw["program"], "croft.py");
    assert_eq!(cfgs[1].name, "Worker");
}

/// #264: a `type: "go"` config defaults delve's `mode` the way delve
/// does, and an explicit `mode` (e.g. `test`) passes through.
#[test]
fn delve_request_defaults_mode_and_passes_an_explicit_one_through() {
    let launch = resolve(
        &config(r#"[{ "name": "G", "type": "go", "request": "launch", "program": "./cmd/app" }]"#),
        &ctx(),
    )
    .unwrap();
    assert_eq!(launch.kind, AdapterKind::Delve);
    let req = delve_request(&launch);
    assert_eq!(req["command"], "launch");
    assert_eq!(req["arguments"]["mode"], "debug");
    assert_eq!(req["arguments"]["program"], "./cmd/app");

    let test = resolve(
        &config(r#"[{ "name": "T", "type": "go", "request": "launch", "mode": "test", "program": "." }]"#),
        &ctx(),
    )
    .unwrap();
    assert_eq!(delve_request(&test)["arguments"]["mode"], "test");

    let attach = resolve(
        &config(r#"[{ "name": "A", "type": "go", "request": "attach", "processId": 77 }]"#),
        &ctx(),
    )
    .unwrap();
    let req = delve_request(&attach);
    assert_eq!(req["command"], "attach");
    assert_eq!(req["arguments"]["mode"], "local");
    assert_eq!(req["arguments"]["processId"], 77);
    assert!(req["arguments"].get("stopOnEntry").is_none());
}

/// #1395: the Go extension's default "Launch Package" config says
/// `"mode": "auto"`, which delve rejects. It resolves as the extension
/// resolves it: `test` for a `_test.go` program (run on its package
/// folder) or a folder program while a `_test.go` file is active,
/// `debug` otherwise. An explicit delve mode passes through.
#[test]
fn a_go_auto_mode_resolves_to_test_or_debug_as_the_go_extension_does() {
    let request = |cfg: &str, active: &str| {
        let ctx = SubstCtx {
            file: Some(PathBuf::from(active)),
            ..ctx()
        };
        delve_request(&resolve(&config(cfg), &ctx).unwrap())
    };
    let launch = |mode: &str, program: &str| {
        format!(
            r#"[{{ "name": "L", "type": "go", "request": "launch",
                       "mode": "{mode}", "program": "{program}" }}]"#
        )
    };
    let req = request(&launch("auto", "/w/cmd/app"), "/w/cmd/app/main.go");
    assert_eq!(req["arguments"]["mode"], "debug");
    assert_eq!(req["arguments"]["program"], "/w/cmd/app");

    let req = request(&launch("auto", "/w/pkg/foo_test.go"), "/w/cmd/app/main.go");
    assert_eq!(req["arguments"]["mode"], "test");
    assert_eq!(
        req["arguments"]["program"], "/w/pkg",
        "delve tests the package"
    );

    // The repro's `${fileDirname}`: the active file decides.
    let req = request(&launch("auto", "${fileDirname}"), "/w/pkg/foo_test.go");
    assert_eq!(req["arguments"]["mode"], "test");
    assert_eq!(req["arguments"]["program"], "/w/pkg");
    let req = request(&launch("auto", "${fileDirname}"), "/w/pkg/foo.go");
    assert_eq!(req["arguments"]["mode"], "debug");
    assert_eq!(req["arguments"]["program"], "/w/pkg");

    let req = request(&launch("exec", "/w/bin/app"), "/w/pkg/foo_test.go");
    assert_eq!(req["arguments"]["mode"], "exec");
    assert_eq!(req["arguments"]["program"], "/w/bin/app");
}

/// #1418: the Go extension's "Connect to server" config (attach, mode
/// remote, host and port) targets that headless delve, and its request
/// keeps `mode: "remote"`.
#[test]
fn a_go_remote_attach_targets_the_headless_delve() {
    let rc = resolve(
        &config(
            r#"[{ "name": "Connect to server", "type": "go", "request": "attach",
                      "mode": "remote", "port": 2345, "host": "10.0.0.5",
                      "substitutePath": [{ "from": "${workspaceFolder}", "to": "/app" }] }]"#,
        ),
        &ctx(),
    )
    .unwrap();
    assert_eq!(
        delve_remote_address(&rc),
        Some((String::from("10.0.0.5"), 2345))
    );
    let req = delve_request(&rc);
    assert_eq!(req["command"], "attach");
    assert_eq!(req["arguments"]["mode"], "remote");
    assert_eq!(
        req["arguments"]["substitutePath"][0]["from"], "/work/proj",
        "path mapping passes through"
    );
    assert!(req["arguments"].get("processId").is_none());

    let local_host = resolve(
        &config(r#"[{ "name": "C", "type": "go", "request": "attach", "mode": "remote", "port": 2345 }]"#),
        &ctx(),
    )
    .unwrap();
    assert_eq!(
        delve_remote_address(&local_host),
        Some((String::from("127.0.0.1"), 2345))
    );
}

/// #1418, negative: a remote attach with no port, a local attach and a
/// launch have no headless delve to connect to.
#[test]
fn only_a_remote_go_attach_with_a_port_connects_to_a_server() {
    for text in [
        r#"[{ "name": "A", "type": "go", "request": "attach", "mode": "remote" }]"#,
        r#"[{ "name": "A", "type": "go", "request": "attach", "processId": 77, "port": 2345 }]"#,
        r#"[{ "name": "A", "type": "go", "request": "launch", "mode": "remote", "port": 2345, "program": "." }]"#,
    ] {
        let rc = resolve(&config(text), &ctx()).unwrap();
        assert_eq!(delve_remote_address(&rc), None, "{text}");
    }
}

#[test]
fn maps_the_adapter_families_and_rejects_unknown_types() {
    use AdapterKind::*;
    for (t, k) in [
        ("python", Debugpy),
        ("debugpy", Debugpy),
        ("lldb", LldbDap),
        ("cppdbg", LldbDap),
        ("node", JsDebug),
        ("pwa-node", JsDebug),
        ("go", Delve),
    ] {
        assert_eq!(adapter_for_type(t), Some(k), "{t}");
    }
    let cfg = config(r#"[{ "name": "J", "type": "java", "program": "Main.java" }]"#);
    let err = resolve(&cfg, &ctx()).unwrap_err();
    assert!(err.contains("unsupported type \"java\""), "{err}");
    assert!(err.contains("go"), "the supported list names go: {err}");
}

/// `${command:pickProcess}` asks for a picker at launch (#250) rather
/// than failing as an unknown variable; a literal pid is unchanged.
#[test]
fn pick_process_resolves_to_a_picker_request_not_an_error() {
    let cfg = config(
        r#"[{ "name": "Attach", "type": "lldb", "request": "attach",
                 "processId": "${command:pickProcess}" }]"#,
    );
    let rc = resolve(&cfg, &ctx()).expect("pickProcess resolves");
    assert!(rc.pick_process);
    assert_eq!(rc.process_id, None);
    assert!(!rc.extra.contains_key("processId"));

    let cfg =
        config(r#"[{ "name": "Attach", "type": "lldb", "request": "attach", "processId": 4242 }]"#);
    let rc = resolve(&cfg, &ctx()).unwrap();
    assert!(!rc.pick_process);
    assert_eq!(rc.process_id, Some(4242));
}

#[test]
fn ps_output_parses_to_pids_and_names_without_our_own() {
    let out = "  101 /usr/bin/zsh\n  202 server\n\n  303 croft\n bad line\n  404 \n";
    assert_eq!(
        parse_ps_processes(out, 303),
        vec![
            (101, String::from("/usr/bin/zsh")),
            (202, String::from("server"))
        ]
    );
}

#[test]
fn substitutes_workspace_and_file_variables() {
    let c = ctx();
    assert_eq!(
        substitute("${workspaceFolder}/bin", &c).unwrap(),
        "/work/proj/bin"
    );
    assert_eq!(
        substitute("${workspaceFolderBasename}", &c).unwrap(),
        "proj"
    );
    assert_eq!(substitute("${file}", &c).unwrap(), "/work/proj/src/main.py");
    assert_eq!(substitute("${fileBasename}", &c).unwrap(), "main.py");
    assert_eq!(
        substitute("${fileBasenameNoExtension}", &c).unwrap(),
        "main"
    );
    assert_eq!(substitute("${fileDirname}", &c).unwrap(), "/work/proj/src");
    assert_eq!(substitute("${fileExtname}", &c).unwrap(), ".py");
    assert_eq!(
        substitute("a ${fileBasename} b ${fileBasename}", &c).unwrap(),
        "a main.py b main.py"
    );
    // Text without variables (or with an unclosed brace) passes through.
    assert_eq!(substitute("plain", &c).unwrap(), "plain");
    assert_eq!(substitute("odd ${file", &c).unwrap(), "odd ${file");
}

#[test]
fn env_variable_expands_and_unknowns_error_loudly() {
    let c = ctx();
    // SAFETY: test-only env mutation, name is unique to this test.
    unsafe { std::env::set_var("CROFT_TEST_SUBST_VAR", "xyz") };
    assert_eq!(
        substitute("${env:CROFT_TEST_SUBST_VAR}", &c).unwrap(),
        "xyz"
    );
    let err = substitute("${env:CROFT_TEST_UNSET_VAR}", &c).unwrap_err();
    assert!(err.contains("CROFT_TEST_UNSET_VAR"), "{err}");
    let err = substitute("${command:pickProcess}", &c).unwrap_err();
    assert!(err.contains("command:pickProcess"), "{err}");
    // ${file} family errors when no file is active.
    let no_file = SubstCtx {
        workspace_folder: PathBuf::from("/w"),
        file: None,
        inputs: Default::default(),
        folders: Vec::new(),
    };
    assert!(substitute("${fileBasename}", &no_file).is_err());
}

#[test]
fn resolve_splits_mapped_fields_and_passes_the_rest_through() {
    let cfg = config(
        r#"[{
                "name": "API",
                "type": "python",
                "request": "launch",
                "program": "${workspaceFolder}/api.py",
                "args": ["--port", 8080],
                "env": { "MODE": "dev", "RETRIES": 3 },
                "cwd": "${workspaceFolder}",
                "preLaunchTask": "build",
                "postDebugTask": "cleanup",
                "stopOnEntry": true,
                "justMyCode": true,
                "subProcess": "${workspaceFolderBasename}"
            }]"#,
    );
    let rc = resolve(&cfg, &ctx()).unwrap();
    assert_eq!(rc.program.as_deref(), Some("/work/proj/api.py"));
    assert_eq!(rc.args, vec!["--port", "8080"]);
    assert_eq!(rc.env["MODE"], "dev");
    assert_eq!(rc.env["RETRIES"], "3");
    assert_eq!(rc.cwd.as_deref(), Some(Path::new("/work/proj")));
    assert_eq!(rc.pre_launch_task.as_deref(), Some("build"));
    // A mapped key, so croft runs it rather than handing it to the adapter.
    assert_eq!(rc.post_debug_task.as_deref(), Some("cleanup"));
    assert!(!rc.extra.contains_key("postDebugTask"));
    assert!(rc.stop_on_entry);
    // Unmapped fields survive, substituted, for verbatim passthrough.
    assert_eq!(rc.extra["justMyCode"], json!(true));
    assert_eq!(rc.extra["subProcess"], json!("proj"));
    assert!(!rc.extra.contains_key("program"));
}

#[test]
fn a_relative_cwd_resolves_against_the_workspace_folder() {
    let cfg = config(r#"[{ "name": "R", "type": "python", "program": "a.py", "cwd": "backend" }]"#);
    let rc = resolve(&cfg, &ctx()).unwrap();
    assert_eq!(rc.cwd.as_deref(), Some(Path::new("/work/proj/backend")));
    // An absolute cwd is kept as written.
    let cfg =
        config(r#"[{ "name": "R", "type": "python", "program": "a.py", "cwd": "/srv/app" }]"#);
    let rc = resolve(&cfg, &ctx()).unwrap();
    assert_eq!(rc.cwd.as_deref(), Some(Path::new("/srv/app")));
}

#[test]
fn env_file_merges_under_explicit_env() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join(".env"),
        "# comment\nexport FROM_FILE=1\nSHARED=file\nQUOTED=\"a b\"\n",
    )
    .unwrap();
    let cfg = config(
        r#"[{
                "name": "E", "type": "python", "program": "a.py",
                "envFile": "${workspaceFolder}/.env",
                "env": { "SHARED": "config" }
            }]"#,
    );
    let c = SubstCtx {
        workspace_folder: tmp.path().to_path_buf(),
        file: None,
        inputs: Default::default(),
        folders: Vec::new(),
    };
    let rc = resolve(&cfg, &c).unwrap();
    assert_eq!(rc.env["FROM_FILE"], "1");
    assert_eq!(rc.env["QUOTED"], "a b");
    assert_eq!(rc.env["SHARED"], "config", "explicit env wins per key");
    // A missing envFile is a hard, named error.
    let cfg = config(
        r#"[{ "name": "E", "type": "python", "program": "a.py", "envFile": "/nope/.env" }]"#,
    );
    let err = resolve(&cfg, &c).unwrap_err();
    assert!(err.contains("/nope/.env"), "{err}");
}

/// #1519: a launch.json configuration that does not set `justMyCode`
/// steps only through the user's code, debugpy's and VS Code's default,
/// instead of croft forcing it off.
#[test]
fn a_debugpy_config_without_just_my_code_steps_through_user_code_only() {
    let cfg = config(r#"[{ "name": "API", "type": "python", "program": "/p/api.py" }]"#);
    let rc = resolve(&cfg, &ctx()).unwrap();
    let req = debugpy_request(&rc, Path::new("/venv/bin/python"));
    assert_eq!(req["arguments"]["justMyCode"], true);
}

/// Negative: a configuration that turns `justMyCode` off (to step into
/// a library) keeps it off.
#[test]
fn a_debugpy_config_can_still_step_into_libraries() {
    let cfg = config(
        r#"[{ "name": "API", "type": "python", "program": "/p/api.py", "justMyCode": false }]"#,
    );
    let rc = resolve(&cfg, &ctx()).unwrap();
    let req = debugpy_request(&rc, Path::new("/venv/bin/python"));
    assert_eq!(req["arguments"]["justMyCode"], false);
}

#[test]
fn debugpy_launch_request_carries_env_args_and_forces_internal_console() {
    let cfg = config(
        r#"[{
                "name": "API", "type": "python", "program": "/p/api.py",
                "args": ["--serve"], "env": { "MODE": "dev" },
                "console": "integratedTerminal"
            }]"#,
    );
    let rc = resolve(&cfg, &ctx()).unwrap();
    let req = debugpy_request(&rc, Path::new("/venv/bin/python"));
    assert_eq!(req["command"], "launch");
    assert_eq!(req["arguments"]["program"], "/p/api.py");
    assert_eq!(req["arguments"]["args"][0], "--serve");
    assert_eq!(req["arguments"]["env"]["MODE"], "dev");
    assert_eq!(req["arguments"]["python"][0], "/venv/bin/python");
    // integratedTerminal would hang on the declined runInTerminal.
    assert_eq!(req["arguments"]["console"], "internalConsole");
}

#[test]
fn debugpy_module_launch_drops_program_and_attach_builds_connect() {
    let cfg =
        config(r#"[{ "name": "T", "type": "python", "module": "pytest", "args": ["-k", "x"] }]"#);
    let rc = resolve(&cfg, &ctx()).unwrap();
    let req = debugpy_request(&rc, Path::new("/venv/bin/python"));
    assert_eq!(req["arguments"]["module"], "pytest");
    assert!(req["arguments"].get("program").is_none());

    let cfg = config(r#"[{ "name": "A", "type": "python", "request": "attach", "port": 5678 }]"#);
    let rc = resolve(&cfg, &ctx()).unwrap();
    let req = debugpy_request(&rc, Path::new("/venv/bin/python"));
    assert_eq!(req["command"], "attach");
    assert_eq!(req["arguments"]["connect"]["port"], 5678);
    assert_eq!(req["arguments"]["connect"]["host"], "127.0.0.1");
}

/// #864: the project interpreter is only the default. A config naming
/// its own `python` keeps it, and one using the legacy `pythonPath`
/// gets no `python` beside it, which debugpy would refuse.
#[test]
fn a_configs_own_python_wins_over_the_project_interpreter() {
    let cfg = config(
        r#"[{ "name": "P", "type": "python", "program": "a.py", "python": "/opt/py/bin/python" }]"#,
    );
    let rc = resolve(&cfg, &ctx()).unwrap();
    let req = debugpy_request(&rc, Path::new("/proj/.venv/bin/python"));
    assert_eq!(req["arguments"]["python"], "/opt/py/bin/python");

    let cfg = config(
        r#"[{ "name": "L", "type": "python", "program": "a.py", "pythonPath": "/opt/py/bin/python" }]"#,
    );
    let rc = resolve(&cfg, &ctx()).unwrap();
    let req = debugpy_request(&rc, Path::new("/proj/.venv/bin/python"));
    assert!(req["arguments"].get("python").is_none());
    assert_eq!(req["arguments"]["pythonPath"], "/opt/py/bin/python");
}

#[test]
fn lldb_requests_use_kv_env_and_attach_by_pid() {
    let cfg = config(
        r#"[{
                "name": "Bin", "type": "lldb", "program": "/t/debug/app",
                "args": ["--flag"], "env": { "RUST_LOG": "debug" }
            }]"#,
    );
    let rc = resolve(&cfg, &ctx()).unwrap();
    let req = lldb_request(&rc);
    assert_eq!(req["command"], "launch");
    assert_eq!(req["arguments"]["program"], "/t/debug/app");
    assert_eq!(req["arguments"]["env"][0], "RUST_LOG=debug");

    let cfg =
        config(r#"[{ "name": "At", "type": "lldb", "request": "attach", "processId": "4242" }]"#);
    let rc = resolve(&cfg, &ctx()).unwrap();
    assert_eq!(rc.process_id, Some(4242), "numeric string processId parses");
    let req = lldb_request(&rc);
    assert_eq!(req["command"], "attach");
    assert_eq!(req["arguments"]["pid"], 4242);
    assert!(req["arguments"].get("stopOnEntry").is_none());
}

/// #1419: an lldb attach by program name (with `waitFor`) or through
/// `attachCommands` (gdbserver, QEMU, an embedded probe) has a target and
/// goes out as written, with no `pid`.
#[test]
fn an_lldb_attach_by_program_or_attach_commands_has_a_target() {
    let cfg = config(
        r#"[{ "name": "By name", "type": "lldb", "request": "attach",
                  "program": "${workspaceFolder}/app", "waitFor": true }]"#,
    );
    let rc = resolve(&cfg, &ctx()).unwrap();
    assert!(lldb_attach_has_target(&rc));
    let req = lldb_request(&rc);
    assert_eq!(req["command"], "attach");
    assert_eq!(req["arguments"]["program"], "/work/proj/app");
    assert_eq!(req["arguments"]["waitFor"], true);
    assert!(req["arguments"].get("pid").is_none());
    assert!(req["arguments"].get("stopOnEntry").is_none());

    let cfg = config(
        r#"[{ "name": "gdbserver", "type": "lldb", "request": "attach",
                  "program": "${workspaceFolder}/app",
                  "attachCommands": ["target create ${workspaceFolder}/app", "gdb-remote localhost:1234"] }]"#,
    );
    let rc = resolve(&cfg, &ctx()).unwrap();
    assert!(lldb_attach_has_target(&rc));
    let req = lldb_request(&rc);
    assert_eq!(
        req["arguments"]["attachCommands"],
        json!(["target create /work/proj/app", "gdb-remote localhost:1234"])
    );
    assert!(req["arguments"].get("pid").is_none());

    // attachCommands alone is enough.
    let cfg = config(
        r#"[{ "name": "Remote", "type": "lldb", "request": "attach",
                  "attachCommands": ["gdb-remote 10.0.0.2:3333"] }]"#,
    );
    assert!(lldb_attach_has_target(&resolve(&cfg, &ctx()).unwrap()));
}

/// #1419, negative: an lldb attach with no pid, no program and no (or
/// empty) attachCommands has nothing to attach to.
#[test]
fn an_lldb_attach_with_nothing_to_attach_to_has_no_target() {
    for text in [
        r#"[{ "name": "A", "type": "lldb", "request": "attach" }]"#,
        r#"[{ "name": "A", "type": "lldb", "request": "attach", "attachCommands": [] }]"#,
        r#"[{ "name": "A", "type": "lldb", "request": "attach", "program": "  " }]"#,
    ] {
        let rc = resolve(&config(text), &ctx()).unwrap();
        assert!(!lldb_attach_has_target(&rc), "{text}");
    }
}

#[test]
fn js_requests_are_pwa_node_and_attach_by_port() {
    let cfg = config(
        r#"[{ "name": "S", "type": "node", "program": "/p/server.js", "sourceMaps": false }]"#,
    );
    let rc = resolve(&cfg, &ctx()).unwrap();
    let req = js_request(&rc);
    assert_eq!(req["command"], "launch");
    assert_eq!(req["arguments"]["type"], "pwa-node");
    assert_eq!(
        req["arguments"]["sourceMaps"], false,
        "user value respected"
    );
    assert_eq!(req["arguments"]["console"], "internalConsole");

    let cfg = config(r#"[{ "name": "A", "type": "node", "request": "attach", "port": 9229 }]"#);
    let rc = resolve(&cfg, &ctx()).unwrap();
    let req = js_request(&rc);
    assert_eq!(req["command"], "attach");
    assert_eq!(req["arguments"]["port"], 9229);
    assert_eq!(req["arguments"]["address"], "127.0.0.1");
}

/// #1414: js-debug launches and attaches carry the workspace folder, and
/// one the config sets is kept.
#[test]
fn js_requests_carry_the_workspace_folder_unless_the_config_sets_one() {
    let root = Path::new("/work/proj");
    for text in [
        r#"[{ "name": "L", "type": "node", "program": "${workspaceFolder}/dist/main.js", "outFiles": ["${workspaceFolder}/dist/**/*.js"] }]"#,
        r#"[{ "name": "A", "type": "node", "request": "attach", "port": 9229 }]"#,
    ] {
        let rc = resolve(&config(text), &ctx()).unwrap();
        let req = with_workspace_folder(js_request(&rc), root);
        assert_eq!(
            req["arguments"]["__workspaceFolder"], "/work/proj",
            "{text}"
        );
    }
    let rc = resolve(
        &config(r#"[{ "name": "W", "type": "node", "program": "a.js", "__workspaceFolder": "/elsewhere" }]"#),
        &ctx(),
    )
    .unwrap();
    let req = with_workspace_folder(js_request(&rc), root);
    assert_eq!(
        req["arguments"]["__workspaceFolder"], "/elsewhere",
        "the config's own value wins"
    );
    // Negative: a request without arguments is left as it is.
    let bare = json!({ "type": "request", "command": "launch" });
    assert_eq!(with_workspace_folder(bare.clone(), root), bare);
}

/// #1410: VS Code's browser types run on js-debug, which croft already
/// bundles.
#[test]
fn chrome_and_edge_types_resolve_to_js_debug() {
    for (t, js) in [
        ("chrome", "pwa-chrome"),
        ("pwa-chrome", "pwa-chrome"),
        ("msedge", "pwa-msedge"),
        ("pwa-msedge", "pwa-msedge"),
    ] {
        assert_eq!(adapter_for_type(t), Some(AdapterKind::JsDebug), "{t}");
        assert_eq!(browser_type(t), Some(js), "{t}");
    }
}

/// #1410, negative: Node stays Node, and a browser js-debug does not
/// drive is still refused.
#[test]
fn node_is_not_a_browser_and_firefox_is_still_refused() {
    for t in ["node", "pwa-node", "node-terminal", "python"] {
        assert_eq!(browser_type(t), None, "{t}");
    }
    assert_eq!(adapter_for_type("firefox"), None);
    let cfg = config(r#"[{ "name": "F", "type": "firefox", "url": "http://localhost" }]"#);
    let err = resolve(&cfg, &ctx()).unwrap_err();
    assert!(err.contains("unsupported type \"firefox\""), "{err}");
}

/// #1410: a browser config needs no `program` or `port`, which a Node
/// one still does.
#[test]
fn a_browser_config_needs_no_program_or_port() {
    for json in [
        r#"[{ "name": "C", "type": "chrome", "url": "http://localhost:3000" }]"#,
        r#"[{ "name": "C", "type": "msedge", "request": "attach" }]"#,
    ] {
        let rc = resolve(&config(json), &ctx()).unwrap();
        assert_eq!(js_config_error(&rc), None, "{json}");
    }
    let rc = resolve(&config(r#"[{ "name": "N", "type": "node" }]"#), &ctx()).unwrap();
    assert_eq!(
        js_config_error(&rc),
        Some("a node launch needs a \"program\", a \"runtimeExecutable\" or a \"command\"")
    );
    let rc = resolve(
        &config(r#"[{ "name": "N", "type": "node", "request": "attach" }]"#),
        &ctx(),
    )
    .unwrap();
    assert_eq!(js_config_error(&rc), Some("a node attach needs a \"port\""));
}

/// #1410: a chrome launch is a `pwa-chrome` request carrying the
/// config's own browser fields, with no Node console setting.
#[test]
fn a_chrome_launch_is_a_pwa_chrome_request() {
    let cfg = config(
        r#"[{ "name": "C", "type": "chrome", "request": "launch",
                  "url": "http://localhost:5173", "webRoot": "${workspaceFolder}/web",
                  "runtimeExecutable": "/usr/bin/chromium", "userDataDir": false }]"#,
    );
    let rc = resolve(&cfg, &ctx()).unwrap();
    let req = js_request(&rc);
    let a = &req["arguments"];
    assert_eq!(req["command"], "launch");
    assert_eq!(a["type"], "pwa-chrome");
    assert_eq!(a["url"], "http://localhost:5173");
    assert_eq!(a["webRoot"], "/work/proj/web");
    assert_eq!(a["runtimeExecutable"], "/usr/bin/chromium");
    assert_eq!(a["userDataDir"], false);
    assert!(a.get("console").is_none(), "{a}");
    assert!(a.get("program").is_none(), "{a}");
}

/// #1410: with no `webRoot`, a browser config maps onto the workspace
/// folder, as VS Code's default does; a Node config gets none.
#[test]
fn a_browser_config_without_a_web_root_uses_the_workspace_folder() {
    let cfg =
        config(r#"[{ "name": "E", "type": "msedge", "file": "${workspaceFolder}/index.html" }]"#);
    let req = js_request(&resolve(&cfg, &ctx()).unwrap());
    assert_eq!(req["arguments"]["type"], "pwa-msedge");
    assert_eq!(req["arguments"]["webRoot"], "/work/proj");
    assert_eq!(req["arguments"]["file"], "/work/proj/index.html");

    let cfg = config(r#"[{ "name": "N", "type": "node", "program": "/p/a.js" }]"#);
    let req = js_request(&resolve(&cfg, &ctx()).unwrap());
    assert!(req["arguments"].get("webRoot").is_none());
}

/// #1410: a browser attach with no port uses 9222, the browsers' own
/// default; a port the config names wins.
#[test]
fn a_browser_attach_defaults_to_port_9222() {
    let cfg = config(r#"[{ "name": "A", "type": "chrome", "request": "attach" }]"#);
    let req = js_request(&resolve(&cfg, &ctx()).unwrap());
    assert_eq!(req["command"], "attach");
    assert_eq!(req["arguments"]["type"], "pwa-chrome");
    assert_eq!(req["arguments"]["port"], 9222);

    let cfg = config(r#"[{ "name": "A", "type": "msedge", "request": "attach", "port": 9333 }]"#);
    let req = js_request(&resolve(&cfg, &ctx()).unwrap());
    assert_eq!(req["arguments"]["port"], 9333);
}

/// #1410: with no display, the browser launches headless; a config that
/// already picks a headless mode keeps its own.
#[test]
fn a_headless_browser_adds_its_flag_once() {
    let cfg = config(
        r#"[{ "name": "C", "type": "chrome", "url": "http://x", "runtimeArgs": ["--incognito"] }]"#,
    );
    let req = with_headless_browser(js_request(&resolve(&cfg, &ctx()).unwrap()));
    assert_eq!(
        req["arguments"]["runtimeArgs"],
        json!(["--incognito", "--headless=new"])
    );
    let cfg = config(r#"[{ "name": "C", "type": "chrome", "runtimeArgs": ["--headless"] }]"#);
    let req = with_headless_browser(js_request(&resolve(&cfg, &ctx()).unwrap()));
    assert_eq!(req["arguments"]["runtimeArgs"], json!(["--headless"]));
    let cfg = config(r#"[{ "name": "C", "type": "chrome", "runtimeArgs": null }]"#);
    let req = with_headless_browser(js_request(&resolve(&cfg, &ctx()).unwrap()));
    assert_eq!(req["arguments"]["runtimeArgs"], json!(["--headless=new"]));
}

/// The issue's launch.json: a debugpy config for debugging tests.
const DEBUG_TESTS: &str = r#"[
        { "name": "Run", "type": "debugpy", "request": "launch", "program": "${file}" },
        { "name": "Debug Tests", "type": "debugpy", "request": "launch",
          "purpose": ["debug-test"], "console": "integratedTerminal",
          "env": { "APP_MODE": "test" }, "justMyCode": false,
          "args": ["--maxfail=1"], "cwd": "${workspaceFolder}/tests",
          "subProcess": true }
    ]"#;

/// #1398: a debugpy config whose `purpose` names `debug-test` is the
/// one Debug Test uses; a plain one is not.
#[test]
fn the_debug_test_config_is_found_by_its_purpose() {
    let cfgs = parse_launch_json(DEBUG_TESTS, ".vscode/launch.json");
    assert_eq!(
        debug_test_config(&cfgs).map(|c| c.name.as_str()),
        Some("Debug Tests")
    );
    assert!(!is_debug_test_config(&cfgs[0]));
    let single = config(r#"[{ "name": "T", "type": "python", "purpose": "debug-test" }]"#);
    assert!(is_debug_test_config(&single), "a single string counts too");
}

/// #1398, negative: no `purpose`, another purpose, or a debug-test
/// purpose on a non-Python config is not a test-debugging config.
#[test]
fn other_purposes_and_other_debuggers_are_not_debug_test_configs() {
    let cfgs = parse_launch_json(
        r#"[
                { "name": "A", "type": "debugpy", "purpose": ["debug-in-terminal"] },
                { "name": "B", "type": "node", "purpose": ["debug-test"], "program": "x.js" },
                { "name": "C", "type": "debugpy" }
            ]"#,
        ".vscode/launch.json",
    );
    assert!(debug_test_config(&cfgs).is_none());
}

/// #1398: the config's env, cwd, justMyCode and other keys land on the
/// pytest request; its args follow the test selection; pytest stays
/// the module and the console stays croft's.
#[test]
fn a_debug_test_config_overlays_the_pytest_request() {
    let cfgs = parse_launch_json(DEBUG_TESTS, ".vscode/launch.json");
    let rc = resolve(debug_test_config(&cfgs).unwrap(), &ctx()).unwrap();
    let base = crate::dap::session::pytest_debug_launch_request(
        Path::new("/venv/bin/python"),
        Path::new("/work/proj"),
        "test_mode",
    );
    let req = with_debug_test_config(base, &rc);
    let a = &req["arguments"];
    assert_eq!(a["env"]["APP_MODE"], "test");
    assert_eq!(a["cwd"], "/work/proj/tests");
    assert_eq!(a["justMyCode"], false);
    assert_eq!(a["subProcess"], true);
    assert_eq!(a["args"], json!(["-v", "-k", "test_mode", "--maxfail=1"]));
    assert_eq!(a["module"], "pytest");
    assert_eq!(a["console"], "internalConsole");
    assert_eq!(a["python"], json!(["/venv/bin/python"]));
    assert!(a.get("purpose").is_none(), "{a}");
}

/// #1398, negative: a config that sets nothing leaves croft's request
/// as it was.
#[test]
fn an_empty_debug_test_config_changes_nothing() {
    let cfg = config(r#"[{ "name": "T", "type": "debugpy", "purpose": ["debug-test"] }]"#);
    let rc = resolve(&cfg, &ctx()).unwrap();
    let base = crate::dap::session::pytest_debug_launch_request(
        Path::new("/venv/bin/python"),
        Path::new("/work/proj"),
        "test_mode",
    );
    assert_eq!(with_debug_test_config(base.clone(), &rc), base);
}

/// The issue's cppdbg configuration (#1394).
const CPPDBG: &str = r#"[{
        "name": "(gdb) Launch", "type": "cppdbg", "request": "launch",
        "program": "${workspaceFolder}/app", "args": ["one"],
        "stopAtEntry": true, "cwd": "${workspaceFolder}",
        "environment": [{ "name": "GREETING", "value": "hi" }, { "name": "N", "value": 3 }],
        "externalConsole": false, "MIMode": "gdb",
        "setupCommands": [{ "text": "-enable-pretty-printing" }]
    }]"#;

/// #1394: cppdbg's `environment` list and `stopAtEntry` reach lldb-dap
/// as its `env` and `stopOnEntry`.
#[test]
fn a_cppdbg_environment_and_stop_at_entry_reach_lldb_dap() {
    let rc = resolve(&config(CPPDBG), &ctx()).unwrap();
    let req = lldb_request(&rc);
    let a = &req["arguments"];
    assert_eq!(a["env"], json!(["GREETING=hi", "N=3"]));
    assert_eq!(a["stopOnEntry"], true);
    assert_eq!(a["args"], json!(["one"]));
    assert!(a.get("environment").is_none(), "{a}");
    assert!(a.get("stopAtEntry").is_none(), "{a}");
}

/// #1394: an explicit `env` wins over `environment` for the same key.
#[test]
fn a_cppdbg_env_wins_over_its_environment_list() {
    let cfg = config(
        r#"[{ "name": "C", "type": "cppdbg", "program": "/a",
                  "environment": [{ "name": "A", "value": "1" }, { "name": "B", "value": "x" }],
                  "env": { "A": "2" } }]"#,
    );
    let req = lldb_request(&resolve(&cfg, &ctx()).unwrap());
    assert_eq!(req["arguments"]["env"], json!(["A=2", "B=x"]));
}

/// #1394: the gdb-only keys are named, so the debug console can say
/// lldb-dap ignores them; only for cppdbg.
#[test]
fn a_cppdbg_config_names_the_keys_lldb_dap_ignores() {
    assert_eq!(
        ignored_cppdbg_keys(&config(CPPDBG)),
        ["MIMode", "setupCommands", "externalConsole"]
    );
    // Carried on the resolved config, which is all a preLaunchTask
    // resume or a compound member launch has to go on.
    assert_eq!(
        resolve(&config(CPPDBG), &ctx()).unwrap().ignored_keys,
        ["MIMode", "setupCommands", "externalConsole"]
    );
}

/// #1394, negative: an lldb config keeps its own spellings: an
/// `environment` or `stopAtEntry` key there is not lldb-dap's and is
/// passed through, not mapped, and it names no ignored keys.
#[test]
fn only_cppdbg_configs_get_cppdbg_spellings() {
    let cfg = config(
        r#"[{ "name": "L", "type": "lldb", "program": "/a", "MIMode": "gdb",
                  "environment": [{ "name": "A", "value": "1" }], "stopAtEntry": true }]"#,
    );
    let rc = resolve(&cfg, &ctx()).unwrap();
    assert!(rc.env.is_empty());
    assert!(!rc.stop_on_entry);
    assert!(ignored_cppdbg_keys(&cfg).is_empty());
    let rc = resolve(
        &config(r#"[{ "name": "C", "type": "cppdbg", "program": "/a" }]"#),
        &ctx(),
    )
    .unwrap();
    assert!(rc.env.is_empty());
    assert!(!rc.stop_on_entry);
}

#[test]
fn parse_env_file_handles_comments_export_and_quotes() {
    let env =
        parse_env_file("# a comment\n\nexport A=1\nB = spaced \nC='sq'\nnot a pair\nBAD KEY=x\n");
    assert_eq!(env["A"], "1");
    assert_eq!(env["B"], "spaced");
    assert_eq!(env["C"], "sq");
    assert!(!env.contains_key("not a pair"));
    assert!(!env.contains_key("BAD KEY"));
}

/// #1389: `${VAR}` expands as VS Code and python-dotenv expand it, from
/// the file's earlier lines, then croft's environment, else to nothing.
#[test]
fn an_env_file_expands_variable_references() {
    let process = |name: &str| (name == "HOME").then(|| String::from("/home/u"));
    let env = parse_env_file_in(
        "A=/x\nB=${A}/src\nP=${HOME}/data\nC=${MISSING}x\nD=${MISSING:-fallback}\n\
             E=\"${A}:\\t\"\nF='${A}'\nG=${A\n",
        process,
    );
    assert_eq!(env["B"], "/x/src");
    assert_eq!(env["P"], "/home/u/data");
    assert_eq!(env["C"], "x");
    assert_eq!(env["D"], "fallback");
    assert_eq!(env["E"], "/x:\t", "double quotes expand and unescape");
    assert_eq!(env["F"], "${A}", "single quotes are literal");
    assert_eq!(env["G"], "${A", "an unterminated reference stays");
}

/// #1389: an inline comment is dropped from an unquoted value, double
/// quotes take escapes and may span lines, and single quotes keep
/// everything as written.
#[test]
fn an_env_file_reads_inline_comments_escapes_and_multiline_values() {
    let env = parse_env_file_in(
        "PORT=8000 # dev port\nHASH=a#b\nBLANK= # nothing\n\
             GREETING=\"hello\\nworld\" # greeting\n\
             QUOTE=\"say \\\"hi\\\"\"\n\
             KEY=\"-----BEGIN-----\nabc\n-----END-----\"\nAFTER=1\n\
             LITERAL='a # b'\nRAW='a\\nb'\nOPEN=\"never closed\n",
        |_| None,
    );
    assert_eq!(env["PORT"], "8000");
    assert_eq!(env["HASH"], "a#b", "a # inside a word is no comment");
    assert_eq!(env["BLANK"], "");
    assert_eq!(env["GREETING"], "hello\nworld");
    assert_eq!(env["QUOTE"], "say \"hi\"");
    assert_eq!(env["KEY"], "-----BEGIN-----\nabc\n-----END-----");
    assert_eq!(env["AFTER"], "1", "the line after a multi-line value");
    assert_eq!(env["LITERAL"], "a # b");
    assert_eq!(env["RAW"], "a\\nb");
    assert_eq!(env["OPEN"], "\"never closed", "an unclosed quote is kept");
}

/// #250 phase 2: `compounds` names several configurations to launch
/// together. It is a sibling key of `configurations` in the same file, so
/// it parses alongside them rather than needing its own discovery pass.
/// Multi-root workspaces write members as `{ "folder": …, "name": … }`
/// rather than bare strings. croft resolves against one root, so the name
/// is taken either way — dropping the row would silently shrink a compound
/// to the members that happen to be written in the short form.
#[test]
fn compound_members_may_be_folder_name_objects_as_well_as_bare_strings() {
    let text = r#"{"configurations":[
              {"name":"Server","type":"lldb","request":"launch"},
              {"name":"Client","type":"lldb","request":"launch"}],
            "compounds":[{"name":"Mixed","configurations":[
              "Server",
              {"folder":"web","name":"Client"}]}]}"#;

    let compounds = parse_compounds(text, ".vscode/launch.json");
    assert_eq!(compounds.len(), 1, "precondition: the compound parsed");
    assert_eq!(
        compounds[0].configurations,
        vec!["Server", "Client"],
        "an object member contributes its name, exactly as a bare string does"
    );
}

/// A compound must bind to the configurations ITS OWN FILE declares. Both
/// files may declare the same name — `.croft` wins the picker row, but a
/// `.vscode` compound naming "Server" means .vscode's Server, which may be
/// a different program under a different adapter. Matching by bare name
/// against the merged list launches something other than what the user
/// wrote, which is the same rule the parse and resolve boundaries already
/// enforce for missing and malformed members.
#[test]
fn a_compound_binds_to_its_own_files_configuration_not_a_same_named_one() {
    let croft = r#"{"configurations":[
              {"name":"Server","type":"python","request":"launch","program":"croft.py"}]}"#;
    let vscode = r#"{"configurations":[
              {"name":"Server","type":"node","request":"launch","program":"vscode.js"}],
            "compounds":[{"name":"VsCompound","configurations":["Server"]}]}"#;

    // Through the PRODUCTION path, not hand-stitched input. Building the
    // config list with two `parse_launch_json` calls asserts a property on
    // an input shape the shipped program never produces: `discover_configs`
    // keeps only the first occurrence per name, so the duplicate this
    // preference exists to disambiguate would already be gone.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".croft")).unwrap();
    std::fs::create_dir_all(tmp.path().join(".vscode")).unwrap();
    std::fs::write(tmp.path().join(".croft/launch.json"), croft).unwrap();
    std::fs::write(tmp.path().join(".vscode/launch.json"), vscode).unwrap();

    let configs = discover_configs_all(tmp.path());
    let compounds = discover_compounds(tmp.path());
    assert_eq!(compounds.len(), 1, "precondition: the compound parsed");
    assert_eq!(
        compounds[0].source, ".vscode/launch.json",
        "precondition: the compound remembers which file declared it"
    );
    assert_eq!(
        configs.iter().filter(|c| c.name == "Server").count(),
        2,
        "precondition: the list resolve_compound receives still holds BOTH \
             Servers — `discover_configs` would have dropped one, leaving the \
             source preference nothing to prefer"
    );
    assert_eq!(
        discover_configs(tmp.path())
            .iter()
            .filter(|c| c.name == "Server")
            .count(),
        1,
        "and the picker's list drops it, which is why the two lists differ"
    );

    let members = resolve_compound(&compounds[0], &configs, &[]).expect("resolves");
    assert_eq!(
        members[0].source, ".vscode/launch.json",
        "the .vscode compound binds .vscode's Server, not .croft's same-named one"
    );
    assert_eq!(
        members[0].type_name, "node",
        "which is a different adapter — binding .croft's python config here \
             would debug a different program than the compound declares"
    );
}

/// A compound with SOME unusable members must be dropped whole, not
/// silently shrunk to the ones that parse. `resolve_compound` already
/// refuses to launch a subset when a member names no configuration —
/// "launching the subset that happens to resolve would debug something
/// other than what the user asked for, and silently" — and a malformed
/// member is the same situation one boundary earlier.
#[test]
fn a_compound_with_one_unusable_member_is_dropped_rather_than_silently_shrunk() {
    let text = r#"{"configurations":[
              {"name":"A","type":"lldb","request":"launch"},
              {"name":"B","type":"lldb","request":"launch"}],
            "compounds":[
              {"name":"Partial","configurations":["A", 7, "B"]},
              {"name":"Whole","configurations":["A","B"]}]}"#;

    let names: Vec<String> = parse_compounds(text, ".vscode/launch.json")
        .into_iter()
        .map(|c| c.name)
        .collect();
    assert_eq!(
        names,
        vec!["Whole"],
        "the compound with a malformed member is dropped entirely — shrinking \
             it to [\"A\", \"B\"] would launch two of the three the user wrote"
    );
}

/// A compound whose members all fail to parse would present as a picker row
/// that does nothing, so it is skipped entirely rather than listed empty.
#[test]
fn a_compound_with_no_usable_members_is_skipped_rather_than_listed_empty() {
    let text = r#"{"configurations":[{"name":"A","type":"lldb","request":"launch"}],
            "compounds":[
              {"name":"Empty","configurations":[]},
              {"name":"Unusable","configurations":[7, true]},
              {"name":"Real","configurations":["A"]}]}"#;

    let names: Vec<String> = parse_compounds(text, ".vscode/launch.json")
        .into_iter()
        .map(|c| c.name)
        .collect();
    assert_eq!(
        names,
        vec!["Real"],
        "an empty compound and one whose members are all unusable are both dropped"
    );
}

#[test]
fn discover_compounds_prefers_croft_over_vscode_for_a_duplicate_name() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join(".croft")).unwrap();
    std::fs::create_dir_all(root.join(".vscode")).unwrap();
    std::fs::write(
        root.join(".croft/launch.json"),
        r#"{"configurations":[{"name":"A","type":"lldb","request":"launch"}],
                "compounds":[{"name":"Both","configurations":["A"]}]}"#,
    )
    .unwrap();
    std::fs::write(
        root.join(".vscode/launch.json"),
        r#"{"configurations":[{"name":"B","type":"lldb","request":"launch"}],
                "compounds":[{"name":"Both","configurations":["B"]},
                             {"name":"VsOnly","configurations":["B"]}]}"#,
    )
    .unwrap();

    let found = discover_compounds(root);
    let names: Vec<&str> = found.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["Both", "VsOnly"],
        "both files contribute, .croft first"
    );
    assert_eq!(
        found[0].configurations,
        vec!["A"],
        "the .croft definition of a duplicate name wins, not .vscode's"
    );
}

#[test]
fn compounds_are_parsed_alongside_the_configurations_they_name() {
    let text = r#"{
            "configurations": [
                { "name": "Server", "type": "node", "program": "s.js" },
                { "name": "Client", "type": "node", "program": "c.js" }
            ],
            "compounds": [
                { "name": "Full Stack", "configurations": ["Server", "Client"] }
            ]
        }"#;
    // Precondition: the configurations themselves parse, so a compound
    // finding nothing cannot be blamed on the file being unreadable.
    let cfgs = parse_launch_json(text, ".vscode/launch.json");
    assert_eq!(cfgs.len(), 2, "precondition: both configurations parsed");

    let compounds = parse_compounds(text, ".vscode/launch.json");
    assert_eq!(compounds.len(), 1, "the compound is found");
    assert_eq!(compounds[0].name, "Full Stack");
    assert_eq!(compounds[0].configurations, vec!["Server", "Client"]);
}

/// A compound naming a configuration that does not exist must fail loudly
/// at resolve time rather than silently launching the subset it can find —
/// the same posture as an unresolvable `${...}` variable (#250).
#[test]
fn a_compound_naming_a_missing_configuration_errors_rather_than_launching_a_subset() {
    let text = r#"{
            "configurations": [ { "name": "Server", "type": "node", "program": "s.js" } ],
            "compounds": [ { "name": "Both", "configurations": ["Server", "Ghost"] } ]
        }"#;
    let cfgs = parse_launch_json(text, ".vscode/launch.json");
    let compounds = parse_compounds(text, ".vscode/launch.json");
    assert_eq!(compounds.len(), 1, "precondition: the compound parsed");

    let err = resolve_compound(&compounds[0], &cfgs, &[])
        .expect_err("a missing member is an error, not a shorter list");
    assert!(
        err.contains("Ghost"),
        "the error names the missing configuration, got: {err}"
    );
}

/// A key PRESENT but requesting nothing must not be recorded as
/// unsupported. `contains_key` was the first attempt and it refused these:
/// `stopAll: false` is VS Code's own default written out, and a null or
/// empty `preLaunchTask` asks for nothing at all. croft honours every one
/// of them perfectly by doing nothing, so naming them in a refusal tells
/// the user croft cannot do what it is already doing.
///
/// `stopAll` is excluded at either value for ONE member: it decides whether
/// ending one session ends the others, and a one-member compound has no
/// others. At two it is recorded at `true` only - see
/// `stop_all_true_is_refused_only_where_there_are_siblings_to_stop`.
#[test]
fn a_compound_key_that_requests_nothing_is_not_recorded_as_unsupported() {
    let text = r#"{
            "configurations": [
                { "name": "A", "type": "node", "program": "a.js" },
                { "name": "B", "type": "node", "program": "b.js" }
            ],
            "compounds": [
                { "name": "DefaultStopAll", "configurations": ["A"], "stopAll": false },
                { "name": "TrueStopAll", "configurations": ["A"], "stopAll": true },
                { "name": "NulledTask", "configurations": ["A"], "preLaunchTask": null },
                { "name": "EmptyTask", "configurations": ["A"], "preLaunchTask": "" },
                { "name": "BoolTask", "configurations": ["A"], "preLaunchTask": true },
                { "name": "EmptyPresentation", "configurations": ["A"], "presentation": {} },
                { "name": "RealPresentation", "configurations": ["A"], "presentation": { "order": 1 } },
                { "name": "Malformed", "configurations": ["A"], "presentation": 7 }
            ]
        }"#;
    let cs = parse_compounds(text, ".vscode/launch.json");
    assert_eq!(cs.len(), 8, "precondition: every compound parsed");
    let by = |n: &str| {
        cs.iter()
            .find(|c| c.name == n)
            .unwrap_or_else(|| panic!("{n} parsed"))
    };

    assert!(
        by("DefaultStopAll").unsupported_keys.is_empty(),
        "stopAll:false is VS Code's default and cannot change a one-member \
             outcome: {:?}",
        by("DefaultStopAll").unsupported_keys
    );
    // A task that asks for something croft cannot READ is refused, unlike
    // one that asks for nothing. Without this pair the rule reads as
    // "preLaunchTask is never recorded", which was the bug.
    assert_eq!(
        by("BoolTask").unsupported_keys,
        vec!["preLaunchTask"],
        "a non-string task cannot be named or run, so it is refused \
             rather than skipped in silence"
    );
    assert!(
        by("NulledTask").unsupported_keys.is_empty(),
        "while a null one asks for nothing: {:?}",
        by("NulledTask").unsupported_keys
    );
    assert!(
        by("TrueStopAll").unsupported_keys.is_empty(),
        "nor can stopAll:true — with one session there are no others to \
             stop: {:?}",
        by("TrueStopAll").unsupported_keys
    );
    assert!(
        by("NulledTask").unsupported_keys.is_empty(),
        "a null preLaunchTask asks for nothing: {:?}",
        by("NulledTask").unsupported_keys
    );
    assert!(
        by("EmptyTask").unsupported_keys.is_empty(),
        "nor does an empty one: {:?}",
        by("EmptyTask").unsupported_keys
    );
    assert!(
        by("EmptyPresentation").unsupported_keys.is_empty(),
        "nor an empty presentation object: {:?}",
        by("EmptyPresentation").unsupported_keys
    );

    // The positive control: without it, six empties could equally mean
    // the detection never records anything at all. Every honourable key
    // is honoured now (#318), so the only shapes left to carry the
    // control are the MALFORMED ones - a `presentation` that is not an
    // object is the cheapest.
    assert_eq!(
        by("Malformed").unsupported_keys,
        vec!["presentation"],
        "and a shape croft cannot act on is still recorded"
    );
    assert_eq!(
        by("RealPresentation").unsupported_keys,
        Vec::<&str>::new(),
        "while a well-formed order is honoured rather than refused"
    );
}

/// `stopAll: true` is honoured, not refused (#567); only an unreadable
/// value is refused, and only where there are siblings to stop.
#[test]
fn stop_all_true_is_honoured_and_only_an_unreadable_value_refused() {
    let text = r#"{
            "configurations": [
                { "name": "A", "type": "node", "program": "a.js" },
                { "name": "B", "type": "node", "program": "b.js" }
            ],
            "compounds": [
                { "name": "OneTrue", "configurations": ["A"], "stopAll": true },
                { "name": "TwoTrue", "configurations": ["A", "B"], "stopAll": true },
                { "name": "TwoQuoted", "configurations": ["A", "B"], "stopAll": "true" },
                { "name": "TwoFalse", "configurations": ["A", "B"], "stopAll": false },
                { "name": "TwoAbsent", "configurations": ["A", "B"] }
            ]
        }"#;
    let cs = parse_compounds(text, ".vscode/launch.json");
    assert_eq!(cs.len(), 5, "precondition: every compound parsed");
    let by = |n: &str| cs.iter().find(|c| c.name == n).expect("parsed");

    assert!(
        by("TwoTrue").unsupported_keys.is_empty() && by("TwoTrue").stop_all,
        "`true` is honoured: the set is torn down when a member ends"
    );
    assert!(!by("TwoFalse").stop_all && !by("TwoAbsent").stop_all);
    assert_eq!(
        by("TwoQuoted").unsupported_keys,
        vec!["stopAll"],
        "and `\"true\"` is the same typo the presentation keys already \
             refuse - matching only the BOOL let it launch as `false`"
    );
    assert!(
        by("OneTrue").unsupported_keys.is_empty(),
        "but one member has no siblings to stop, so the same value is \
             honoured by doing nothing: {:?}",
        by("OneTrue").unsupported_keys
    );
    assert!(
        by("TwoFalse").unsupported_keys.is_empty(),
        "and `false` is VS Code's default, which croft already does: {:?}",
        by("TwoFalse").unsupported_keys
    );
    assert!(
        by("TwoAbsent").unsupported_keys.is_empty(),
        "as is omitting it: {:?}",
        by("TwoAbsent").unsupported_keys
    );
}

/// #318: a compound's own `preLaunchTask` is now read and run, so it is no
/// longer a reason to refuse the launch. The VALUE has to survive parsing
/// — recording only that the key was present was enough to refuse, and is
/// not enough to honour it.
#[test]
fn a_compounds_pre_launch_task_is_captured_and_no_longer_unsupported() {
    let text = r#"{
            "configurations": [ { "name": "A", "type": "node", "program": "a.js" } ],
            "compounds": [
                { "name": "Builds", "configurations": ["A"], "preLaunchTask": "build" },
                { "name": "Nulled", "configurations": ["A"], "preLaunchTask": null },
                { "name": "Empty", "configurations": ["A"], "preLaunchTask": "" },
                { "name": "Presents", "configurations": ["A"], "presentation": { "order": 1 } }
            ]
        }"#;
    let cs = parse_compounds(text, ".vscode/launch.json");
    let by = |n: &str| {
        cs.iter()
            .find(|c| c.name == n)
            .unwrap_or_else(|| panic!("{n} parsed"))
    };

    assert_eq!(
        by("Builds").pre_launch_task.as_deref(),
        Some("build"),
        "the task's NAME is what running it needs; the key alone cannot"
    );
    assert!(
        by("Builds").unsupported_keys.is_empty(),
        "preLaunchTask is honoured now, so it must stop being a refusal              reason: {:?}",
        by("Builds").unsupported_keys
    );

    // Requesting nothing stays nothing: null and "" must not park a launch
    // behind a task that does not exist.
    assert_eq!(by("Nulled").pre_launch_task, None);
    assert_eq!(by("Empty").pre_launch_task, None);

    // `presentation.order` is HONOURED now (#318), so it must stop being
    // a refusal reason - and it must be read, not merely tolerated.
    assert!(
        by("Presents").unsupported_keys.is_empty(),
        "order is honoured now and must stop refusing: {:?}",
        by("Presents").unsupported_keys
    );
    assert_eq!(
        by("Presents").presentation.as_ref().and_then(|p| p.order),
        Some(1.0),
        "the order's VALUE is what sorting needs; tolerating the key cannot"
    );
    assert_eq!(by("Presents").pre_launch_task, None);
}

#[test]
fn presentation_keys_are_honoured_and_only_unreadable_values_refuse() {
    let text = r#"{
          "compounds": [
            { "name": "Hid",   "configurations": ["A"], "presentation": { "hidden": true } },
            { "name": "Shown", "configurations": ["A"], "presentation": { "hidden": false } },
            { "name": "Grouped", "configurations": ["A"], "presentation": { "group": "g" } },
            { "name": "Ordered", "configurations": ["A"], "presentation": { "order": 2 } },
            { "name": "Both",  "configurations": ["A"],
              "presentation": { "hidden": true, "order": 1 } },
            { "name": "Empty", "configurations": ["A"], "presentation": {} }
          ]
        }"#;
    let cs = parse_compounds(text, "test");
    let by = |n: &str| cs.iter().find(|c| c.name == n).expect("compound present");

    assert!(by("Hid").hidden(), "hidden: true asks not to be listed");
    assert!(
        !by("Shown").hidden(),
        "hidden: false is the default, spelled out"
    );
    assert!(
        !by("Empty").hidden(),
        "an empty presentation asks for nothing"
    );

    // A compound asking ONLY to be hidden is fully served, so it must not
    // be refused for a capability it never requested.
    assert!(
        by("Hid").unsupported_keys.is_empty(),
        "hidden alone is honoured, so nothing is unsupported: {:?}",
        by("Hid").unsupported_keys
    );
    assert!(
        by("Empty").unsupported_keys.is_empty(),
        "an empty presentation refuses nothing"
    );

    // `group` and `order` are honoured now (#318): read, not refused.
    assert!(
        by("Grouped").unsupported_keys.is_empty(),
        "group no longer refuses: {:?}",
        by("Grouped").unsupported_keys
    );
    assert_eq!(
        by("Grouped")
            .presentation
            .as_ref()
            .and_then(|p| p.group.clone()),
        Some(String::from("g")),
        "and its VALUE is what places the row"
    );
    assert!(by("Ordered").unsupported_keys.is_empty());
    assert_eq!(
        by("Ordered").presentation.as_ref().and_then(|p| p.order),
        Some(2.0)
    );
    // A malformed shape must keep refusing. On main these refused and the
    // user found out; silently launching a compound whose typo'd
    // `{"hidden": "true"}` did nothing would be a regression dressed as
    // a feature.
    let bad = parse_compounds(
        r#"{
              "compounds": [
                { "name": "QuotedBool", "configurations": ["A"],
                  "presentation": { "hidden": "true" } },
                { "name": "NotAnObject", "configurations": ["A"],
                  "presentation": "grouped" }
              ]
            }"#,
        "test",
    );
    let bad_by = |n: &str| bad.iter().find(|c| c.name == n).expect("present");
    assert!(
        !bad_by("QuotedBool").hidden(),
        "a quoted bool is not a bool"
    );
    assert_eq!(
        bad_by("QuotedBool").unsupported_keys,
        vec!["presentation.hidden"],
        "and it refuses rather than launching unhidden in silence"
    );
    assert_eq!(
        bad_by("NotAnObject").unsupported_keys,
        vec!["presentation"],
        "a non-object presentation refuses under its own name"
    );

    assert!(by("Both").hidden(), "Both asks to be hidden");
    assert!(
        by("Both").unsupported_keys.is_empty(),
        "and both halves are honoured now, so nothing refuses: {:?}",
        by("Both").unsupported_keys
    );
}

/// A value croft cannot READ is a typo, whichever key carries it (#318).
/// `{"order": "1"}` is the same mistake as `{"hidden": "true"}`, and
/// dropping it silently would leave a picker that ignores the ordering the
/// user wrote with nothing to explain why.
#[test]
fn a_malformed_presentation_value_refuses_whichever_key_it_is_on() {
    let cs = parse_compounds(
        r#"{
              "compounds": [
                { "name": "QuotedOrder", "configurations": ["A"],
                  "presentation": { "order": "1" } },
                { "name": "NumberGroup", "configurations": ["A"],
                  "presentation": { "group": 7 } },
                { "name": "FractionalOrder", "configurations": ["A"],
                  "presentation": { "order": 1.5 } },
                { "name": "NulledKeys", "configurations": ["A"],
                  "presentation": { "group": null, "order": null } }
              ]
            }"#,
        ".vscode/launch.json",
    );
    assert_eq!(cs.len(), 4, "precondition: every compound parsed");
    let by = |n: &str| {
        cs.iter()
            .find(|c| c.name == n)
            .unwrap_or_else(|| panic!("{n} parsed"))
    };
    assert_eq!(
        by("QuotedOrder").unsupported_keys,
        vec!["presentation.order"]
    );
    assert_eq!(
        by("NumberGroup").unsupported_keys,
        vec!["presentation.group"]
    );
    // A FRACTION is not a typo: VS Code's schema says `number` and its
    // comparator subtracts, so 1.5 is a legal position between two
    // neighbours and must be honoured rather than refused.
    assert!(
        by("FractionalOrder").unsupported_keys.is_empty(),
        "a fractional order is a position, not a mistake: {:?}",
        by("FractionalOrder").unsupported_keys
    );
    assert_eq!(
        by("FractionalOrder")
            .presentation
            .as_ref()
            .and_then(|p| p.order),
        Some(1.5)
    );
    // Explicit nulls ask for nothing, which croft delivers by doing
    // nothing - naming them would report a limitation that is not one.
    assert!(
        by("NulledKeys").unsupported_keys.is_empty(),
        "null asks for nothing: {:?}",
        by("NulledKeys").unsupported_keys
    );
}

/// #318: the picker's order is VS Code's `getVisibleAndSorted`, which is a
/// transcription and not a guess. Each arm below is a rule croft would
/// plausibly have got wrong on its own.
#[test]
fn presentation_sorts_the_picker_the_way_vs_code_does() {
    fn p(group: Option<&str>, order: Option<f64>) -> Option<Presentation> {
        Some(Presentation {
            hidden: false,
            group: group.map(str::to_string),
            order,
        })
    }
    let items: Vec<(&str, Option<Presentation>)> = vec![
        ("plain-first", None),
        ("b-group", p(Some("b"), None)),
        ("no-group-ord-2", p(None, Some(2.0))),
        (
            "hidden",
            Some(Presentation {
                hidden: true,
                group: None,
                order: None,
            }),
        ),
        ("a-group-ord-9", p(Some("a"), Some(9.0))),
        ("plain-second", None),
        ("a-group-ord-1", p(Some("a"), Some(1.0))),
        ("a-group-no-ord", p(Some("a"), None)),
        ("no-group-ord-1", p(None, Some(1.0))),
    ];
    let sorted: Vec<&str> = visible_and_sorted(items, |(_, p)| p.as_ref())
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    assert_eq!(
        sorted,
        vec![
            // grouped first, groups by name, order ascending within one,
            // and a missing order last inside its group
            "a-group-ord-1",
            "a-group-ord-9",
            "a-group-no-ord",
            "b-group",
            // a presentation WITHOUT a group still beats no presentation
            "no-group-ord-1",
            "no-group-ord-2",
            // and entries that asked for nothing keep their file order
            "plain-first",
            "plain-second",
        ],
        "hidden is dropped, and every other rule is VS Code's"
    );
}

/// An empty `presentation` is still a `presentation`, and VS Code places
/// it accordingly: its comparator tests the OBJECT for truthiness, so
/// `{}` sorts ahead of an entry carrying no block at all. Reading it as
/// "no preference" would put those rows somewhere VS Code does not.
#[test]
fn an_empty_presentation_block_still_outranks_no_block() {
    let cfgs = parse_launch_json(
        r#"{ "configurations": [
                { "name": "Bare", "type": "node", "program": "a.js" },
                { "name": "EmptyBlock", "type": "node", "program": "b.js",
                  "presentation": {} },
                { "name": "OnlyShown", "type": "node", "program": "c.js",
                  "presentation": { "hidden": false } }
            ] }"#,
        ".vscode/launch.json",
    );
    let by = |n: &str| {
        cfgs.iter()
            .find(|c| c.name == n)
            .unwrap_or_else(|| panic!("{n} parsed"))
            .clone()
    };
    assert_eq!(by("Bare").presentation, None, "no block is no block");
    assert_eq!(
        by("EmptyBlock").presentation,
        Some(Presentation::default()),
        "an empty block asks for nothing but is still declared"
    );
    assert_eq!(by("OnlyShown").presentation, Some(Presentation::default()));

    let sorted: Vec<String> = visible_and_sorted(cfgs, |c| c.presentation.as_ref())
        .into_iter()
        .map(|c| c.name)
        .collect();
    assert_eq!(
        sorted,
        vec!["EmptyBlock", "OnlyShown", "Bare"],
        "both declared blocks sort ahead of the bare entry, in file order"
    );
}

/// A key that REQUESTS NOTHING is honoured by doing nothing, so it must
/// not be named in a refusal - that would report a limitation croft does
/// not have. `false` and `""` are that shape, and both used to launch
/// before `presentation` gained its own reader.
#[test]
fn a_presentation_that_requests_nothing_is_not_a_refusal() {
    let cs = parse_compounds(
        r#"{
              "compounds": [
                { "name": "FalseBlock", "configurations": ["A"], "presentation": false },
                { "name": "EmptyString", "configurations": ["A"], "presentation": "" },
                { "name": "NulledBlock", "configurations": ["A"], "presentation": null },
                { "name": "Truthy", "configurations": ["A"], "presentation": true }
              ]
            }"#,
        ".vscode/launch.json",
    );
    assert_eq!(cs.len(), 4, "precondition: every compound parsed");
    let by = |n: &str| {
        cs.iter()
            .find(|c| c.name == n)
            .unwrap_or_else(|| panic!("{n} parsed"))
    };
    for name in ["FalseBlock", "EmptyString", "NulledBlock"] {
        assert!(
            by(name).unsupported_keys.is_empty(),
            "{name} asks for nothing and must launch: {:?}",
            by(name).unsupported_keys
        );
    }
    // The control: a value that DOES claim to say something, and is still
    // unreadable, must refuse - or the three above prove only that this
    // function never records anything.
    assert_eq!(
        by("Truthy").unsupported_keys,
        vec!["presentation"],
        "a non-object that asks for something is unreadable"
    );
}

/// The property that must not break: a launch.json that says nothing
/// about presentation is left exactly as written.
#[test]
fn a_workspace_with_no_presentation_keeps_its_file_order() {
    let items: Vec<(&str, Option<Presentation>)> = vec![("z", None), ("a", None), ("m", None)];
    let sorted: Vec<&str> = visible_and_sorted(items, |(_, p)| p.as_ref())
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    assert_eq!(
        sorted,
        vec!["z", "a", "m"],
        "sorting must be stable, or introducing the feature would silently \
             reorder every existing workspace's picker"
    );
}

/// A configuration carries `presentation` too, and sorts against
/// compounds in one list - VS Code puts both kinds through the same
/// comparator, so a config and a compound naming one group belong
/// together.
#[test]
fn a_configuration_reads_presentation_like_a_compound_does() {
    let cfgs = parse_launch_json(
        r#"{ "configurations": [
                { "name": "Plain", "type": "node", "program": "a.js" },
                { "name": "Grouped", "type": "node", "program": "b.js",
                  "presentation": { "group": "servers", "order": 3 } },
                { "name": "Hid", "type": "node", "program": "c.js",
                  "presentation": { "hidden": true } }
            ] }"#,
        ".vscode/launch.json",
    );
    let by = |n: &str| {
        cfgs.iter()
            .find(|c| c.name == n)
            .unwrap_or_else(|| panic!("{n} parsed"))
            .clone()
    };
    assert_eq!(by("Plain").presentation, None, "no block asks for nothing");
    assert_eq!(
        by("Grouped").presentation,
        Some(Presentation {
            hidden: false,
            group: Some(String::from("servers")),
            order: Some(3.0),
        })
    );
    assert!(by("Hid").presentation.is_some_and(|p| p.hidden));
}

/// #1425: a multi-root workspace, `api` and `web`, each with its own
/// launch.json.
fn multi_root() -> (tempfile::TempDir, Vec<(String, PathBuf)>) {
    let tmp = tempfile::tempdir().unwrap();
    for (folder, text) in [
        (
            "api",
            r#"{ "configurations": [
                    { "name": "Run api", "type": "debugpy", "request": "launch", "program": "${workspaceFolder}/main.py" }
                ],
                "compounds": [
                    { "name": "API + web", "configurations": ["Run api", { "folder": "web", "name": "Run web" }] },
                    { "name": "Nowhere", "configurations": [{ "folder": "docs", "name": "Run web" }] },
                    { "name": "Missing", "configurations": [{ "folder": "web", "name": "Run api" }] }
                ] }"#,
        ),
        (
            "web",
            r#"{ "configurations": [
                    { "name": "Run web", "type": "debugpy", "request": "launch", "program": "${workspaceFolder}/main.py" }
                ] }"#,
        ),
    ] {
        std::fs::create_dir_all(tmp.path().join(folder).join(".vscode")).unwrap();
        std::fs::write(tmp.path().join(folder).join(".vscode/launch.json"), text).unwrap();
    }
    let roots = [tmp.path().join("api"), tmp.path().join("web")];
    let folders = workspace_folders(roots.iter().map(PathBuf::as_path));
    (tmp, folders)
}

/// #1425: a member written `{ "folder": "web", "name": "Run web" }`
/// resolves to web's own configuration, with `${workspaceFolder}` the
/// web root; a bare member still resolves beside the compound.
#[test]
fn a_folder_member_resolves_in_that_workspace_folder() {
    let (tmp, folders) = multi_root();
    let api = tmp.path().join("api");
    let compounds = discover_compounds(&api);
    assert_eq!(compounds[0].folders, vec![None, Some(String::from("web"))]);
    let members = resolve_compound(&compounds[0], &discover_configs_all(&api), &folders).unwrap();
    assert_eq!(
        members.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
        vec!["Run api", "Run web"]
    );
    assert_eq!(members[0].root, None);
    let web = tmp.path().join("web");
    assert_eq!(members[1].root.as_deref(), Some(web.as_path()));
    let ctx = SubstCtx {
        workspace_folder: members[1].root.clone().unwrap(),
        file: None,
        inputs: Default::default(),
        folders: folders.clone(),
    };
    let rc = resolve(&members[1], &ctx).unwrap();
    assert_eq!(
        rc.program.as_deref(),
        Some(web.join("main.py").to_string_lossy().as_ref())
    );
    assert_eq!(
        rc.cwd.as_deref(),
        Some(web.as_path()),
        "a foreign member with no cwd runs in its own folder"
    );
}

/// #1425, negative: a folder the workspace does not have, or a name
/// that folder's launch.json does not declare, is refused by name
/// rather than resolved against the compound's own file.
#[test]
fn a_folder_member_with_no_such_folder_or_config_is_refused() {
    let (tmp, folders) = multi_root();
    let api = tmp.path().join("api");
    let compounds = discover_compounds(&api);
    let all = discover_configs_all(&api);
    let err = resolve_compound(&compounds[1], &all, &folders).unwrap_err();
    assert_eq!(
        err,
        "compound \"Nowhere\" names folder \"docs\", which is not a folder of this workspace"
    );
    let err = resolve_compound(&compounds[2], &all, &folders).unwrap_err();
    assert_eq!(
        err,
        "compound \"Missing\" names configuration \"Run api\" in folder \"web\", which no launch.json there declares"
    );
    // A non-string folder is a malformed member: the compound is dropped.
    let bad = parse_compounds(
        r#"{ "compounds": [{ "name": "Bad", "configurations": [{ "folder": 3, "name": "x" }] }] }"#,
        ".vscode/launch.json",
    );
    assert!(bad.is_empty());
}

/// #1425: `${workspaceFolder:web}` is the web root; an unknown folder
/// name is an error naming it.
#[test]
fn workspace_folder_by_name_expands_or_names_the_missing_folder() {
    let (tmp, folders) = multi_root();
    let ctx = SubstCtx {
        workspace_folder: tmp.path().join("api"),
        file: None,
        inputs: Default::default(),
        folders,
    };
    assert_eq!(
        substitute("${workspaceFolder:web}/main.py", &ctx).unwrap(),
        tmp.path().join("web/main.py").to_string_lossy()
    );
    assert_eq!(
        substitute("${workspaceFolder}", &ctx).unwrap(),
        tmp.path().join("api").to_string_lossy()
    );
    assert_eq!(
        substitute("${workspaceFolder:docs}/x", &ctx).unwrap_err(),
        "${workspaceFolder:docs}: no workspace folder is named \"docs\""
    );
}

/// A compound resolves to its members in the order written, so the picker
/// and the launch sequence agree with the file.
#[test]
fn a_resolved_compound_yields_its_members_in_declaration_order() {
    let text = r#"{
            "configurations": [
                { "name": "Client", "type": "node", "program": "c.js" },
                { "name": "Server", "type": "node", "program": "s.js" }
            ],
            "compounds": [ { "name": "Both", "configurations": ["Server", "Client"] } ]
        }"#;
    let cfgs = parse_launch_json(text, ".vscode/launch.json");
    let compounds = parse_compounds(text, ".vscode/launch.json");
    let members = resolve_compound(&compounds[0], &cfgs, &[]).unwrap();
    assert_eq!(
        members.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
        vec!["Server", "Client"],
        "compound order wins over the order in `configurations`"
    );
}

/// #250: the flow asks what each request needs, in order: a launch its
/// program and the optional rest, an attach only where to attach.
#[test]
fn a_draft_asks_for_what_its_request_needs() {
    use DraftField::*;
    let fields = |d: &ConfigDraft| {
        let mut out = Vec::new();
        let mut at = d.next_field(None);
        while let Some(f) = at {
            out.push(f);
            at = d.next_field(Some(f));
        }
        out
    };
    let launch = ConfigDraft::new("python", RequestKind::Launch);
    assert_eq!(
        fields(&launch),
        [Name, Program, Args, Cwd, Env, PreLaunchTask]
    );
    for t in ["python", "node"] {
        assert_eq!(
            fields(&ConfigDraft::new(t, RequestKind::Attach)),
            [Name, Port]
        );
    }
    for t in ["lldb", "go"] {
        assert_eq!(
            fields(&ConfigDraft::new(t, RequestKind::Attach)),
            [Name, ProcessId]
        );
    }
    // Every offered type resolves to an adapter.
    for (t, _) in DRAFT_TYPES {
        assert!(adapter_for_type(t).is_some(), "{t}");
    }
}

/// #250: typed values are checked as they are entered, and what is
/// written reads back as the configuration that was described.
#[test]
fn a_launch_draft_writes_an_entry_that_reads_back() {
    let mut d = ConfigDraft::new("python", RequestKind::Launch);
    assert_eq!(d.prompt(DraftField::Name).2, "Launch Python");
    assert_eq!(d.prompt(DraftField::Program).2, "${file}");
    d.set(DraftField::Name, "Serve").unwrap();
    d.set(DraftField::Program, "${workspaceFolder}/api.py")
        .unwrap();
    d.set(DraftField::Args, r#"--port 8000 "a b" 'c d'"#)
        .unwrap();
    assert_eq!(d.args, ["--port", "8000", "a b", "c d"]);
    assert!(d.set(DraftField::Args, r#"--x "open"#).is_err());
    d.set(DraftField::Cwd, "").unwrap();
    d.set(DraftField::Env, "DEBUG=1; URL=http://x?a=b").unwrap();
    assert!(d.set(DraftField::Env, "NOEQUALS").is_err());
    d.set(DraftField::PreLaunchTask, "build").unwrap();
    let text = serde_json::to_string(&json!({ "configurations": [d.to_json()] })).unwrap();
    let cfgs = parse_launch_json(&text, ".croft/launch.json");
    assert_eq!(cfgs.len(), 1);
    assert_eq!(cfgs[0].name, "Serve");
    assert_eq!(cfgs[0].request, RequestKind::Launch);
    let raw = d.to_json();
    assert_eq!(raw["args"], json!(["--port", "8000", "a b", "c d"]));
    assert_eq!(raw["env"], json!({ "DEBUG": "1", "URL": "http://x?a=b" }));
    assert_eq!(raw["preLaunchTask"], json!("build"));
    assert!(raw.get("cwd").is_none(), "a blank field is left out");
}

/// #250: each adapter is attached to the way it reads the request.
#[test]
fn an_attach_draft_uses_each_adapters_own_fields() {
    let attach = |t: &str, field: DraftField, value: &str| {
        let mut d = ConfigDraft::new(t, RequestKind::Attach);
        d.set(DraftField::Name, "A").unwrap();
        d.set(field, value).unwrap();
        d.to_json()
    };
    assert_eq!(
        attach("python", DraftField::Port, "5678")["port"],
        json!(5678)
    );
    assert_eq!(
        attach("node", DraftField::Port, "9229")["port"],
        json!(9229)
    );
    assert_eq!(
        attach("lldb", DraftField::ProcessId, "42")["processId"],
        json!(42)
    );
    let go = attach("go", DraftField::ProcessId, "7");
    assert_eq!(
        (go["processId"].clone(), go["mode"].clone()),
        (json!(7), json!("local"))
    );
    // And each resolves to the attach request its adapter is sent.
    let resolved = |t: &str, field: DraftField, value: &str| {
        let text = json!({ "configurations": [attach(t, field, value)] }).to_string();
        resolve(&config(&text), &ctx()).unwrap()
    };
    let py = debugpy_request(
        &resolved("python", DraftField::Port, "5678"),
        Path::new("/usr/bin/python3"),
    );
    assert_eq!(py["arguments"]["connect"]["port"], 5678);
    let lldb = lldb_request(&resolved("lldb", DraftField::ProcessId, "42"));
    assert_eq!(lldb["arguments"]["pid"], 42);
    let dlv = delve_request(&resolved("go", DraftField::ProcessId, "7"));
    assert_eq!(dlv["arguments"]["processId"], 7);
    let mut d = ConfigDraft::new("node", RequestKind::Attach);
    assert!(d.set(DraftField::Port, "0").is_err());
    assert!(d.set(DraftField::Port, "70000").is_err());
    assert!(d.set(DraftField::ProcessId, "pid").is_err());
}

/// #250: the entry goes into `.croft/launch.json`, created when absent
/// and added to when present; a duplicate name and a file whose
/// comments rewriting would lose are refused, the file untouched.
#[test]
fn a_draft_is_added_to_croft_launch_json() {
    let tmp = tempfile::tempdir().unwrap();
    let mut d = ConfigDraft::new("node", RequestKind::Launch);
    d.set(DraftField::Name, "Web").unwrap();
    d.set(DraftField::Program, "${workspaceFolder}/index.js")
        .unwrap();
    let path = add_to_croft_launch_json(tmp.path(), &d).unwrap();
    assert_eq!(path, tmp.path().join(".croft/launch.json"));
    let mut d2 = d.clone();
    d2.name = String::from("Web 2");
    add_to_croft_launch_json(tmp.path(), &d2).unwrap();
    let names: Vec<String> = discover_configs(tmp.path())
        .into_iter()
        .map(|c| c.name)
        .collect();
    assert_eq!(names, ["Web", "Web 2"]);
    let err = add_to_croft_launch_json(tmp.path(), &d).unwrap_err();
    assert!(err.contains("already has"), "{err}");
    // A bare list is added to as a list.
    let bare = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(bare.path().join(".croft")).unwrap();
    std::fs::write(bare.path().join(".croft/launch.json"), "[]").unwrap();
    add_to_croft_launch_json(bare.path(), &d).unwrap();
    assert_eq!(discover_configs(bare.path()).len(), 1);
    // Comments would be lost: refused, and the file is as it was.
    let jsonc = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(jsonc.path().join(".croft")).unwrap();
    let text = "{ // mine\n \"configurations\": [] }";
    std::fs::write(jsonc.path().join(".croft/launch.json"), text).unwrap();
    let err = add_to_croft_launch_json(jsonc.path(), &d).unwrap_err();
    assert!(err.contains("by hand") && err.contains("\"Web\""), "{err}");
    // Written aside and renamed: nothing is left beside the file.
    let leftovers: Vec<_> = std::fs::read_dir(tmp.path().join(".croft"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(leftovers, [std::ffi::OsString::from("launch.json")]);
    assert_eq!(
        std::fs::read_to_string(jsonc.path().join(".croft/launch.json")).unwrap(),
        text
    );
}

/// #250: `.croft` is workspace content, so a symlink there or at the
/// file is refused rather than followed, and its target is untouched.
#[cfg(unix)]
#[test]
fn adding_a_configuration_refuses_a_symlinked_croft_launch_json() {
    let mut d = ConfigDraft::new("node", RequestKind::Launch);
    d.set(DraftField::Name, "Web").unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let target = elsewhere.path().join("victim.json");
    std::fs::write(&target, "[]").unwrap();
    let file_link = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(file_link.path().join(".croft")).unwrap();
    std::os::unix::fs::symlink(&target, file_link.path().join(".croft/launch.json")).unwrap();
    let err = add_to_croft_launch_json(file_link.path(), &d).unwrap_err();
    assert!(err.contains("symlink"), "{err}");
    let dir_link = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(elsewhere.path(), dir_link.path().join(".croft")).unwrap();
    let err = add_to_croft_launch_json(dir_link.path(), &d).unwrap_err();
    assert!(err.contains("symlink"), "{err}");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "[]");
    assert!(!elsewhere.path().join("launch.json").exists());

    // A link planted at the temp file's name is not written through: the
    // real file keeps what it had until the new one replaces it whole.
    let planted = tempfile::tempdir().unwrap();
    let dir = planted.path().join(".croft");
    std::fs::create_dir_all(&dir).unwrap();
    let mut first = ConfigDraft::new("node", RequestKind::Launch);
    first.set(DraftField::Name, "First").unwrap();
    add_to_croft_launch_json(planted.path(), &first).unwrap();
    let before = std::fs::read_to_string(dir.join("launch.json")).unwrap();
    let bait = elsewhere.path().join("bait.json");
    std::fs::write(&bait, "bait").unwrap();
    std::os::unix::fs::symlink(&bait, temp_candidate(&dir, 0)).unwrap();
    add_to_croft_launch_json(planted.path(), &d).unwrap();
    assert_eq!(
        std::fs::read_to_string(&bait).unwrap(),
        "bait",
        "the link is not followed"
    );
    let names: Vec<String> = discover_configs(planted.path())
        .into_iter()
        .map(|c| c.name)
        .collect();
    assert_eq!(names, ["First", "Web"], "and the file is whole: {before}");
}

/// #1424: `${input:id}` takes the answer for `id`, each id is listed
/// once in the order it first appears, and the file's `inputs` say how
/// to ask: `pickString` options (strings or label/value), defaults and
/// `password`.
#[test]
fn input_variables_take_the_answers() {
    let text = r#"{ "configurations": [
            { "name": "Run", "type": "python", "program": "app.py",
              "args": ["--env", "${input:envName}", "--user", "${input:userName}"],
              "env": { "ENV": "${input:envName}" } }
        ],
        "inputs": [
            { "id": "envName", "type": "pickString", "description": "Environment",
              "options": ["dev", { "label": "Staging", "value": "stg" }], "default": "dev" },
            { "id": "userName", "type": "promptString", "default": "alice", "password": true },
            { "id": "branch", "type": "command", "command": "git.branch" }
        ]}"#;
    let cfg = config(text);
    assert_eq!(cfg.input_refs(), ["envName", "userName"]);
    let ask = inputs_to_ask(&cfg.input_refs(), &BTreeMap::new(), &cfg.inputs).unwrap();
    assert_eq!(ask.len(), 2);
    assert_eq!(ask[0].description, "Environment");
    assert_eq!(ask[0].default.as_deref(), Some("dev"));
    assert_eq!(
        ask[0].kind,
        InputKind::Pick {
            options: vec![
                (String::from("dev"), String::from("dev")),
                (String::from("Staging"), String::from("stg")),
            ]
        }
    );
    assert_eq!(ask[1].description, "userName", "no description: the id");
    assert_eq!(ask[1].kind, InputKind::Prompt { password: true });
    assert_eq!(
        cfg.inputs[2].kind,
        InputKind::Command {
            command: String::from("git.branch")
        }
    );
    let mut c = ctx();
    c.inputs
        .insert(String::from("envName"), String::from("stg"));
    c.inputs
        .insert(String::from("userName"), String::from("bob"));
    let rc = resolve(&cfg, &c).unwrap();
    assert_eq!(rc.args, ["--env", "stg", "--user", "bob"]);
    assert_eq!(rc.env.get("ENV").map(String::as_str), Some("stg"));
    let answered = BTreeMap::from([(String::from("envName"), String::from("x"))]);
    let ask = inputs_to_ask(&cfg.input_refs(), &answered, &cfg.inputs).unwrap();
    assert_eq!(ask.len(), 1, "an answered input is not asked again");
    assert_eq!(ask[0].id, "userName");
}

/// #1424 negatives: an id no entry declares is an error naming it; an
/// unanswered input refuses the config rather than passing its text;
/// entries with no id, an unknown type or no options are not inputs;
/// `${input:}` text outside a variable is no reference.
#[test]
fn an_unknown_or_unanswered_input_is_refused() {
    let text = r#"{ "configurations": [
            { "name": "Run", "type": "python", "program": "app.py",
              "args": ["${input:nope}", "$input:plain", "${inputs:x}"] }
        ],
        "inputs": [
            { "type": "promptString" },
            { "id": "odd", "type": "colorPicker" },
            { "id": "empty", "type": "pickString", "options": [] },
            { "id": "nope2", "type": "promptString" }
        ]}"#;
    let cfg = config(text);
    assert!(
        cfg.inputs.len() == 1 && cfg.inputs[0].id == "nope2",
        "{:?}",
        cfg.inputs
    );
    assert_eq!(cfg.input_refs(), ["nope"]);
    let err = inputs_to_ask(&cfg.input_refs(), &BTreeMap::new(), &cfg.inputs).unwrap_err();
    assert!(
        err.contains("${input:nope}") && err.contains("\"nope\""),
        "{err}"
    );
    let err = resolve(&cfg, &ctx()).unwrap_err();
    assert!(err.contains("${input:nope}: no answer"), "{err}");
    let none = config(r#"[{ "name": "Plain", "type": "python", "program": "a.py" }]"#);
    assert!(none.input_refs().is_empty() && none.inputs.is_empty());
}

fn attach_address(cfg: &str) -> Option<(String, u16)> {
    let rc = resolve(&config(cfg), &ctx()).unwrap();
    debugpy_connect_address(&debugpy_request(&rc, Path::new("python3")))
}

/// #1417: a debugpy attach with `connect` (or the older `host`/`port`)
/// goes to that address, the `debugpy --listen` process's own adapter.
#[test]
fn a_debugpy_connect_attach_goes_to_the_listening_port() {
    assert_eq!(
        attach_address(
            r#"[{ "name": "R", "type": "debugpy", "request": "attach",
                     "connect": { "host": "127.0.0.1", "port": 5679 } }]"#
        ),
        Some((String::from("127.0.0.1"), 5679))
    );
    assert_eq!(
        attach_address(
            r#"[{ "name": "R", "type": "debugpy", "request": "attach",
                     "connect": { "host": "box", "port": "5680" } }]"#
        ),
        Some((String::from("box"), 5680))
    );
    assert_eq!(
        attach_address(
            r#"[{ "name": "R", "type": "python", "request": "attach",
                     "host": "10.0.0.5", "port": 5678 }]"#
        ),
        Some((String::from("10.0.0.5"), 5678))
    );
    assert_eq!(
        attach_address(
            r#"[{ "name": "R", "type": "debugpy", "request": "attach", "connect": { "port": 5678 } }]"#
        ),
        Some((String::from("127.0.0.1"), 5678)),
        "no host is this machine"
    );
}

/// #1417 negatives: the `listen` form (the debuggee connects back), a
/// launch, an attach by process id and a port that is not one keep the
/// local adapter.
#[test]
fn a_debugpy_listen_attach_or_launch_keeps_the_local_adapter() {
    for cfg in [
        r#"[{ "name": "L", "type": "debugpy", "request": "attach", "listen": { "host": "0.0.0.0", "port": 5678 } }]"#,
        r#"[{ "name": "P", "type": "debugpy", "request": "launch", "program": "a.py", "connect": { "port": 5678 } }]"#,
        r#"[{ "name": "I", "type": "debugpy", "request": "attach", "processId": 4242 }]"#,
        r#"[{ "name": "B", "type": "debugpy", "request": "attach", "connect": { "port": 70000 } }]"#,
    ] {
        assert_eq!(attach_address(cfg), None, "{cfg}");
    }
}

/// #1391: a node launch that starts an npm script names no `program`;
/// js-debug runs its `runtimeExecutable` and `runtimeArgs` instead.
#[test]
fn a_node_launch_can_start_an_npm_script() {
    let cfg = config(
        r#"[{ "name": "npm start", "type": "node", "request": "launch",
                 "runtimeExecutable": "npm", "runtimeArgs": ["run", "start"],
                 "cwd": "${workspaceFolder}" }]"#,
    );
    let rc = resolve(&cfg, &ctx()).unwrap();
    assert!(js_launch_has_target(&rc));
    let req = js_request(&rc);
    assert_eq!(req["arguments"]["type"], "pwa-node");
    assert_eq!(req["arguments"]["runtimeExecutable"], "npm");
    assert_eq!(req["arguments"]["runtimeArgs"], json!(["run", "start"]));
    assert!(req["arguments"].get("program").is_none());
}

/// #1391: a `node-terminal` launch's `command` is the runtime js-debug
/// starts, split into words, or run by `sh -c` when it uses shell syntax.
#[test]
fn a_node_terminal_command_becomes_the_runtime() {
    let rc = |command: &str| {
        let text = format!(
            r#"[{{ "name": "dev", "type": "node-terminal", "request": "launch", "command": {} }}]"#,
            json!(command)
        );
        resolve(&config(&text), &ctx()).unwrap()
    };
    let dev = rc("npm run dev -- --port 3001");
    assert!(js_launch_has_target(&dev));
    let req = js_request(&dev);
    assert_eq!(req["arguments"]["type"], "pwa-node");
    assert_eq!(req["arguments"]["runtimeExecutable"], "npm");
    assert_eq!(
        req["arguments"]["runtimeArgs"],
        json!(["run", "dev", "--", "--port", "3001"])
    );
    assert!(req["arguments"].get("command").is_none());
    let quoted = js_request(&rc("node 'my app.js'"));
    assert_eq!(quoted["arguments"]["runtimeArgs"], json!(["my app.js"]));
    let chained = js_request(&rc("npm run build && npm start"));
    assert_eq!(chained["arguments"]["runtimeExecutable"], "sh");
    assert_eq!(
        chained["arguments"]["runtimeArgs"],
        json!(["-c", "npm run build && npm start"])
    );
    let env = js_request(&rc("NODE_ENV=production npm start"));
    assert_eq!(env["arguments"]["runtimeExecutable"], "sh");
    assert_eq!(
        env["arguments"]["runtimeArgs"],
        json!(["-c", "NODE_ENV=production npm start"])
    );
    let flag = js_request(&rc("node --max-old-space-size=4096 app.js"));
    assert_eq!(flag["arguments"]["runtimeExecutable"], "node");
}

/// #1391 negatives: a node launch with nothing to run still has no
/// target; `command` means nothing on a plain `node` config; an
/// explicit `runtimeExecutable` on `node-terminal` wins over `command`;
/// an empty command adds nothing.
#[test]
fn a_node_launch_with_nothing_to_run_has_no_target() {
    let one = |text: &str| resolve(&config(text), &ctx()).unwrap();
    let bare = one(r#"[{ "name": "n", "type": "node", "request": "launch" }]"#);
    assert!(!js_launch_has_target(&bare));
    let node = one(r#"[{ "name": "n", "type": "node", "command": "npm start" }]"#);
    assert!(
        !js_launch_has_target(&node),
        "only node-terminal reads command"
    );
    assert_eq!(node.extra["command"], "npm start");
    let explicit = one(
        r#"[{ "name": "t", "type": "node-terminal", "command": "npm start",
                 "runtimeExecutable": "yarn", "runtimeArgs": ["dev"] }]"#,
    );
    assert_eq!(explicit.extra["runtimeExecutable"], "yarn");
    assert_eq!(explicit.extra["runtimeArgs"], json!(["dev"]));
    assert!(!explicit.extra.contains_key("command"));
    let empty = one(r#"[{ "name": "t", "type": "node-terminal", "command": "  " }]"#);
    assert!(!js_launch_has_target(&empty));
    let hollow = one(r#"[{ "name": "n", "type": "node", "request": "launch",
                 "runtimeExecutable": null, "runtimeArgs": [] }]"#);
    assert!(!js_launch_has_target(&hollow));
    let blank = one(r#"[{ "name": "n", "type": "node", "runtimeExecutable": "" }]"#);
    assert!(!js_launch_has_target(&blank));
}

/// What `cargo build`/`cargo test --no-run --message-format=json` print
/// for a crate with a lib and a bin named `rc` (shapes from cargo 1.97).
const CARGO_JSON_1392: &str = concat!(
    r#"{"reason":"compiler-artifact","target":{"kind":["lib"],"name":"rc","src_path":"/w/src/lib.rs"},"profile":{"test":false},"executable":null}"#,
    "\n",
    r#"{"reason":"compiler-artifact","target":{"kind":["bin"],"name":"rc","src_path":"/w/src/main.rs"},"profile":{"test":false},"executable":"/w/target/debug/rc"}"#,
    "\n",
    r#"{"reason":"compiler-artifact","target":{"kind":["lib"],"name":"rc","src_path":"/w/src/lib.rs"},"profile":{"test":true},"executable":"/w/target/debug/deps/rc-1a2b"}"#,
    "\n",
    r#"{"reason":"build-finished","success":true}"#,
    "\n"
);

fn cargo_cfg(cargo: &str) -> ResolvedConfig {
    let text = format!(
        r#"[{{ "name": "Debug executable 'rc'", "type": "lldb", "request": "launch", "cargo": {cargo}, "args": [] }}]"#
    );
    resolve(&config(&text), &ctx()).unwrap()
}

/// #1392: a CodeLLDB `cargo` launch builds with cargo's JSON messages
/// and debugs the executable its filter picks; the block is croft's
/// build step, not something lldb-dap is sent.
#[test]
fn a_cargo_launch_debugs_the_executable_its_filter_picks() {
    let rc = cargo_cfg(
        r#"{ "args": ["build", "--bin=rc", "--package=rc"], "filter": { "name": "rc", "kind": "bin" },
                 "env": { "RUSTFLAGS": "-g" }, "cwd": "sub" }"#,
    );
    assert!(rc.program.is_none());
    let spec = cargo_spec(&rc).expect("a cargo block").unwrap();
    assert_eq!(
        cargo_build_args(&spec),
        ["build", "--bin=rc", "--package=rc", "--message-format=json"]
    );
    assert_eq!(spec.env.get("RUSTFLAGS").map(String::as_str), Some("-g"));
    assert_eq!(spec.cwd, Some(PathBuf::from("sub")));
    assert_eq!(
        cargo_artifact(CARGO_JSON_1392, &spec).unwrap(),
        PathBuf::from("/w/target/debug/rc")
    );
    let lib_tests = cargo_spec(&cargo_cfg(
        r#"{ "args": ["test", "--lib"], "filter": { "kind": "lib" } }"#,
    ))
    .unwrap()
    .unwrap();
    assert_eq!(
        cargo_build_args(&lib_tests),
        ["test", "--no-run", "--lib", "--message-format=json"]
    );
    assert_eq!(
        cargo_artifact(CARGO_JSON_1392, &lib_tests).unwrap(),
        PathBuf::from("/w/target/debug/deps/rc-1a2b")
    );
    let mut with_program = rc.clone();
    with_program.program = Some(String::from("/w/target/debug/rc"));
    let req = lldb_request(&with_program);
    assert!(req["arguments"].get("cargo").is_none(), "{req}");
    assert_eq!(req["arguments"]["program"], "/w/target/debug/rc");
}

/// #1392 negatives: no executable or several matching the filter is an
/// error naming it; a `cargo` block with no args is refused; a config
/// without one has no cargo step.
#[test]
fn a_cargo_launch_refuses_no_match_or_several() {
    let spec = |cargo: &str| cargo_spec(&cargo_cfg(cargo)).unwrap();
    let none = spec(r#"{ "args": ["build"], "filter": { "name": "nope" } }"#).unwrap();
    let err = cargo_artifact(CARGO_JSON_1392, &none).unwrap_err();
    assert!(
        err.contains("no executable matching name \"nope\""),
        "{err}"
    );
    let both = spec(r#"{ "args": ["build"] }"#).unwrap();
    let err = cargo_artifact(CARGO_JSON_1392, &both).unwrap_err();
    assert!(
        err.contains("2 executables matching no filter") && err.contains("rc, rc-1a2b"),
        "{err}"
    );
    assert!(spec(r#"{ "filter": { "name": "rc" } }"#).is_err());
    assert!(spec(r#"{ "args": [] }"#).is_err());
    let plain = config(r#"[{ "name": "P", "type": "lldb", "program": "/bin/true" }]"#);
    assert!(cargo_spec(&resolve(&plain, &ctx()).unwrap()).is_none());
}

/// #1392 end to end: cargo really builds a crate and the bin is picked;
/// a crate that does not compile is an error carrying cargo's message.
#[test]
fn cargo_builds_the_program_or_says_why_not() {
    let tmp = tempfile::tempdir().unwrap();
    let w = tmp.path();
    std::fs::create_dir_all(w.join("src")).unwrap();
    std::fs::write(
        w.join("Cargo.toml"),
        "[package]\nname = \"rc\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    )
    .unwrap();
    std::fs::write(w.join("src/main.rs"), "fn main() { println!(\"hi\"); }\n").unwrap();
    let spec = CargoSpec {
        args: vec![
            String::from("build"),
            String::from("--bin=rc"),
            String::from("--offline"),
        ],
        name: Some(String::from("rc")),
        kind: Some(String::from("bin")),
        // Its own target dir: the suite's is locked by the outer cargo.
        env: BTreeMap::from([(
            String::from("CARGO_TARGET_DIR"),
            w.join("target").to_string_lossy().into_owned(),
        )]),
        cwd: None,
    };
    let exe = build_cargo_program(&spec, w).unwrap();
    assert!(exe.is_file(), "{}", exe.display());
    assert_eq!(exe.file_name().unwrap(), "rc");
    std::fs::write(w.join("src/main.rs"), "fn main() { let x: u8 = \"no\"; }\n").unwrap();
    let err = build_cargo_program(&spec, w).unwrap_err();
    assert!(
        err.starts_with("cargo build --bin=rc --offline failed: ")
            && err.contains("mismatched types"),
        "{err}"
    );
}
