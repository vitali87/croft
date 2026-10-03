//! Hand-curated "IN THIS RELEASE" highlights for the welcome panel.
//!
//! These describe, in plain language, what shipped in the current version:
//! new features and fixed bugs. They are baked straight into the binary as
//! data, so the welcome panel needs zero network and never shells out to
//! `git log` or a forge API — the list is an accurate property of the build.
//!
//! A pull request that changes what ships writes its highlights to a
//! fragment of its own, `src/release_notes/unreleased/<name>.md`: one per
//! line, each prefixed `feature:` or `fix:`, each summary one short sentence.
//! It never touches `version`. After the merge, the version-bump workflow
//! folds the pending fragments into `src/release_notes/<version>.md` and
//! bumps the version once, on main (`scripts/release.py`).
//!
//! A build carries the pending fragments when there are any, since those are
//! the changes it has on top of its version's release, and its card says so
//! (`heading`); otherwise it carries its version's notes. A build with
//! neither does not build, so a binary always describes itself
//! (`select::baked`).
//!
//! The layout exists because shared notes, and then a version bump in every
//! pull request, put each open PR on the others' conflict path (#399): two
//! changes' notes never conflict in content, only in the file or the version
//! line they shared.

use ratatui::style::Color;

/// Whether a highlight introduces a new capability or fixes a defect. Selects
/// the gutter glyph and tint the welcome panel paints beside the summary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoteKind {
    // Which variants a version's notes construct depends on what it
    // shipped; a fix-only release builds no `Feature` (and vice versa), so
    // either variant may go unconstructed in a given build. `icon()`/`color()`
    // reference both regardless; allow the unused one without tripping dead-code.
    #[allow(dead_code)]
    Feature,
    #[allow(dead_code)]
    Fix,
}

impl NoteKind {
    /// Nerd Font glyph for the gutter (Font Awesome range, same family as the
    /// existing forge badges): a rocket for new features, a bug for fixes.
    pub fn icon(self) -> &'static str {
        match self {
            Self::Feature => "\u{f135}", // fa-rocket
            Self::Fix => "\u{f188}",     // fa-bug
        }
    }

    /// Tint for the glyph: green for features, amber for fixes.
    pub fn color(self) -> Color {
        match self {
            Self::Feature => Color::Rgb(0x8c, 0xc2, 0x65),
            Self::Fix => Color::Rgb(0xe0, 0x9a, 0x4e),
        }
    }
}

/// A single welcome-panel highlight: a kind and a one-line description of what
/// shipped or what was fixed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReleaseNote {
    pub kind: NoteKind,
    pub summary: &'static str,
}

#[cfg(test)]
mod select;

/// This build's highlights, as `build.rs` chose them (`select::baked`): the
/// pending fragments in `src/release_notes/unreleased/` when there are any,
/// otherwise `src/release_notes/<version>.md`.
const NOTES_MD: &str = include_str!(concat!(env!("OUT_DIR"), "/release_notes.md"));

/// Whether the baked notes are pending fragments rather than a release's:
/// the build carries changes that no release has yet.
pub fn unreleased() -> bool {
    env!("CROFT_NOTES_UNRELEASED") == "1"
}

/// The card's heading. A build of a release names it; a build carrying
/// changes past its version's release says so, since the notes below are
/// not that release's.
pub fn heading(unreleased: bool, version: &str) -> String {
    if unreleased {
        format!("IN THIS BUILD (v{version}+)")
    } else {
        format!("IN THIS RELEASE (v{version})")
    }
}

/// Parse the baked notes: one highlight per line, `feature:` or `fix:` first.
///
/// Blank lines and `#` comments are skipped so a notes file can carry a
/// heading. A line with no recognised prefix is a `Fix`, which is the
/// conservative reading: describing a fix as a feature oversells the release,
/// and the opposite merely undersells it.
fn parse(md: &'static str) -> Vec<ReleaseNote> {
    md.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        // A markdown list is the obvious way to write a file of one-liners,
        // and a leading bullet would otherwise make `- feature: x` an
        // unrecognised prefix: a Fix whose summary still carries the dash.
        .map(|l| l.strip_prefix("- ").or_else(|| l.strip_prefix("* ")).unwrap_or(l).trim())
        .map(|line| match line.split_once(':') {
            Some((kind, rest)) if kind.eq_ignore_ascii_case("feature") => ReleaseNote {
                kind: NoteKind::Feature,
                summary: rest.trim_start(),
            },
            Some((kind, rest)) if kind.eq_ignore_ascii_case("fix") => ReleaseNote {
                kind: NoteKind::Fix,
                summary: rest.trim_start(),
            },
            _ => ReleaseNote {
                kind: NoteKind::Fix,
                summary: line,
            },
        })
        .collect()
}

