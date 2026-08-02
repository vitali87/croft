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
        summary: "Run Test at Cursor no longer crashes croft when no file is open, and when a discovered test has a unique leaf name it runs exactly that test instead of every test whose name contains the same word.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "Running a suite from its header runs only that suite on every runner: cargo anchors on the module separator, vitest and jest anchor the describe chain, so suite auth no longer sweeps auth-helper tests along.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "A test run that dies before reporting (a compile error, a test renamed since discovery) no longer strands spinning Running dots or hides behind the old tally, even one with a stale failure: the marks roll back and the summary leads with run failed.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "Clicking a test's name locates its source on a background thread (a big workspace no longer freezes the UI mid-click), honours .gitignore even outside a git checkout, never jumps into target or node_modules, and works for vitest and jest titles too: the jump prefers the test declaration over a comment that merely mentions the title.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "Wide characters in test names clip at the Testing panel's edge instead of painting through the scrollbar and border into the neighbouring pane.",
    },
];
