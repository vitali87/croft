use assert_cmd::Command;

#[test]
fn help_flag_prints_usage_and_exits_zero() {
    let out = Command::cargo_bin("croft").unwrap().arg("--help").assert();
    let out = out.success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("Usage:"), "stdout was: {stdout}");
    assert!(stdout.contains("setup-terminal"));
}

#[test]
fn version_flag_prints_version() {
    let out = Command::cargo_bin("croft")
        .unwrap()
        .arg("--version")
        .assert();
    let out = out.success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("croft"));
}

#[test]
fn setup_terminal_help_works() {
    Command::cargo_bin("croft")
        .unwrap()
        .args(["setup-terminal", "--help"])
        .assert()
        .success();
}

/// The remote launch script runs `croft session-host --probe` (and the
/// collab-relay twin) over SSH on every connect as a pure liveness check.
/// A remote macOS host hands SSH sessions the stripped launchd PATH, so if
/// the GUI-PATH repair ran before the probe early-out, every attach would
/// stall on a login-shell probe — against the "remote attach never waits"
/// invariant. The probe subcommands must answer without consulting a shell.
#[test]
fn probe_subcommands_answer_instantly_even_with_a_stripped_path() {
    let dir = tempfile::tempdir().unwrap();
    let slow_shell = dir.path().join("slow-shell");
    std::fs::write(&slow_shell, "#!/bin/sh\nsleep 10\n").unwrap();
    let mut perm = std::fs::metadata(&slow_shell).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
    std::fs::set_permissions(&slow_shell, perm).unwrap();
    for sub in ["session-host", "collab-relay"] {
        let start = std::time::Instant::now();
        Command::cargo_bin("croft")
            .unwrap()
            .args([sub, "--probe"])
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("SHELL", &slow_shell)
            .assert()
            .success();
        // The discriminant is the 10s sleeping shell: a probe that runs the
        // PATH repair blocks on it, one that early-outs answers in well
        // under a second. 5s tolerates a loaded box without ever passing
        // the failure mode.
        assert!(
            start.elapsed() < std::time::Duration::from_secs(5),
            "{sub} --probe took {:?} with a stripped PATH",
            start.elapsed()
        );
    }
}

#[test]
fn nonexistent_path_fails_cleanly() {
    Command::cargo_bin("croft")
        .unwrap()
        .arg("/nonexistent/path/that/does/not/exist/abc123")
        .assert()
        .failure();
}

/// The persisted pair record is the baseline for every `croft pair`
/// invocation: `--off` (with or without provider flags) and a plain
/// re-activation must both keep a custom ollama endpoint. Resolving bare
/// flags in isolation used to clobber the custom URL with the default and
/// silently converted a disabled ollama record into a cloud claude seat.
#[test]
fn pair_off_and_reactivation_keep_the_recorded_backend() {
    // A SHORT home: the activation path binds a relay socket under
    // $HOME/.cache/croft/sessions, and the default tempdir base pushes the
    // path past the AF_UNIX 104-byte limit.
    let home = tempfile::Builder::new()
        .prefix("h")
        .tempdir_in("/tmp")
        .unwrap();
    let ws = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        Command::cargo_bin("croft")
            .unwrap()
            .args(args)
            .arg("--workspace")
            .arg(ws.path())
            .env("HOME", home.path())
            .assert()
            .success();
    };
    run(&[
        "pair",
        "--provider",
        "ollama",
        "--model",
        "qwen3-coder:30b",
        "--base-url",
        "http://localhost:9999",
    ]);
    let record_path = || {
        let dir = home.path().join(".cache/croft/sessions");
        std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .find(|p| p.to_string_lossy().ends_with(".pair.json"))
            .expect("a pair record exists")
    };
    let read = || std::fs::read_to_string(record_path()).unwrap();
    assert!(read().contains("http://localhost:9999"));
    // Deactivate with provider flags but no URL: the custom URL survives.
    run(&["pair", "--provider", "ollama", "--off"]);
    let r = read();
    assert!(r.contains("\"enabled\":false"), "{r}");
    assert!(
        r.contains("http://localhost:9999"),
        "the default endpoint must not clobber the custom one: {r}"
    );
    // Plain re-activation: the whole recorded backend comes back.
    run(&["pair"]);
    let r = read();
    assert!(r.contains("\"enabled\":true"), "{r}");
    assert!(r.contains("\"provider\":\"ollama\""), "{r}");
    assert!(r.contains("http://localhost:9999"), "{r}");
    assert!(r.contains("qwen3-coder:30b"), "{r}");
    // Activation detached a relay for the workspace; reap it so test runs
    // do not accumulate idle processes.
    let sessions = home.path().join(".cache/croft/sessions");
    if let Ok(entries) = std::fs::read_dir(&sessions) {
        for e in entries.filter_map(|e| e.ok()) {
            let p = e.path();
            if p.to_string_lossy().ends_with(".collab.sock") {
                let _ = std::process::Command::new("pkill")
                    .args(["-f", &p.to_string_lossy()])
                    .status();
            }
        }
    }
}
