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
        summary: "Croft.app starts again: the Dock-launch PATH repair asked your login shell for its PATH on croft's own terminal, and an interactive zsh steals the terminal's foreground for job control, so croft came up as a background process and died with an I/O error before drawing anything. The probe now runs in its own session, where it cannot touch croft's terminal.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "Navigator comment boxes carry only the model's own words: claude's startup chatter and host notices (a suppressed edit, a file nobody serves) now go to the OUTPUT panel instead of being anchored in your file as the navigator's remarks and counted as its comments.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "The editor can no longer hang on a wedged navigator: nothing on the render or tick path waits on the pilot's internals anymore (a stalled seat reads as busy and the last known carets and comments keep painting), closing a same-process deadlock introduced when the navigator moved in-process.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "A send that never reaches the model leaves the seat exactly as it was: no permanently busy navigator, the cancel note it still owes the model is kept, and a failed yield no longer destroys the diff baseline or latches the next turn into comment-only mode.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "The seat stays busy until croft has actually consumed the turn's end (a turn started in the finish window stole the previous turn's anchor and comment count), and a fence header cut off by the end of a turn is dropped instead of leaking raw protocol markers into a comment box.",
    },
];