/// What shipped in the current release.
pub fn release_notes() -> &'static [ReleaseNote] {
    static NOTES: std::sync::OnceLock<Vec<ReleaseNote>> = std::sync::OnceLock::new();
    NOTES.get_or_init(|| parse(NOTES_MD))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kind_prefix_selects_the_glyph_and_the_rest_is_the_summary() {
        let notes = parse("feature: Something new.\nfix: Something repaired.\n");
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0].kind, NoteKind::Feature);
        assert_eq!(notes[0].summary, "Something new.");
        assert_eq!(notes[1].kind, NoteKind::Fix);
        assert_eq!(notes[1].summary, "Something repaired.");
    }

    /// A markdown bullet is the obvious way to write a file of one-liners,
    /// and it must not swallow the kind prefix.
    #[test]
    fn a_bullet_does_not_hide_the_kind_prefix() {
        for line in ["- feature: One.", "* feature: One.", "feature: One."] {
            let notes = parse(Box::leak(line.to_string().into_boxed_str()));
            assert_eq!(notes[0].kind, NoteKind::Feature, "{line:?}");
            assert_eq!(notes[0].summary, "One.", "{line:?}");
        }
    }

    #[test]
    fn headings_and_blank_lines_are_not_highlights() {
        let notes = parse("# 0.1.808\n\nfeature: One.\n\n   \nfix: Two.\n");
        assert_eq!(notes.len(), 2, "a notes file may carry a heading");
    }

    /// A summary containing a colon must not be cut at it: only a recognised
    /// KIND prefix splits the line.
    #[test]
    fn a_colon_inside_the_summary_is_kept() {
        let notes = parse("feature: croft plot: numbers in, a chart out.\n");
        assert_eq!(notes[0].summary, "croft plot: numbers in, a chart out.");

        // And a line with an unrecognised prefix keeps its whole text rather
        // than losing everything before the colon.
        let notes = parse("note: something worth saying.\n");
        assert_eq!(notes[0].summary, "note: something worth saying.");
        assert_eq!(
            notes[0].kind,
            NoteKind::Fix,
            "an unrecognised prefix reads as a fix: overselling a release is \
             the worse error"
        );
    }

    /// The panel must show the notes this build carries: the pending
    /// fragments when there are any, otherwise this version's own.
    ///
    /// Non-emptiness alone cannot check that: ANY version's file is
    /// non-empty, so a build.rs that baked the wrong one would pass. The
    /// source tree is read here directly, through `select::baked` and the
    /// path and version cargo built with, and the baked text and its
    /// unreleased flag must equal what it chooses.
    #[test]
    fn the_baked_notes_are_the_ones_this_build_carries_and_are_not_empty() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("release_notes");
        let expected = select::baked(&dir, env!("CARGO_PKG_VERSION")).unwrap();
        assert_eq!(
            NOTES_MD,
            expected.text,
            "the binary carries notes other than v{}'s build's",
            env!("CARGO_PKG_VERSION")
        );
        assert_eq!(unreleased(), expected.unreleased);

        let notes = release_notes();
        assert!(
            !notes.is_empty(),
            "every version ships highlights; the build fails without them"
        );
        for n in notes {
            assert!(
                !n.summary.trim().is_empty(),
                "a highlight with no text would paint an empty row"
            );
        }
    }

    /// A build of a release names it. A build carrying changes past its
    /// version's release must not claim the notes below are that release's.
    #[test]
    fn the_heading_names_the_release_or_the_build_past_it() {
        assert_eq!(heading(false, "0.2.11"), "IN THIS RELEASE (v0.2.11)");
        assert_eq!(heading(true, "0.2.11"), "IN THIS BUILD (v0.2.11+)");
    }
}
