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
//!
//! The same background step also wires zoxide's shell hook into the host's
//! shell rc (`~/.zshrc` / `~/.bashrc`) so the terminal pane's `j` command
//! and the popup share one populated database — without it a fresh remote
//! has the binary but an empty database that no `cd` ever feeds, the exact
//! local/remote divergence the GOLDEN RULE forbids. The write is idempotent:
//! it is skipped whenever the rc already contains a `zoxide init` line (the
//! user's own, or one croft wrote on a previous launch).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::lsp::log_file;

/// Official zoxide installer. Cross-platform (macOS + Linux), needs only
/// `curl` + `sh`, and installs a static binary to `~/.local/bin` without
/// root — the same destination `binary()` probes below.
const INSTALL_SCRIPT: &str =
    "curl -sSfL https://raw.githubusercontent.com/ajeetdsouza/zoxide/main/install.sh | sh";

/// The `--cmd` name croft wires into the shell, matching the Cmd+Z popup it
/// mirrors: the jump command is `j` (and the interactive `ji`), not the
/// default `z`/`zi`. Kept in sync with the popup's intent in this module.
const ZOXIDE_CMD: &str = "j";

/// Sentinels bracketing the block croft appends to the shell rc, so a prior
/// launch's block is recognisable and never duplicated.
const INIT_MARKER_START: &str = "# >>> croft zoxide (managed) >>>";
const INIT_MARKER_END: &str = "# <<< croft zoxide (managed) <<<";

/// Substring whose presence in an rc file means zoxide is already wired —
/// by the user themselves or by a previous croft run — so croft must not
/// append again. Matches every `zoxide init <shell> …` form.
const INIT_SENTINEL: &str = "zoxide init";

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

/// Optimal-string-alignment (restricted Damerau-Levenshtein) distance of
/// `pattern` against `text`, but with a *free* text prefix and suffix so
/// `pattern` is matched as an approximate *substring* of `text`. A single
/// adjacent transposition costs 1 edit, which is the whole point: it is
/// what recovers a typo like `spilt` against the `split` inside
/// `pr-split`, something plain subsequence matching (zoxide's own
/// algorithm, fzf, croft's Cmd+P matcher) can never do because a
/// transposition reorders characters.
///
/// `pattern`/`text` are expected pre-lowercased by the caller.
fn osa_substring_distance(pattern: &[char], text: &[char]) -> usize {
    let n = pattern.len();
    let m = text.len();
    if n == 0 {
        return 0;
    }
    // d[i][j] = best edits to match pattern[..i] ending anywhere within
    // text[..j]. Row 0 is all-zero (skipping any text prefix is free);
    // column 0 is i (the pattern char has no text to match against).
    let mut d = vec![vec![0usize; m + 1]; n + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for i in 1..=n {
        for j in 1..=m {
            let cost = usize::from(pattern[i - 1] != text[j - 1]);
            let mut best = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
            if i >= 2 && j >= 2 && pattern[i - 1] == text[j - 2] && pattern[i - 2] == text[j - 1] {
                best = best.min(d[i - 2][j - 2] + 1);
            }
            d[i][j] = best;
        }
    }
    // Free text suffix: the match may end at any column of the last row.
    d[n].iter().copied().min().unwrap_or(n)
}

/// Maximum edit distance tolerated for a needle of `len` characters. Kept
/// deliberately tight (≈ one edit per three chars, never more than three)
/// so this *fallback* surfaces genuine typos without flooding the popup
/// with loosely-related directories.
fn fuzzy_threshold(len: usize) -> usize {
    (len / 3).clamp(1, 3)
}

/// The basename used for fuzzy matching: the final path component,
/// lowercased into a `char` vector. zoxide matches its last keyword
/// against the final component, so the fallback mirrors that — fuzzing the
/// whole absolute path would let the long shared prefix dominate the score.
fn basename_chars(path: &std::path::Path) -> Vec<char> {
    path.file_name()
        .map(|n| n.to_string_lossy().to_lowercase().chars().collect())
        .unwrap_or_default()
}

/// Typo-tolerant fallback ranking over the cached zoxide database `dirs`
/// (already in frecency order). Used only when the strict `query` returns
/// no matches: the last whitespace token of `needle` is matched against
/// each directory's basename with `osa_substring_distance`, entries within
/// `fuzzy_threshold` are kept, and the survivors are sorted by edit
/// distance ascending, breaking ties by the original frecency order so the
/// most-used directory wins an equal-distance tie.
pub fn fuzzy_rank(needle: &str, dirs: &[PathBuf]) -> Vec<PathBuf> {
    let Some(token) = needle.split_whitespace().last() else {
        return Vec::new();
    };
    let pattern: Vec<char> = token.to_lowercase().chars().collect();
    let threshold = fuzzy_threshold(pattern.len());
    let mut scored: Vec<(usize, usize, PathBuf)> = dirs
        .iter()
        .enumerate()
        .filter_map(|(rank, path)| {
            let dist = osa_substring_distance(&pattern, &basename_chars(path));
            (dist <= threshold).then(|| (dist, rank, path.clone()))
        })
        .collect();
    scored.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, _, path)| path).collect()
}

