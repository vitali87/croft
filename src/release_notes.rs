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
        kind: NoteKind::Feature,
        summary: "Occurrences highlight: rest the caret on a symbol and every use in the file tints after a beat, writes stronger than reads (LSP documentHighlight, VS Code's word highlight).",
    },
    ReleaseNote {
        kind: NoteKind::Feature,
        summary: "JS test runners: the Testing view now detects vitest and jest from package.json, discovers, runs, and scopes to a file or test just like cargo and pytest.",
    },
    ReleaseNote {
        kind: NoteKind::Feature,
        summary: "Debug a test: Cmd+K Shift+Enter (or Alt+click the gutter play glyph) hands the test at the caret to the debugger — pytest under debugpy, cargo test binaries under lldb-dap.",
    },
    ReleaseNote {
        kind: NoteKind::Feature,
        summary: "A green play glyph marks test functions in the editor gutter (#[test] fns, pytest test_* defs); click it to run that test.",
    },
    ReleaseNote {
        kind: NoteKind::Feature,
        summary: "Call hierarchy: Cmd+K H lists everyone calling the symbol at the caret, Cmd+K Shift+H everything it calls; pick a row to jump, invoke again to walk the next level.",
    },
    ReleaseNote {
        kind: NoteKind::Feature,
        summary: "Logpoints: Shift+Alt+F9 or the gutter menu attaches a log message to a breakpoint (amber diamond) — the debugger prints it, with {expr} interpolated, instead of pausing.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "Cmd+K Enter reaches Run Test at Cursor again — an undocumented Keep Open chord had shadowed it since it shipped; pin moved to Cmd+K P.",
    },
];
