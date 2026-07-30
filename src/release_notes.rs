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
        summary: "Clicking a PDF in a grid of three or more editor groups no longer crashes croft, and the preview bakes into the focused group.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "PDF links open exactly what you clicked: each link covers only its own words, overlapping TOC lines resolve to the most-covered one, and only web and mail links may leave croft (a document's file:// or custom scheme is dropped).",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "A PDF rebuilt on disk (pdflatex finishing) reloads back to the page you were reading instead of snapping to page 1.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "GUI-launch PATH repair now survives rc files that background helpers, works under fish and tcsh, and never delays the instant remote-attach probes.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "Broadcast input encodes arrows per pane, so a zsh pane and a bash pane each receive the form their own shell asked for.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "Stray errors from the OS opener, Reveal in Finder, and new-window AppleScript can no longer scribble on the screen, and JS debugging finds node even when a chatty login shell overflows the pipe.",
    },
];
