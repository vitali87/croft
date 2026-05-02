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
    let out = Command::cargo_bin("croft").unwrap().arg("--version").assert();
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

#[test]
fn nonexistent_path_fails_cleanly() {
    Command::cargo_bin("croft")
        .unwrap()
        .arg("/nonexistent/path/that/does/not/exist/abc123")
        .assert()
        .failure();
}
