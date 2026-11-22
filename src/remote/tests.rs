//! remote.rs's own tests, moved out of remote.rs (#2062).

/// A fake `ssh` for this test thread: it drops ssh's own flags and the
/// host, then runs the remote command locally with `home` as `$HOME`.
#[cfg(unix)]
fn fake_ssh(dir: &Path, home: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let shim = dir.join("ssh");
    std::fs::write(&shim, fake_ssh_script(home)).unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    TEST_SSH.with(|p| *p.borrow_mut() = Some(shim));
}

/// [`fake_ssh`]'s script: drop ssh's own flags and the host, then run the
/// remote command with `home` as `$HOME`.
#[cfg(unix)]
fn fake_ssh_script(home: &Path) -> String {
    format!(
        "#!/bin/sh\nwhile [ $# -gt 0 ]; do case \"$1\" in -S|-o) shift 2;; -*) shift;; *) break;; esac; done\nshift\nHOME='{}' exec sh -c \"$*\"\n",
        home.display()
    )
}

/// [`fake_ssh`], still open for writing for `hold`, the way a test's
/// freshly written script is while another test thread forks: running it
/// fails with "Text file busy" until the writer lets go (#2560).
#[cfg(unix)]
fn busy_fake_ssh(dir: &Path, home: &Path, hold: std::time::Duration) {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt;
    let shim = dir.join("ssh");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o755)
        .open(&shim)
        .unwrap();
    f.write_all(fake_ssh_script(home).as_bytes()).unwrap();
    f.flush().unwrap();
    std::thread::spawn(move || {
        std::thread::sleep(hold);
        drop(f);
    });
    TEST_SSH.with(|p| *p.borrow_mut() = Some(shim));
}

#[cfg(unix)]
#[test]
fn the_config_hash_read_sees_the_remotes_files() {
    // #1247: background ssh nulls stdout, so `.output()` read nothing
    // and every remote copy counted as absent, then got overwritten.
    let bin = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join(".config/croft");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("triggers.json"), "{\"c\": \"#123456\"}\n").unwrap();
    fake_ssh(bin.path(), home.path());
    let got = read_remote_hashes(ssh_socket_read(
        &bin.path().join("ctl"),
        "box",
        &crate::config_sync::remote_hash_script(),
    ))
    .expect("the listing reads");
    assert_eq!(
        got.get("triggers.json"),
        Some(&crate::config_sync::content_hash(b"{\"c\": \"#123456\"}\n")),
        "{got:?}"
    );
}

#[cfg(unix)]
#[test]
fn the_config_diff_read_returns_the_remote_copy() {
    let bin = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join(".config/croft");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("triggers.json"), "remote copy\n").unwrap();
    fake_ssh(bin.path(), home.path());
    let got = read_remote_config(&bin.path().join("ctl"), "box", "triggers.json").unwrap();
    assert_eq!(String::from_utf8_lossy(&got), "remote copy\n");
}

/// #2560: an ssh that is "Text file busy" for a moment (just installed or
/// updated, or a test's stand-in written while another test forked) still
/// gives the remote's copy, as the hash read already did.
#[cfg(unix)]
#[test]
fn a_briefly_busy_ssh_still_reads_the_remote_copy() {
    let bin = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join(".config/croft");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("triggers.json"), "remote copy\n").unwrap();
    busy_fake_ssh(
        bin.path(),
        home.path(),
        std::time::Duration::from_millis(100),
    );
    let got = read_remote_config(&bin.path().join("ctl"), "box", "triggers.json").unwrap();
    assert_eq!(String::from_utf8_lossy(&got), "remote copy\n");
}

#[cfg(unix)]
#[test]
fn a_read_still_keeps_ssh_off_the_terminal() {
    // Negative: the read must not take the user's stdin or write
    // diagnostics over the screen: `-n`, and `cat` on the remote ends at
    // once on an empty stdin.
    let bin = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    fake_ssh(bin.path(), home.path());
    let mut cmd = ssh_socket_read(&bin.path().join("ctl"), "box", "cat && echo done >&2");
    assert!(cmd.get_args().any(|a| a == "-n"));
    // Started as croft starts its reads, waiting out "Text file busy" (#2560).
    let out = crate::review_ops::spawn_retrying_busy(&mut cmd)
        .and_then(std::process::Child::wait_with_output)
        .unwrap();
    assert!(out.stdout.is_empty());
    assert!(out.stderr.is_empty(), "stderr stays off the terminal");
}

#[cfg(unix)]
#[test]
fn an_unreadable_listing_pushes_nothing() {
    // Negative: a failed read is unknown, never "absent everywhere".
    let bin = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    fake_ssh(bin.path(), home.path());
    assert_eq!(
        read_remote_hashes(ssh_socket_read(&bin.path().join("ctl"), "box", "exit 255")),
        None
    );
}

