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
        summary: "A click on the shell prompt can no longer crash croft: with the prompt row rewritten shorter than where input started (a background progress writer, a shrunk pane), click-to-move-cursor hit inverted clamp bounds and panicked. It now degrades to the sane target instead.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "Terminal timestamps survive pagers and long sessions: entering git log, vim, or htop used to erase every arrival time and re-stamp the whole scrollback with the exit time, any multi-line progress redraw wiped the gutter, and panes past their scrollback depth froze to wrong times. Stamps now ride the same scroll clock as selections.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "Terminal annotations stay glued to their text: a note typed while output streamed used to land rows below the selected line, and in panes past their scrollback depth the amber span froze in place while content slid underneath. Notes now anchor where the prompt opened and follow content like selections do.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "Copy-mode keys continue from where the highlight is: with output streaming, each keypress used to teleport the selection back to stale coordinates while the pane had re-anchored it to the content in view.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "The pinned command header no longer corrupts the screen when the command spans several rows (a heredoc newline, a wrap shown in a widened pane): the row join printed a raw newline byte into the frame. Clicking an annotated note also no longer leaves a stray one-cell highlight behind.",
    },
];
