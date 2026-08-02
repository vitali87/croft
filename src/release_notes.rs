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
        summary: "An imgcat picture rides the scrollback in long-lived panes: once the 5000-line history saturated, the anchor froze onto a viewport row and the picture sat on top of every later command's output.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "Clearing a pane clears its pictures too: the pane's Clear (and a program-emitted scrollback wipe, e.g. typing clear) used to leave the captured image floating over the fresh prompt.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "Scrolling under an inline image is smooth: every scrolled row used to re-decode and re-resize the full photo on the render thread, stalling the UI during fast output.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "On Ghostty/Kitty the terminal picture and the source-control badge no longer evict each other: both overlays shared one Kitty image id, so the badge's 2-second keepalive and the picture's re-emit kept replacing one another.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "The pane's right-click menu stays readable over a picture: the image now sits below text on Kitty and yields to the open menu on iTerm2/Sixel, matching the editor preview.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "Shift+End reaches full-screen programs again: in the alternate screen it was swallowed by the scrollback reset while Shift+Home, Shift+PageUp and Shift+PageDown correctly fell through.",
    },
];