#[cfg(unix)]
#[test]
fn a_session_host_or_dtach_on_the_remote_makes_the_session_persistent() {
    // #831: the check accepted only dtach, so a host whose session
    // survived under `croft session-host` was never reconnected to.
    use std::os::unix::fs::PermissionsExt;
    let exe = |path: &Path, body: &str| {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    let check = |home: &Path, extra_path: &Path| {
        std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(session_supervisor_check_command())
            .env("HOME", home)
            .env("PATH", format!("{}:/usr/bin:/bin", extra_path.display()))
            .status()
            .unwrap()
            .success()
    };
    let home = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    assert!(!check(home.path(), bin.path()), "neither supervisor");

    let croft = home.path().join(".cargo/bin/croft");
    exe(&croft, "[ \"$1 $2\" = 'session-host --probe' ]");
    assert!(check(home.path(), bin.path()), "croft's session host");

    // A croft too old for session-host, with dtach beside it.
    exe(&croft, "exit 2");
    assert!(!check(home.path(), bin.path()));
    exe(&bin.path().join("dtach"), "exit 0");
    assert!(check(home.path(), bin.path()), "dtach");

    // The check probes exactly as the launched session does.
    let launch = remote_croft_command_for_terminal(None, None, None, None, false, &[], false);
    assert!(launch.contains("croft session-host --probe >/dev/null 2>&1"));
    assert!(launch.contains("command -v dtach >/dev/null 2>&1"));
}

fn report(path: &str, host: Option<&str>) -> CwdReport {
    (std::path::PathBuf::from(path), host.map(str::to_string))
}

#[test]
fn a_new_remote_report_is_the_workspace_path() {
    let before = report("/Users/me/code", Some("localhost"));
    let now = report("/srv/app", Some("db-1.internal"));
    assert_eq!(
        remote_workspace_path("db-1", Some(&before), Some(&now)).as_deref(),
        Some("/srv/app")
    );
    // No report at all when the offer appeared (no local integration).
    assert_eq!(
        remote_workspace_path("db-1", None, Some(&now)).as_deref(),
        Some("/srv/app")
    );
}

#[test]
fn an_unchanged_report_counts_only_when_it_names_the_host() {
    let same = report("/srv/app", Some("db-1"));
    assert_eq!(
        remote_workspace_path("db-1", Some(&same), Some(&same)).as_deref(),
        Some("/srv/app"),
        "the remote prompt reported before the offer sampled"
    );
    let fqdn = report("/srv/app", Some("DB-1.internal"));
    assert_eq!(
        remote_workspace_path("db-1", Some(&fqdn), Some(&fqdn)).as_deref(),
        Some("/srv/app"),
        "a domain-qualified name for the same host, any case"
    );
    let stale = report("/home/old", Some("web-7"));
    assert_eq!(
        remote_workspace_path("db-1", Some(&stale), Some(&stale)),
        None,
        "left over from an earlier session on another host"
    );
    let prefix = report("/x", Some("db-10"));
    assert_eq!(
        remote_workspace_path("db-1", Some(&prefix), Some(&prefix)),
        None,
        "db-10 is not db-1"
    );
}

#[test]
fn a_local_or_hostless_report_is_never_remote() {
    let local = report("/Users/me", Some("localhost"));
    assert_eq!(remote_workspace_path("db-1", None, Some(&local)), None);
    let hostless = report("/Users/me", None);
    assert_eq!(remote_workspace_path("db-1", None, Some(&hostless)), None);
    assert_eq!(remote_workspace_path("db-1", None, None), None);
}

/// The destination an ssh command line names, from the shapes people
/// actually type (#364).
///
/// Everything here is a real invocation rather than a synthetic one,
/// because the whole difficulty is that `ssh` takes flags with separate
/// arguments — `-p 2222`, `-i key`, `-o Opt=v` — and a parser that does
/// not know which flags consume the next word will take `2222` as the
/// host.
#[test]
fn an_ssh_command_line_yields_its_destination() {
    let d = |cmd: &[&str]| ssh_destination(cmd);
    assert_eq!(d(&["ssh", "box"]).as_deref(), Some("box"));
    assert_eq!(d(&["ssh", "user@box"]).as_deref(), Some("user@box"));
    // A flag that takes a separate argument must not donate it.
    assert_eq!(d(&["ssh", "-p", "2222", "box"]).as_deref(), Some("box"));
    assert_eq!(
        d(&["ssh", "-i", "~/.ssh/id", "box"]).as_deref(),
        Some("box")
    );
    assert_eq!(
        d(&["ssh", "-o", "StrictHostKeyChecking=no", "box"]).as_deref(),
        Some("box")
    );
    // Bundled boolean flags take no argument.
    assert_eq!(d(&["ssh", "-4qt", "box"]).as_deref(), Some("box"));
    // An attached value (`-p2222`) consumes nothing further.
    assert_eq!(d(&["ssh", "-p2222", "box"]).as_deref(), Some("box"));
    // Every arg-taking flag in ssh(1)'s synopsis, each given a value that
    // would be taken as the host if the flag were missing from the list.
    // `-P` was missing on the first pass, which is exactly this bug.
    for f in [
        "B", "b", "c", "D", "E", "e", "F", "I", "i", "J", "L", "l", "m", "O", "o", "P", "p", "Q",
        "R", "S", "W", "w",
    ] {
        let flag = format!("-{f}");
        assert_eq!(
            d(&["ssh", &flag, "decoy", "box"]).as_deref(),
            Some("box"),
            "-{f} swallowed its argument's place"
        );
    }
    // The FIRST non-flag word is the destination; anything after it is
    // the remote command, not another host.
    assert_eq!(
        d(&["ssh", "box", "tail", "-f", "log"]).as_deref(),
        Some("box")
    );
    // `--` ends option parsing.
    assert_eq!(d(&["ssh", "--", "box"]).as_deref(), Some("box"));
    assert_eq!(d(&["ssh", "-4", "--", "box"]).as_deref(), Some("box"));
    // An absolute path invocation is still ssh.
    assert_eq!(d(&["/usr/bin/ssh", "box"]).as_deref(), Some("box"));
    // `--` ends option parsing, so even a host spelled like a flag is
    // taken verbatim — which is what real ssh does.
    assert_eq!(d(&["ssh", "--", "-4"]).as_deref(), Some("-4"));
}

/// The offer's whole decision, as one pure function (#364).
///
/// Kept pure so the interesting cases can be swept without a live ssh
/// session: what the user sees is decided here, and the app layer only
/// samples argv and renders the result.
#[test]
fn the_offer_appears_only_for_a_known_host_in_an_ssh_pane() {
    let targets = parse_ssh_config("Host db-1\n  HostName 10.0.0.4\n");
    let o = |cmd: &[&str]| {
        ssh_reroot_decision(cmd, &targets)
            .ok()
            .map(|h| h.alias.clone())
    };

    assert_eq!(o(&["ssh", "db-1"]).as_deref(), Some("db-1"));
    assert_eq!(
        o(&["ssh", "-p", "2222", "deploy@10.0.0.4"]).as_deref(),
        Some("db-1")
    );

    // A shell is not an ssh session.
    assert_eq!(o(&["zsh"]), None);
    // An ssh session to a box croft has no config entry for: no offer,
    // because the remote flow it would hand off to is keyed on one.
    assert_eq!(o(&["ssh", "unknown-box"]), None);
    // ssh with no destination yet — the user is still typing.
    assert_eq!(o(&["ssh"]), None);
    // sshfs is a mount, not a session; offering to re-root onto it would
    // answer a question the user did not ask.
    assert_eq!(o(&["sshfs", "db-1:/srv", "/mnt"]), None);
}

/// A destination is matched to a configured host, and the user part is
/// not part of the identity (#364).
#[test]
fn a_destination_resolves_against_the_configured_hosts() {
    let targets = parse_ssh_config(
        "Host db-1\n  HostName 10.0.0.4\n  User deploy\n\nHost web\n  HostName web.example\n",
    );
    let m = |dest: &str| resolve_offer_host(dest, &targets).map(|t| t.alias.clone());

    assert_eq!(m("db-1").as_deref(), Some("db-1"));
    // `user@alias` names the same box: the user is how you log in, not
    // which machine it is.
    assert_eq!(m("deploy@db-1").as_deref(), Some("db-1"));
    assert_eq!(m("root@db-1").as_deref(), Some("db-1"));
    // The HostName resolves too — people type either.
    assert_eq!(m("10.0.0.4").as_deref(), Some("db-1"));
    assert_eq!(m("web.example").as_deref(), Some("web"));
    // Aliases are case-insensitive, as ssh treats them.
    assert_eq!(m("DB-1").as_deref(), Some("db-1"));

    // A host croft knows nothing about gets no offer: the prompt exists
    // to reuse the remote machinery, which is keyed on a config entry.
    assert_eq!(m("some-random-box"), None);
    assert_eq!(m(""), None);
    // A port suffix is not part of the host name.
    assert_eq!(m("db-1:22"), None);

    // An entry's own alias beats an EARLIER entry's HostName. Testing
    // both in one pass let `jump` win here, so `ssh db-1` offered to
    // re-root onto a different machine than the one named.
    let shadowed =
        parse_ssh_config("Host jump\n  HostName db-1\n\nHost db-1\n  HostName 10.0.0.4\n");
    assert_eq!(
        resolve_offer_host("db-1", &shadowed)
            .map(|t| t.alias.clone())
            .as_deref(),
        Some("db-1"),
        "an alias must beat another entry's HostName"
    );
    // The HostName fallback still works when no alias matches.
    assert_eq!(
        resolve_offer_host("10.0.0.4", &shadowed)
            .map(|t| t.alias.clone())
            .as_deref(),
        Some("db-1")
    );
}

/// Every outcome the user can be told, decided in one place.
#[test]
fn the_decision_names_the_host_or_says_why_not() {
    let targets = parse_ssh_config("Host db-1\n  HostName 10.0.0.4\n");
    let d = |argv: &[&str]| ssh_reroot_decision(argv, &targets).map(|t| t.alias.clone());
    assert_eq!(d(&["ssh", "db-1"]), Ok(String::from("db-1")));

    // A known shape of failure names the host, because that is the half
    // the user can act on — add it to ~/.ssh/config and try again.
    let err = d(&["ssh", "other-box"]).unwrap_err();
    assert!(err.contains("other-box"), "unhelpful: {err}");
    assert!(err.contains("ssh/config"), "unhelpful: {err}");

    // Not an ssh session at all is a different message: there is no host
    // to name, and saying one is missing would be misleading.
    let err = d(&["zsh"]).unwrap_err();
    assert_eq!(err, "This pane is not an SSH session");
    assert!(!err.contains("ssh/config"));
}

/// mosh and et are refused rather than mis-parsed.
///
/// They reach a box the same way and were once in `SSH_PROGRAMS`, but
/// they take LONG options and ssh's single-letter grammar mis-reads
/// those: `--port` scans as `-port`, finds `p` mid-word, consumes
/// nothing, and the VALUE becomes the host. A wrong host re-roots onto
/// the wrong machine, so refusing is the only honest answer until a
/// per-program table exists. Pinned here so re-adding them without that
/// table fails rather than silently regressing.
#[test]
fn a_long_option_program_is_refused_rather_than_misparsed() {
    let d = |cmd: &[&str]| ssh_destination(cmd);
    for cmd in [
        vec!["mosh", "box"],
        vec!["mosh", "--port", "60000", "box"],
        vec!["mosh", "--ssh", "ssh -p 2222", "box"],
        vec!["et", "box"],
        vec!["et", "--port", "2022", "box"],
    ] {
        assert_eq!(
            d(&cmd),
            None,
            "{cmd:?} must be refused while the grammar cannot parse it"
        );
    }
}

/// Things that look like an ssh session and are not one.
#[test]
fn a_non_ssh_command_line_yields_nothing() {
    let d = |cmd: &[&str]| ssh_destination(cmd);
    for cmd in [
        vec!["zsh"],
        vec!["ssh"],               // no destination yet
        vec!["ssh", "-p", "2222"], // flag ate the last word
        vec!["ssh-agent"],         // not ssh
        vec!["ssh-keygen", "-t", "ed25519"],
        vec!["sshfs", "box:/srv", "/mnt"], // a mount, not a session
        vec!["git", "push", "ssh://box/repo"],
        vec![],
    ] {
        assert_eq!(d(&cmd), None, "must not match: {cmd:?}");
    }
}

// The destination host must come BEFORE the script. ssh reads the first
// non-flag argument as the destination, so a missing host does not fail
// loudly - it turns the command itself into the hostname, the connection
// fails, and whatever depended on it silently does nothing. That is
// exactly what happened to config sync: `mkdir -p ~/.config/croft` became
// the host, the mkdir "failed", and the feature returned before pushing a
// single file.
#[test]
fn a_background_shell_puts_the_host_before_the_script() {
    let ssh = SshControl {
        host: String::from("somebox"),
        socket_dir: PathBuf::from("/tmp/x"),
        socket_path: PathBuf::from("/tmp/x/ctl"),
    };
    let cmd = ssh.background_shell("mkdir -p ~/.config/croft");
    let args: Vec<String> = cmd
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let host_at = args.iter().position(|a| a == "somebox");
    let script_at = args.iter().position(|a| a == "mkdir -p ~/.config/croft");
    assert!(
        host_at.is_some(),
        "the destination host must be passed: {args:?}"
    );
    assert!(script_at.is_some(), "the script must be passed: {args:?}");
    assert!(
        host_at < script_at,
        "ssh takes the destination BEFORE the command, or it reads the \
             command as the hostname: {args:?}"
    );
    assert!(
        args.iter().any(|a| a == "-n"),
        "a background command must not inherit stdin: {args:?}"
    );
}

use super::*;

/// The relay-setup failure message carries ssh's own stderr when there is
/// one, and never ends in a dangling ": " when there is not.
#[test]
#[cfg(unix)]
fn relay_setup_error_appends_stderr_only_when_present() {
    use std::os::unix::process::ExitStatusExt as _;
    let status = ExitStatus::from_raw(256); // exit code 1
    assert_eq!(
        relay_setup_error(status, b"  ssh: connect refused \n"),
        format!("remote relay setup failed with {status}: ssh: connect refused")
    );
    assert_eq!(
        relay_setup_error(status, b" \n"),
        format!("remote relay setup failed with {status}")
    );
    assert!(!relay_setup_error(status, b"").ends_with(": "));
}

/// The source sync must ship the BUILD INPUTS and nothing else.
///
/// It used to deny-list `target`, which never matched this repo's actual
/// build directory, `target.noindex`. Every fallback install therefore
/// rsynced 176 GB of build artifacts to the user's box, throttled to a
/// couple of MB/s, to deliver a source tree whose only job was to be
/// compiled there. Three boxes were 23-27 GB deep in it before anyone
/// noticed. The stamp learned this lesson already (SOURCE_STAMP_INPUTS);
/// the rsync did not, so now they share one list and cannot drift apart.
#[test]
fn the_source_sync_ships_only_build_inputs() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("checkout");
    let dst = tmp.path().join("staged");
    for dir in [
        "src",
        "assets",
        "vendor/async-lsp/src",
        "target.noindex/debug",
        "target/debug",
        ".git",
    ] {
        std::fs::create_dir_all(src.join(dir)).unwrap();
    }
    std::fs::write(src.join("src/main.rs"), "fn main() {}").unwrap();
    std::fs::write(src.join("assets/logo.png"), "png").unwrap();
    std::fs::write(src.join("Cargo.toml"), "[package]").unwrap();
    std::fs::write(src.join("Cargo.lock"), "lock").unwrap();
    std::fs::write(src.join("build.rs"), "fn main() {}").unwrap();
    std::fs::write(src.join("rust-toolchain.toml"), "[toolchain]").unwrap();
    std::fs::write(src.join("vendor/async-lsp/src/lib.rs"), "//").unwrap();
    // The three things that must never cross the wire.
    std::fs::write(src.join("target.noindex/debug/croft"), "HUGE").unwrap();
    std::fs::write(src.join("target/debug/croft"), "HUGE").unwrap();
    std::fs::write(src.join(".git/config"), "gitdir").unwrap();
    // Nested junk: a root-anchored allow-list alone re-admits these,
    // because `/assets/***` beats `--exclude=*`. The stamp walk skips them
    // at every level, so the sync has to as well or the two disagree.
    std::fs::write(src.join("assets/.DS_Store"), "finder").unwrap();
    std::fs::create_dir_all(src.join("src/vendor/target")).unwrap();
    std::fs::write(src.join("src/vendor/target/blob"), "HUGE").unwrap();

    let mut rsync = Command::new("rsync");
    rsync.arg("-a").args(source_sync_filter_args());
    rsync
        .arg(format!("{}/", src.display()))
        .arg(format!("{}/", dst.display()));
    let Ok(status) = rsync.status() else {
        return; // no rsync on this box; the remote path is unusable anyway
    };
    assert!(status.success(), "rsync exited with {status}");

    for shipped in [
        "src/main.rs",
        "assets/logo.png",
        "Cargo.toml",
        "Cargo.lock",
        "build.rs",
        "rust-toolchain.toml",
        "vendor/async-lsp/src/lib.rs",
    ] {
        assert!(dst.join(shipped).exists(), "{shipped} must be shipped");
    }
    for withheld in [
        "target.noindex",
        "target",
        ".git",
        "assets/.DS_Store",
        "src/vendor/target",
    ] {
        assert!(
            !dst.join(withheld).exists(),
            "{withheld} must never be shipped: this is the 176 GB bug"
        );
    }
}

/// Stage a remote source dir holding both legitimate content and every
/// kind of junk a previous croft could have left behind.
fn staged_remote_source(home: &std::path::Path) -> std::path::PathBuf {
    let source = home.join(".cache/croft/source");
    std::fs::create_dir_all(source.join("target/remote-fast")).unwrap();
    std::fs::create_dir_all(source.join("target.noindex/debug")).unwrap();
    std::fs::create_dir_all(source.join(".cargo")).unwrap();
    std::fs::write(source.join(".cargo/config.toml"), "[build]\njobs = 1\n").unwrap();
    std::fs::write(source.join("build.rs"), "fn main() {}").unwrap();
    std::fs::create_dir_all(source.join("src")).unwrap();
    std::fs::write(source.join("src/main.rs"), "fn main() {}").unwrap();
    std::fs::write(source.join("Cargo.toml"), "[package]\n").unwrap();
    source
}

fn assert_prep_outcome(source: &std::path::Path) {
    assert!(
        source.join("target/remote-fast").is_dir(),
        "removing the remote's own target/ would force a from-scratch rebuild every install"
    );
    assert!(
        source.join("src/main.rs").is_file() && source.join("Cargo.toml").is_file(),
        "inputs present in the local checkout must survive: deleting them re-ships \
             everything and yanks the tree out from under a concurrent cargo install"
    );
    assert!(
        !source.join("target.noindex").exists(),
        "the artifact tree a previous croft shipped must be reclaimed"
    );
    assert!(
        !source.join(".cargo").exists(),
        "a stale dotfile like .cargo/config.toml would silently reconfigure every later remote build"
    );
    assert!(
        !source.join("build.rs").exists(),
        "an input no longer in the local checkout must not survive on the remote (tar never deletes)"
    );
}

