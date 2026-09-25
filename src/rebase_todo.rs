//! Interactive rebase (#620): the commits of `base..HEAD` as rows you give
//! an action and reorder, then the `git rebase -i` todo that carries them
//! out.
//!
//! croft is not git's editor, so it drives the rebase itself: it writes the
//! todo, runs `git rebase -i <base>` with `GIT_SEQUENCE_EDITOR` copying that
//! todo over git's, and `GIT_EDITOR=true` so squash messages are taken as
//! git combines them. A reword's new message is asked for up front and
//! applied by an `exec git commit --amend` after the pick, so nothing waits
//! on an editor. Pure: the caller runs git and writes the files.

use std::path::Path;

/// What happens to one commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Pick,
    Reword,
    Edit,
    Squash,
    Fixup,
    Drop,
}

impl Action {
    /// The todo keyword.
    pub fn word(self) -> &'static str {
        match self {
            Action::Pick => "pick",
            Action::Reword => "reword",
            Action::Edit => "edit",
            Action::Squash => "squash",
            Action::Fixup => "fixup",
            Action::Drop => "drop",
        }
    }

    /// The action a key sets: p r e s f d.
    pub fn from_key(c: char) -> Option<Action> {
        Some(match c {
            'p' => Action::Pick,
            'r' => Action::Reword,
            'e' => Action::Edit,
            's' => Action::Squash,
            'f' => Action::Fixup,
            'd' => Action::Drop,
            _ => return None,
        })
    }
}

/// One commit row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub action: Action,
    pub sha: String,
    pub subject: String,
    /// A reword's new message.
    pub message: Option<String>,
}

/// `git log` arguments listing `base..HEAD` oldest first, as
/// `<sha>\t<subject>` lines.
pub fn log_args(base: &str) -> Vec<String> {
    vec![
        String::from("log"),
        String::from("--reverse"),
        String::from("--format=%H%x09%s"),
        format!("{base}..HEAD"),
    ]
}

/// The rows of that log, every one a pick.
pub fn parse_log(out: &str) -> Vec<Entry> {
    out.lines()
        .filter_map(|l| {
            let (sha, subject) = l.split_once('\t')?;
            Some(Entry {
                action: Action::Pick,
                sha: sha.trim().to_string(),
                subject: subject.to_string(),
                message: None,
            })
        })
        .collect()
}

/// Move row `i` one place up or down; the index it lands on.
pub fn move_entry(entries: &mut [Entry], i: usize, up: bool) -> usize {
    let j = if up {
        i.checked_sub(1)
    } else {
        Some(i + 1).filter(|&j| j < entries.len())
    };
    match j {
        Some(j) if i < entries.len() => {
            entries.swap(i, j);
            j
        }
        _ => i,
    }
}

/// Why this list cannot run, if it cannot: a squash or fixup with no kept
/// commit before it, or a reword without a message.
pub fn problem(entries: &[Entry]) -> Option<String> {
    let mut kept = false;
    for e in entries {
        match e.action {
            Action::Squash | Action::Fixup if !kept => {
                return Some(format!(
                    "{} {} has no kept commit before it to fold into",
                    e.action.word(),
                    short(&e.sha)
                ));
            }
            Action::Reword if e.message.as_deref().is_none_or(|m| m.trim().is_empty()) => {
                return Some(format!("reword {} needs a new message", short(&e.sha)));
            }
            Action::Drop => {}
            _ => kept = true,
        }
    }
    None
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
}

/// Quote `s` for a POSIX shell.
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The todo git runs. A reword is a pick followed by an amend from the file
/// `message_file(i)` names, which the caller writes with the new message.
pub fn todo(entries: &[Entry], message_file: &dyn Fn(usize) -> std::path::PathBuf) -> String {
    let mut out = String::new();
    for (i, e) in entries.iter().enumerate() {
        if e.action == Action::Reword {
            out.push_str(&format!("pick {} {}\n", e.sha, e.subject));
            out.push_str(&format!(
                "exec git commit --amend --only --file={}\n",
                quote(&message_file(i).display().to_string())
            ));
        } else {
            out.push_str(&format!("{} {} {}\n", e.action.word(), e.sha, e.subject));
        }
    }
    out
}

