//! zoxide integration for the Explorer "jump" popup (Cmd+Z).
//!
//! croft delegates all directory ranking to the `zoxide` binary rather
//! than parsing its database: `zoxide query -l <tokens>` returns the
//! frecency-ranked matches, exactly the algorithm the user already gets
//! from `j <tokens>` in the shell (their init is `zoxide init zsh --cmd j`).
//!
//! GOLDEN RULE — identical local/remote: croft runs *on* the host (the
//! user's Mac locally, a Linux box under the remote-launch flow), so we
//! invoke whichever `zoxide` lives on that host and read that host's own
//! frecency database. The same code path serves both platforms, and the
//! database naturally stays consistent with the terminal pane's own
//! zoxide `chpwd` hook (the `cd` croft writes to the PTY bumps the score).
//!
//! zoxide is a hard dependency: `ensure_installed_in_background` probes
//! for the binary at startup and, if it is missing, runs the official
//! cross-platform installer (no root, installs to `~/.local/bin`). When
//! the binary still cannot be found the popup degrades to an "unavailable"
//! message instead of failing.

use std::path::PathBuf;
use std::process::Command;

/// Official zoxide installer. Cross-platform (macOS + Linux), needs only
/// `curl` + `sh`, and installs a static binary to `~/.local/bin` without
/// root — the same destination `binary()` probes below.
const INSTALL_SCRIPT: &str =
    "curl -sSfL https://raw.githubusercontent.com/ajeetdsouza/zoxide/main/install.sh | sh";

/// Absolute fallbacks probed after `PATH`, covering the default install
/// dir used by the official script (`~/.local/bin`) plus the common
/// Homebrew / system locations. Resolving an absolute path matters
/// because a binary the background installer drops into `~/.local/bin`
/// is not necessarily on croft's inherited `PATH`.
fn fallback_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        paths.push(
            PathBuf::from(&home)
                .join(".local")
                .join("bin")
                .join("zoxide"),
        );
    }
    paths.push(PathBuf::from("/opt/homebrew/bin/zoxide"));
    paths.push(PathBuf::from("/usr/local/bin/zoxide"));
    paths.push(PathBuf::from("/usr/bin/zoxide"));
    paths
}

/// True when `candidate --version` runs and exits zero.
fn probe(candidate: &PathBuf) -> bool {
    Command::new(candidate)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Resolve an invocable `zoxide`: first the bare name (so a `PATH` install
/// wins), then the absolute fallbacks. `None` means zoxide is not
/// installed on this host.
pub fn binary() -> Option<PathBuf> {
    let on_path = PathBuf::from("zoxide");
    if probe(&on_path) {
        return Some(on_path);
    }
    fallback_paths()
        .into_iter()
        .find(|p| p.exists() && probe(p))
}

/// Build the `zoxide query` argument vector for `needle`. Whitespace
/// splits the needle into independent keyword tokens, mirroring how the
/// shell `j a b` passes multiple keywords that zoxide subsequence-matches
/// against the path. An empty / whitespace needle yields `query -l`,
/// which lists every entry by score.
fn query_args(needle: &str) -> Vec<String> {
    let mut args = vec![String::from("query"), String::from("-l")];
    args.extend(needle.split_whitespace().map(String::from));
    args
}

/// Parse `zoxide query -l` stdout into directory paths, dropping blank
/// lines. zoxide prints one absolute path per line, best-ranked first.
pub fn parse_query_output(stdout: &str) -> Vec<PathBuf> {
    stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(PathBuf::from)
        .collect()
}

/// Run `zoxide query -l <needle>` and return the ranked matches.
///
/// `None` => the binary could not be found or spawned (unavailable on
/// this host). `Some(vec)` => zoxide ran; the vec is its ranked output,
/// which is legitimately empty when nothing matches (zoxide exits
/// non-zero on no-match, so a non-zero status is NOT treated as failure).
pub fn query(needle: &str) -> Option<Vec<PathBuf>> {
    let bin = binary()?;
    let output = Command::new(bin).args(query_args(needle)).output().ok()?;
    Some(parse_query_output(&String::from_utf8_lossy(&output.stdout)))
}

/// Probe for `zoxide` and, if it is missing, install it via the official
/// script on a detached thread so launch is never blocked. Best-effort:
/// any failure leaves the popup in its "unavailable" state. Invoked once
/// from `app::run`, so it covers both the local Mac and the remote Linux
/// box (both reach `run` through the normal launch path).
pub fn ensure_installed_in_background() {
    std::thread::spawn(|| {
        if binary().is_some() {
            return;
        }
        let _ = Command::new("sh").arg("-c").arg(INSTALL_SCRIPT).output();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_args_splits_needle_into_keyword_tokens() {
        assert_eq!(
            query_args("foo bar"),
            vec![
                String::from("query"),
                String::from("-l"),
                String::from("foo"),
                String::from("bar"),
            ],
            "multi-word needles must become separate zoxide keyword args, mirroring `j foo bar`"
        );
    }

    #[test]
    fn query_args_with_empty_needle_lists_everything() {
        assert_eq!(
            query_args("   "),
            vec![String::from("query"), String::from("-l")],
            "a blank needle must yield `query -l` so the popup opens on the full frecency list"
        );
    }

    #[test]
    fn parse_query_output_keeps_one_path_per_nonblank_line_in_order() {
        let out = "/Users/v/Documents/croft\n/Users/v/Documents\n\n/Users/v/.ssh\n";
        assert_eq!(
            parse_query_output(out),
            vec![
                PathBuf::from("/Users/v/Documents/croft"),
                PathBuf::from("/Users/v/Documents"),
                PathBuf::from("/Users/v/.ssh"),
            ],
            "blank lines must be dropped and zoxide's best-first order preserved"
        );
    }

    #[test]
    fn parse_query_output_is_empty_for_no_matches() {
        assert!(parse_query_output("").is_empty());
        assert!(parse_query_output("\n  \n").is_empty());
    }
}