/// rsync will not clean up what a previous croft already shipped: the
/// allow-list's `--exclude=*` PROTECTS every unlisted path from
/// `--delete`, and the tar fallback never deletes anything. So the prep
/// must clear the remote source dir of everything except `target/` (the
/// incremental build cache) and the inputs the local checkout actually
/// contains - shipped artifact trees, a stale `.cargo/config.toml` that
/// would silently reconfigure every later remote build, and inputs since
/// removed from the checkout all go. Current inputs stay: deleting them
/// would re-ship the whole tree every sync and race a concurrent
/// `cargo install` reading it. The test runs the real command against a
/// staged HOME.
#[test]
fn the_remote_dir_prep_evicts_stale_entries_but_keeps_cache_and_inputs() {
    let checkout = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(checkout.path().join("src")).unwrap();
    std::fs::write(checkout.path().join("Cargo.toml"), "[package]\n").unwrap();
    // No build.rs in this checkout: the staged remote copy is stale.
    let prep = remote_source_dir_prep(checkout.path());

    let tmp = tempfile::tempdir().unwrap();
    let source = staged_remote_source(tmp.path());
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(&prep)
        .env("HOME", tmp.path())
        .status()
        .unwrap();
    assert!(status.success(), "the prep command must run under plain sh");
    assert_prep_outcome(&source);

    let empty = tempfile::tempdir().unwrap();
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(&prep)
        .env("HOME", empty.path())
        .status()
        .unwrap();
    assert!(status.success(), "a first install has no source dir yet");
}

/// A relocated cache is ordinary on a small-root VPS: the source dir is a
/// symlink onto a bigger volume. `find` does not follow a symlinked start
/// point on its own, silently cleaning nothing with a success exit code,
/// so the prep must dereference it explicitly.
#[test]
fn the_remote_dir_prep_follows_a_symlinked_source_dir() {
    let checkout = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(checkout.path().join("src")).unwrap();
    std::fs::write(checkout.path().join("Cargo.toml"), "[package]\n").unwrap();
    let prep = remote_source_dir_prep(checkout.path());

    let tmp = tempfile::tempdir().unwrap();
    let real = tempfile::tempdir().unwrap();
    let source = staged_remote_source(real.path());
    std::fs::create_dir_all(tmp.path().join(".cache/croft")).unwrap();
    std::os::unix::fs::symlink(
        real.path().join(".cache/croft/source"),
        tmp.path().join(".cache/croft/source"),
    )
    .unwrap();

    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(&prep)
        .env("HOME", tmp.path())
        .status()
        .unwrap();
    assert!(status.success(), "the prep command must run under plain sh");
    assert_prep_outcome(&source);
}

/// The tar fallback (no rsync on either side) shipped `.` under the same
/// broken `--exclude=target`, so it packed the whole artifact tree into a
/// gzip pipe - worse than the rsync path, and reached on exactly the same
/// first-install flow. It names its members from the allow-list now.
#[test]
fn the_tar_fallback_packs_only_build_inputs() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("checkout");
    std::fs::create_dir_all(src.join("src")).unwrap();
    std::fs::create_dir_all(src.join("target.noindex")).unwrap();
    std::fs::write(src.join("src/main.rs"), "fn main() {}").unwrap();
    std::fs::write(src.join("Cargo.toml"), "[package]").unwrap();
    std::fs::write(src.join("target.noindex/blob"), "HUGE").unwrap();

    let members = source_sync_tar_members(&src);
    assert!(members.contains(&"src"), "src is a build input");
    assert!(
        members.contains(&"Cargo.toml"),
        "Cargo.toml is a build input"
    );
    assert!(
        !members.iter().any(|m| m.starts_with("target")),
        "the artifact tree must never be packed"
    );
    assert!(
        !members.contains(&"build.rs"),
        "a member that does not exist must not be named, or tar fails"
    );
}

/// A rustup target belongs to ONE toolchain. Bumping `rust-toolchain.toml`
/// therefore orphans every cross target, and `croft <host>` degrades from
/// "ship a 34 MB prebuilt binary" to "compile the whole crate graph on the
/// user's VPS" with nothing but a line in `~/.cache/croft/install.log` to
/// say so. That is exactly what the 1.95.0 -> 1.97.1 bump did, unnoticed
/// for four days. This test is the thing that notices.
#[test]
fn the_pinned_toolchain_has_every_cross_target() {
    // Only machines that have actually set up the cross fast path are held
    // to it. A fresh clone, a CI lint runner, Termux, a Nix shell with no
    // zig: none of them ship remote installs, and failing their `cargo
    // test` over a target they will never use is noise, not a signal. The
    // skip is announced, because a guard that vanishes silently is how the
    // original guard managed to be useless.
    if let Some(reason) = cross_compile_unavailable_reason() {
        eprintln!("SKIP the_pinned_toolchain_has_every_cross_target: {reason}");
        return;
    }
    let Ok(out) = cross_tool_command_in_checkout("rustup")
        .args(["target", "list", "--installed"])
        .output()
    else {
        eprintln!("SKIP the_pinned_toolchain_has_every_cross_target: rustup would not run");
        return;
    };
    if !out.status.success() {
        eprintln!("SKIP the_pinned_toolchain_has_every_cross_target: rustup errored");
        return;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let installed: Vec<&str> = text.lines().map(str::trim).collect();
    let missing: Vec<&str> = crate::cli::CROSS_TARGETS
        .iter()
        .copied()
        .filter(|t| !installed.contains(t))
        .collect();
    assert!(
        missing.is_empty(),
        "the pinned toolchain is missing {missing:?}, so every remote \
             update will compile croft on the remote box instead of shipping \
             a prebuilt binary. Fix from inside this checkout, so the \
             rust-toolchain.toml override applies:\n    rustup target add {}",
        missing.join(" ")
    );
}

/// The target query and the build must ask the SAME toolchain.
/// `cargo zigbuild` runs in the checkout, where `rust-toolchain.toml` pins
/// the channel; a rustup query with no working directory answers for the
/// DEFAULT channel instead. When those differ the guard reports "target
/// installed" and the build dies with E0463, which is why a missing target
/// went unreported for four days.
///
/// The plain probes must NOT inherit that directory: their answer does not
/// depend on the toolchain, and running `cargo --version` inside the
/// checkout makes an availability check auto-install the pinned channel.
#[test]
fn only_toolchain_sensitive_calls_run_in_the_checkout() {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    assert_eq!(
        cross_tool_command_in_checkout("rustup").get_current_dir(),
        Some(source.as_path()),
        "a target query must resolve the pinned toolchain"
    );
    assert_eq!(
        cross_tool_command("cargo").get_current_dir(),
        None,
        "a presence probe must not trigger a toolchain auto-install"
    );
}

#[test]
fn a_release_binary_is_stamped_by_its_version_not_a_missing_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let gone = tmp.path().join("home/runner/work/croft/croft");
    assert_eq!(build_stamp_for(&gone, "0.1.9").unwrap(), "release-v0.1.9");
    assert_ne!(
        build_stamp_for(&gone, "0.1.9").unwrap(),
        build_stamp_for(&gone, "0.1.10").unwrap(),
        "a new release is a new stamp, so the remote updates"
    );
    std::fs::write(tmp.path().join("Cargo.toml"), "[package]").unwrap();
    let root = tmp.path().to_path_buf();
    assert_eq!(
        build_stamp_for(&root, "0.1.9").unwrap(),
        source_stamp_for(&root).unwrap(),
        "a source tree is still hashed"
    );
}

#[test]
fn source_stamp_tracks_build_inputs_only() {
    // The stamp decides whether a remote reinstall is needed, so it must
    // cover exactly the inputs that shape the shipped binary. Build
    // artifacts (`target.noindex` reached 98 GB) and docs must not feed
    // it: hashing them made every connect stall for a minute and reship
    // croft even when the source was untouched.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("target.noindex")).unwrap();
    std::fs::create_dir_all(root.join("docs")).unwrap();
    std::fs::write(root.join("Cargo.toml"), "[package]").unwrap();
    std::fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
    std::fs::write(root.join("target.noindex/dep.rlib"), "artifact-v1").unwrap();
    std::fs::write(root.join("docs/NOTES.md"), "notes-v1").unwrap();
    let base = source_stamp_for(&root).unwrap();

    std::fs::write(root.join("target.noindex/dep.rlib"), "artifact-v2").unwrap();
    std::fs::write(root.join("docs/NOTES.md"), "notes-v2").unwrap();
    assert_eq!(
        source_stamp_for(&root).unwrap(),
        base,
        "build artifacts and docs must not change the source stamp"
    );

    std::fs::write(root.join("src/main.rs"), "fn main() { changed(); }").unwrap();
    assert_ne!(
        source_stamp_for(&root).unwrap(),
        base,
        "a source edit must change the stamp"
    );
}

#[test]
fn drop_to_local_exit_code_maps_to_return_to_local() {
    assert_eq!(
        classify_remote_status(Some(DROP_TO_LOCAL_EXIT_CODE)),
        RemoteStatusClass::ReturnToLocal
    );
    assert_eq!(classify_remote_status(Some(0)), RemoteStatusClass::Exited);
    assert_eq!(
        classify_remote_status(Some(127)),
        RemoteStatusClass::NotInstalled
    );
    assert_eq!(classify_remote_status(Some(1)), RemoteStatusClass::Failed);
    assert_eq!(classify_remote_status(None), RemoteStatusClass::Failed);
}

#[test]
fn parse_ssh_config_extracts_explicit_hosts() {
    let targets = parse_ssh_config(
        r#"
Host *
  User nobody

Host genesis-cloud github.com
  HostName 10.0.0.2
  User ubuntu

Host !blocked *.internal
  User root
"#,
    );
    assert_eq!(
        targets,
        vec![
            RemoteTarget {
                alias: String::from("genesis-cloud"),
                host_name: Some(String::from("10.0.0.2")),
                user: Some(String::from("ubuntu")),
            },
            RemoteTarget {
                alias: String::from("github.com"),
                host_name: Some(String::from("10.0.0.2")),
                user: Some(String::from("ubuntu")),
            },
        ]
    );
}

#[test]
fn remote_croft_command_quotes_paths() {
    // A workspace path with a single quote must be shell-quoted in the
    // croft invocation (which now appears in both the dtach and the
    // direct-exec branch).
    let command = remote_croft_command_for_terminal(
        Some("/tmp/it's here"),
        None,
        None,
        None,
        false,
        &[],
        false,
    );
    assert!(command.contains("croft '/tmp/it'\"'\"'s here'"));
    assert!(command.starts_with("export CROFT_REMOTE_AUTOUPDATE=1;"));
}

#[test]
fn remote_croft_command_forwards_supported_terminal_program() {
    let command =
        remote_croft_command_for_terminal(None, Some("iTerm.app"), None, None, false, &[], false);
    assert!(command.contains("export CROFT_FORCE_INLINE_IMAGES=1 TERM_PROGRAM='iTerm.app';"));
}

#[test]
fn remote_croft_command_honors_croft_pane_image_gate() {
    let blocked =
        remote_croft_command_for_terminal(None, Some("croft"), None, Some("0"), false, &[], false);
    assert!(!blocked.contains("CROFT_FORCE_INLINE_IMAGES"));
    let allowed =
        remote_croft_command_for_terminal(None, Some("croft"), None, Some("1"), false, &[], false);
    assert!(allowed.contains("export CROFT_FORCE_INLINE_IMAGES=1 TERM_PROGRAM='croft';"));
}

#[test]
fn remote_croft_command_exports_kitty_hint_for_ghostty() {
    // Ghostty: TERM_PROGRAM=ghostty locally, but SSH does not forward it, so
    // croft must export the hint itself. It must NOT force the OSC-1337 path
    // (Ghostty parses but ignores it); the remote resolves the Kitty
    // protocol from the exported TERM_PROGRAM.
    let command =
        remote_croft_command_for_terminal(None, Some("ghostty"), None, None, false, &[], false);
    assert!(command.contains("export TERM_PROGRAM=ghostty;"));
    assert!(!command.contains("CROFT_FORCE_INLINE_IMAGES"));
}

#[test]
fn remote_croft_command_exports_kitty_hint_from_term_only() {
    // A bare `kitty` terminal sets no TERM_PROGRAM; detection keys off TERM.
    let command =
        remote_croft_command_for_terminal(None, None, Some("xterm-kitty"), None, false, &[], false);
    assert!(command.contains("export TERM_PROGRAM=ghostty;"));
}

#[test]
fn remote_launch_carries_only_a_deterministic_relay_key() {
    // The relay rendezvous reaches the long-lived remote croft as the
    // deterministic `CROFT_RELAY_KEY` env var, NOT the old per-connection
    // `CROFT_DROP_RELAY_*` paths. Because the key is `hash(launch arg)` it is
    // identical on every reconnect, so dtach freezing it at first launch can
    // never desync the running croft from a fresh pump.
    let id = relay_session_id("");
    let env = vec![(String::from("CROFT_RELAY_KEY"), id.clone())];
    let command = remote_croft_command_for_terminal(None, None, None, None, false, &env, false);
    assert!(
        command.contains(&format!("export CROFT_RELAY_KEY='{id}'")),
        "launch must export the deterministic relay key, got: {command}",
    );
    assert!(
        !command.contains("CROFT_DROP_RELAY"),
        "launch must not use the old per-connection relay env, got: {command}",
    );
}