/// `git rebase -i` arguments for `base`, stashing local changes around it.
pub fn rebase_args(base: &str) -> Vec<String> {
    vec![
        String::from("rebase"),
        String::from("-i"),
        String::from("--autostash"),
        base.to_string(),
    ]
}

/// `GIT_SEQUENCE_EDITOR`: copy `todo_file` over the todo git hands it.
pub fn sequence_editor(todo_file: &Path) -> String {
    format!("cp {}", quote(&todo_file.display().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn e(action: Action, sha: &str, subject: &str) -> Entry {
        Entry {
            action,
            sha: sha.into(),
            subject: subject.into(),
            message: None,
        }
    }

    #[test]
    fn actions_have_todo_words_and_keys() {
        let all = [
            (Action::Pick, "pick", 'p'),
            (Action::Reword, "reword", 'r'),
            (Action::Edit, "edit", 'e'),
            (Action::Squash, "squash", 's'),
            (Action::Fixup, "fixup", 'f'),
            (Action::Drop, "drop", 'd'),
        ];
        for (a, word, key) in all {
            assert_eq!(a.word(), word);
            assert_eq!(Action::from_key(key), Some(a));
        }
        assert_eq!(Action::from_key('x'), None);
    }

    #[test]
    fn the_log_lists_oldest_first_and_parses_into_picks() {
        assert_eq!(
            log_args("main"),
            vec!["log", "--reverse", "--format=%H%x09%s", "main..HEAD"]
        );
        let rows = parse_log("aaa111\tfirst change\nbbb222\tsecond\twith tab\n\n");
        assert_eq!(
            rows,
            vec![
                e(Action::Pick, "aaa111", "first change"),
                e(Action::Pick, "bbb222", "second\twith tab")
            ]
        );
    }

    #[test]
    fn rows_move_within_bounds() {
        let mut rows = vec![
            e(Action::Pick, "a", "1"),
            e(Action::Pick, "b", "2"),
            e(Action::Pick, "c", "3"),
        ];
        assert_eq!(move_entry(&mut rows, 2, true), 1);
        assert_eq!(
            rows.iter().map(|r| r.sha.as_str()).collect::<String>(),
            "acb"
        );
        assert_eq!(move_entry(&mut rows, 0, true), 0, "the top stays put");
        assert_eq!(move_entry(&mut rows, 2, false), 2, "the bottom stays put");
    }

    #[test]
    fn a_squash_needs_a_kept_commit_before_it_and_a_reword_needs_a_message() {
        assert!(problem(&[e(Action::Pick, "a", "1"), e(Action::Squash, "b", "2")]).is_none());
        assert!(problem(&[e(Action::Squash, "a", "1")]).is_some());
        assert!(problem(&[e(Action::Drop, "a", "1"), e(Action::Fixup, "b", "2")]).is_some());
        assert!(problem(&[e(Action::Reword, "a", "1")]).is_some());
        let mut r = e(Action::Reword, "a", "1");
        r.message = Some(String::from("better"));
        assert!(problem(&[r]).is_none());
    }

    #[test]
    fn the_todo_carries_every_action_and_amends_after_a_reword() {
        let mut reword = e(Action::Reword, "bbb", "two");
        reword.message = Some(String::from("Two, reworded"));
        let rows = vec![
            e(Action::Pick, "aaa", "one"),
            reword,
            e(Action::Fixup, "ccc", "three"),
            e(Action::Drop, "ddd", "four"),
            e(Action::Edit, "eee", "five"),
        ];
        let text = todo(&rows, &|i| PathBuf::from(format!("/tmp/msg {i}")));
        assert_eq!(
            text,
            "pick aaa one\npick bbb two\nexec git commit --amend --only --file='/tmp/msg 1'\nfixup ccc three\ndrop ddd four\nedit eee five\n"
        );
    }

    #[test]
    fn the_rebase_runs_with_our_todo_and_quotes_its_path() {
        assert_eq!(
            rebase_args("main"),
            vec!["rebase", "-i", "--autostash", "main"]
        );
        assert_eq!(
            sequence_editor(Path::new("/tmp/a b/todo")),
            "cp '/tmp/a b/todo'"
        );
        assert_eq!(
            sequence_editor(Path::new("/tmp/it's")),
            "cp '/tmp/it'\\''s'"
        );
    }
}
