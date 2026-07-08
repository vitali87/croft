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
        summary: "Open Scrollback in Editor (Cmd+K D): the pane's whole history lands in a scratch editor tab, where find, vim mode, block selection, save-to-file, and path:line jumps all work on the captured log.",
    },
    ReleaseNote {
        kind: NoteKind::Feature,
        summary: "Annotations (Cmd+K N): pin a note to selected terminal output; the span wears an amber underline that scrolls with the content, a click pops the note, and Cmd+K Shift+N deletes it.",
    },
    ReleaseNote {
        kind: NoteKind::Feature,
        summary: "Sticky command header: scrolled deep into one command's output, the command that produced it pins to the pane's top row with the scroll depth, like Warp.",
    },
    ReleaseNote {
        kind: NoteKind::Feature,
        summary: "Terminal timestamps (palette toggle): each row's arrival time paints down the right edge, amber with a warning mark where the output stalled for a minute or more.",
    },
    ReleaseNote {
        kind: NoteKind::Feature,
        summary: "Click the prompt line to move the shell cursor there (arrow keys are synthesized, the typed text is untouched), and host_accents rules in config.json dress panes on matching hostnames — production shells get a red border, warning pill, and badge watermark.",
    },
];
