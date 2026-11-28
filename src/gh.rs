//! The GitHub CLI: [`Client`], how the review tab and code scanning start `gh` (#2062),
//! and what croft says when it cannot do the job (#917): one
//! message for a `gh` that is not installed and one for a `gh` with no
//! account signed in, shared by everything that shells out to it (`croft
//! pr`, the PR review tab, review comments, code scanning, and CodeQL's
//! GitHub databases and variant analysis), so each names the fix instead
//! of "No such file or directory (os error 2)".

use std::path::Path;
use std::process::{ExitStatus, Stdio};

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
    let mut cmd = crate::proc::command(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    crate::proc::spawn_retrying_busy(&mut cmd)
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
    let mut cmd = crate::proc::command(program);
    cmd.args(["auth", "status"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = crate::proc::spawn_retrying_busy(&mut cmd)
        .and_then(|child| child.wait_with_output())
        .map_err(|e| spawn_error(&e, None))?;
    let says = |b: &[u8]| String::from_utf8_lossy(b).contains("Logged in to");
    Ok(out.status.success() || says(&out.stdout) || says(&out.stderr))
}

/// One way to run `gh` from the editor (#2062, card 12 step 7): which
/// program, in which folder, and for how long at most. The review tab, its
/// write side and code scanning all start gh through it, named through
/// [`crate::proc::command`] so a test can put a stand-in in its place.
///
/// Both outputs are read while gh runs, so a log larger than a pipe cannot
/// block it on its write until the limit, and a helper gh leaves holding
/// them cannot keep the call waiting past gh's own exit.
#[derive(Debug, Clone)]
pub struct Client {
    program: std::path::PathBuf,
    dir: std::path::PathBuf,
    limit: std::time::Duration,
}

/// Why a `gh` call did not answer as asked.
#[derive(Debug)]
pub enum Error {
    /// It could not be started, in [`spawn_error`]'s words: not installed,
    /// or its folder is gone.
    Start(String),
    /// It has no account signed in.
    SignedOut,
    /// It ran, but gave no answer: past its time limit, more output than
    /// croft keeps, or it could not be waited on. The words say which.
    NoAnswer(String),
    /// It answered with a failing exit; what it printed.
    Refused { stdout: Vec<u8>, stderr: String },
}

/// The most of either output a `gh` call keeps: far past any log, list or
/// SARIF file gh returns, so a longer one is refused, never cut short.
const OUTPUT_CAP: usize = 1 << 30;

/// How much of a refusal's stderr [`Error::tail`] shows: the end, where gh
/// says why.
const ERROR_TAIL: usize = 16 * 1024;

impl Client {
    /// `program` run in `dir`, killed after `limit`.
    pub fn new(
        program: impl Into<std::path::PathBuf>,
        dir: impl Into<std::path::PathBuf>,
        limit: std::time::Duration,
    ) -> Self {
        Self {
            program: program.into(),
            dir: dir.into(),
            limit,
        }
    }

    /// Run gh with `args` and an empty stdin; its stdout when it succeeds.
    pub fn run<S: AsRef<std::ffi::OsStr>>(&self, args: &[S]) -> Result<Vec<u8>, Error> {
        self.call(args, None)
    }

    /// [`Self::run`], handing gh `input` on its stdin.
    pub fn run_fed<S: AsRef<std::ffi::OsStr>>(
        &self,
        args: &[S],
        input: &[u8],
    ) -> Result<Vec<u8>, Error> {
        self.call(args, Some(input))
    }

    fn call<S: AsRef<std::ffi::OsStr>>(
        &self,
        args: &[S],
        input: Option<&[u8]>,
    ) -> Result<Vec<u8>, Error> {
        use crate::proc::{Apart, Bounded, Keep, RunError};
        let bounded = Bounded {
            stdout: Keep::All { cap: OUTPUT_CAP },
            stderr: Keep::All { cap: OUTPUT_CAP },
            apart: Apart::No,
            limit: self.limit,
            retry_busy: true,
        };
        let mut cmd = match self.program.to_str() {
            Some(name) => crate::proc::command(name),
            None => std::process::Command::new(&self.program),
        };
        cmd.args(args).current_dir(&self.dir);
        let ran = match input {
            Some(input) => bounded.run_fed(&mut cmd, input),
            None => bounded.run(&mut cmd),
        };
        let program = self.program.display();
        match ran {
            Ok(ran) if ran.status.success() => Ok(ran.stdout.into_bytes()),
            Ok(ran) if signed_out(ran.status) => Err(Error::SignedOut),
            Ok(ran) => Err(Error::Refused {
                stderr: ran.stderr.text(),
                stdout: ran.stdout.into_bytes(),
            }),
            Err(RunError::Spawn(e)) => Err(Error::Start(spawn_error(&e, Some(&self.dir)))),
            Err(RunError::TimedOut) => Err(Error::NoAnswer(format!(
                "{program} took longer than {}s",
                self.limit.as_secs()
            ))),
            Err(RunError::TooLong { .. }) => Err(Error::NoAnswer(format!(
                "{program} answered more than {} MiB",
                OUTPUT_CAP >> 20
            ))),
            Err(RunError::Wait(e) | RunError::Pipe(e)) => {
                Err(Error::NoAnswer(format!("could not wait on {program}: {e}")))
            }
        }
    }
}

impl Error {
    /// Why, for a call that did not get as far as gh's own words.
    fn unanswered(&self) -> Option<String> {
        match self {
            Self::Start(why) | Self::NoAnswer(why) => Some(why.clone()),
            Self::SignedOut => Some(String::from(SIGNED_OUT)),
            Self::Refused { .. } => None,
        }
    }

    /// Why, with a refusal read as the first line of gh's stderr: gh names
    /// the problem there (a scope, a rate limit, a line outside the diff).
    pub fn first_line(&self) -> String {
        self.unanswered().unwrap_or_else(|| match self {
            Self::Refused { stderr, .. } => stderr
                .trim()
                .lines()
                .next()
                .unwrap_or("gh failed")
                .to_string(),
            _ => unreachable!(),
        })
    }

    /// Why, with a refusal read as the end of gh's stderr, marked "…" when
    /// its front was left out: a failed log or check says why at the end.
    pub fn tail(&self) -> String {
        self.unanswered().unwrap_or_else(|| match self {
            Self::Refused { stderr, .. } => match stderr.len().checked_sub(ERROR_TAIL) {
                Some(drop) if drop > 0 => {
                    let start = (drop..stderr.len())
                        .find(|&i| stderr.is_char_boundary(i))
                        .unwrap_or(stderr.len());
                    format!("…{}", stderr[start..].trim())
                }
                _ => stderr.trim().to_string(),
            },
            _ => unreachable!(),
        })
    }

    /// Why, with a refusal read as GitHub's own message when the API sent
    /// one (on stdout, else on stderr), else gh's first stderr line.
    pub fn api_message(&self) -> String {
        self.unanswered().unwrap_or_else(|| match self {
            Self::Refused { stdout, stderr } => {
                let message = |text: &str| {
                    serde_json::from_str::<serde_json::Value>(text)
                        .ok()
                        .and_then(|v| v.get("message")?.as_str().map(str::to_string))
                };
                message(&String::from_utf8_lossy(stdout))
                    .or_else(|| {
                        let s = stderr.trim();
                        message(s).or_else(|| s.lines().next().map(str::to_string))
                    })
                    .unwrap_or_else(|| String::from("gh failed"))
            }
            _ => unreachable!(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

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

    /// Each caller's wording of a refusal, kept as it was before the client
    /// (#2062, card 12 step 7).
    #[test]
    fn a_refusal_reads_as_each_caller_put_it() {
        let refused = |stdout: &str, stderr: &str| Error::Refused {
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.to_string(),
        };
        let e = refused("", "HTTP 422: Unprocessable\nsee the docs\n");
        assert_eq!(e.first_line(), "HTTP 422: Unprocessable");
        assert_eq!(e.tail(), "HTTP 422: Unprocessable\nsee the docs");
        assert_eq!(e.api_message(), "HTTP 422: Unprocessable");
        let api = refused(
            r#"{"message":"Alert is already dismissed"}"#,
            "gh: exit 1\n",
        );
        assert_eq!(api.api_message(), "Alert is already dismissed");
        assert_eq!(api.first_line(), "gh: exit 1");
        assert_eq!(refused("", "").first_line(), "gh failed");
        assert_eq!(refused("", "").api_message(), "gh failed");
        let long = format!("{}\nthe end", "x".repeat(ERROR_TAIL * 2));
        let tail = refused("", &long).tail();
        assert!(tail.starts_with('…') && tail.ends_with("the end"), "{tail}");
        assert!(tail.len() <= ERROR_TAIL + '…'.len_utf8());
        for e in [Error::SignedOut, Error::Start(String::from(MISSING))] {
            assert!(unusable(&e.first_line()) && unusable(&e.tail()) && unusable(&e.api_message()));
        }
    }
}
