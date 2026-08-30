//! Hand-curated "IN THIS RELEASE" highlights for the welcome panel.
//!
//! These describe, in plain language, what shipped in the current version:
//! new features and fixed bugs. They are baked straight into the binary as
//! data, so the welcome panel needs zero network and never shells out to
//! `git log` or a forge API — the list is an accurate property of the build.
//!
//! Write `src/release_notes/<version>.md` on every version bump so the panel
//! always tells the truth about what the running binary ships. One highlight
//! per line, each prefixed `feature:` or `fix:`; newest or most notable
//! first, each summary one short sentence.
//!
//! ONE FILE PER VERSION, and a version's file describes that version alone
//! rather than accumulating a changelog. A missing file for the current
//! version is a BUILD error, so a binary always describes itself.
//!
//! The layout exists because the single shared file was on every open pull
//! request's rebase path (#399): two versions' notes never conflict in
//! content, only in the file they shared, and contributors had to reserve
//! version numbers by hand to keep the bumps from colliding.

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

/// This version's highlights, as written in `src/release_notes/<version>.md`
/// and baked in by `build.rs`.
///
/// One file per version rather than one shared file: two versions' notes
/// never conflict in CONTENT, only in the file they shared, and that file was
/// on every open pull request's rebase path (#399).
const NOTES_MD: &str = include_str!(concat!(env!("OUT_DIR"), "/release_notes.md"));

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

    /// The panel must show THIS version's notes, which is the guarantee the
    /// single shared file used to give.
    #[test]
    fn the_baked_notes_are_this_versions_and_are_not_empty() {
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
}
