//! Hand-curated "IN THIS RELEASE" highlights for the welcome panel.
//!
//! These describe, in plain language, what shipped in the current version:
//! new features and fixed bugs. They are baked straight into the binary as
//! data, so the welcome panel needs zero network and never shells out to
//! `git log` or a forge API — the list is an accurate property of the build.
//!
//! Update [`RELEASE_NOTES`] on every version bump so the panel always tells
//! the truth about what the running binary ships (see the project memory
//! "always populate release narratives after every build"). Newest / most
//! notable highlight first; keep each summary to one short sentence.
//!
//! REPLACE this list on every bump; do not append. The panel describes the
//! single version it is baked into (`v{CARGO_PKG_VERSION}`), not a running
//! changelog. Each patch bump is one change, so the list is usually one entry:
//! what this bump fixed or added, and nothing carried over from prior versions.

use ratatui::style::Color;

/// Whether a highlight introduces a new capability or fixes a defect. Selects
/// the gutter glyph and tint the welcome panel paints beside the summary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoteKind {
    // Which variants RELEASE_NOTES constructs depends on what this version
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

/// What shipped in the current release. Replace on every version bump.
pub const RELEASE_NOTES: &[ReleaseNote] = &[
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "The editor tab strip now follows the selected theme: on the ten editor-inspired themes it used to stay the built-ins' navy chrome regardless of the palette. Tab colors are theme data (strip, tab bodies, hover lift, close pill), derived from each theme's palette when not declared, with Croft Black and Croft Dark rendering byte-for-byte as before.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "Seeding the Search sidebar from the last terminal grep/rg now describes that command faithfully: leftover include/exclude filters from an earlier manual search are replaced instead of silently narrowing the seeded results, rg's -s (case-sensitive) flag is honored, !-negated globs land in files-to-exclude instead of matching nothing, and every -g/--glob accumulates instead of only the first.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "Seeding a search can no longer crash croft: a text selection left in the files-to-include field survived the seed as a stale byte range into the shorter seeded text, and the next keystroke panicked out of bounds. The seed now resets field selections and hands focus to the query it just filled.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "croft ls no longer reports a workspace's collab relay as a phantom persistent session: the relay socket shares the sessions directory and hash keying, so it used to print an (unknown) row whose id collided with the real session's.",
    },
];