/// Normalise a `$SHELL` path to the shell name croft knows how to wire,
/// or `None` for anything else (we never guess an rc layout for an
/// unfamiliar shell). Only `zsh` and `bash` are supported — the two shells
/// croft actually launches (zsh on the Mac, bash on the Linux remote).
fn supported_shell(shell_path: &str) -> Option<&'static str> {
    let base = Path::new(shell_path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(shell_path);
    match base {
        "zsh" => Some("zsh"),
        "bash" => Some("bash"),
        _ => None,
    }
}

/// The rc file croft appends the init block to for a (already validated)
/// shell. bash targets `~/.bashrc` (zoxide's documented target; the login
/// shell reaches it via `~/.profile` sourcing `~/.bashrc`), zsh targets
/// `~/.zshrc` (sourced for every interactive zsh).
fn rc_file_name(shell: &str) -> &'static str {
    match shell {
        "zsh" => ".zshrc",
        _ => ".bashrc",
    }
}

/// The managed block croft appends for `shell`: prepend `~/.local/bin` to
/// `PATH` (so the binary croft drops there is reachable before the eval),
/// then install zoxide's `cd` hook and the `j` command. Wrapped in marker
/// comments and led by a blank line so it never fuses onto existing content.
fn init_block(shell: &str) -> String {
    format!(
        "\n{INIT_MARKER_START}\nexport PATH=\"$HOME/.local/bin:$PATH\"\neval \"$(zoxide init {shell} --cmd {ZOXIDE_CMD})\"\n{INIT_MARKER_END}\n"
    )
}

/// Decide what (if anything) to append to an rc file whose current contents
/// are `existing`. `None` means already wired — the rc contains a
/// `zoxide init` line, whether the user's own or a prior croft block — so
/// croft leaves it untouched. This is the idempotency guard that keeps
/// repeated launches from duplicating the block.
fn pending_init_block(shell: &str, existing: &str) -> Option<String> {
    if existing.contains(INIT_SENTINEL) {
        return None;
    }
    Some(init_block(shell))
}

/// Wire zoxide's shell hook into the host's shell rc so the terminal `j`
/// command and the Cmd+Z popup share one database. Best-effort and
/// idempotent: resolves `$SHELL`/`$HOME`, skips unsupported shells, and
/// appends only when no `zoxide init` line is already present.
fn ensure_shell_init() {
    let Ok(shell_path) = std::env::var("SHELL") else {
        log_file::log("zoxide: $SHELL unset, skipping shell-init wiring");
        return;
    };
    let Some(shell) = supported_shell(&shell_path) else {
        log_file::log(&format!(
            "zoxide: unsupported shell {shell_path:?}, skipping shell-init wiring"
        ));
        return;
    };
    let Some(home) = std::env::var_os("HOME") else {
        log_file::log("zoxide: $HOME unset, skipping shell-init wiring");
        return;
    };
    let rc = PathBuf::from(home).join(rc_file_name(shell));
    let existing = std::fs::read_to_string(&rc).unwrap_or_default();
    let Some(block) = pending_init_block(shell, &existing) else {
        return;
    };
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&rc)
    {
        Ok(mut f) => match f.write_all(block.as_bytes()) {
            Ok(()) => log_file::log(&format!(
                "zoxide: wired `{ZOXIDE_CMD}` shell init into {rc:?}"
            )),
            Err(e) => log_file::log(&format!("zoxide: failed to append init to {rc:?}: {e}")),
        },
        Err(e) => log_file::log(&format!(
            "zoxide: could not open {rc:?} for init wiring: {e}"
        )),
    }
}

