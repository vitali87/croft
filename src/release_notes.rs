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
        summary: "Undoing past an auto save re-dirties the buffer and re-saves, so Cmd+Z can no longer leave the editor silently disagreeing with the file on disk; a save also breaks typing coalescing, so one undo never discards work from both sides of a save point.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "Auto save now surfaces the reload-or-keep prompt when a dirty buffer collides with an external write, instead of silently stopping all auto saves for that file.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "Replace All rewrites exactly the matches the find bar shows: zero-width regex matches and Unicode case foldings the highlighter cannot display are no longer silently replaced, and the match count recounts after the replace instead of reading No results.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "Regex replacements understand VS Code's backslash escapes: a replacement \\n splits the line, \\t inserts a tab.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "Switching a preview tab from a conflicted file to an image no longer lets a palette merge action crash croft on a stale conflict list; merge commands refuse non-text tabs outright.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "Auto save reloads croft's own config files after writing them, matching Cmd+S, and session-restored unsaved tabs are eligible for auto save immediately.",
    },
];
