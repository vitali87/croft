//! What git said, in one line: the headline and summary of a command's output, the result
//! of a merge, pull or push, conflict summaries, errors, and refusals to switch branches.

/// The note git prints for each file a merge, rebase or stash pop merged
/// (`Auto-merging pricing.py`), before the line that says how that went.
pub(super) const AUTO_MERGING: &str = "Auto-merging ";

/// What git's conflict line says before the file (`CONFLICT (content):
/// Merge conflict in pricing.py`).
pub(super) const MERGE_CONFLICT_IN: &str = "Merge conflict in ";

/// `line` as a terminal shows it. Git redraws a progress line (`Rebasing
/// (1/1)`) by returning the carriage and, on a capable terminal, erasing
/// the line (`ESC [K`) before the next text, so what is left on screen is
/// the last redraw that says something (#858).
pub(super) fn shown(line: &str) -> &str {
    line.rsplit('\r')
        .map(|part| part.trim_start_matches("\u{1b}[K"))
        .find(|part| !part.trim().is_empty())
        .unwrap_or_default()
}

/// `line` with its runs of whitespace collapsed to one space.
pub(super) fn squash(line: &str) -> String {
    line.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The first line of git's `output` (for a commit, `[branch sha] subject`):
/// what the Source Control panel and the status bar report. The rest, a
/// commit's diffstat and one `create mode` row per new file, ran on past
/// both (#858); the Git Output log keeps it whole. A merge's `Auto-merging`
/// notes give way to the result after them ("Merge made by the 'ort'
/// strategy."), and a line git redrew reads as [`shown`] ("Successfully
/// rebased …", not "Rebasing (1/1)"); a text of nothing but those notes is
/// its first one, never an empty line.
pub fn headline(output: &str) -> &str {
    let said = || {
        output
            .lines()
            .map(shown)
            .filter(|line| !line.trim().is_empty())
    };
    said()
        .find(|line| !line.starts_with(AUTO_MERGING))
        .or_else(|| said().next())
        .unwrap_or_default()
        .trim_end()
}

/// The one line of git's `output` the status bar and the Source Control
/// panel report (#858); the Git Output log keeps the whole text. For a push
/// that is its ref update (`5a6cd5c..9f1e2d3 main -> main`, `[new branch]
/// main -> main`, `[rejected] main -> main (fetch first)`), otherwise the
/// first line that is not `remote:` chatter, a `hint:`, the `To <url>`
/// destination, the `On branch` / `Your branch` preamble git prints before
/// "nothing to commit" or an `Auto-merging` note. Lines read as [`shown`],
/// and runs of spaces collapse to one. Empty when nothing is left, as for a
/// push that printed only `remote:` lines.
pub fn summary_line(output: &str) -> String {
    const NOISE: [&str; 6] = [
        "remote:",
        "hint:",
        "To ",
        "On branch ",
        "Your branch ",
        AUTO_MERGING,
    ];
    let lines = || {
        output
            .lines()
            .map(|l| shown(l).trim())
            .filter(|l| !l.is_empty() && !NOISE.iter().any(|n| l.starts_with(n)))
    };
    let Some(line) = lines()
        .find(|l| l.contains(" -> "))
        .or_else(|| lines().next())
    else {
        return String::new();
    };
    let mut words = line.split_whitespace().peekable();
    // A ref update leads with git's one-character flag (`*`, `+`, `-`, `!`,
    // `=`, or a space, which trimming already took).
    if words
        .peek()
        .is_some_and(|w| matches!(*w, "*" | "+" | "-" | "!" | "="))
    {
        words.next();
    }
    words.collect::<Vec<_>>().join(" ")
}

/// The one line a git operation that succeeded reports in the status bar
/// and the Source Control panel (#858): git's result ("Already up to
/// date.", "Fast-forward", "Merge made by the 'ort' strategy.",
/// "Successfully rebased and updated refs/heads/main.", "Dropped
/// refs/stash@{0} (…)", "[main 5a6cd5c] subject"). Passed over on the way:
/// the fetch's `From <url>` and the push's `To <url>`, `remote:` chatter,
/// `hint:`s, `Updating a..b`, `Fetching <remote>`, `Auto-merging` notes,
/// the `warning:` / `error:` / `fatal:` asides a success can still carry
/// (a failed push negotiation), the `git status` dump a stash or a switch
/// prints, the `M\tpath` rows a switch lists, and every indented line (ref
/// updates, the diffstat, file lists). Lines read as [`shown`]. When
/// nothing else is left, the ref update if there is one ([`summary_line`]),
/// else empty, and the caller's own word ("Pulled", "Applied stash") says
/// it; the Git Output log keeps git's whole text.
pub fn result_line(output: &str) -> String {
    const NOISE: [&str; 18] = [
        "From ",
        "To ",
        "remote:",
        "hint:",
        "Updating ",
        "Fetching ",
        AUTO_MERGING,
        "warning:",
        "error:",
        "fatal:",
        "On branch ",
        "Your branch ",
        "Changes not staged for commit:",
        "Changes to be committed:",
        "Untracked files:",
        "Unmerged paths:",
        "no changes added to commit",
        "nothing added to commit",
    ];
    // `M\tpath`: a file `git switch` carried over.
    let file_row = |line: &str| {
        let bytes = line.as_bytes();
        bytes.len() > 1 && bytes[0].is_ascii_uppercase() && bytes[1] == b'\t'
    };
    let result = output.lines().map(shown).find(|line| {
        !line.trim().is_empty()
            && !line.starts_with(char::is_whitespace)
            && !file_row(line)
            && !NOISE.iter().any(|n| line.starts_with(n))
    });
    match result {
        Some(line) => line.trim_end().to_string(),
        None if output.contains(" -> ") => summary_line(output),
        None => String::new(),
    }
}

/// The conflicts git stopped on, from the `CONFLICT …` lines of its
/// `output` (#858): "1 conflict (pricing.py)", "2 conflicts (a.py, b.py)",
/// naming three files at most before "…". A lone conflict git words
/// without "Merge conflict in <file>" (`CONFLICT (modify/delete): …`) is
/// git's own line. `None` when git reported no conflict.
pub fn conflict_summary(output: &str) -> Option<String> {
    const NAMED: usize = 3;
    let conflicts: Vec<&str> = output
        .lines()
        .map(|l| shown(l).trim())
        .filter(|l| l.starts_with("CONFLICT ("))
        .collect();
    match conflicts.as_slice() {
        [] => return None,
        [only] if !only.contains(MERGE_CONFLICT_IN) => return Some(squash(only)),
        _ => {}
    }
    let mut files: Vec<&str> = Vec::new();
    let mut unnamed = 0;
    for line in &conflicts {
        match line.split_once(MERGE_CONFLICT_IN) {
            Some((_, file)) if !files.contains(&file.trim()) => files.push(file.trim()),
            Some(_) => {}
            None => unnamed += 1,
        }
    }
    let count = files.len() + unnamed;
    let noun = if count == 1 { "conflict" } else { "conflicts" };
    if files.is_empty() {
        return Some(format!("{count} {noun}"));
    }
    let mut names = files[..files.len().min(NAMED)].join(", ");
    if count > NAMED.min(files.len()) {
        names.push_str(", \u{2026}");
    }
    Some(format!("{count} {noun} ({names})"))
}

/// The one line of git's error text `err` the status bar and the Source
/// Control panel report (#858); the Git Output log keeps the whole text.
/// First the conflicts git stopped on ([`conflict_summary`]), then a
/// rejected ref update (`[rejected] main -> main (fetch first)`), then
/// git's first `error:` or `fatal:` line, with the files git listed under
/// it ([`with_listed_files`]), then [`summary_line`]. Failing all of those,
/// the first line, so a refusal always says something.
pub fn error_line(err: &str) -> String {
    if let Some(conflicts) = conflict_summary(err) {
        return conflicts;
    }
    let lines = || err.lines().map(|l| shown(l).trim());
    if let Some(rejected) = lines().find_map(|l| l.strip_prefix('!').filter(|_| l.contains(" -> ")))
    {
        return squash(rejected);
    }
    if let Some(at) = lines().position(|l| l.starts_with("error:") || l.starts_with("fatal:")) {
        let line = lines().nth(at).map(squash).unwrap_or_default();
        return with_listed_files(line, err.lines().skip(at + 1));
    }
    let line = summary_line(err);
    if line.is_empty() {
        headline(err).to_string()
    } else {
        line
    }
}

/// `line`, git's `error:` line, followed by the files git listed under it
/// when it ends in a colon (#925): "…would be overwritten by checkout:
/// a.txt" names what is in the way, where the line alone stopped at the
/// colon. Git indents each file on a line of its own after it; three are
/// named at most before "…".
pub(super) fn with_listed_files<'a>(
    mut line: String,
    after: impl Iterator<Item = &'a str>,
) -> String {
    const NAMED: usize = 3;
    if !line.ends_with(':') {
        return line;
    }
    let files: Vec<&str> = after
        .map(shown)
        .take_while(|l| l.starts_with(char::is_whitespace) && !l.trim().is_empty())
        .map(str::trim)
        .collect();
    if files.is_empty() {
        return line;
    }
    line.push(' ');
    line.push_str(&files[..files.len().min(NAMED)].join(", "));
    if files.len() > NAMED {
        line.push_str(", \u{2026}");
    }
    line
}

/// Whether git refused a branch switch because it would overwrite local
/// changes (#925), the refusal a stash gets past. Git ends it with "Please
/// commit your changes or stash them before you switch branches."; a switch
/// blocked by untracked files asks to "move or remove them" instead, and
/// when git names both a stash of the tracked changes still leaves the
/// switch refused, so that is not one either.
pub fn local_changes_block_switch(err: &str) -> bool {
    err.contains("commit your changes or stash them") && !err.contains("move or remove them")
}

/// A failed git operation's one line (#858), `noun` naming the operation
/// ("Merge", "Pop stash"): "Merge: 1 conflict (pricing.py)" when git
/// stopped on conflicts, which wait on the user to resolve them, otherwise
/// "Merge failed: " and git's [`error_line`].
pub fn failure_line(noun: &str, err: &str) -> String {
    match conflict_summary(err) {
        Some(conflicts) => format!("{noun}: {conflicts}"),
        None => format!("{noun} failed: {}", error_line(err)),
    }
}
