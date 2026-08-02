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
        summary: "Copying a soft-wrapped line yields one logical line: selections, the durable command history, and the pinned command header used to join wrapped rows with a newline, so a long stored command re-ran only its first fragment when typed back from the history popup.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "Clicking a CAPTURES row finds its line in panes narrower than the captured text: the jump matched a fixed 60-character prefix against grid rows bounded by the pane width, so narrow splits (and wide-glyph lines) always reported the line missing.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "Copy mode is bound to the pane it opened on: clicking a sibling pane now leaves the mode, where it used to keep a stale cursor on the old pane and paint a garbage selection onto the new one on the next keypress. vim's e motion also wraps to the next row like w and b instead of dying at a row's last word.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "A tiny host window can no longer crash croft: the command-history popup (and two sibling popups) inverted their size clamps below 40x10, and a pane squeezed to a sliver panicked inside the terminal engine's resize.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "Command history distinguishes machines: a command run over an in-pane SSH session at /some/path no longer shows up in the directory-scoped history of a local pane sitting at the same path.",
    },
];
