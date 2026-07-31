//! Repair the `PATH` a GUI launch inherits.
//!
//! A terminal launch hands croft the PATH the user's shell built: homebrew,
//! nix, cargo, uv, everything. A GUI launch does not. macOS launchd starts
//! Croft.app (Dock click, Spotlight, an opened document) with the bare
//! `/usr/bin:/bin:/usr/sbin:/sbin`, and Ghostty's `--initial-command` runs
//! croft under `bash --noprofile --norc`, so no rc file ever puts the missing
//! directories back. Every tool croft shells out to then becomes invisible:
//! `pdftoppm` disappears and PDF previews freeze on page 1, `pdfinfo`
//! disappears and the page count with it, and the same goes for git, ripgrep,
//! language servers and the pair-programming CLIs.
//!
//! The fix is to ask the user's login shell what the PATH actually is, once,
//! before croft spawns anything. This is what VS Code and Zed both do for the
//! same reason. Only a launch that arrives with launchd's exact PATH pays for
//! the probe, so a terminal launch costs nothing.

use std::ffi::OsStr;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Exactly what launchd hands a GUI process. Any other PATH came from a
/// shell that already ran the user's configuration, so we leave it alone.
const LAUNCHD_DIRS: [&str; 4] = ["/usr/bin", "/bin", "/usr/sbin", "/sbin"];

/// How long the login shell gets to answer. A shell that hangs on its own rc
/// file must not hang croft's startup; we keep the stripped PATH instead.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// Markers around the value so an interactive rc file's own chatter (banners,
/// version notices, `fastfetch`) can never be mistaken for the PATH.
const BEGIN: &str = "__CROFT_PATH_BEGIN__";
const END: &str = "__CROFT_PATH_END__";

/// Whether this PATH is a GUI launch's stripped inheritance rather than a
/// shell's. Empty or unset counts: it is no more usable than launchd's.
///
/// Directories inside an application bundle do not count either way. The app
/// that launched us puts its own there (Ghostty appends
/// `/Applications/Ghostty.app/Contents/MacOS`, iTerm2 its `utilities`), which
/// no user's shell would ever do, so their presence is not evidence that a
/// shell ran.
fn is_stripped(path: Option<&str>) -> bool {
    let Some(path) = path.map(str::trim).filter(|p| !p.is_empty()) else {
        return true;
    };
    path.split(':')
        .filter(|e| !e.is_empty())
        .filter(|e| !e.contains(".app/"))
        .all(|e| LAUNCHD_DIRS.contains(&e.trim_end_matches('/')))
}

/// The login shell's PATH ahead of what we already have, with duplicates
/// dropped. Keeping the inherited entries means the system directories
/// survive even if the user's shell drops them.
fn merge(current: Option<&str>, probed: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for entry in probed
        .split(':')
        .chain(current.unwrap_or_default().split(':'))
        .filter(|e| !e.is_empty())
    {
        if !out.contains(&entry) {
            out.push(entry);
        }
    }
    out.join(":")
}

/// Ask `shell` for its PATH. `-l -i` so both the login files (`.zprofile`,
/// `.bash_profile`) and the interactive ones (`.zshrc`) get a say, since
/// either may be where the user builds their PATH (csh/tcsh accept `-l` only
/// as the sole argument, so that family runs `-i` alone). The value is
/// printed by an inner `/bin/sh` reading the *environment*: fish expands its
/// own quoted `$PATH` list space-joined, while the exported variable is
/// colon-joined in every shell. stdin is closed so a shell that prompts gets
/// EOF instead of blocking, and the whole thing is bounded by
/// [`PROBE_TIMEOUT`].
fn probe(shell: &str) -> Option<String> {
    run_probe(probe_cmd(shell), PROBE_TIMEOUT)
}

