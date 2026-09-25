//! Terminal command suggestions (#614): given what is typed at a shell
//! prompt, what could come next. History first (whole commands that start
//! with the input), then paths (the last word, completed from the
//! directory it names), then flags (ones this command was run with
//! before). Pure: the caller supplies history and a directory lister.

use std::path::{Path, PathBuf};

/// Where a suggestion came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    History,
    Path,
    Flag,
}

/// One suggestion: what to type to accept it, and what to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub kind: Kind,
    /// The text appended at the cursor when accepted.
    pub insert: String,
    /// The whole line (history) or the whole word (path, flag) shown.
    pub label: String,
}

/// At most this many suggestions.
pub const MAX: usize = 8;

/// The suggestions for `input`, most useful first. `history` is newest
/// first; `list_dir` returns a directory's entries as (name, is_dir).
/// Nothing for blank input.
pub fn suggest(
    input: &str,
    history: &[String],
    cwd: &Path,
    home: Option<&Path>,
    list_dir: &mut dyn FnMut(&Path) -> Vec<(String, bool)>,
) -> Vec<Suggestion> {
    if input.trim().is_empty() {
        return Vec::new();
    }
    let mut out: Vec<Suggestion> = Vec::new();
    let push = |out: &mut Vec<Suggestion>, s: Suggestion| {
        if !s.insert.is_empty() && !out.iter().any(|o| o.insert == s.insert) {
            out.push(s);
        }
    };
    for cmd in history {
        if cmd.len() > input.len() && cmd.starts_with(input) {
            push(
                &mut out,
                Suggestion {
                    kind: Kind::History,
                    insert: cmd[input.len()..].to_string(),
                    label: cmd.clone(),
                },
            );
        }
    }
    let word_start = input
        .rfind(char::is_whitespace)
        .map(|i| i + input[i..].chars().next().map_or(1, char::len_utf8))
        .unwrap_or(0);
    let word = &input[word_start..];
    let first_word = !input.trim_start().contains(char::is_whitespace);
    if word.starts_with('-') {
        let command = input.split_whitespace().next().unwrap_or("");
        for cmd in history {
            let mut words = cmd.split_whitespace();
            if words.next() != Some(command) {
                continue;
            }
            for flag in words.filter(|w| w.len() > word.len() && w.starts_with(word)) {
                push(
                    &mut out,
                    Suggestion {
                        kind: Kind::Flag,
                        insert: flag[word.len()..].to_string(),
                        label: flag.to_string(),
                    },
                );
            }
        }
    } else if !first_word || word.contains('/') || word.starts_with(['.', '~']) {
        let (dir_part, base) = match word.rfind('/') {
            Some(i) => (&word[..=i], &word[i + 1..]),
            None => ("", word),
        };
        let dir = resolve_dir(dir_part, cwd, home);
        let mut entries = list_dir(&dir);
        entries.sort();
        for (name, is_dir) in entries {
            if !name.starts_with(base) || name == base {
                continue;
            }
            if name.starts_with('.') && !base.starts_with('.') {
                continue;
            }
            let slash = if is_dir { "/" } else { "" };
            push(
                &mut out,
                Suggestion {
                    kind: Kind::Path,
                    insert: format!("{}{slash}", &name[base.len()..]),
                    label: format!("{name}{slash}"),
                },
            );
        }
    }
    out.truncate(MAX);
    out
}

