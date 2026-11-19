//! The navigator's visible caret: parking it at a row until the file goes live, moving it
//! with an edit or a note, and this seat's site ids.

use super::*;

impl PairState {
    /// Park the navigator's visible caret at `row` (0-based, column 0):
    /// broadcast now when the file is live, else once its bootstrap lands
    /// (the pump resolves it). A newer park supersedes an unresolved one.
    pub(crate) fn park_caret(&mut self, file: &str, row: usize) {
        self.pending_caret = (!self.send_parked_caret(file, row)).then(|| (file.to_string(), row));
    }

    /// Broadcast the caret at an exact position and drop any unresolved
    /// pending park — a streamed edit or a landed note is newer attention,
    /// and a stale park firing afterwards would yank the caret back.
    pub(super) fn caret_now(&mut self, file: &str, row: usize, col: usize) {
        self.pending_caret = None;
        self.session.send_caret(file, row, col);
    }

    /// This seat's site id in every live file: the navigator's wire
    /// identity (the App keys caret color and unseat cleanup off these).
    pub(crate) fn my_site_ids(&self) -> Vec<u64> {
        self.session.my_site_ids()
    }

    /// Broadcast the parked caret when `file` is live, the row clamped to
    /// the document's last line. False = not live yet.
    pub(super) fn send_parked_caret(&mut self, file: &str, row: usize) -> bool {
        let Some(lines) = self.doc_lines(file) else {
            return false;
        };
        let row = row.min(lines.len().saturating_sub(1));
        self.session.send_caret(file, row, 0);
        true
    }

    /// Resolve a pending caret park (one pump tick): broadcast it if the
    /// file went live, drop it if the bootstrap died unanswered.
    pub(super) fn resolve_pending_caret(&mut self) {
        let Some((file, row)) = self.pending_caret.clone() else {
            return;
        };
        if self.send_parked_caret(&file, row) || !self.session.is_bootstrapping(&file) {
            self.pending_caret = None;
        }
    }
}