#[test]
fn relay_session_id_matches_dtach_socket_and_is_deterministic() {
    // The relay id is keyed on the launch arg, exactly like the dtach socket,
    // so `relay-<id>` and `sessions/<id>.sock` share one id and the two sides
    // always agree across reconnects. It must be deterministic (same arg ->
    // same id) and arg-sensitive; the random per-connection id it replaced
    // silently broke every drop after the first reconnect.
    assert_eq!(relay_session_id(""), relay_session_id(""));
    assert_ne!(relay_session_id("/srv/app"), relay_session_id("/srv/other"));
    // Same hash the dtach socket path embeds for the same launch arg.
    assert!(dtach_socket_path(None).contains(&relay_session_id("")));
    assert!(dtach_socket_path(Some("/srv/app")).contains(&relay_session_id("/srv/app")));
}

#[test]
fn cross_tool_search_dirs_cover_gui_launch_tool_locations() {
    // launchd hands a Croft.app / Ghostty launch a stripped PATH; the
    // toolchain lives in dirs that PATH omits, so the search must add them.
    let launchd_path = OsString::from("/usr/bin:/bin:/usr/sbin:/sbin");
    let home = PathBuf::from("/Users/example");
    let dirs = cross_tool_search_dirs_from(Some(launchd_path.as_os_str()), Some(home.as_os_str()));

    assert_eq!(dirs.first(), Some(&PathBuf::from("/usr/bin")));
    assert!(
        dirs.contains(&home.join(".cargo").join("bin")),
        "cargo, rustup, and cargo-zigbuild installed by rustup live here, but launchd omits it"
    );
    assert!(
        dirs.contains(&PathBuf::from("/opt/homebrew/bin")),
        "Apple Silicon Homebrew's zig lives here, but launchd omits it"
    );
    assert!(dirs.contains(&PathBuf::from("/usr/local/bin")));
}

#[test]
fn cross_tool_lookup_finds_home_cargo_bin_outside_inherited_path() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().unwrap();
    let bin_dir = tmp.path().join(".cargo").join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let tool = bin_dir.join("cargo-zigbuild");
    std::fs::write(&tool, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();

    let launchd_path = OsString::from("/usr/bin:/bin:/usr/sbin:/sbin");
    let dirs =
        cross_tool_search_dirs_from(Some(launchd_path.as_os_str()), Some(tmp.path().as_os_str()));

    assert_eq!(
        find_executable_in_dirs("cargo-zigbuild", &dirs),
        Some(tool),
        "the remote fast path must work when Croft.app starts with launchd's stripped PATH"
    );
}

#[test]
fn remote_croft_command_forwards_osk_flag_when_local_osk_is_armed() {
    // SSH from Termux does not forward TERMUX_VERSION, so a remote croft
    // would never auto-arm the on-screen keyboard; the launcher carries
    // the local detection across the hop explicitly.
    let command = remote_croft_command_for_terminal(None, None, None, None, true, &[], false);
    assert!(command.contains("export CROFT_FORCE_OSK=1;"));
    let command = remote_croft_command_for_terminal(None, None, None, None, false, &[], false);
    assert!(!command.contains("CROFT_FORCE_OSK"));
}

#[test]
fn remote_command_prefers_session_host_and_falls_back_to_dtach() {
    let command =
        remote_croft_command_for_terminal(Some("/srv/app"), None, None, None, false, &[], false);
    // Preferred: croft's own session host (multiplayer mux), gated on a
    // probe so an old remote binary is never sent an unknown subcommand.
    assert!(command.contains("if croft session-host --probe"));
    assert!(command.contains("exec croft session-host --socket"));
    assert!(command.contains(".mux.sock"));
    assert!(command.contains("--workspace '/srv/app' -- croft '/srv/app'"));
    // Fallback: dtach with the exact flags croft needs (attach-or-create,
    // no detach/suspend key theft, WINCH redraw on reattach).
    assert!(command.contains("elif command -v dtach"));
    assert!(command.contains("dtach -A"));
    assert!(command.contains("-E"));
    assert!(command.contains("-z"));
    assert!(command.contains("-r winch"));
    // Both persistent branches flag the session for the status line.
    assert!(command.contains("export CROFT_SESSION_PERSISTENT=1;"));
    // Hosts with neither supervisor still launch croft directly.
    assert!(command.contains("else exec croft"));
}

#[test]
fn remote_command_omits_workspace_flag_without_a_path() {
    let command = remote_croft_command_for_terminal(None, None, None, None, false, &[], false);
    assert!(command.contains("exec croft session-host --socket"));
    assert!(!command.contains("--workspace"));
    assert!(command.contains(" -- croft;"));
}

#[test]
fn dtach_socket_is_stable_per_workspace_and_differs_across_paths() {
    let a1 = dtach_socket_path(Some("/srv/app"));
    let a2 = dtach_socket_path(Some("/srv/app"));
    let b = dtach_socket_path(Some("/srv/other"));
    assert_eq!(a1, a2, "same workspace must map to the same dtach session");
    assert_ne!(a1, b, "different workspaces must not share a session");
    assert!(a1.contains("/.cache/croft/sessions/"));
    // The mux socket shares the keying but never the name, so a mux
    // client can never connect to a live legacy dtach server.
    let m = mux_socket_path(Some("/srv/app"));
    assert_ne!(m, a1);
    assert!(m.ends_with(".mux.sock"));
    assert!(m.contains(&relay_session_id("/srv/app")));
    // The collab socket (Phase D op relay) shares the keying too but is
    // its own endpoint: it never carries PTY bytes.
    let c = collab_socket_path(Some("/srv/app"));
    assert_ne!(c, a1);
    assert_ne!(c, m);
    assert!(c.ends_with(".collab.sock"));
    assert!(c.contains(&relay_session_id("/srv/app")));
}

#[test]
fn remote_command_solo_skips_the_mux_and_wires_the_collab_relay() {
    let command = remote_croft_command_for_terminal(
        Some("/srv/app"),
        Some("iTerm.app"),
        Some("xterm-256color"),
        None,
        false,
        &[],
        true,
    );
    // A solo guest never attaches the shared PTY.
    assert!(!command.contains("session-host --socket"));
    assert!(!command.contains("dtach -A"));
    // It probes for collab support (old remote binaries fall back to a
    // plain croft), ensures the relay, and exports the channel env.
    assert!(command.contains("if croft collab-relay --probe"));
    assert!(command.contains("collab-relay --ensure --socket"));
    assert!(command.contains(&collab_socket_path(Some("/srv/app"))));
    assert!(command.contains("export CROFT_COLLAB_SOCKET="));
    assert!(command.contains("export CROFT_COLLAB_ROLE=guest;"));
    assert!(command.contains("exec croft '/srv/app'"));
    // Non-solo stays on the shared-session path, untouched.
    let shared = remote_croft_command_for_terminal(
        Some("/srv/app"),
        Some("iTerm.app"),
        Some("xterm-256color"),
        None,
        false,
        &[],
        false,
    );
    assert!(shared.contains("if croft session-host --probe"));
    assert!(!shared.contains("CROFT_COLLAB_ROLE"));
}

#[test]
fn transport_failure_is_exit_255_only() {
    // 255 is ssh's own connection-died code; everything else is a real
    // remote exit and must not trigger an auto-reconnect.
    assert!(is_transport_failure(Some(255)));
    assert!(!is_transport_failure(Some(0)));
    assert!(!is_transport_failure(Some(1)));
    assert!(!is_transport_failure(Some(101)));
    assert!(!is_transport_failure(None));
}

#[test]
fn a_lost_session_turns_off_every_mode_croft_turned_on() {
    // #1460: the remote croft's modes outlived the dropped connection,
    // so the reconnect lines scrolled its screen and mouse moves typed
    // `^[[<35;…M`. Whatever croft's takeover turns on, the release
    // turns off, so a mode added there later cannot be missed here.
    let release = lost_session_release_seq();
    let takeover = String::from_utf8(crate::app::takeover_mode_seq()).unwrap();
    let mut modes = 0;
    for set in takeover.split("\x1b[?").skip(1) {
        let Some(mode) = set.split('h').next().filter(|m| set.len() > m.len()) else {
            continue;
        };
        for mode in mode.split(';') {
            modes += 1;
            assert!(
                release.contains(&format!("\x1b[?{mode}l")),
                "mode {mode} is left on: {release:?}"
            );
        }
    }
    assert!(
        modes >= 4,
        "the takeover's modes were all read: {takeover:?}"
    );
    assert!(
        release.contains("\x1b[0 q"),
        "the terminal's own cursor shape"
    );
    assert!(release.contains("\x1b[?25h"), "a visible cursor");
    let rest = release.replace("\x1b[?25h", "");
    assert!(
        !rest.contains("h\x1b") && !rest.ends_with('h'),
        "nothing else is turned on: {release:?}"
    );
}

#[test]
fn a_lost_session_leaves_the_alt_screen_last_and_its_colours_come_back_on_resume() {
    let release = lost_session_release_seq();
    // The kitty flags are cleared on the alt screen that holds them,
    // before the screen changes.
    let kitty = release.find("\x1b[=0;1u").expect("the kitty flags cleared");
    assert!(release.find("\x1b[<u").is_some_and(|pop| pop < kitty));
    // Leaving is the last thing, right after a DECSC, so it is harmless
    // when the main screen already shows.
    assert!(release.ends_with("\x1b7\x1b[?1049l"), "{release:?}");
    assert!(kitty < release.len() - "\x1b7\x1b[?1049l".len());
    // A session given up runs nothing after this, so the shell gets its
    // own colours back here...
    assert!(
        release.contains(crate::app::reset_host_colors_seq()),
        "{release:?}"
    );
    // ...and a resumed croft sets its theme's again with its modes.
    let theme = crate::theme::Theme::BLACK;
    let reattach = String::from_utf8(crate::app::reattach_reassert_seq(theme)).unwrap();
    let takeover = String::from_utf8(crate::app::takeover_mode_seq()).unwrap();
    assert!(reattach.starts_with(&takeover), "{reattach:?}");
    let (r, g, b) = theme.editor_bg_rgb();
    assert!(
        reattach.contains(&format!("\x1b]11;rgb:{r:02x}/{g:02x}/{b:02x}\x07")),
        "the theme's background: {reattach:?}"
    );
    assert!(
        reattach.contains("\x1b]10;rgb:"),
        "and foreground: {reattach:?}"
    );
}

#[test]
fn the_ssh_control_dir_is_fresh_and_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let dir = super::ssh_control_dir().unwrap();
    let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o700);
    // A directory already at the name (made by someone else) is an
    // error, not something to adopt.
    assert!(std::fs::DirBuilder::new().create(&dir).is_err());
    std::fs::remove_dir(&dir).unwrap();
}

