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
        summary: "Cmd-clicking a terminal hyperlink only opens web links now: an OSC 8 cell can carry any URI behind unrelated visible text, and a file:// or custom-scheme target would have launched an arbitrary application off one disguised click. Non-web links are refused with the real destination shown in the status bar.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "Ctrl+F reaches the program in the terminal again on macOS (vim's page-forward, the shell's forward-char); find-in-terminal lives on Cmd+F there, matching VS Code and iTerm2. Linux keeps Ctrl+F opening find.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "Find-in-terminal is bound to the pane it opened on: switching panes closes it and clears its highlight, instead of leaving the old pane lit forever and aiming Enter/Shift+Enter at the wrong pane's scrollback.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "The active find match rides the scroll clock: output streaming under an open find bar no longer detaches the bright highlight from its text or makes next/previous walk from a phantom row.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "Reopen with Encoding clears the undo/redo history: a redo after re-decoding could silently reinstate the whole buffer as decoded under the previous encoding.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "Clearing a terminal truly empties its scrollback now: the erase order used to slide the cleared screen contents into scrollback, still reachable by scrolling up.",
    },
];
