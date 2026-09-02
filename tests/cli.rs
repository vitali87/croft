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

/// #282 end to end, against the real binary: `--version` is a bare `x.y.z`
/// and the provenance is reachable behind `--build-info`. The unit test pins
/// the constants and clap's rendering; this pins that the `--build-info`
/// handler actually runs and prints the verbose form.
#[test]
fn build_info_carries_provenance_and_version_does_not() {
    let out = Command::cargo_bin("croft")
        .unwrap()
        .arg("--build-info")
        .assert();
    let out = out.success();
    let build_info = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        build_info.contains(env!("CARGO_PKG_VERSION")) && build_info.contains("built "),
        "--build-info carries version and build time: {build_info}"
    );

    let out = Command::cargo_bin("croft")
        .unwrap()
        .arg("--version")
        .assert();
    let out = out.success();
    let version = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert_eq!(
        version.trim(),
        format!("croft {}", env!("CARGO_PKG_VERSION")),
        "--version stays bare"
    );
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
    std::fs::write(&slow_shell, "#!/bin/sh\nsleep 30\n").unwrap();
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
        // The discriminant is the 30s sleeping shell: a probe that runs the
        // PATH repair blocks on it, one that early-outs answers in well
        // under a second. The measured span also covers the cargo_bin
        // spawn, which a suite-saturated box once pushed past a 5s ceiling
        // (#50) — half the sleep keeps the failure mode unreachable while
        // giving load an order of magnitude more headroom.
        assert!(
            start.elapsed() < std::time::Duration::from_secs(15),
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
    // An explicit provider SWITCH starts fresh: the other backend's
    // endpoint and model must not ride along into the claude record.
    run(&["pair", "--provider", "claude"]);
    let r = read();
    assert!(r.contains("\"provider\":\"claude\""), "{r}");
    assert!(
        !r.contains("http://localhost:9999"),
        "the ollama endpoint must not survive a provider switch: {r}"
    );
    assert!(!r.contains("qwen3-coder:30b"), "{r}");
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

/// #362 acceptance criterion 3, against the real binary: outside croft the
/// command exits 1 with a one-line explanation rather than trying to render.
///
/// `env_remove` rather than a bare run: this test suite is itself often
/// launched from a croft pane, where `CROFT_VIEW_SOCK` is set and inherited,
/// and the test would then quietly exercise the connected path instead.
#[test]
fn view_outside_croft_exits_one_with_an_explanation() {
    let out = Command::cargo_bin("croft")
        .unwrap()
        .env_remove("CROFT_VIEW_SOCK")
        .args(["view", "Cargo.toml"])
        .assert();
    let out = out.failure().code(1);
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("croft view needs a croft"),
        "stderr must say why, was: {stderr}"
    );
    assert_eq!(
        stderr.lines().filter(|l| !l.trim().is_empty()).count(),
        1,
        "one line, not a backtrace: {stderr}"
    );
}

/// An empty `CROFT_VIEW_SOCK` must read as absent, not as a socket at "".
#[test]
fn view_treats_an_empty_socket_variable_as_no_croft_at_all() {
    let out = Command::cargo_bin("croft")
        .unwrap()
        .env("CROFT_VIEW_SOCK", "")
        .args(["view", "Cargo.toml"])
        .assert();
    let out = out.failure().code(1);
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("croft view needs a croft"),
        "stderr was: {stderr}"
    );
}

/// A stale socket path (the croft that set it has exited) must name that
/// situation rather than surfacing a raw connect error.
#[test]
fn view_against_a_vanished_croft_says_the_croft_is_gone() {
    let tmp = tempfile::tempdir().unwrap();
    let out = Command::cargo_bin("croft")
        .unwrap()
        .env("CROFT_VIEW_SOCK", tmp.path().join("nobody.sock"))
        .args(["view", "Cargo.toml"])
        .assert();
    let out = out.failure().code(1);
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("is gone"),
        "stderr must name the situation, was: {stderr}"
    );
}