#[test]
fn a_pulled_folder_leaves_its_hidden_entries_behind() {
    let dir = tempfile::Builder::new()
        .prefix("croft-pull")
        .tempdir()
        .unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(home.join(".ssh")).unwrap();
    std::fs::create_dir_all(home.join("docs/.git")).unwrap();
    std::fs::write(home.join(".ssh/id"), "key").unwrap();
    std::fs::write(home.join("docs/.git/cfg"), "x").unwrap();
    std::fs::write(home.join("docs/a.txt"), "a").unwrap();
    std::fs::write(home.join("-dash"), "d").unwrap();
    let list = |parent: &Path, name: &str| {
        let packed = Command::new("tar")
            .args(super::pull_tar_args(parent, std::ffi::OsStr::new(name)))
            .output()
            .unwrap();
        assert!(
            packed.status.success(),
            "{}",
            String::from_utf8_lossy(&packed.stderr)
        );
        let mut listing = Command::new("tar")
            .args(["-t", "-f", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        listing
            .stdin
            .take()
            .unwrap()
            .write_all(&packed.stdout)
            .unwrap();
        String::from_utf8(listing.wait_with_output().unwrap().stdout).unwrap()
    };
    let names = list(dir.path(), "home");
    assert!(
        names.contains("home/docs/a.txt") && names.contains("home/-dash"),
        "{names}"
    );
    assert!(
        !names.contains(".ssh") && !names.contains(".git"),
        "{names}"
    );
    assert_eq!(list(&home, "-dash").trim(), "-dash");
}

#[test]
fn a_pull_refuses_hidden_paths_and_links_into_them() {
    // Not `tempdir()`: its `.tmpXXXX` name is itself hidden.
    let dir = tempfile::Builder::new()
        .prefix("croft-pull")
        .tempdir()
        .unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    std::fs::create_dir(root.join(".ssh")).unwrap();
    std::fs::write(root.join(".ssh/id_ed25519"), "key").unwrap();
    std::fs::write(root.join("-notes.txt"), "hi").unwrap();
    std::os::unix::fs::symlink(root.join(".ssh/id_ed25519"), root.join("key")).unwrap();
    assert!(super::pull_source_allowed(&root.join("-notes.txt")));
    assert!(!super::pull_source_allowed(&root.join(".ssh/id_ed25519")));
    assert!(
        !super::pull_source_allowed(&root.join("key")),
        "a link into .ssh"
    );
    assert!(!super::pull_source_allowed(
        &root.join("x/../.ssh/id_ed25519")
    ));
    assert!(!super::pull_source_allowed(Path::new("relative.txt")));
}

#[test]
fn parse_relay_request_pulls_have_id_and_path() {
    match super::parse_relay_request("pull\tabc-123\t/Users/v/foo.txt") {
        Some(super::RelayRequest::Pull { id, src }) => {
            assert_eq!(id, "abc-123");
            assert_eq!(src, "/Users/v/foo.txt");
        }
        other => panic!("expected Pull, got {:?}", other.is_some()),
    }
}

#[test]
fn activate_command_writes_stamp_then_clears_updating_marker() {
    let command = super::activate_command("deadbeef");
    assert!(command.contains("mv \"$HOME/.cargo/bin/croft.new\" \"$HOME/.cargo/bin/croft\""));
    let stamp_at = command
        .find("printf %s 'deadbeef' > \"$HOME/.cache/croft/install-stamp\"")
        .expect("stamp write present");
    let clear_at = command
        .find("rm -f \"$HOME/.cache/croft/updating\"")
        .expect("marker clear present");
    assert!(
        stamp_at < clear_at,
        "stamp must be written before marker clear"
    );
}

#[test]
fn parse_relay_request_open_extracts_id_and_url() {
    match super::parse_relay_request("open\topen-1\thttps://example.com/x") {
        Some(super::RelayRequest::Open { id, url }) => {
            assert_eq!(id, "open-1");
            assert_eq!(url, "https://example.com/x");
        }
        _ => panic!("expected Open variant"),
    }
}

#[test]
fn url_is_safe_to_open_accepts_http_https_mailto() {
    assert!(super::url_is_safe_to_open("https://example.com/path"));
    assert!(super::url_is_safe_to_open("http://example.com"));
    assert!(super::url_is_safe_to_open("HTTPS://Example.COM"));
    assert!(super::url_is_safe_to_open("mailto:foo@example.com"));
}

#[test]
fn url_is_safe_to_open_rejects_other_schemes_and_control_chars() {
    assert!(!super::url_is_safe_to_open("file:///etc/passwd"));
    assert!(!super::url_is_safe_to_open("javascript:alert(1)"));
    assert!(!super::url_is_safe_to_open("ssh://attacker"));
    assert!(!super::url_is_safe_to_open("https://example.com\nrm -rf /"));
    assert!(!super::url_is_safe_to_open("https://example.com\r\nx"));
    assert!(!super::url_is_safe_to_open("plaintext"));
}

#[test]
fn parse_relay_request_clipboard_requires_id_only() {
    match super::parse_relay_request("clipboard\tclip-7") {
        Some(super::RelayRequest::Clipboard { id }) => assert_eq!(id, "clip-7"),
        _ => panic!("expected Clipboard variant"),
    }
}

#[test]
fn parse_relay_request_copy_carries_only_an_id() {
    match super::parse_relay_request("copy\tcopy-7") {
        Some(super::RelayRequest::Copy { id }) => assert_eq!(id, "copy-7"),
        _ => panic!("expected Copy variant"),
    }
    // No id is no request, as for every other verb.
    assert!(super::parse_relay_request("copy\t").is_none());
}

/// The pump derives a remote PATH from this id, so an id that could walk
/// out of the inbox is refused before it reaches a shell.
#[test]
fn a_copy_id_that_leaves_the_inbox_is_refused() {
    for id in [
        "../../../../etc/passwd",
        "../outside",
        "copy/7",
        "copy 7",
        "copy;rm -rf /",
        "copy$(id)",
        "",
    ] {
        assert!(
            !super::copy_request_id_is_safe(id),
            "{id:?} must not be accepted as a copy id"
        );
    }
    // What croft actually mints stays acceptable.
    assert!(super::copy_request_id_is_safe("copy-12345-0"));
}

#[test]
fn parse_relay_request_rejects_malformed_lines() {
    assert!(super::parse_relay_request("ping\tabc\t/x").is_none());
    assert!(super::parse_relay_request("pull\t\t/x").is_none());
    assert!(super::parse_relay_request("pull\tabc\t").is_none());
    assert!(super::parse_relay_request("clipboard\t").is_none());
}

#[test]
fn parse_relay_request_forward_extracts_port_and_open_flag() {
    match super::parse_relay_request("forward\tfwd-1\t3000\t1") {
        Some(super::RelayRequest::Forward { id, port, open }) => {
            assert_eq!(id, "fwd-1");
            assert_eq!(port, "3000");
            assert!(open);
        }
        _ => panic!("expected Forward variant"),
    }
}

#[test]
fn parse_relay_request_forward_defaults_open_to_false() {
    match super::parse_relay_request("forward\tfwd-2\t8080") {
        Some(super::RelayRequest::Forward { open, .. }) => assert!(!open),
        _ => panic!("expected Forward variant"),
    }
    assert!(super::parse_relay_request("forward\tfwd-3\t").is_none());
}

#[test]
fn pick_local_port_mirrors_a_free_port() {
    // Port 0 can never be mirrored (it's the "any" sentinel), but a high
    // port is almost certainly free in the test environment.
    let p = super::pick_local_port(54321);
    assert!(p == 54321 || p >= 1024, "got {p}");
}

#[test]
fn remote_install_command_installs_from_staged_source() {
    let command = remote_install_command("abc123");
    assert!(command.contains("cargo install --path \"$HOME/.cache/croft/source\""));
    assert!(command.contains("rustup.rs"));
    assert!(command.contains("printf %s 'abc123' > \"$HOME/.cache/croft/install-stamp\""));
}

/// #950: the on-box compile leaves the remote user's shell startup files
/// alone, never waits on a sudo password prompt, and the question asked
/// first names what it installs.
#[test]
fn remote_install_leaves_rc_files_alone_and_never_prompts_for_sudo() {
    let command = remote_install_command("abc123");
    let rustup = command
        .lines()
        .find(|l| l.contains("sh.rustup.rs"))
        .expect("rustup bootstraps a box without cargo");
    assert!(rustup.contains("--no-modify-path"), "{rustup}");
    assert!(command.contains("sudo -n \"$@\""));
    assert!(
        !command.contains("CROFT_SUDO"),
        "a variable holding `sudo -n` is one word to zsh"
    );
    let said = REMOTE_COMPILE_SIDE_EFFECTS.join(" ");
    for named in [
        "rustup",
        "~/.rustup",
        "~/.cargo",
        "sudo",
        "dtach",
        "curl",
        "ca-certificates",
    ] {
        assert!(said.contains(named), "the prompt names {named}: {said}");
    }
}

// The 2026-06-12 "horrendous typing latency on various" report: the
// fast cross-build silently fell back to `cargo install` ON the remote
// box, and even niced, a rustc compile on a small VPS wrecks the live
// session sharing it. When a croft session is running on the box, the
// compile must yield everything: one job and idle-class IO.
/// #262 review: a read of the settings that skipped a broken layer keeps
/// the last clean exclusions rather than falling back to none, which
/// would push to a host the user excluded.
#[test]
fn a_broken_config_read_keeps_the_last_good_sync_exclusions() {
    let mut ex = SyncExclusions { known: None };
    let broken = crate::config_layers::MergedConfig {
        prefs: crate::prefs::Prefs::default(),
        provenance: Default::default(),
        chain: Vec::new(),
        warnings: vec![String::from("config.json: expected value at line 1")],
        user_config_error: Some(String::from("config.json: expected value at line 1")),
    };
    // No clean read yet: the exclusions are unknown, not empty.
    ex.update(broken.clone());
    assert_eq!(ex.known, None);
    // A clean read applies.
    let prefs = crate::prefs::Prefs {
        config_sync_excluded_hosts: vec![String::from("shared-box")],
        config_sync_excluded_files: vec![String::from("macros.json")],
        ..Default::default()
    };
    let clean = crate::config_layers::MergedConfig {
        prefs,
        provenance: Default::default(),
        chain: Vec::new(),
        warnings: Vec::new(),
        user_config_error: None,
    };
    ex.update(clean);
    let good = Some((
        vec![String::from("shared-box")],
        vec![String::from("macros.json")],
    ));
    assert_eq!(ex.known, good);
    // A later broken read keeps it rather than falling back to none.
    ex.update(broken);
    assert_eq!(ex.known, good);
}

/// #262: a config file edited during the session is pushed once, on the
/// next check; an excluded file or host gets nothing; a failed push is
/// logged, kept, and retried after `REPUSH_RETRY` (not sooner), so an
/// edit made while the connection was down lands after the reconnect.
#[test]
fn a_live_repush_ships_each_edit_once_and_retries_a_failed_one() {
    use crate::config_sync::ConfigWatch;
    let dir = tempfile::tempdir().unwrap();
    let keys = dir.path().join("keybindings.json");
    let snips = dir.path().join("snippets.json");
    let macros = dir.path().join("macros.json");
    std::fs::write(&keys, "[]").unwrap();
    let mut state = RepushState::new(ConfigWatch::new(vec![
        keys.clone(),
        snips.clone(),
        macros.clone(),
    ]));
    let t0 = std::time::Instant::now();
    let mut sent: Vec<Vec<&'static str>> = Vec::new();
    let mut lines: Vec<String> = Vec::new();
    // `excluded`: None is an excluded host, else the excluded files.
    let tick = |state: &mut RepushState,
                now: std::time::Instant,
                excluded: Option<Vec<String>>,
                fail: &'static [&'static str],
                sent: &mut Vec<Vec<&'static str>>,
                lines: &mut Vec<String>| {
        repush_tick(
            state,
            dir.path(),
            now,
            &mut || match excluded.clone() {
                Some(files) => RepushGate::Push(files),
                None => RepushGate::HostExcluded,
            },
            &mut |files| {
                sent.push(files.iter().map(|(s, _)| s.name).collect());
                PushResult {
                    failed: fail.to_vec(),
                    held: Vec::new(),
                }
            },
            &mut |l| lines.push(l),
        );
    };
    let step = ConfigWatch::INTERVAL;
    tick(
        &mut state,
        t0 + step,
        Some(vec![]),
        &[],
        &mut sent,
        &mut lines,
    );
    assert!(sent.is_empty(), "nothing edited yet");
    std::fs::write(&keys, r#"[{"key": "ctrl+k"}]"#).unwrap();
    std::fs::write(&snips, "{}").unwrap();
    std::fs::write(&macros, "{}").unwrap();
    // macros.json is in config_sync_excluded_files: it stays home. The
    // snippets push fails (the connection is down).
    let failed_at = t0 + step * 2;
    tick(
        &mut state,
        failed_at,
        Some(vec![String::from("macros.json")]),
        &["snippets.json"],
        &mut sent,
        &mut lines,
    );
    assert_eq!(sent, vec![vec!["keybindings.json", "snippets.json"]]);
    assert_eq!(
        lines,
        vec![
            "Config sync: re-pushed keybindings.json".to_string(),
            "Config sync: could not re-push snippets.json (the remote keeps its copy)".to_string(),
        ]
    );
    // Before the retry delay nothing is sent; keybindings is not resent.
    tick(
        &mut state,
        t0 + step * 3,
        Some(vec![]),
        &[],
        &mut sent,
        &mut lines,
    );
    assert_eq!(sent.len(), 1, "no retry before REPUSH_RETRY");
    // After it, only the failed file goes again, and lands.
    tick(
        &mut state,
        failed_at + REPUSH_RETRY,
        Some(vec![]),
        &[],
        &mut sent,
        &mut lines,
    );
    assert_eq!(sent.last().unwrap(), &vec!["snippets.json"]);
    assert!(state.pending.is_empty());
    assert_eq!(
        lines.last().unwrap(),
        "Config sync: re-pushed snippets.json"
    );
    // A host in config_sync_excluded_hosts gets nothing.
    std::fs::write(&keys, "[ ]").unwrap();
    let later = failed_at + REPUSH_RETRY + step;
    tick(&mut state, later, None, &[], &mut sent, &mut lines);
    assert_eq!(sent.len(), 2, "an excluded host is never pushed to");
    assert!(state.pending.is_empty());
}

/// #262 review: while the exclusions have not read cleanly, a changed
/// file is held, not pushed, and goes once they do.
#[test]
fn a_live_repush_holds_changes_until_the_exclusions_read_cleanly() {
    use crate::config_sync::ConfigWatch;
    let dir = tempfile::tempdir().unwrap();
    let keys = dir.path().join("keybindings.json");
    std::fs::write(&keys, "[]").unwrap();
    let mut state = RepushState::new(ConfigWatch::new(vec![keys.clone()]));
    let t0 = std::time::Instant::now();
    std::fs::write(&keys, r#"[{"key": "ctrl+k"}]"#).unwrap();
    let mut sent = 0;
    let mut lines = Vec::new();
    let first = t0 + ConfigWatch::INTERVAL * 2;
    repush_tick(
        &mut state,
        dir.path(),
        first,
        &mut || RepushGate::Unknown,
        &mut |_| {
            sent += 1;
            PushResult::default()
        },
        &mut |l| lines.push(l),
    );
    assert_eq!(sent, 0, "nothing leaves while the exclusions are unknown");
    assert_eq!(state.pending.len(), 1, "the edit is held");
    assert!(lines[0].contains("holding changes"), "{lines:?}");
    repush_tick(
        &mut state,
        dir.path(),
        first + REPUSH_RETRY,
        &mut || RepushGate::Push(Vec::new()),
        &mut |_| {
            sent += 1;
            PushResult::default()
        },
        &mut |l| lines.push(l),
    );
    assert_eq!(sent, 1, "it goes once they read cleanly");
    assert!(state.pending.is_empty());
}

/// #262: a live re-push that finds the remote copy edited since croft
/// last pushed it holds the file: not reported as re-pushed, not
/// retried, and the log names the command that compares the two.
#[test]
fn a_live_repush_holds_a_file_edited_on_the_remote() {
    use crate::config_sync::ConfigWatch;
    let dir = tempfile::tempdir().unwrap();
    let keys = dir.path().join("keybindings.json");
    std::fs::write(&keys, "[]").unwrap();
    let mut state = RepushState::new(ConfigWatch::new(vec![keys.clone()]));
    let t0 = std::time::Instant::now();
    std::fs::write(&keys, r#"[{"key": "ctrl+k"}]"#).unwrap();
    let mut lines = Vec::new();
    repush_tick(
        &mut state,
        dir.path(),
        t0 + ConfigWatch::INTERVAL * 2,
        &mut || RepushGate::Push(Vec::new()),
        &mut |_| PushResult {
            failed: Vec::new(),
            held: vec!["keybindings.json"],
        },
        &mut |l| lines.push(l),
    );
    assert!(state.pending.is_empty(), "a held file is not retried");
    assert!(state.retry_at.is_none());
    assert!(!lines.iter().any(|l| l.contains("re-pushed")), "{lines:?}");
    assert!(
        lines
            .iter()
            .any(|l| l.contains("left keybindings.json alone") && l.contains("--diff")),
        "{lines:?}"
    );
}

/// #694: the build, and every rustc under it, is the kernel's first
/// choice when memory runs out, and the script still parses as sh.
#[test]
fn remote_install_marks_the_build_for_the_oom_killer_first() {
    let command = remote_install_command("abc123");
    let adj = command
        .find("echo 1000 > /proc/self/oom_score_adj")
        .unwrap();
    let install = command.find("cargo install --path").unwrap();
    assert!(adj < install, "the score must be set before the compile");
    let out = Command::new("sh")
        .args(["-n", "-c", &command])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn remote_install_compile_yields_to_a_live_croft_session() {
    let command = remote_install_command("abc123");
    assert!(
        command.contains("pgrep -x croft"),
        "must detect a live croft session on the box"
    );
    assert!(
        command.contains("CROFT_JOBS=1"),
        "a live session drops the compile to a single job"
    );
    assert!(
        command.contains("ionice") && command.contains("-c3"),
        "codegen writes must run in the idle IO class so the session's PTY never waits on them"
    );
    let gate = command.find("pgrep -x croft").unwrap();
    let install = command.find("cargo install --path").unwrap();
    assert!(gate < install, "the session check must precede the compile");
}

// #694: nice/ionice ration CPU and IO but not memory, and a two-job
// compile beside rust-analyzer OOM-killed croft on 8 GB hosts. The
// build must drop to one job below 16 GB, run memory-capped where
// systemd allows it, and flag itself so a live croft stops its language
// servers — all before the compile starts.
#[test]
fn remote_install_compile_is_memory_bounded() {
    let command = remote_install_command("abc123");
    let install = command.find("cargo install --path").unwrap();
    let before = |needle: &str| {
        let at = command
            .find(needle)
            .unwrap_or_else(|| panic!("missing {needle:?}"));
        assert!(at < install, "{needle:?} must come before the compile");
    };
    before("/proc/meminfo");
    before("-lt 16777216");
    before("systemd-run --user --scope");
    before("MemoryMax=");
    before("MemorySwapMax=0");
    before("CROFT_MARK=\"$HOME/.cache/croft/building.$$\"");
    before("trap '");
    assert!(
        command.contains(r#"eval "exec $CROFT_MEMCAP $CROFT_NICE $CROFT_IONICE""#),
        "the memory cap must wrap the compile itself"
    );
    // Little memory FREE drops to one job too, not just a small total:
    // a warning that promised one job without setting it was the bug.
    let avail = command.find("-lt 2097152").unwrap();
    let avail_branch = &command[avail..command[avail..].find("fi\n").unwrap() + avail];
    assert!(
        avail_branch.contains("CROFT_JOBS=1"),
        "low available memory must drop to one job: {avail_branch}"
    );
    // The marker goes as soon as the compile does, so language servers
    // are not held back through the rest of the install.
    let cleared = command
        .rfind("rm -f \"$CROFT_MARK\" \"$CROFT_MARK.boot\"")
        .unwrap();
    assert!(cleared > install);
}

/// The rendered install script as a `sh` command under a scratch home in
/// `dir`, with stub tools, not yet started.
///
/// Stubs stand in for everything the script would otherwise install or
/// probe, so it goes straight to the compile: `cc`, `pkg-config` and
/// `dtach` exist, and `pgrep` finds no live croft (so no 5 s wait).
/// `systemd-run` is the stub `systemd_run` rather than the host's: the
/// host's may well be on `PATH`, and its probe then fails only because
/// `env_clear` drops the user bus, which is not what a test should rest
/// on (#737).
///
/// The stub `cargo` waits, within a load-scaled budget rather than a
/// fixed sleep, until the marker names its own pid, writes `<its pid>
/// <the pid the marker holds> <the marker's boot sidecar>` to `seen`,
/// then, given `hold`, touches `<hold>.ready` and waits for `hold` to
/// exist before exiting with `cargo_exit`.
#[cfg(unix)]
fn install_command(
    dir: &std::path::Path,
    cargo_exit: u8,
    systemd_run: &str,
    seen: &std::path::Path,
    hold: Option<&std::path::Path>,
) -> std::process::Command {
    install_command_under("sh", dir, cargo_exit, systemd_run, seen, hold)
}

/// [`install_command`] under a given shell: the script runs under the
/// remote user's login shell, which is often zsh (#734).
#[cfg(unix)]
fn install_command_under(
    shell: &str,
    dir: &std::path::Path,
    cargo_exit: u8,
    systemd_run: &str,
    seen: &std::path::Path,
    hold: Option<&std::path::Path>,
) -> std::process::Command {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("bin");
    let home = dir.join("home");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(home.join(".cache/croft")).unwrap();
    let stub = |name: &str, body: &str| {
        let path = bin.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    for ok in ["cc", "pkg-config", "dtach"] {
        stub(ok, "exit 0");
    }
    stub("pgrep", "exit 1");
    stub("systemd-run", systemd_run);
    let tries =
        crate::test_budget::spawn_budget(std::time::Duration::from_secs(5)).as_millis() / 50;
    stub(
        "cargo",
        &format!(
            r#"i=0
while [ "$i" -lt {tries} ]; do
  for f in "$HOME"/.cache/croft/building.*; do
    case "$f" in
      *.boot) ;;
      *) if [ "$(cat "$f" 2>/dev/null)" = "$$" ]; then m=$$; b=$(cat "$f.boot" 2>/dev/null); break 2; fi ;;
    esac
  done
  sleep 0.05; i=$((i+1))
done
printf '%s %s %s' "$$" "$m" "$b" > "$CROFT_TEST_SEEN"
if [ -n "$CROFT_TEST_HOLD" ]; then
  : > "$CROFT_TEST_HOLD.ready"
  j=0
  while [ ! -e "$CROFT_TEST_HOLD" ] && [ "$j" -lt {tries} ]; do sleep 0.05; j=$((j+1)); done
fi
exit {cargo_exit}"#
        ),
    );
    let path = format!("{}:/usr/bin:/bin", bin.display());
    let mut cmd = std::process::Command::new(shell);
    cmd.arg("-c")
        .arg(remote_install_command("abc123"))
        .env_clear()
        .env("HOME", &home)
        .env("PATH", path)
        .env("CROFT_TEST_SEEN", seen)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if let Some(hold) = hold {
        cmd.env("CROFT_TEST_HOLD", hold);
    }
    cmd
}

/// Run the rendered install script to the end (see [`install_command`])
/// and report whether it succeeded and what the stub `cargo` saw.
#[cfg(unix)]
fn run_install_script(dir: &std::path::Path, cargo_exit: u8) -> (bool, String) {
    let seen = dir.join("seen");
    let status = install_command(dir, cargo_exit, "exit 1", &seen, None)
        .status()
        .unwrap();
    let seen = std::fs::read_to_string(seen).unwrap_or_default();
    (status.success(), seen)
}

/// Alive and not a zombie: a killed script's cargo is reparented, and
/// reads as alive to `kill -0` until its new parent reaps it.
#[cfg(target_os = "linux")]
fn running(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|s| s.rsplit_once(") ").map(|(_, rest)| !rest.starts_with('Z')))
        .unwrap_or(false)
}

/// Whether croft would see a live build in the scratch home's cache.
#[cfg(target_os = "linux")]
fn build_seen(dir: &std::path::Path) -> bool {
    crate::update_watch::source_build_running(
        &dir.join("home/.cache/croft"),
        crate::update_watch::boot_id,
        crate::update_watch::boot_time,
        running,
    )
}

/// #737: SIGTERM to the script's shell mid-compile leaves cargo running,
/// and the marker keeps naming it: croft still sees the build, and sees
/// it end when cargo does.
#[cfg(target_os = "linux")]
#[test]
fn a_killed_install_shell_leaves_the_marker_naming_the_live_compile() {
    let tmp = tempfile::tempdir().unwrap();
    let hold = tmp.path().join("hold");
    let seen = tmp.path().join("seen");
    let mut shell = install_command(tmp.path(), 0, "exit 1", &seen, Some(&hold))
        .spawn()
        .unwrap();
    let ready = tmp.path().join("hold.ready");
    crate::test_budget::await_spawned(
        std::time::Duration::from_secs(10),
        "the stub compile to start",
        || ready.exists(),
    );
    let seen = std::fs::read_to_string(&seen).unwrap();
    let cargo: u32 = seen.split(' ').next().unwrap().parse().unwrap();
    assert!(seen.starts_with(&format!("{cargo} {cargo}")), "{seen}");

    // SAFETY: signals the shell this test spawned, by its own pid.
    unsafe {
        libc::kill(shell.id() as libc::pid_t, libc::SIGTERM);
    }
    let _ = shell.wait();
    assert!(running(cargo), "the compile must outlive its shell");
    assert_eq!(
        leftover_markers(tmp.path())
            .iter()
            .filter(|n| !n.ends_with(".boot"))
            .map(|n| std::fs::read_to_string(tmp.path().join("home/.cache/croft").join(n)).unwrap())
            .collect::<Vec<_>>(),
        vec![cargo.to_string()],
        "the marker must keep naming the compile"
    );
    assert!(build_seen(tmp.path()), "croft must still see the build");

    std::fs::write(&hold, "").unwrap();
    crate::test_budget::await_spawned(
        std::time::Duration::from_secs(10),
        "the build to read as over once the compile exits",
        || !build_seen(tmp.path()),
    );
}

/// #737: two installs at once each keep their own marker, so the one
/// finishing first cannot make the other's build look over.
#[cfg(target_os = "linux")]
#[test]
fn a_second_install_finishing_first_leaves_the_first_ones_marker() {
    let tmp = tempfile::tempdir().unwrap();
    let hold = tmp.path().join("hold");
    let mut long = install_command(
        tmp.path(),
        0,
        "exit 1",
        &tmp.path().join("seen-long"),
        Some(&hold),
    )
    .spawn()
    .unwrap();
    let ready = tmp.path().join("hold.ready");
    crate::test_budget::await_spawned(
        std::time::Duration::from_secs(10),
        "the long build's compile to start",
        || ready.exists(),
    );
    let short = install_command(
        tmp.path(),
        0,
        "exit 1",
        &tmp.path().join("seen-short"),
        None,
    )
    .status()
    .unwrap();
    assert!(short.success());
    assert!(
        build_seen(tmp.path()),
        "the short build's end hid the long one: {:?}",
        leftover_markers(tmp.path())
    );

    std::fs::write(&hold, "").unwrap();
    assert!(long.wait().unwrap().success());
    assert!(leftover_markers(tmp.path()).is_empty());
}

/// #737: the compile runs under the memory cap only when the probe reads
/// the limit back from its test scope; `max` means the controller is not
/// delegated and the limit would be ignored.
#[cfg(target_os = "linux")]
#[test]
fn the_install_script_caps_the_compile_only_when_the_limit_reads_back() {
    for (probe, capped) in [("67108864", true), ("max", false)] {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("capped");
        let systemd_run = format!(
            r#"while [ $# -gt 0 ]; do case "$1" in -p) shift 2 ;; --*) shift ;; *) break ;; esac; done
if [ "$1" = sh ]; then echo {probe}; exit 0; fi
: > "{}"
exec "$@""#,
            log.display()
        );
        let seen = tmp.path().join("seen");
        let status = install_command(tmp.path(), 0, &systemd_run, &seen, None)
            .status()
            .unwrap();
        assert!(status.success(), "probe {probe}");
        assert_eq!(log.exists(), capped, "probe {probe}");
        // Capped or not, the marker named the compile itself.
        let seen = std::fs::read_to_string(&seen).unwrap();
        let mut f = seen.split(' ');
        assert_eq!(f.next(), f.next(), "probe {probe}: {seen}");
    }
}

/// Any `building.*` marker left under the scratch home.
#[cfg(unix)]
fn leftover_markers(dir: &std::path::Path) -> Vec<String> {
    std::fs::read_dir(dir.join("home/.cache/croft"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("building."))
        .collect()
}

/// #694 review: the marker names the COMPILE's pid while it runs, and is
/// gone after the build, whether it succeeded or failed.
#[cfg(unix)]
#[test]
fn the_install_script_marks_the_compile_and_clears_the_mark() {
    let tmp = tempfile::tempdir().unwrap();
    let (ok, seen) = run_install_script(tmp.path(), 0);
    assert!(ok, "the stubbed install must succeed");
    let mut fields = seen.splitn(3, ' ');
    let (own, marked, boot) = (
        fields.next().unwrap(),
        fields.next().expect("cargo ran and reported"),
        fields.next().unwrap_or_default(),
    );
    // #736: the marker carries the boot it was written in, where the
    // host has one.
    let host_boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .map(|b| b.trim().to_string())
        .unwrap_or_default();
    assert_eq!(boot.trim(), host_boot, "the marker's boot sidecar");
    assert_eq!(
        own, marked,
        "while cargo runs the marker must name cargo itself, so a killed \
             script shell cannot make the build look finished"
    );
    assert!(leftover_markers(tmp.path()).is_empty());
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("home/.cache/croft/install-stamp")).unwrap(),
        "abc123"
    );

    // A failing compile: `set -e` ends the script, and the EXIT trap
    // still removes the marker.
    let tmp = tempfile::tempdir().unwrap();
    let (ok, _) = run_install_script(tmp.path(), 3);
    assert!(!ok, "a failing compile must fail the install");
    assert!(
        leftover_markers(tmp.path()).is_empty(),
        "a failed build left its marker behind"
    );
}

/// #734: zsh aborts on a glob that matches nothing, and the marker
/// sweep's glob matches nothing on every host with an empty cache, so a
/// zsh login shell never reached cargo. It must reach it, and a failed
/// compile must still fail the install.
#[cfg(unix)]
#[test]
fn the_install_script_reaches_cargo_under_zsh() {
    if !crate::lsp::manager::is_on_path("zsh") {
        eprintln!("SKIPPED: zsh not on PATH");
        return;
    }
    let under_zsh = |dir: &std::path::Path, cargo_exit| {
        let seen = dir.join("seen");
        let status = install_command_under("zsh", dir, cargo_exit, "exit 1", &seen, None)
            .status()
            .unwrap();
        (
            status.success(),
            std::fs::read_to_string(seen).unwrap_or_default(),
        )
    };
    let tmp = tempfile::tempdir().unwrap();
    let (ok, seen) = under_zsh(tmp.path(), 0);
    assert!(ok, "an empty cache must not stop the install under zsh");
    let mut fields = seen.splitn(3, ' ');
    let own = fields.next().unwrap();
    let marked = fields.next().expect("cargo ran under zsh");
    assert_eq!(own, marked);
    assert!(leftover_markers(tmp.path()).is_empty());
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("home/.cache/croft/install-stamp")).unwrap(),
        "abc123"
    );

    let tmp = tempfile::tempdir().unwrap();
    let (ok, _) = under_zsh(tmp.path(), 3);
    assert!(!ok, "a failing compile must fail the install under zsh");
    assert!(
        !tmp.path().join("home/.cache/croft/install-stamp").exists(),
        "a failed compile must not write the install stamp"
    );
}

/// Stale markers from dead builds are swept; a live one is kept.
#[cfg(unix)]
#[test]
fn the_install_script_sweeps_only_dead_markers() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("home/.cache/croft");
    std::fs::create_dir_all(&cache).unwrap();
    // This test process is alive and ours, as a real build's compile
    // is: `kill -0` on another user's pid (pid 1, say) fails with EPERM
    // for a non-root user and would read as dead. A pid past pid_max is
    // never alive.
    std::fs::write(cache.join("building.1"), std::process::id().to_string()).unwrap();
    std::fs::write(cache.join("building.2"), "2147483646").unwrap();
    // #736: a live pid from another boot is someone else's process now,
    // and an orphaned sidecar goes with nothing to describe.
    let this_boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok();
    if let Some(boot) = &this_boot {
        std::fs::write(cache.join("building.1.boot"), boot.trim()).unwrap();
        std::fs::write(cache.join("building.3"), std::process::id().to_string()).unwrap();
        std::fs::write(cache.join("building.3.boot"), "another-boot").unwrap();
    }
    std::fs::write(cache.join("building.4.boot"), "orphan").unwrap();
    let (ok, _) = run_install_script(tmp.path(), 0);
    assert!(ok);
    let mut left = leftover_markers(tmp.path());
    left.sort();
    let expect: Vec<String> = if this_boot.is_some() {
        vec!["building.1".into(), "building.1.boot".into()]
    } else {
        vec!["building.1".into()]
    };
    assert_eq!(
        left, expect,
        "only the live build of this boot, and its sidecar, may survive another build"
    );
}