/// The directory a typed path prefix names: `~` is home, a relative one is
/// under `cwd`, and `.` components drop out.
fn resolve_dir(dir_part: &str, cwd: &Path, home: Option<&Path>) -> PathBuf {
    let raw = if let Some(rest) = dir_part.strip_prefix('~') {
        let Some(h) = home else {
            return cwd.to_path_buf();
        };
        h.join(rest.trim_start_matches('/'))
    } else if dir_part.starts_with('/') {
        PathBuf::from(dir_part)
    } else {
        cwd.join(dir_part)
    };
    raw.components()
        .filter(|c| !matches!(c, std::path::Component::CurDir))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lister(p: &Path) -> Vec<(String, bool)> {
        match p.to_str().unwrap() {
            "/ws" => vec![
                ("src".into(), true),
                ("Cargo.toml".into(), false),
                ("scripts".into(), true),
                (".git".into(), true),
            ],
            "/ws/src" => vec![("main.rs".into(), false), ("app".into(), true)],
            "/home/me" => vec![("notes.md".into(), false)],
            _ => Vec::new(),
        }
    }

    fn run(input: &str, history: &[&str]) -> Vec<Suggestion> {
        let h: Vec<String> = history.iter().map(|s| s.to_string()).collect();
        suggest(
            input,
            &h,
            Path::new("/ws"),
            Some(Path::new("/home/me")),
            &mut lister,
        )
    }

    fn inserts(s: &[Suggestion]) -> Vec<(Kind, &str)> {
        s.iter().map(|x| (x.kind, x.insert.as_str())).collect()
    }

    #[test]
    fn blank_input_suggests_nothing() {
        assert!(run("", &["ls"]).is_empty());
        assert!(run("   ", &["ls"]).is_empty());
    }

    #[test]
    fn history_completes_whole_commands_newest_first_without_repeats() {
        let s = run(
            "cargo t",
            &[
                "cargo test -q",
                "cargo build",
                "cargo test -q",
                "cargo tree",
            ],
        );
        assert_eq!(
            inserts(&s)[..2],
            [(Kind::History, "est -q"), (Kind::History, "ree")]
        );
        assert_eq!(s[0].label, "cargo test -q");
        assert!(
            run("cargo test -q", &["cargo test -q"])
                .iter()
                .all(|x| x.kind != Kind::History),
            "the input itself is not a suggestion"
        );
    }

    #[test]
    fn paths_complete_the_last_word_from_the_directory_it_names() {
        let s = run("cat s", &[]);
        assert_eq!(inserts(&s), [(Kind::Path, "cripts/"), (Kind::Path, "rc/")]);
        assert_eq!(s[1].label, "src/");
        let s = run("cat src/m", &[]);
        assert_eq!(inserts(&s), [(Kind::Path, "ain.rs")]);
        let s = run("vim ~/no", &[]);
        assert_eq!(inserts(&s), [(Kind::Path, "tes.md")]);
        // A dot shows hidden entries; otherwise they stay hidden.
        assert_eq!(inserts(&run("ls .g", &[])), [(Kind::Path, "it/")]);
        assert!(run("ls ", &[]).iter().all(|x| !x.label.starts_with('.')));
    }

    #[test]
    fn a_first_word_is_not_completed_as_a_path_unless_it_looks_like_one() {
        assert!(run("sc", &[]).iter().all(|x| x.kind != Kind::Path));
        assert_eq!(inserts(&run("./sc", &[])), [(Kind::Path, "ripts/")]);
    }

    #[test]
    fn flags_come_from_earlier_runs_of_the_same_command() {
        let s = run(
            "grep --c",
            &["grep --color=auto -n x", "ls --color", "grep --count y"],
        );
        let flags: Vec<&str> = s
            .iter()
            .filter(|x| x.kind == Kind::Flag)
            .map(|x| x.insert.as_str())
            .collect();
        assert_eq!(flags, ["olor=auto", "ount"]);
    }

    #[test]
    fn history_comes_before_paths_and_the_list_is_capped() {
        let s = run("cat s", &["cat src/main.rs"]);
        assert_eq!(s[0].kind, Kind::History);
        assert!(s.iter().skip(1).all(|x| x.kind != Kind::History));
        let many: Vec<String> = (0..20).map(|i| format!("echo {i}")).collect();
        let many: Vec<&str> = many.iter().map(|s| s.as_str()).collect();
        assert_eq!(run("echo", &many).len(), MAX);
    }
}
