//! The changed files: `git status --porcelain` parsed into entries, each change's kind,
//! section and badge, the Explorer's decorations, and each file's added and removed line
//! counts.

use super::*;

/// Working-tree change reported by `git status --porcelain`. The Source
/// Control panel groups entries by `kind.section()` and renders a one-letter
/// badge per row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeEntry {
    /// Workspace-relative path. For renames this is the destination path.
    pub path: String,
    pub kind: ChangeKind,
    pub additions: usize,
    pub deletions: usize,
}

impl Default for ChangeEntry {
    fn default() -> Self {
        Self {
            path: String::new(),
            kind: ChangeKind::Modified,
            additions: 0,
            deletions: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    StagedAdded,
    StagedModified,
    StagedDeleted,
    StagedRenamed,
    Modified,
    Deleted,
    Untracked,
    Conflicted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeSection {
    Staged,
    Changes,
    Untracked,
    Conflicts,
}

impl ChangeKind {
    pub fn section(self) -> ChangeSection {
        match self {
            Self::StagedAdded
            | Self::StagedModified
            | Self::StagedDeleted
            | Self::StagedRenamed => ChangeSection::Staged,
            Self::Modified | Self::Deleted => ChangeSection::Changes,
            Self::Untracked => ChangeSection::Untracked,
            Self::Conflicted => ChangeSection::Conflicts,
        }
    }

    pub fn badge(self) -> char {
        match self {
            Self::StagedAdded => 'A',
            Self::StagedModified | Self::Modified => 'M',
            Self::StagedDeleted | Self::Deleted => 'D',
            Self::StagedRenamed => 'R',
            Self::Untracked => 'U',
            Self::Conflicted => '!',
        }
    }
}

/// Undo the C-style quoting `git status --porcelain` applies to paths
/// containing unusual characters (space, control chars, double-quote,
/// backslash, or — when `core.quotepath = true`, which is the default —
/// any non-ASCII byte). Plain ASCII paths pass through unchanged.
///
/// Quoted form: surrounded by `"`, with `\a \b \t \n \v \f \r \" \\`
/// single-char escapes and `\NNN` 3-digit octal escapes for any other
/// byte. Octal escapes are collected into a byte buffer first so that
/// multi-byte UTF-8 sequences (e.g. `\303\244` for `ä`) decode back to
/// the original character.
pub fn unquote_porcelain_path(raw: &str) -> String {
    if !raw.starts_with('"') {
        return raw.to_string();
    }
    let bytes = raw.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i: usize = 1;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'"' {
            break;
        }
        if b == b'\\' && i + 1 < bytes.len() {
            let n = bytes[i + 1];
            match n {
                b'a' => {
                    out.push(0x07);
                    i += 2;
                }
                b'b' => {
                    out.push(0x08);
                    i += 2;
                }
                b't' => {
                    out.push(b'\t');
                    i += 2;
                }
                b'n' => {
                    out.push(b'\n');
                    i += 2;
                }
                b'v' => {
                    out.push(0x0B);
                    i += 2;
                }
                b'f' => {
                    out.push(0x0C);
                    i += 2;
                }
                b'r' => {
                    out.push(b'\r');
                    i += 2;
                }
                b'"' => {
                    out.push(b'"');
                    i += 2;
                }
                b'\\' => {
                    out.push(b'\\');
                    i += 2;
                }
                b'0'..=b'7' => {
                    // Consume a run of `\NNN` triplets so multi-byte
                    // UTF-8 sequences land in `out` as raw bytes ready
                    // for the lossy decode below.
                    while i + 3 < bytes.len()
                        && bytes[i] == b'\\'
                        && (b'0'..=b'7').contains(&bytes[i + 1])
                        && (b'0'..=b'7').contains(&bytes[i + 2])
                        && (b'0'..=b'7').contains(&bytes[i + 3])
                    {
                        let val = ((bytes[i + 1] - b'0') as u16) * 64
                            + ((bytes[i + 2] - b'0') as u16) * 8
                            + ((bytes[i + 3] - b'0') as u16);
                        out.push(val as u8);
                        i += 4;
                    }
                }
                _ => {
                    out.push(b'\\');
                    i += 1;
                }
            }
        } else {
            out.push(b);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The change kinds one porcelain v1 `XY` status code reports: a conflict
/// or an untracked file is one kind; otherwise the index (`X`) and worktree
/// (`Y`) columns each contribute one when set.
pub(super) fn status_kinds(x: char, y: char) -> [Option<ChangeKind>; 2] {
    // Conflicts: AA, DD, AU, UA, DU, UD, UU.
    let conflict = matches!(
        (x, y),
        ('A', 'A') | ('D', 'D') | ('A', 'U') | ('U', 'A') | ('D', 'U') | ('U', 'D') | ('U', 'U')
    );
    if conflict {
        return [Some(ChangeKind::Conflicted), None];
    }
    if x == '?' && y == '?' {
        return [Some(ChangeKind::Untracked), None];
    }
    let staged = (x != ' ' && x != '?').then_some(match x {
        'A' => ChangeKind::StagedAdded,
        'M' => ChangeKind::StagedModified,
        'D' => ChangeKind::StagedDeleted,
        'R' | 'C' => ChangeKind::StagedRenamed,
        _ => ChangeKind::StagedModified,
    });
    let unstaged = (y != ' ' && y != '?').then_some(match y {
        'M' => ChangeKind::Modified,
        'D' => ChangeKind::Deleted,
        _ => ChangeKind::Modified,
    });
    [staged, unstaged]
}

/// Parse `git status --porcelain` (v1) into the change entries the Source
/// Control panel renders. Each line is `XY <path>` where `X` is the index
/// status and `Y` is the worktree status; renames carry both old and new
/// paths separated by ` -> `. Paths with special characters arrive in
/// C-style quoted form — see `unquote_porcelain_path`.
pub fn parse_porcelain_changes(out: &str) -> Vec<ChangeEntry> {
    let mut entries = Vec::new();
    for line in out.lines() {
        if line.len() < 3 {
            continue;
        }
        let bytes = line.as_bytes();
        let x = bytes[0] as char;
        let y = bytes[1] as char;
        let path_part = &line[3..];
        let path = if let Some((_, dst)) = path_part.split_once(" -> ") {
            unquote_porcelain_path(dst)
        } else {
            unquote_porcelain_path(path_part)
        };
        for kind in status_kinds(x, y).into_iter().flatten() {
            entries.push(ChangeEntry {
                path: path.clone(),
                kind,
                ..Default::default()
            });
        }
    }
    entries
}

/// How the Explorer marks a changed path (#1513), after VS Code's git
/// decorations. Ordered weakest to strongest: a folder wears the strongest
/// status among the files beneath it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GitDecoration {
    Untracked,
    Added,
    Renamed,
    Deleted,
    Modified,
    Conflicted,
}

impl GitDecoration {
    /// The letter VS Code puts after a changed file's name.
    pub fn letter(self) -> char {
        match self {
            Self::Untracked => 'U',
            Self::Added => 'A',
            Self::Renamed => 'R',
            Self::Deleted => 'D',
            Self::Modified => 'M',
            Self::Conflicted => '!',
        }
    }

    pub(super) fn from_kind(kind: ChangeKind) -> Self {
        match kind {
            ChangeKind::StagedAdded => Self::Added,
            ChangeKind::StagedModified | ChangeKind::Modified => Self::Modified,
            ChangeKind::StagedDeleted | ChangeKind::Deleted => Self::Deleted,
            ChangeKind::StagedRenamed => Self::Renamed,
            ChangeKind::Untracked => Self::Untracked,
            ChangeKind::Conflicted => Self::Conflicted,
        }
    }
}

/// The Explorer's decorations from `git status --porcelain -z` output
/// (#1513). Porcelain paths are toplevel-relative, so `prefix` (`rev-parse
/// --show-prefix`: where `root` sits in the repository) is stripped and the
/// rest joined onto `root`; a change outside `root` is not in its tree. A
/// partially staged file keeps its strongest status, and each folder between
/// a file and `root` takes the strongest status beneath it.
///
/// `-z` records are `XY <path>` with raw, unquoted path bytes, and a rename
/// or copy is followed by one more record holding its source. Paths stay
/// bytes until joined, so a filename that is not UTF-8 or that contains
/// ` -> ` still keys the exact path the file tree holds.
pub fn parse_decorations(
    root: &Path,
    prefix: &str,
    porcelain: &[u8],
) -> HashMap<PathBuf, GitDecoration> {
    let prefix = prefix.trim().as_bytes();
    let mut out: HashMap<PathBuf, GitDecoration> = HashMap::new();
    let mut records = porcelain.split(|b| *b == 0);
    while let Some(record) = records.next() {
        if record.len() < 4 {
            continue;
        }
        let (x, y) = (record[0] as char, record[1] as char);
        if matches!(x, 'R' | 'C') {
            // The source path; a rename decorates its destination.
            records.next();
        }
        let Some(rel) = record[3..].strip_prefix(prefix) else {
            continue;
        };
        if rel.is_empty() {
            continue;
        }
        let Some(dec) = status_kinds(x, y)
            .into_iter()
            .flatten()
            .map(GitDecoration::from_kind)
            .max()
        else {
            continue;
        };
        let mut path = join_raw(root, rel);
        loop {
            match out.get(&path) {
                // Every ancestor already wears at least this status.
                Some(&have) if have >= dec => break,
                _ => {
                    out.insert(path.clone(), dec);
                }
            }
            match path.parent() {
                Some(parent) if parent != root && parent.starts_with(root) => {
                    path = parent.to_path_buf();
                }
                _ => break,
            }
        }
    }
    out
}

/// How many rows the Source Control list shows for `git status --porcelain
/// -z` output, by the same rule it lists them ([`status_kinds`]): a staged
/// and an unstaged change of one file are two rows (Staged Changes and
/// Changes), an untracked or conflicted file one. VS Code's count badge
/// counts the same (#2629). A rename's or copy's source record is not a row.
pub(super) fn porcelain_z_rows(porcelain: &[u8]) -> usize {
    let mut rows = 0;
    let mut records = porcelain.split(|b| *b == 0);
    while let Some(record) = records.next() {
        let [x, y, ..] = record else {
            continue;
        };
        rows += status_kinds(*x as char, *y as char)
            .into_iter()
            .flatten()
            .count();
        if matches!(x, b'R' | b'C') {
            records.next();
        }
    }
    rows
}

/// Run `git status --porcelain` against `root` and return parsed entries.
/// Returns an empty vec on any error so the panel renders cleanly even
/// when the workspace isn't a git repo or git is missing.
///
/// Does not call `run_git`: that helper trims stdout, which destroys the
/// fixed-width porcelain v1 format (a line like ` M README.md` becomes
/// `M README.md` after `.trim()`, shifting the status code by one column).
pub fn query_changes(root: &Path) -> Vec<ChangeEntry> {
    // Porcelain and numstat paths are TOPLEVEL-relative whatever `-C`
    // names, so any filesystem read of an entry must join the toplevel,
    // not the workspace root (#139).
    let Some(toplevel) = repo_toplevel(root) else {
        return Vec::new();
    };
    let path_str = match root.to_str() {
        Some(s) => s,
        None => return Vec::new(),
    };
    let output = match Git::new()
        .at(path_str)
        // A poll: it must not take index.lock.
        .without_optional_locks()
        .args(["status", "--porcelain", "--untracked-files=all"])
        .into_command()
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };
    let raw = String::from_utf8_lossy(&output.stdout);
    let mut entries = parse_porcelain_changes(&raw);
    let staged = numstat_map(path_str, &["diff", "--cached", "--numstat", "-z"]);
    let unstaged = numstat_map(path_str, &["diff", "--numstat", "-z"]);
    for entry in &mut entries {
        let stats = match entry.kind.section() {
            ChangeSection::Staged => staged.get(&entry.path),
            ChangeSection::Changes => unstaged.get(&entry.path),
            ChangeSection::Untracked => None,
            ChangeSection::Conflicts => None,
        };
        if let Some((a, d)) = stats {
            entry.additions = *a;
            entry.deletions = *d;
        }
    }
    for entry in &mut entries {
        if matches!(entry.kind, ChangeKind::Untracked) {
            entry.additions = count_lines_in_file(&toplevel, &entry.path);
        }
    }
    entries
}

/// Parse `git diff --numstat -z` output into a path→(additions, deletions)
/// map. Each record is `\d+\t\d+\t<path>\0` for a text diff, or
/// `-\t-\t<path>\0` for a binary file (mapped to (0, 0): there's no
/// meaningful line count). A rename or copy leaves the path field empty and
/// follows it with two more fields, `<old>\0<new>\0`; we key on the
/// destination so the lookup matches what porcelain emitted (#1164). Without
/// `-z` git writes a rename as `src/{old.rs => new.rs}`, which matches no
/// porcelain path, and `-z` paths are never C-quoted.
pub fn parse_numstat(out: &str) -> std::collections::HashMap<String, (usize, usize)> {
    let mut map = std::collections::HashMap::new();
    let mut fields = out.split('\0');
    while let Some(record) = fields.next() {
        let mut parts = record.splitn(3, '\t');
        let (Some(adds), Some(dels), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let additions = adds.parse::<usize>().unwrap_or(0);
        let deletions = dels.parse::<usize>().unwrap_or(0);
        let path = if path.is_empty() {
            let _old = fields.next();
            match fields.next() {
                Some(new) => new.to_string(),
                None => break,
            }
        } else {
            path.to_string()
        };
        map.insert(path, (additions, deletions));
    }
    map
}

pub(super) fn numstat_map(
    workdir: &str,
    args: &[&str],
) -> std::collections::HashMap<String, (usize, usize)> {
    let output = match Git::new()
        // A poll: it must not take index.lock. `git diff` does not honour
        // GIT_OPTIONAL_LOCKS: a file whose timestamp changed but whose bytes
        // did not has it write the refreshed index back, so its refresh is
        // turned off too (#2593). The counts are the same: a file with the
        // same bytes has none either way.
        .config("diff.autoRefreshIndex", "false")
        .at(workdir)
        .without_optional_locks()
        .args(args)
        .into_command()
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return std::collections::HashMap::new(),
    };
    parse_numstat(&String::from_utf8_lossy(&output.stdout))
}

pub(super) fn count_lines_in_file(root: &Path, rel_path: &str) -> usize {
    let abs = root.join(rel_path);
    let Ok(bytes) = std::fs::read(&abs) else {
        return 0;
    };
    if bytes.is_empty() {
        return 0;
    }
    let mut lines = bytes.iter().filter(|b| **b == b'\n').count();
    if !bytes.ends_with(b"\n") {
        lines += 1;
    }
    lines
}