/// #362 end to end against the REAL binary: `croft view <file>` connects to
/// the socket named by the environment, sends the resolved path, and exits 0
/// on an ok reply.
///
/// The unit tests drive the client and server halves in one process, where
/// they can agree with each other. This stands up a socket the binary knows
/// nothing about and makes the shipped `croft view` talk to it.
#[test]
fn view_sends_the_resolved_path_to_the_socket_and_exits_zero() {
    use std::io::{BufRead, BufReader, Write};
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("v.sock");
    let target = tmp.path().join("report.txt");
    std::fs::write(&target, b"hello").unwrap();
    let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();

    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).unwrap();
        stream.write_all(b"{\"status\":\"ok\"}\n").unwrap();
        line
    });

    // Run from a DIFFERENT directory with a RELATIVE argument: the client is
    // the side that resolves, and running from the file's own directory
    // would pass even if it did not.
    Command::cargo_bin("croft")
        .unwrap()
        .env("CROFT_VIEW_SOCK", &sock)
        .current_dir(tmp.path())
        .args(["view", "report.txt"])
        .assert()
        .success();

    // The wire carries the path as raw bytes (a filename is bytes, not
    // UTF-8), so decode rather than substring-match: asserting on the text
    // form would pass only for paths that happen to be valid UTF-8, which is
    // the case the byte encoding exists to stop relying on.
    let request = server.join().unwrap();
    let bytes: Vec<u8> = request
        .trim()
        .trim_start_matches("{\"path\":[")
        .trim_end_matches("]}")
        .split(',')
        .map(|n| n.trim().parse::<u8>().expect("the wire is a byte array"))
        .collect();
    // Canonicalised on both sides: macOS hands a process a cwd under
    // `/private/var` for a `/var` tempdir, so the client resolves against a
    // path that is the same directory by a different name and a raw compare
    // fails there while passing on Linux.
    let received = std::path::PathBuf::from(String::from_utf8(bytes).unwrap());
    assert_eq!(
        received.canonicalize().unwrap_or(received),
        target.canonicalize().unwrap_or(target.clone()),
        "the server must receive the path resolved against the client's cwd"
    );
}

/// A file that does not exist is refused by the client, before any croft is
/// asked to open it: the message can then name the path as the user typed it,
/// resolved against the cwd they typed it in.
#[test]
fn view_refuses_a_missing_file_without_bothering_the_socket() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("v.sock");
    let _listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
    let out = Command::cargo_bin("croft")
        .unwrap()
        .env("CROFT_VIEW_SOCK", &sock)
        .current_dir(tmp.path())
        .args(["view", "nope.pdf"])
        .assert();
    let out = out.failure().code(1);
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("no such file") && stderr.contains("nope.pdf"),
        "stderr must name the missing path, was: {stderr}"
    );
}

/// The wire carries a path as raw bytes so a filename that is not UTF-8
/// survives. That is only true if the CLI takes the argument as an
/// `OsString`: a `String` parameter throws the bytes away one call before
/// the encoding that preserves them, which is where this started.
///
/// Not on macOS: APFS rejects a filename that is not valid UTF-8, so the
/// fixture cannot be created there and the test would fail for a reason
/// that has nothing to do with the wire.
#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn view_transmits_a_non_utf8_filename_byte_for_byte() {
    use std::ffi::OsStr;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::ffi::OsStrExt;

    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("v.sock");
    let name = OsStr::from_bytes(b"od\xffd.txt");
    let target = tmp.path().join(name);
    std::fs::write(&target, b"x").unwrap();
    assert!(
        target.to_str().is_none(),
        "fixture must be invalid UTF-8, or this test proves nothing"
    );
    let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();

    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).unwrap();
        stream.write_all(b"{\"status\":\"ok\"}\n").unwrap();
        line
    });

    Command::cargo_bin("croft")
        .unwrap()
        .env("CROFT_VIEW_SOCK", &sock)
        .current_dir(tmp.path())
        .arg("view")
        .arg(name)
        .assert()
        .success();

    let request = server.join().unwrap();
    let bytes: Vec<u8> = request
        .trim()
        .trim_start_matches("{\"path\":[")
        .trim_end_matches("]}")
        .split(',')
        .map(|n| n.trim().parse::<u8>().expect("the wire is a byte array"))
        .collect();
    assert!(
        bytes.contains(&0xff),
        "the 0xff byte must reach the server intact, got {bytes:?}"
    );
    assert_eq!(std::path::PathBuf::from(OsStr::from_bytes(&bytes)), target);
}

/// An `--as` value that cannot become a filename is refused, not ignored:
/// falling back to the sniff would stage the bytes under a name the user did
/// not ask for and still report success.
#[test]
fn view_refuses_an_as_flag_that_cannot_be_a_file_extension() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("v.sock");
    let _listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
    let out = Command::cargo_bin("croft")
        .unwrap()
        .env("CROFT_VIEW_SOCK", &sock)
        .current_dir(tmp.path())
        .write_stdin("a,b\n1,2\n")
        .args(["view", "-", "--as", "../x"])
        .assert();
    let out = out.failure().code(1);
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("--as"),
        "the message must name the flag, was: {stderr}"
    );
}
