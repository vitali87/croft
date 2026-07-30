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
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
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
