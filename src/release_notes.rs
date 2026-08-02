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
        summary: "Closing a terminal pane can no longer freeze croft on Linux: with a background job still holding the pty, the reader thread never saw EOF and the whole UI hung on the join; a shutdown pipe now wakes it instantly. The shell is also reaped on every path, so a HUP-trapping shell no longer leaves a zombie process per closed pane.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "Clicking a Local snapshot in the TIMELINE no longer destroys unsaved edits: a dirty tab is left intact and the snapshot diff opens beside it instead of replacing the buffer wholesale.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "Local history survives auto save: saves within 10 seconds merge into the newest snapshot (VS Code's mergeWindow) instead of churning the whole 50-entry store out in a minute of typing, snapshots store raw bytes so non-UTF-8 files get history too, recording runs off the render thread, the store key no longer depends on the Rust toolchain (existing stores migrate), and files outside the workspace root now list their snapshots instead of loading forever.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "Restore Snapshot is honest again: a failed snapshot click disarms it (it used to silently write an older file's snapshot over a file not even on screen), and the restored file's tab shows the restored text instead of keeping the stale diff view.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "Inline blame never paints another file's authors: switching files with the same line count used to wear the previous file's blame until the new fetch landed.",
    },
];