/// Probe for `zoxide` and, if it is missing, install it via the official
/// script on a detached thread so launch is never blocked, then wire the
/// shell hook. Best-effort: any failure leaves the popup in its
/// "unavailable" state. Invoked once from `app::run`, so it covers both the
/// local Mac and the remote Linux box (both reach `run` through the normal
/// launch path).
pub fn ensure_installed_in_background() {
    std::thread::spawn(|| {
        if binary().is_none() {
            let _ = Command::new("sh").arg("-c").arg(INSTALL_SCRIPT).output();
        }
        // Wire the shell hook only once the binary is actually resolvable,
        // otherwise the `eval "$(zoxide init …)"` line errors on every shell
        // start. `binary()` finds the freshly-installed `~/.local/bin` copy
        // even though it is not yet on the login shell's PATH — which is why
        // the appended block prepends that dir before the eval.
        if binary().is_some() {
            ensure_shell_init();
        }
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

    #[test]
    fn osa_substring_distance_treats_adjacent_transposition_as_one_edit() {
        let p: Vec<char> = "spilt".chars().collect();
        let t: Vec<char> = "split".chars().collect();
        assert_eq!(
            osa_substring_distance(&p, &t),
            1,
            "a single adjacent swap (i<->l) must cost exactly one edit"
        );
    }

    #[test]
    fn osa_substring_distance_matches_token_inside_longer_basename() {
        // The free prefix/suffix means `spilt` is found against the
        // `split` buried in `pr-split` at the cost of the one transposition.
        let p: Vec<char> = "spilt".chars().collect();
        let t: Vec<char> = "pr-split".chars().collect();
        assert_eq!(osa_substring_distance(&p, &t), 1);
        // Exact substring is free.
        let exact: Vec<char> = "split".chars().collect();
        assert_eq!(osa_substring_distance(&exact, &t), 0);
    }

    #[test]
    fn fuzzy_rank_recovers_transposed_typo_and_drops_unrelated_dirs() {
        let dirs = vec![
            PathBuf::from("/home/u/documents"),
            PathBuf::from("/home/u/pr-split"),
            PathBuf::from("/home/u/node_modules"),
        ];
        let ranked = fuzzy_rank("spilt", &dirs);
        assert_eq!(
            ranked,
            vec![PathBuf::from("/home/u/pr-split")],
            "the transposed needle must surface pr-split and only pr-split"
        );
    }

    #[test]
    fn fuzzy_rank_orders_by_distance_then_frecency() {
        // `docs` is an exact substring (distance 0) of the first two; the
        // earlier (more frecent) one must win the tie. `dosc` is a
        // transposition (distance 1) so the exact hits rank ahead of it.
        let dirs = vec![
            PathBuf::from("/a/docs"),       // rank 0, dist 0
            PathBuf::from("/b/my-docs"),    // rank 1, dist 0
            PathBuf::from("/c/dosc-stuff"), // rank 2, dist 1
            PathBuf::from("/d/unrelated"),  // dropped
        ];
        let ranked = fuzzy_rank("docs", &dirs);
        assert_eq!(
            ranked,
            vec![
                PathBuf::from("/a/docs"),
                PathBuf::from("/b/my-docs"),
                PathBuf::from("/c/dosc-stuff"),
            ],
            "distance ascending, frecency order breaking equal-distance ties"
        );
    }

    #[test]
    fn fuzzy_rank_uses_only_the_last_whitespace_token() {
        // Mirrors zoxide's "last keyword matches the final component":
        // the leading `pr` token is ignored, `spilt` drives the match.
        let dirs = vec![PathBuf::from("/home/u/pr-split")];
        assert_eq!(fuzzy_rank("pr spilt", &dirs), dirs);
    }

    #[test]
    fn fuzzy_threshold_is_tight_and_capped() {
        assert_eq!(fuzzy_threshold(0), 1, "never below one edit");
        assert_eq!(fuzzy_threshold(5), 1);
        assert_eq!(fuzzy_threshold(9), 3);
        assert_eq!(fuzzy_threshold(30), 3, "capped at three edits");
    }

    #[test]
    fn supported_shell_maps_only_bash_and_zsh_by_basename() {
        assert_eq!(supported_shell("/bin/zsh"), Some("zsh"));
        assert_eq!(supported_shell("/usr/bin/bash"), Some("bash"));
        assert_eq!(supported_shell("/opt/homebrew/bin/zsh"), Some("zsh"));
        assert_eq!(
            supported_shell("/usr/bin/fish"),
            None,
            "an unfamiliar shell must be skipped, not guessed"
        );
        assert_eq!(supported_shell("/bin/sh"), None);
    }

    #[test]
    fn rc_file_name_targets_the_conventional_rc_per_shell() {
        assert_eq!(rc_file_name("zsh"), ".zshrc");
        assert_eq!(rc_file_name("bash"), ".bashrc");
    }

    #[test]
    fn init_block_installs_the_hook_and_j_command_for_the_shell() {
        let bash = init_block("bash");
        assert!(
            bash.contains("zoxide init bash --cmd j"),
            "must install the cd-hook and rename the command to `j`"
        );
        assert!(
            bash.contains("export PATH=\"$HOME/.local/bin:$PATH\""),
            "must put the croft-installed binary on PATH before the eval"
        );
        assert!(bash.contains(INIT_MARKER_START) && bash.contains(INIT_MARKER_END));
        assert!(
            bash.starts_with('\n'),
            "a leading blank line keeps the block off the end of existing content"
        );
        assert!(init_block("zsh").contains("zoxide init zsh --cmd j"));
    }

    #[test]
    fn pending_init_block_appends_when_rc_has_no_zoxide() {
        let block = pending_init_block("bash", "# my bashrc\nalias ll='ls -la'\n");
        assert!(block.is_some_and(|b| b.contains("zoxide init bash --cmd j")));
    }

    #[test]
    fn pending_init_block_skips_when_user_already_wired_zoxide() {
        // The Mac case: the user's own `~/.zshrc` line. croft must never
        // append a duplicate on top of it.
        let existing = "eval \"$(zoxide init zsh --cmd j)\"\n";
        assert_eq!(
            pending_init_block("zsh", existing),
            None,
            "an existing user `zoxide init` line must suppress croft's block"
        );
    }

    #[test]
    fn pending_init_block_is_idempotent_across_relaunches() {
        // A second launch sees the block croft wrote on the first and must
        // not append it again (the block itself contains `zoxide init`).
        let once = init_block("bash");
        assert_eq!(
            pending_init_block("bash", &once),
            None,
            "croft's own block must suppress a second append"
        );
    }
}