// A backgrounded install's log lines died with the connect dialog,
// which is why the various fallback went undiagnosed. Every line must
// also land in a persistent local file.
#[test]
fn install_log_tee_writes_lines_to_file_and_forwards_them_unchanged() {
    let dir = std::env::temp_dir().join(format!("croft-log-tee-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("install.log");
    let (down_tx, down_rx) = std::sync::mpsc::channel::<String>();
    let tee = spawn_log_tee(path.clone(), down_tx);
    tee.send(String::from("Cross-compiling croft locally"))
        .unwrap();
    tee.send(String::from("Local cross-build skipped (zig missing)"))
        .unwrap();
    let timeout = std::time::Duration::from_secs(2);
    assert_eq!(
        down_rx.recv_timeout(timeout).unwrap(),
        "Cross-compiling croft locally",
        "downstream consumers must still receive every line"
    );
    assert_eq!(
        down_rx.recv_timeout(timeout).unwrap(),
        "Local cross-build skipped (zig missing)"
    );
    drop(tee);
    let deadline = std::time::Instant::now() + timeout;
    let mut contents = String::new();
    while std::time::Instant::now() < deadline {
        contents = std::fs::read_to_string(&path).unwrap_or_default();
        if contents.contains("zig missing") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(contents.contains("Cross-compiling croft locally"));
    assert!(
        contents.contains("Local cross-build skipped (zig missing)"),
        "the skip REASON is the whole point of the persistent log"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn remote_install_command_ensures_c_toolchain_unconditionally() {
    // Regression: cargo can be present on a box that still lacks `cc`, so a
    // bare `cargo install` dies with `linker `cc` not found`. The toolchain
    // ensure must run on its own, not only when cargo+curl are both absent,
    // and it must precede the compile.
    let command = remote_install_command("abc123");
    assert!(command.contains("croft_ensure_build_toolchain"));
    assert!(command.contains("build-essential"));
    let ensure_call = command
        .rfind("\ncroft_ensure_build_toolchain")
        .expect("toolchain ensure must be invoked");
    let cargo_install = command
        .find("cargo install --path")
        .expect("cargo install must be present");
    assert!(
        ensure_call < cargo_install,
        "the C toolchain must be ensured before `cargo install` runs"
    );
}

#[test]
fn a_relay_request_is_claimed_by_exactly_one_pump() {
    // `mkdir` without -p on the id: the second pump's mkdir fails.
    let tmp = tempfile::tempdir().unwrap();
    let inbox = tmp.path().join("inbox").display().to_string();
    let run = |cmd: String| {
        std::process::Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .status()
            .unwrap()
            .success()
    };
    assert!(run(claim_command(&inbox, "open-1-2")));
    assert!(
        !run(claim_command(&inbox, "open-1-2")),
        "a second claim loses"
    );
    assert!(run(claim_command(&inbox, "open-1-3")));
    assert_eq!(
        super::parse_relay_request("open\topen-1\thttps://x/")
            .and_then(|r| r.id().map(str::to_string)),
        Some(String::from("open-1"))
    );
}

#[test]
fn arch_to_musl_triple_maps_every_arch_uname_reports_for_supported_targets() {
    assert_eq!(
        arch_to_musl_triple("x86_64"),
        Some("x86_64-unknown-linux-musl")
    );
    assert_eq!(
        arch_to_musl_triple("amd64"),
        Some("x86_64-unknown-linux-musl"),
        "BSD-style `uname -m` reports amd64 for the same machine class linux reports as x86_64; the fast-install path must accept both"
    );
    assert_eq!(
        arch_to_musl_triple("aarch64"),
        Some("aarch64-unknown-linux-musl")
    );
    assert_eq!(
        arch_to_musl_triple("arm64"),
        Some("aarch64-unknown-linux-musl"),
        "Apple Silicon Linux VMs report arm64; same triple as aarch64"
    );
}

#[test]
fn arch_to_musl_triple_returns_none_for_unsupported_archs_so_caller_falls_back() {
    for arch in ["i686", "armv7l", "ppc64le", "riscv64", "mips", ""] {
        assert_eq!(
            arch_to_musl_triple(arch),
            None,
            "{arch} has no statically-known musl target in croft's bundled toolchain; caller must fall back to remote cargo install"
        );
    }
}

#[test]
fn remote_install_check_uses_cargo_path_before_probe() {
    let command = remote_install_check_command();
    let path_pos = command
        .find("export PATH=\"$HOME/.cargo/bin:$PATH\"")
        .unwrap();
    let probe_pos = command.find("command -v croft").unwrap();
    assert!(command.contains(". \"$HOME/.cargo/env\""));
    assert!(path_pos < probe_pos);
    assert!(command.contains("cat \"$HOME/.cache/croft/install-stamp\""));
}

#[test]
fn shell_quote_for_e_arg_passes_alnum_paths_through() {
    assert_eq!(
        shell_quote_for_e_arg(Path::new("/tmp/croft-ssh-1234/ctl")),
        "/tmp/croft-ssh-1234/ctl",
    );
}

#[test]
fn shell_quote_for_e_arg_quotes_paths_with_spaces() {
    let q = shell_quote_for_e_arg(Path::new("/Users/v a/croft/ctl"));
    assert_eq!(q, "'/Users/v a/croft/ctl'");
}

#[test]
fn shell_quote_for_e_arg_escapes_single_quotes_inside_path() {
    let q = shell_quote_for_e_arg(Path::new("/tmp/it's/ctl"));
    // /tmp/it's → /tmp/it'\''s after the standard sh single-quote
    // escaping trick (close, escaped quote, reopen).
    assert_eq!(q, "'/tmp/it'\\''s/ctl'");
}

#[test]
fn ssh_control_socket_path_is_short_and_inside_temp_dir() {
    let dir = std::env::temp_dir().join("croft-ssh-test");
    let socket = ssh_control_socket_path_for_test(&dir);
    assert!(socket.starts_with(&dir));
    assert_eq!(socket.file_name().and_then(|s| s.to_str()), Some("ctl"));
}

fn args_of(cmd: &Command) -> Vec<String> {
    cmd.get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
}

// The other half of the launch-now bug class. Bytes are not the only
// shared resource: ssh forwards the caller's stdin to the remote command
// unless `-n` is given, and on this path that stdin is the terminal the
// attached remote croft is reading. Two processes then read() the same
// tty and each keystroke reaches exactly one of them, so typing into the
// remote session silently vanishes for as long as the background command
// runs. Verified against a real host: `ssh h 'sleep 1' < probe` drains
// all 16 bytes even though the remote command never reads stdin, while
// `ssh -n h 'sleep 1' < probe` leaves every byte for the next reader.
#[test]
fn background_ssh_never_reads_the_attached_sessions_stdin() {
    let socket = PathBuf::from("/tmp/croft-ctl/ctl");
    let background = args_of(&ssh_socket_command(&socket, true));
    assert!(
        background.iter().any(|a| a == "-n"),
        "a background command sharing the session's tty steals its keystrokes"
    );
    let interactive = args_of(&ssh_socket_command(&socket, false));
    assert!(
        !interactive.iter().any(|a| a == "-n"),
        "the interactive session is the one process that must keep the tty"
    );
}

// The launch-now bug class: a background install shipping bytes through
// the SAME multiplexed TCP connection as the live remote session queues
// megabytes ahead of every keystroke (SSH head-of-line blocking) and
// makes the running croft unusable. Every bulk rsync must honor the
// bulk lane's routing and pacing.
#[test]
fn binary_ship_never_rides_the_interactive_master_when_lane_is_dedicated() {
    let lane = crate::remote_bulk::BulkLane::new(
        crate::remote_bulk::LaneMode::Dedicated {
            socket_path: PathBuf::from("/tmp/bulk/ctl"),
        },
        512,
    );
    let cmd = ship_file_rsync_command(
        &lane,
        Path::new("/tmp/interactive/ctl"),
        Path::new("/tmp/target/croft"),
        "host:.cargo/bin/croft.new",
    );
    let args = args_of(&cmd);
    let e = args
        .iter()
        .position(|a| a == "-e")
        .map(|i| args[i + 1].clone())
        .expect("rsync must use an explicit -e remote shell");
    assert!(e.contains("/tmp/bulk/ctl"));
    assert!(!e.contains("/tmp/interactive/ctl"));
    assert!(
        args.iter().any(|a| a == "--bwlimit=512"),
        "unpaced bulk saturates the uplink queue and lags the session even on its own connection"
    );
}

#[test]
fn a_config_push_rides_the_bulk_lane_like_the_binary_does() {
    // Config files are small, but they ship on the same connection as
    // the live session; an unpaced push still queues ahead of keystrokes.
    let lane = crate::remote_bulk::BulkLane::new(
        crate::remote_bulk::LaneMode::Dedicated {
            socket_path: PathBuf::from("/tmp/bulk/ctl"),
        },
        512,
    );
    let cmd = ship_file_rsync_command(
        &lane,
        Path::new("/tmp/interactive/ctl"),
        Path::new("/home/u/.config/croft/keybindings.json"),
        &crate::config_sync::remote_dest("host", "keybindings.json"),
    );
    let args = args_of(&cmd);
    let e = args
        .iter()
        .position(|a| a == "-e")
        .map(|i| args[i + 1].clone())
        .expect("rsync must use an explicit -e remote shell");
    assert!(e.contains("/tmp/bulk/ctl"));
    assert!(!e.contains("/tmp/interactive/ctl"));
    assert!(args.iter().any(|a| a == "--bwlimit=512"));
    assert!(
        args.iter().any(|a| a == "--checksum"),
        "content comparison is what makes the push a no-op when nothing changed"
    );
    assert!(
        args.iter()
            .any(|a| a == "host:.config/croft/keybindings.json"),
        "the file must land in the remote's config dir, got {args:?}"
    );
}

#[test]
fn a_config_push_never_carries_the_trust_bearing_config_json() {
    // The argv is the last place this can be caught, so assert on it
    // rather than only on the allow-list the argv is built from.
    let lane = crate::remote_bulk::BulkLane::new(crate::remote_bulk::LaneMode::SharedMux, 300);
    for s in crate::config_sync::SYNCABLE {
        let cmd = ship_file_rsync_command(
            &lane,
            Path::new("/tmp/ctl"),
            Path::new("/home/u/.config/croft").join(s.name).as_path(),
            &crate::config_sync::remote_dest("host", s.name),
        );
        let args = args_of(&cmd);
        assert!(
            !args.iter().any(|a| a.ends_with("config.json")),
            "config.json carries MCP consent and must never be in a push argv, got {args:?}"
        );
    }
}

#[test]
fn binary_ship_throttles_on_the_shared_mux_when_no_lane_is_available() {
    let lane = crate::remote_bulk::BulkLane::new(crate::remote_bulk::LaneMode::SharedMux, 300);
    let cmd = ship_file_rsync_command(
        &lane,
        Path::new("/tmp/interactive/ctl"),
        Path::new("/tmp/target/croft"),
        "host:.cargo/bin/croft.new",
    );
    let args = args_of(&cmd);
    let e = args
        .iter()
        .position(|a| a == "-e")
        .map(|i| args[i + 1].clone())
        .expect("rsync must use an explicit -e remote shell");
    assert!(e.contains("/tmp/interactive/ctl"));
    assert!(args.iter().any(|a| a == "--bwlimit=300"));
    assert!(args.iter().any(|a| a == "--checksum"));
}

/// The lane wiring, plus the allow-list that replaced the deny-list.
///
/// The previous version of this test asserted `--exclude=target` under the
/// comment "shipping target/ would be gigabytes through the lane". It
/// passed on every run while 176 GB of `target.noindex` went over the wire,
/// because it checked the RULE and never the OUTCOME. The outcome is now
/// pinned by `the_source_sync_ships_only_build_inputs`, which runs rsync
/// for real; this one only guarantees the allow-list reaches the command.
#[test]
fn source_sync_honors_the_bulk_lane_and_allow_lists_the_build_inputs() {
    let lane = crate::remote_bulk::BulkLane::new(
        crate::remote_bulk::LaneMode::Dedicated {
            socket_path: PathBuf::from("/tmp/bulk/ctl"),
        },
        700,
    );
    let cmd = source_sync_rsync_command(
        &lane,
        Path::new("/tmp/interactive/ctl"),
        std::ffi::OsStr::new("/Users/v/croft/"),
        "host:.cache/croft/source/",
    );
    let args = args_of(&cmd);
    let e = args
        .iter()
        .position(|a| a == "-e")
        .map(|i| args[i + 1].clone())
        .expect("rsync must use an explicit -e remote shell");
    assert!(e.contains("/tmp/bulk/ctl"));
    assert!(!e.contains("/tmp/interactive/ctl"));
    assert!(args.iter().any(|a| a == "--bwlimit=700"));
    for name in SOURCE_STAMP_INPUTS {
        assert!(
            args.iter().any(|a| a == &format!("--include=/{name}")),
            "{name} is a build input and must be shipped"
        );
    }
    assert!(
        args.iter().any(|a| a == "--exclude=*"),
        "the allow-list is only an allow-list if everything else is denied"
    );
    for name in SOURCE_SKIP_NAMES {
        assert!(
            args.iter().any(|a| a == &format!("--exclude={name}")),
            "{name} is skipped by the stamp walk, so it must not be shipped"
        );
    }
    assert!(args.iter().any(|a| a == "--delete"));
}

// The 2026-06-12 root cause on various: croft inherited macOS's default
// 256-fd soft limit, the zig linker needed ~250 rlibs open at once and
// died with ProcessFdQuotaExceeded, and the installer silently fell
// back to compiling on the remote box under the live session.
#[test]
fn raise_fd_limit_lifts_the_soft_limit_beyond_the_linkers_needs() {
    let soft = raise_fd_limit();
    assert!(
        soft >= 4096,
        "croft's cross-link opens ~250 rlibs plus zig's own files; a {soft}-fd soft limit reproduces ProcessFdQuotaExceeded"
    );
}

// Guards the exact bug class `sync_workspace_lock` exists to fix: a version
// bump in Cargo.toml that leaves Cargo.lock one patch behind makes the
// `--locked` remote cross-build fail and silently host-compile. A drifted
// lock should never reach a commit, so fail the suite if the two disagree.
#[test]
fn cargo_lock_croft_version_matches_cargo_toml_so_locked_cross_build_never_falls_back() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let toml = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
    let toml_version = toml
        .lines()
        .find_map(|l| {
            l.strip_prefix("version = \"")
                .and_then(|r| r.strip_suffix('"'))
        })
        .expect("Cargo.toml has a package version");

    let lock = std::fs::read_to_string(root.join("Cargo.lock")).unwrap();
    let lock_version = lock
        .split("name = \"croft-software\"")
        .nth(1)
        .and_then(|after| {
            after.lines().find_map(|l| {
                l.trim()
                    .strip_prefix("version = \"")
                    .and_then(|r| r.strip_suffix('"'))
            })
        })
        .expect("Cargo.lock has a croft entry");

    assert_eq!(
        lock_version, toml_version,
        "Cargo.lock croft version {lock_version} drifted from Cargo.toml {toml_version}; run `cargo update -p croft-software`"
    );
}

// 2026-08-22: a croft installed from `git#de1a7ab7` hashed its frozen
// ~/.cargo checkout, said "already up to date", and silently shipped
// nothing while the fix sat in the working repo. The snapshot warning
// exists so that no-op announces itself.
#[test]
fn snapshot_dirs_are_detected_and_real_repos_are_not() {
    assert!(source_dir_is_snapshot(
        "/Users/u/.cargo/registry/src/index.crates.io-6f17d22bba15001f/croft-software-0.1.757"
    ));
    assert!(source_dir_is_snapshot(
        "/home/u/.cargo/git/checkouts/croft-9a1f/de1a7ab"
    ));
    assert!(source_dir_is_snapshot(
        "/custom/cargo-home/.cargo/git/checkouts/croft-9a1f/de1a7ab"
    ));
    // CARGO_HOME=/custom/cargo-home has no `.cargo` component at all
    assert!(source_dir_is_snapshot(
        "/custom/cargo-home/registry/src/index.crates.io-6f17d22bba15001f/croft-software-0.1.757"
    ));
    assert!(source_dir_is_snapshot(
        "/custom/cargo-home/git/checkouts/croft-9a1f/de1a7ab"
    ));
    assert!(!source_dir_is_snapshot("/Users/u/Documents/croft"));
    assert!(!source_dir_is_snapshot("/home/u/work/croft"));
    assert!(
        source_snapshot_warning().is_none(),
        "the test build itself must come from a working tree, not a snapshot"
    );
}