/// The probe invocation for `shell`, before it is spawned. Split from
/// [`probe`] so a test can inject an environment (`HOME`) around it.
fn probe_cmd(shell: &str) -> Command {
    let mut cmd = Command::new(shell);
    let name = std::path::Path::new(shell)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if name.ends_with("csh") {
        // csh/tcsh accept `-l` only as the sole argument, so the login-shell
        // marking goes the other way: a leading dash in argv[0], the same
        // convention login(1) uses. Without it tcsh reads `.cshrc` but never
        // `~/.login` - the conventional place a csh user builds `path` - and
        // the probe recovers nothing for the csh family.
        std::os::unix::process::CommandExt::arg0(&mut cmd, format!("-{name}"));
        cmd.arg("-i");
    } else {
        cmd.args(["-l", "-i"]);
    }
    cmd.arg("-c")
        .arg(format!(r#"/bin/sh -c 'printf "{BEGIN}%s{END}" "$PATH"'"#));
    cmd
}

/// Run a prepared probe command and return the PATH it printed, giving up
/// after `timeout`.
fn run_probe(mut cmd: Command, timeout: Duration) -> Option<String> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    // Read until the END marker, not EOF: a backgrounded grandchild from an
    // rc file (`(brew update &)`, an agent start) inherits the pipe's write
    // end and holds EOF back long after the shell answered and exited.
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            match std::io::Read::read(&mut stdout, &mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    if String::from_utf8_lossy(&buf).contains(END) {
                        break;
                    }
                }
            }
        }
        let _ = tx.send(buf);
    });
    let out = rx.recv_timeout(timeout).ok();
    // Either it answered, or it is wedged and must not outlive the probe
    // holding a pipe open. The reap runs off-thread: a shell stuck in
    // uninterruptible sleep (rc file stat'ing a dead network mount) cannot
    // be killed, and a synchronous wait() on it would wedge startup after
    // all.
    let _ = child.kill();
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    extract(&String::from_utf8_lossy(&out?))
}

/// Pull the PATH out from between the markers.
fn extract(out: &str) -> Option<String> {
    let (_, rest) = out.split_once(BEGIN)?;
    let (path, _) = rest.split_once(END)?;
    let path = path.trim();
    (!path.is_empty()).then(|| path.to_string())
}

/// The PATH to run under, or `None` to keep what we inherited.
fn repaired(current: Option<&str>, probe: impl FnOnce() -> Option<String>) -> Option<String> {
    if !is_stripped(current) {
        return None;
    }
    let probed = probe()?;
    let merged = merge(current, &probed);
    (merged != current.unwrap_or_default()).then_some(merged)
}

/// The user's login shell. launchd sets `SHELL` from the user record for GUI
/// sessions; zsh is the macOS default when it does not.
fn login_shell() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| String::from("/bin/zsh"))
}

