//! Interactive rebase (#620): editing git's `git-rebase-todo` in croft.
//!
//! `git rebase -i` hands its plan to `$GIT_SEQUENCE_EDITOR`: one line per
//! commit, `pick <hash> <subject>`, top to bottom in the order they will be
//! replayed. croft's panes point that variable at `croft edit --wait`, so the
//! plan opens as an ordinary tab, and while that tab is focused a single key
//! sets a commit's action the way GitLens's rebase editor does. Reordering
//! is the editor's own Move Line Up / Down. Saving and closing the tab hands
//! the plan back to git; an empty plan aborts the rebase, which is git's own
//! rule.

use std::path::Path;

/// git's todo actions a single key can set, with their keys.
const ACTIONS: &[(char, &str)] = &[
    ('p', "pick"),
    ('r', "reword"),
    ('e', "edit"),
    ('s', "squash"),
    ('f', "fixup"),
    ('d', "drop"),
];

/// Every spelling git accepts for those actions, long and short.
const SPELLINGS: &[&str] = &[
    "pick", "p", "reword", "r", "edit", "e", "squash", "s", "fixup", "f", "drop", "d",
];

/// The key hint the status line shows while a todo list is open.
pub const HINT: &str = "Rebase: p pick \u{00b7} r reword \u{00b7} e edit \u{00b7} s squash \u{00b7} f fixup \u{00b7} d drop \u{00b7} Alt+\u{2191}\u{2193} reorder \u{00b7} save and close to start (an empty list aborts)";

/// Whether `path` is git's interactive-rebase plan.
pub fn is_todo(path: &Path) -> bool {
    path.file_name().is_some_and(|n| n == "git-rebase-todo")
}

/// The action a key sets, when it is one of the six.
pub fn action_for_key(key: char) -> Option<&'static str> {
    ACTIONS
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, action)| *action)
}

/// `line` with its action replaced by `action`, or `None` when the line is
/// not a commit line (a comment, a blank, an `exec` or `label` line).
pub fn set_action(line: &str, action: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let (word, rest) = trimmed.split_once(char::is_whitespace)?;
    if !SPELLINGS.contains(&word) || rest.trim().is_empty() {
        return None;
    }
    Some(format!("{action} {}", rest.trim_start()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_rewrites_a_commit_lines_action() {
        assert_eq!(
            set_action("pick 1a2b3c Add parser", "squash").as_deref(),
            Some("squash 1a2b3c Add parser")
        );
        assert_eq!(
            set_action("f 1a2b3c Tidy", "reword").as_deref(),
            Some("reword 1a2b3c Tidy"),
            "short spellings count"
        );
    }

    #[test]
    fn comments_blanks_and_other_commands_are_left_alone() {
        assert_eq!(set_action("# Rebase 1a2b..9f8e onto 1a2b", "drop"), None);
        assert_eq!(set_action("", "drop"), None);
        assert_eq!(set_action("exec cargo test", "drop"), None);
        assert_eq!(set_action("pick", "drop"), None, "no hash, no commit");
    }

    #[test]
    fn only_the_six_action_keys_map() {
        assert_eq!(action_for_key('s'), Some("squash"));
        assert_eq!(action_for_key('x'), None);
    }

    #[test]
    fn only_gits_todo_file_is_a_todo() {
        assert!(is_todo(Path::new("/r/.git/rebase-merge/git-rebase-todo")));
        assert!(!is_todo(Path::new("/r/todo.txt")));
    }
}
