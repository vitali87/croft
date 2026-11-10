//! Which release notes a build bakes into its welcome card.
//!
//! `build.rs` includes this file by path and the crate's tests compile it as
//! `release_notes::select`, so the build and the tests that check it read one
//! definition. It uses `std` alone: a build script has no dependencies.

use std::path::{Path, PathBuf};

/// The README that explains `unreleased/` and keeps the directory in git
/// while no note is pending. It is never a note.
pub const UNRELEASED_README: &str = "README.md";

/// The three kinds a highlight takes, as every error that asks for
/// highlights names them.
const PREFIXES: &str = "`feature:`, `fix:` or `refactor:`";

/// What a build bakes in.
pub struct Baked {
    /// The notes, in the form `release_notes::parse` reads.
    pub text: String,
    /// True when the notes are pending fragments: the build carries changes
    /// in use that no release has yet. Pending notes that are all refactor
    /// ones leave it false, since on top of its release the build then
    /// changes nothing in use.
    pub unreleased: bool,
}

/// The filter `release_notes::parse` applies: a line that is neither blank
/// nor a `#` heading. A file holding only a `# 0.1.808` heading is non-blank
/// and still paints an empty card.
pub fn has_highlight(text: &str) -> bool {
    text.lines()
        .map(str::trim)
        .any(|l| !l.is_empty() && !l.starts_with('#'))
}

/// Whether `line` is a refactor highlight: a change that ships but changes
/// nothing in use, such as code moved between files or a rename.
///
/// The line is read as `release_notes::parse` reads it: trimmed, one leading
/// `- ` or `* ` bullet dropped, trimmed again. It is a refactor line when the
/// text before its first colon is `refactor` in any letter case. That text is
/// not trimmed, as `parse` does not trim `feature` or `fix`, so `refactor : x`
/// is not one, and neither is `refactoring: x`. `parse` leaves these lines
/// off the card by the same rule, and `scripts/release.py` mirrors it, so the
/// build, the card and the version bump agree on which merges change
/// something.
pub fn is_refactor_line(line: &str) -> bool {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return false;
    }
    let line = line
        .strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))
        .unwrap_or(line)
        .trim();
    line.split_once(':')
        .is_some_and(|(kind, _)| kind.eq_ignore_ascii_case("refactor"))
}

/// Whether `text` holds a highlight that is not a refactor line: a feature,
/// a fix, or an unprefixed line, which the card reads as a fix. Only such a
/// highlight puts something on the card, and only such a highlight, or a
/// dependency update, makes the version bump cut a version.
pub fn has_user_highlight(text: &str) -> bool {
    text.lines().any(|l| {
        let trimmed = l.trim();
        !trimmed.is_empty() && !trimmed.starts_with('#') && !is_refactor_line(l)
    })
}

/// The pending fragments in `dir/unreleased`, in file-name order, the README
/// left out. Only `.md` files are notes, and never a dot-file: an editor's
/// backup or lock file, or a stray text file beside them, is not.
pub fn fragments(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir.join("unreleased")) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .filter(|p| p.extension().is_some_and(|x| x == "md"))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n != UNRELEASED_README && !n.starts_with('.'))
        })
        .collect();
    found.sort();
    found
}

