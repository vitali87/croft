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
        summary: "A momentum flick over a PDF preview coalesces reliably: the wheel cooldown now starts when the page render finishes, so a render slower than the cooldown no longer lets every queued wheel event through as its own blocking page step.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "Splitting or reopening a live-shared file can no longer wipe the session: the duplicate pane used to load the stale disk copy and, on your first keystroke, broadcast a diff that reverted every participant to the pre-session file. Fresh panes now seed from the shared document (a keystroke arriving before the seed is refused instead of silently wiped), all panes of a shared file stay in step, diff/image/sheet tabs are never mistaken for the document, and a reused preview tab detaches from the old file's doc when it navigates.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "Losing the collab relay is now detected: the session reconnects (files re-join automatically) instead of silently swallowing every edit while the share looked alive, and edits made while the link was down are replayed into the restored session instead of being wiped by the owner's older snapshot — including edits spread across split panes, which mirror each other offline. A disconnected guest can also save its shared files, so a permanent outage no longer strands work in RAM.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "A shared file whose session owner never answers now truly degrades to local editing: the input gate used to re-arm every three seconds forever and the guest could neither type nor save; the guest now edits freely and its save writes to disk.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "One stuck participant can no longer freeze everyone: the collab relay's forwarding writes are bounded, a peer that stops draining is dropped, and a full socket can no longer wedge croft's own UI thread mid-send.",
    },
    ReleaseNote {
        kind: NoteKind::Fix,
        summary: "A collab peer (including an AI seat over the MCP agent) can no longer reach outside the workspace: shared-file keys arriving over the wire are contained to the workspace root, so neither a traversing path nor a symlink pointing beyond the root can read or edit files outside it.",
    },
];