/// Put the user's real PATH back when croft was started by the GUI. Call once
/// at startup, before anything spawns a child or a thread.
pub fn repair() {
    let current = std::env::var("PATH").ok();
    let Some(fixed) = repaired(current.as_deref(), || probe(&login_shell())) else {
        return;
    };
    // SAFETY: `set_var` is only unsafe because another thread may be reading
    // the environment concurrently. This runs at the top of `Cli::run`, before
    // croft starts the tokio runtime, any watcher, or any child process.
    unsafe { std::env::set_var("PATH", OsStr::new(&fixed)) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launchd_path_reads_as_stripped() {
        assert!(is_stripped(Some("/usr/bin:/bin:/usr/sbin:/sbin")));
        // Order and trailing slashes are launchd's business, not a signal.
        assert!(is_stripped(Some("/bin:/usr/bin:/sbin/:/usr/sbin")));
        assert!(is_stripped(Some("")));
        assert!(is_stripped(None));
    }

    /// The PATH a Dock click actually produces, read off a live Croft.app
    /// launch: launchd's four directories plus the bundle dir Ghostty appends
    /// for itself. An app putting its own bundle on PATH is not a shell having
    /// run, so this still needs repairing.
    #[test]
    fn a_dock_launch_through_ghostty_reads_as_stripped() {
        assert!(is_stripped(Some(
            "/usr/bin:/bin:/usr/sbin:/sbin:/Applications/Ghostty.app/Contents/MacOS"
        )));
        assert!(is_stripped(Some(
            "/Applications/iTerm.app/Contents/Resources/utilities:/usr/bin:/bin"
        )));
    }

    #[test]
    fn a_shell_built_path_is_left_alone() {
        assert!(!is_stripped(Some(
            "/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin"
        )));
        assert!(!is_stripped(Some("/usr/bin:/bin:/Users/v/.cargo/bin")));
    }

    #[test]
    fn merge_puts_the_login_shell_first_and_keeps_the_system_dirs() {
        let merged = merge(
            Some("/usr/bin:/bin:/usr/sbin:/sbin"),
            "/opt/homebrew/bin:/usr/bin:/bin",
        );
        assert_eq!(merged, "/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin");
    }

    #[test]
    fn merge_drops_duplicates() {
        let merged = merge(Some("/usr/bin:/usr/bin"), "/usr/bin:/opt/homebrew/bin");
        assert_eq!(merged, "/usr/bin:/opt/homebrew/bin");
    }

    #[test]
    fn extract_ignores_rc_file_chatter() {
        let out = format!("Welcome to zsh!\nnode v22\n{BEGIN}/opt/homebrew/bin:/usr/bin{END}");
        assert_eq!(extract(&out).as_deref(), Some("/opt/homebrew/bin:/usr/bin"));
    }

    #[test]
    fn extract_returns_none_without_both_markers() {
        assert!(extract("/opt/homebrew/bin").is_none());
        assert!(extract(&format!("{BEGIN}/opt/homebrew/bin")).is_none());
        assert!(extract(&format!("{BEGIN}{END}")).is_none());
    }

    /// The whole point: a GUI launch gets the shell's directories back, so
    /// `pdftoppm` and friends are findable again.
    #[test]
    fn a_gui_launch_gets_the_shell_path_back() {
        let fixed = repaired(Some("/usr/bin:/bin:/usr/sbin:/sbin"), || {
            Some(String::from("/opt/homebrew/bin:/usr/bin:/bin"))
        });
        assert_eq!(
            fixed.as_deref(),
            Some("/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin")
        );
    }

    #[test]
    fn a_terminal_launch_never_probes_or_changes_anything() {
        let fixed = repaired(Some("/opt/homebrew/bin:/usr/bin"), || {
            panic!("a shell-built PATH must not be probed")
        });
        assert!(fixed.is_none());
    }

    #[test]
    fn a_failed_probe_keeps_the_inherited_path() {
        assert!(repaired(Some("/usr/bin:/bin"), || None).is_none());
    }

    #[test]
    fn a_probe_that_adds_nothing_changes_nothing() {
        assert!(
            repaired(Some("/usr/bin:/bin:/usr/sbin:/sbin"), || Some(
                String::from("/usr/bin:/bin:/usr/sbin:/sbin")
            ))
            .is_none()
        );
    }

    /// End to end against a real shell: the probe must actually come back with
    /// a PATH, otherwise the markers or the flags are wrong.
    #[test]
    fn probing_a_real_shell_returns_a_usable_path() {
        let shell = if std::path::Path::new("/bin/zsh").exists() {
            "/bin/zsh"
        } else {
            "/bin/sh"
        };
        let path = probe(shell).expect("the login shell must report a PATH");
        assert!(path.contains("/bin"), "got {path:?}");
    }

    /// The whole chain, end to end through a real process spawn: the PATH a
    /// Dock click produces must come back with the login shell's own
    /// directories merged in front, or croft can see nothing the user
    /// installed (`pdftoppm`, `git`, `rg`, the language servers) when
    /// started from Croft.app. A stub login shell stands in for the user's
    /// (the suite must not depend on, or execute, a developer's rc files),
    /// but a strict one: it refuses any argv other than the probe's real
    /// `-l -i -c <script>` shape, contributes a directory the way a login
    /// shell does (into the exported environment), and runs the probe
    /// script through a real `/bin/sh` so the nested quoting is exercised.
    #[test]
    fn a_dock_launch_ends_up_seeing_the_users_own_directories() {
        let dir = tempfile::tempdir().unwrap();
        let stub = dir.path().join("stub-shell");
        std::fs::write(
            &stub,
            "#!/bin/sh\n\
             [ \"$1\" = -l ] || exit 64\n\
             [ \"$2\" = -i ] || exit 64\n\
             [ \"$3\" = -c ] || exit 64\n\
             [ $# -eq 4 ] || exit 64\n\
             PATH=\"/stub/tools/bin:$PATH\"; export PATH\n\
             exec /bin/sh -c \"$4\"\n",
        )
        .unwrap();
        let mut perm = std::fs::metadata(&stub).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
        std::fs::set_permissions(&stub, perm).unwrap();
        let dock = "/usr/bin:/bin:/usr/sbin:/sbin:/Applications/Ghostty.app/Contents/MacOS";
        let fixed = repaired(Some(dock), || probe(&stub.display().to_string()))
            .expect("a Dock launch must have its PATH repaired");
        assert!(
            fixed.starts_with("/stub/tools/bin:"),
            "the shell's directories must lead, got {fixed:?}"
        );
        // And nothing we started with is lost on the way.
        for entry in dock.split(':') {
            assert!(fixed.split(':').any(|e| e == entry), "{entry} dropped");
        }
    }

    /// An rc file that backgrounds a helper (`(brew update &)`, an agent
    /// start) leaves the pipe's write end open in the grandchild after the
    /// shell itself exits. The probe must return the moment the END marker
    /// arrives instead of waiting for an EOF that only comes when the
    /// grandchild eventually dies.
    #[test]
    fn a_backgrounded_grandchild_does_not_stall_the_probe() {
        let mut cmd = Command::new("/bin/sh");
        cmd.args([
            "-c",
            &format!("printf '{BEGIN}/opt/x:/usr/bin{END}'; sleep 5 &"),
        ]);
        let start = std::time::Instant::now();
        let out = run_probe(cmd, Duration::from_millis(1500));
        assert_eq!(out.as_deref(), Some("/opt/x:/usr/bin"));
        assert!(
            start.elapsed() < Duration::from_millis(1200),
            "the probe waited {:?} on a grandchild's pipe",
            start.elapsed()
        );
    }

    /// tcsh rejects `-l` combined with any other flag (`-l` must be the sole
    /// argument), and fish expands a quoted `$PATH` space-joined. The probe
    /// must still get a colon-joined PATH out of both families.
    #[test]
    fn a_csh_family_login_shell_still_answers() {
        if !std::path::Path::new("/bin/tcsh").exists() {
            eprintln!("skipping: tcsh not installed");
            return;
        }
        let path = probe("/bin/tcsh").expect("tcsh must report a PATH");
        assert!(path.contains("/bin"), "got {path:?}");
    }

    /// `~/.login` is the conventional place a csh user builds `path` (it is
    /// the login-only file), and tcsh refuses `-l` next to any other flag -
    /// so the probe must mark itself a login shell the way login(1) does,
    /// with a leading dash in argv[0]. Runs the real tcsh against a stub
    /// HOME: if `.login` is never sourced, the probe recovered nothing and
    /// the whole module is dead for the csh family.
    #[test]
    fn a_csh_family_probe_reads_the_login_file() {
        if !std::path::Path::new("/bin/tcsh").exists() {
            eprintln!("skipping: tcsh not installed");
            return;
        }
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join(".login"),
            "setenv PATH /from/login:$PATH\n",
        )
        .unwrap();
        let mut cmd = probe_cmd("/bin/tcsh");
        cmd.env("HOME", home.path());
        let path = run_probe(cmd, PROBE_TIMEOUT).expect("tcsh must answer");
        assert!(
            path.split(':').any(|e| e == "/from/login"),
            "~/.login was not sourced; the csh user's PATH is lost: {path:?}"
        );
    }

    /// A shell wedged on its own rc file must not wedge croft.
    #[test]
    fn a_hanging_shell_gives_up_instead_of_blocking_startup() {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "sleep 600"]);
        let start = std::time::Instant::now();
        let out = run_probe(cmd, Duration::from_millis(300));
        assert!(out.is_none(), "a wedged shell must not report a PATH");
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "startup waited {:?} on a hung shell",
            start.elapsed()
        );
    }

    /// And the timeout must not be so eager that a slow rc file loses.
    #[test]
    fn a_shell_that_answers_within_the_timeout_is_read() {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", &format!("printf '{BEGIN}/opt/x:/usr/bin{END}'")]);
        assert_eq!(
            run_probe(cmd, PROBE_TIMEOUT).as_deref(),
            Some("/opt/x:/usr/bin")
        );
    }
}