/// The notes a build at `version` carries, read from `dir`
/// (`src/release_notes`).
///
/// The pending fragments when any of them has a feature or a fix: they are
/// the changes this build has on top of its version's release, and the
/// release's own notes would describe a binary without them. They are joined
/// in file-name order, refactor lines included, each ending in a newline,
/// exactly as `scripts/release.py cut` later writes them into the next
/// version's notes. Otherwise `dir/<version>.md`: with nothing pending, or
/// with only refactor notes pending, which change nothing in use, so the
/// build is its release as far as anyone using it can tell, and the cut
/// makes no version for them either.
///
/// An error rather than an empty card when the notes chosen say nothing or
/// do not exist, so a binary always describes itself. A pending fragment
/// that says nothing is an error even when the version's notes are chosen.
pub fn baked(dir: &Path, version: &str) -> Result<Baked, String> {
    let pending = fragments(dir);
    let mut text = String::new();
    let mut user_facing = false;
    for path in &pending {
        let note = std::fs::read_to_string(path)
            .map_err(|e| format!("{} could not be read ({e}).", path.display()))?;
        if !has_highlight(&note) {
            return Err(format!(
                "{} carries no highlights (blank, or nothing but headings). \
                 Write one per line, each prefixed {PREFIXES}.",
                path.display()
            ));
        }
        user_facing |= has_user_highlight(&note);
        text.push_str(&note);
        if !note.ends_with('\n') {
            text.push('\n');
        }
    }
    if user_facing {
        return Ok(Baked {
            text,
            unreleased: true,
        });
    }
    let path = dir.join(format!("{version}.md"));
    match std::fs::read_to_string(&path) {
        Ok(text) if has_user_highlight(&text) => Ok(Baked {
            text,
            unreleased: false,
        }),
        Ok(text) if has_highlight(&text) => Err(format!(
            "{} carries only `refactor:` lines, which the card leaves out, so \
             it would paint an empty card. A version's notes carry at least \
             one `feature:` or `fix:`.",
            path.display()
        )),
        Ok(_) => Err(format!(
            "{} carries no highlights (blank, or nothing but headings). Write \
             one per line, each prefixed {PREFIXES}.",
            path.display()
        )),
        Err(e) => Err(format!(
            "{} could not be read ({e}), and no feature or fix is pending in \
             {}. A build describes itself: a change that ships adds its notes \
             to src/release_notes/unreleased/<name>.md, and a release's are \
             written when the version is cut.",
            path.display(),
            dir.join("unreleased").display()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `src/release_notes` with the 0.2.11 release and `unreleased/`
    /// holding only its README.
    fn notes_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("0.2.11.md"), "fix: The last release.\n").unwrap();
        std::fs::create_dir(dir.path().join("unreleased")).unwrap();
        std::fs::write(
            dir.path().join("unreleased").join(UNRELEASED_README),
            "fix: Not a note.\n",
        )
        .unwrap();
        dir
    }

    fn pend(dir: &tempfile::TempDir, name: &str, text: &str) {
        std::fs::write(dir.path().join("unreleased").join(name), text).unwrap();
    }

    /// A build carrying merged changes that no release has yet describes
    /// those changes, not the last release's: the fragments, in file-name
    /// order, each ending in a newline, the way `scripts/release.py cut`
    /// later writes them into the next version's notes.
    #[test]
    fn pending_fragments_are_baked_in_name_order_and_marked_unreleased() {
        let dir = notes_dir();
        pend(&dir, "862-hot-exit.md", "feature: Hot exit.");
        pend(&dir, "847-plot.md", "fix: Plot lines join.\nfix: Labels.\n");
        let baked = baked(dir.path(), "0.2.11").unwrap();
        assert_eq!(
            baked.text,
            "fix: Plot lines join.\nfix: Labels.\nfeature: Hot exit.\n"
        );
        assert!(baked.unreleased, "the build carries unreleased changes");
    }

    /// A pending fragment wins even where no file for the version exists, as
    /// in a build of a release-less checkout of a fork.
    #[test]
    fn pending_fragments_need_no_notes_file_for_the_version() {
        let dir = notes_dir();
        pend(&dir, "847-plot.md", "fix: Plot.\n");
        let baked = baked(dir.path(), "0.9.0").unwrap();
        assert_eq!(baked.text, "fix: Plot.\n");
        assert!(baked.unreleased);
    }

    /// A fragment that says nothing is an error even where the version has
    /// notes of its own: the build would otherwise describe the last release
    /// while carrying a change the card is silent about.
    #[test]
    fn a_pending_fragment_without_a_highlight_is_an_error() {
        let dir = notes_dir();
        pend(&dir, "847-plot.md", "fix: Plot.\n");
        pend(&dir, "848-edit.md", "# Edit\n\n");
        let err = baked(dir.path(), "0.2.11").err().expect("an error");
        assert!(err.contains("848-edit.md"), "{err}");
    }

    /// Right after a release no fragment is pending, and the build describes
    /// its own version. The README is not a fragment.
    #[test]
    fn with_nothing_pending_the_versions_own_notes_are_baked() {
        let dir = notes_dir();
        let baked = baked(dir.path(), "0.2.11").unwrap();
        assert_eq!(baked.text, "fix: The last release.\n");
        assert!(!baked.unreleased);
    }

    /// Only `.md` files are notes, and never a dot-file: an editor's backup
    /// or lock file, or a stray `.txt`, in the directory is not baked into
    /// the card. `scripts/release.py` skips the same files.
    #[test]
    fn only_markdown_files_are_fragments() {
        let dir = notes_dir();
        pend(&dir, "847-plot.md~", "fix: A backup.\n");
        pend(&dir, ".#847-plot.md", "fix: A lock file.\n");
        pend(&dir, "notes.txt", "fix: A text file.\n");
        let baked = baked(dir.path(), "0.2.11").unwrap();
        assert_eq!(baked.text, "fix: The last release.\n");
        assert!(!baked.unreleased);
    }

    /// The guarantee the single notes file gave: a binary always describes
    /// itself, so a build with neither a pending note nor notes for its
    /// version does not build.
    #[test]
    fn with_nothing_pending_and_no_notes_for_the_version_it_is_an_error() {
        let dir = notes_dir();
        let err = baked(dir.path(), "0.2.12").err().expect("an error");
        assert!(err.contains("0.2.12.md"), "{err}");
    }

    #[test]
    fn a_versions_notes_without_a_highlight_are_an_error() {
        let dir = notes_dir();
        std::fs::write(dir.path().join("0.2.11.md"), "# 0.2.11\n").unwrap();
        let err = baked(dir.path(), "0.2.11").err().expect("an error");
        assert!(err.contains("0.2.11.md"), "{err}");
    }

    /// Notes that are all refactor ones change nothing in use, and the cut
    /// makes no version for them: on top of its release the build is that
    /// release as far as anyone using it can tell, so it says so.
    #[test]
    fn with_only_refactor_notes_pending_the_versions_own_notes_are_baked() {
        let dir = notes_dir();
        pend(&dir, "2062-move.md", "refactor: Moved the tree (#2062).\n");
        pend(&dir, "2063-rename.md", "- Refactor: Renamed a type.\n");
        let baked = baked(dir.path(), "0.2.11").unwrap();
        assert_eq!(baked.text, "fix: The last release.\n");
        assert!(!baked.unreleased, "nothing in use is past the release");
    }

    /// One feature or fix among them, and the build carries every pending
    /// fragment, refactor lines included, as the cut will fold them all into
    /// the next version's notes.
    #[test]
    fn with_a_fix_among_refactor_notes_every_pending_fragment_is_baked() {
        let dir = notes_dir();
        pend(&dir, "2062-move.md", "refactor: Moved the tree.\n");
        pend(&dir, "847-plot.md", "fix: Plot.");
        pend(&dir, "2063-rename.md", "refactor: Renamed a type.\n");
        let baked = baked(dir.path(), "0.2.11").unwrap();
        assert_eq!(
            baked.text,
            "refactor: Moved the tree.\nrefactor: Renamed a type.\nfix: Plot.\n"
        );
        assert!(baked.unreleased);
    }

    /// Refactor notes alone do not stand in for the version's notes: a build
    /// whose version has none still does not build.
    #[test]
    fn with_only_refactor_notes_pending_and_no_notes_for_the_version_it_is_an_error() {
        let dir = notes_dir();
        pend(&dir, "2062-move.md", "refactor: Moved the tree.\n");
        let err = baked(dir.path(), "0.2.12").err().expect("an error");
        assert!(err.contains("0.2.12.md"), "{err}");
    }

    /// A version's notes holding only refactor lines would paint an empty
    /// card, since the card leaves them out. The cut never writes such a
    /// file, so one is a mistake, not a release.
    #[test]
    fn a_versions_notes_with_only_refactor_lines_are_an_error() {
        let dir = notes_dir();
        std::fs::write(
            dir.path().join("0.2.11.md"),
            "# 0.2.11\nrefactor: Moved the tree.\n",
        )
        .unwrap();
        let err = baked(dir.path(), "0.2.11").err().expect("an error");
        assert!(err.contains("0.2.11.md"), "{err}");
        assert!(err.contains("refactor:"), "{err}");
    }

    /// The rule `release_notes::parse` and `scripts/release.py` repeat: the
    /// kind before the first colon, in any letter case, after one bullet.
    #[test]
    fn a_refactor_line_is_the_refactor_kind_before_the_first_colon() {
        for (line, refactor) in [
            ("refactor: Moved.", true),
            ("  REFACTOR:Moved.", true),
            ("- refactor: Moved.", true),
            ("* Refactor: Moved.", true),
            ("refactoring: Moved.", false),
            ("refactor : Moved.", false),
            ("refactor Moved.", false),
            ("# refactor: a heading", false),
            ("", false),
            ("fix: refactor: Moved.", false),
            ("\u{3000}refactor: Moved.\u{b}", true),
            ("refactor: Moved.\u{c}fix: Repaired.", true),
            ("\u{1f}refactor: Moved.", false),
        ] {
            assert_eq!(is_refactor_line(line), refactor, "{line:?}");
        }
        assert!(!has_user_highlight("# 0.2.11\nrefactor: Moved.\n\n"));
        assert!(has_user_highlight("refactor: Moved.\nPlain text.\n"));
        // `lines` breaks at `\n` alone, which `scripts/release.py` matches.
        assert!(!has_user_highlight("refactor: A.\u{c}fix: B.\n"));
        assert!(!has_user_highlight("refactor: A.\u{2028}fix: B.\n"));
        assert!(!has_user_highlight("refactor: A.\u{1c}fix: B.\n"));
        assert!(has_user_highlight("\u{1f}refactor: A.\n"));
    }
}
