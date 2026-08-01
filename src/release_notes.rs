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
        summary: "Debugging a cargo test no longer freezes croft: the test-binary build runs on a background thread with a visible progress status instead of blocking every pane for the whole compile, and a workspace switched mid-build discards the finished binary instead of debugging the old project inside the new one. The debugger also now picks the harness that actually contains the test (probing each candidate with --list), so a lib test in a lib+bin crate or a workspace no longer launches the wrong binary, runs zero tests, and exits before a breakpoint can bind.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "The gutter play bead is honest now: #[cfg(test)] and #[cfg_attr(test, ...)] no longer count as test attributes (every cfg-gated helper in a codebase wore a bead that ran nothing and wiped the Testing tree), a def test_* only gets a bead in a pytest-collectable file, and clicking a breakpoint dot, the paused stop arrow, or the AI-stream square on a test line no longer silently starts a test run — the click hit-test honors the same sign-cell precedence the render does.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "Removing a breakpoint now removes the whole breakpoint: its logpoint message and condition used to survive removal and silently re-attach to the next plain breakpoint on that line — resurrecting a logpoint that printed instead of ever pausing.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "Running one vitest test with parentheses or brackets in its title works: vitest's -t is a regex like jest's, and croft passed the title unescaped, so 'adds (1 + 1)' matched nothing and an unbalanced bracket errored. Titles are now escaped on every vitest path, matching the jest paths.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "Find highlights stay readable when the caret rests on a match: the LSP occurrence tint used to paint its dark background over the find layer's black-on-gold cells, turning every highlighted match (including the orange active-match cue) into unreadable black-on-grey. Occurrences now paint beneath the find layer.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "Keep Open works from the keyboard again on Cmd+K Shift+P — the tab menu had kept advertising the old Cmd+K Enter chord, which now runs the test at the caret. A stale COMMITS-graph scrollbar no longer deadens a band of the sidebar splitter after leaving Source Control, and a slow call-hierarchy reply no longer replaces a menu the user opened while waiting.",
    },
];
