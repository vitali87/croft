//! A misspelled subcommand: the word clap took as the `PATH` argument when it is a typo of
//! a subcommand, turned into a usage error that names the subcommand meant, rather than a
//! folder to open.

use super::*;

/// A clap usage error when the word clap bound to `PATH` is a misspelled
/// subcommand rather than a folder: `croft atach`, `croft sync-confg devbox`.
/// clap tries a word as a subcommand only on an exact match, so without this
/// the typo surfaces as a missing workspace, or as an "unrecognized
/// subcommand" aimed at the next word. `base` is the folder a relative
/// `PATH` resolves against.
pub fn misspelled_subcommand(args: &[std::ffi::OsString], base: &Path) -> Option<clap::Error> {
    use clap::CommandFactory;
    let mut cmd = Cli::command();
    let takes_value = |flag: &str| {
        cmd.get_arguments().any(|a| {
            !a.is_positional()
                && a.get_action().takes_values()
                && match flag.strip_prefix("--") {
                    Some(long) => a.get_long() == Some(long),
                    None => flag.chars().nth(1) == a.get_short(),
                }
        })
    };
    let mut rest = args.iter().skip(1);
    let word = loop {
        let arg = rest.next()?.to_str()?;
        // After `--` the word is a path by the user's own say-so.
        if arg == "--" {
            return None;
        }
        if arg.len() > 1 && arg.starts_with('-') {
            if !arg.contains('=') && takes_value(arg) {
                rest.next();
            }
            continue;
        }
        break arg;
    };
    let is_subcommand = |w: &str| {
        cmd.get_subcommands()
            .any(|s| s.get_name() == w || s.get_all_aliases().any(|a| a == w))
    };
    if is_subcommand(word)
        || word.contains('/')
        || word.contains(std::path::MAIN_SEPARATOR)
        || base.join(word).symlink_metadata().is_ok()
    {
        return None;
    }
    let suggestion = cmd
        .get_subcommands()
        .filter(|s| !s.is_hide_set())
        .map(|s| (edit_distance(word, s.get_name()), s.get_name().to_string()))
        // One slip in a short name, two in a longer one: `croft app` is a
        // folder that is not there, not a stab at `pr`.
        .filter(|(d, name)| *d <= if name.chars().count() <= 4 { 1 } else { 2 })
        .min_by_key(|(d, _)| *d)?
        .1;
    Some(cmd.error(
        clap::error::ErrorKind::InvalidSubcommand,
        format!(
            "'{word}' is not a croft command or an existing path\n\n  tip: a similar subcommand exists: '{suggestion}'"
        ),
    ))
}

/// Levenshtein distance, counted in chars.
pub(super) fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diag = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (diag + usize::from(ca != *cb))
                .min(above + 1)
                .min(row[j] + 1);
            diag = above;
        }
    }
    row[b.len()]
}
