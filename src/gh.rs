//! What croft says when the GitHub CLI cannot do the job (#917): one
//! message for a `gh` that is not installed and one for a `gh` with no
//! account signed in, shared by everything that shells out to it (`croft
//! pr`, the PR review tab, review comments, code scanning, and CodeQL's
//! GitHub databases and variant analysis), so each names the fix instead
//! of "No such file or directory (os error 2)".

use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};

/// Why `gh` could not run when it is not installed.
pub const MISSING: &str = "the GitHub CLI (gh) is not installed; install it from https://cli.github.com, then run gh auth login";

/// Why `gh` refused when it has no account to use.
pub const SIGNED_OUT: &str = "the GitHub CLI (gh) is not signed in; run gh auth login";

/// gh's exit code for "authentication required" (`gh help exit-codes`).
const AUTH_REQUIRED: i32 = 4;

/// What to say when `gh` could not be started in `cwd` (`None` when the
/// call sets none) because of `e`. "Not found" also means a `cwd` that no
/// longer exists, which is no reason to send anyone to install gh.
pub fn spawn_error(e: &std::io::Error, cwd: Option<&Path>) -> String {
    if e.kind() == std::io::ErrorKind::NotFound && cwd.is_none_or(Path::is_dir) {
        String::from(MISSING)
    } else {
        format!("could not run gh: {e}")
    }
}

/// Whether gh exited with `status` because no account is signed in.
pub fn signed_out(status: ExitStatus) -> bool {
    status.code() == Some(AUTH_REQUIRED)
}

/// Whether `why`, a gh failure as [`spawn_error`] or [`SIGNED_OUT`] put
/// it, is about gh itself rather than the request.
pub fn unusable(why: &str) -> bool {
    why == MISSING || why == SIGNED_OUT
}

/// Tokens gh reads from the environment ahead of its own config. Any of
/// them may be the one for the PR's host, which [`check_ready`]'s
/// `gh auth token`, asked about gh's default host only, cannot see.
const TOKEN_VARS: [&str; 4] = [
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
];

/// Whether `program`, run with `args` and its output thrown away, exits 0.
fn succeeds(program: &str, args: &[&str]) -> Result<bool, String> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    crate::review_ops::spawn_retrying_busy(&mut cmd)
        .and_then(|mut child| child.wait())
        .map(|status| status.success())
        .map_err(|e| spawn_error(&e, None))
}

/// Whether `program` runs and has an account, before `croft pr` hands the
/// terminal to the editor. `gh auth token` answers from gh's own config
/// without the network, and its output, the token, goes nowhere. Only
/// when it finds none for gh's default host, and no token is in the
/// environment, does `gh auth status` check every host gh knows, over the
/// network.
pub fn check_ready(program: &str) -> Result<(), String> {
    let env_token = || {
        TOKEN_VARS
            .iter()
            .any(|v| std::env::var_os(v).is_some_and(|t| !t.is_empty()))
    };
    if succeeds(program, &["auth", "token"])? || env_token() {
        return Ok(());
    }
    if any_host_signed_in(program)? {
        Ok(())
    } else {
        Err(String::from(SIGNED_OUT))
    }
}

/// Whether `gh auth status` finds a working account on any host. It exits 1
/// when an account on any host has a problem (`gh auth status --help`), so a
/// stale token for one host failed it with another signed in; a host it
/// says is logged in is enough here, and a PR on one that is not fails
/// later with gh's own message.
fn any_host_signed_in(program: &str) -> Result<bool, String> {
    let mut cmd = Command::new(program);
    cmd.args(["auth", "status"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = crate::review_ops::spawn_retrying_busy(&mut cmd)
        .and_then(|child| child.wait_with_output())
        .map_err(|e| spawn_error(&e, None))?;
    let says = |b: &[u8]| String::from_utf8_lossy(b).contains("Logged in to");
    Ok(out.status.success() || says(&out.stdout) || says(&out.stderr))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_gh_says_how_to_get_it() {
        let dir = tempfile::tempdir().unwrap();
        let gh = dir.path().join("gh");
        assert_eq!(check_ready(&gh.display().to_string()), Err(MISSING.into()));
        let e = std::io::Error::from(std::io::ErrorKind::NotFound);
        assert_eq!(spawn_error(&e, Some(dir.path())), MISSING);
        assert!(unusable(MISSING) && unusable(SIGNED_OUT));
    }

    #[test]
    fn a_vanished_folder_is_not_a_missing_gh() {
        let dir = tempfile::tempdir().unwrap();
        let e = std::io::Error::from(std::io::ErrorKind::NotFound);
        let why = spawn_error(&e, Some(&dir.path().join("gone")));
        assert!(why.starts_with("could not run gh:"), "{why}");
        assert!(!unusable(&why));
    }

    #[cfg(unix)]
    #[test]
    fn a_gh_signed_in_elsewhere_than_its_default_host_is_ready() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let gh = dir.path().join("gh");
        let log = dir.path().join("log");
        let script = format!(
            "#!/bin/sh\necho \"$*\" >> '{}'\n[ \"$2\" = token ] && exit 1\nexit 0\n",
            log.display()
        );
        std::fs::write(&gh, script).unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(check_ready(&gh.display().to_string()), Ok(()));
        let asked = std::fs::read_to_string(&log).unwrap();
        assert!(asked.starts_with("auth token\n"), "offline first: {asked}");
    }

    /// #917: `gh auth status` fails when any host's account has a problem,
    /// so one stale host must not read as signed out while another host is
    /// logged in; with no host logged in it is signed out. Asked directly:
    /// [`check_ready`] would take a token in the environment first.
    #[cfg(unix)]
    #[test]
    fn a_stale_host_beside_a_signed_in_one_is_ready() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let fake = |name: &str, status: &str| {
            let gh = dir.path().join(name);
            let script = format!(
                "#!/bin/sh\n[ \"$2\" = token ] && exit 1\nprintf '%s\\n' {status}\nexit 1\n"
            );
            std::fs::write(&gh, script).unwrap();
            std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
            gh.display().to_string()
        };
        let mixed = fake(
            "mixed",
            "'X Failed to log in to github.com account old' 'Logged in to ghe.example.com account me'",
        );
        assert_eq!(any_host_signed_in(&mixed), Ok(true));
        let stale = fake("stale", "'X Failed to log in to github.com account old'");
        assert_eq!(any_host_signed_in(&stale), Ok(false));
    }

    #[test]
    fn gh_exit_code_4_means_signed_out() {
        let status = |code: &str| {
            Command::new("/bin/sh")
                .args(["-c", &format!("exit {code}")])
                .status()
                .unwrap()
        };
        assert!(signed_out(status("4")));
        assert!(!signed_out(status("1")));
    }
}
