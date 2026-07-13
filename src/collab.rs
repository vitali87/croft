//! Replicated text document for multiplayer Phase D (independent viewports).
//!
//! croft's shared-viewport multiplayer (src/session_host.rs) broadcasts one
//! PTY verbatim, so every participant sees the same screen. Phase D lets
//! participants edit the same buffer from independent viewports, which needs
//! one croft process per participant holding its own replica of the buffer.
//! With no central authority to serialize edits, concurrent inserts and
//! deletes must still converge to the same text on every replica: that is the
//! job of a CRDT.
//!
//! [`CollabDoc`] wraps a [`cola::Replica`] (which tracks only edit *positions*)
//! next to the canonical linear text (which croft owns, exactly as it owns the
//! editor's `Vec<String>`). A local edit returns an [`Op`] to broadcast; a
//! remote [`Op`] is integrated back into the text at the position cola resolves
//! against concurrent edits. This module is deliberately self-contained and
//! transport-agnostic: wiring it to the editor and the control channel is a
//! later Phase D slice.
//!
//! Nothing outside this module constructs a [`CollabDoc`] yet (only its tests
//! do); the editor/control-channel consumer is the next slice, so the whole
//! module is allowed dead until then rather than sprinkling per-item allows.
#![allow(dead_code)]
//
// ponytail: byte-offset model over the whole buffer, ASCII-proven here. The
// row/col <-> byte-offset mapping against the editor's line vector, and the
// UTF-8 boundary handling, land when the editor is wired in (next slice).

use cola::{Deletion, Insertion, Replica};
use serde::{Deserialize, Serialize};

/// One edit to send to (or receive from) other replicas. cola's operations
/// carry position metadata but not the inserted characters, so an insert also
/// carries the literal text; a delete needs only the range metadata.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Op {
    Insert { insertion: Insertion, text: String },
    Delete { deletion: Deletion },
}

/// A replicated text buffer: the canonical text plus the cola replica that
/// resolves concurrent edits into a convergent order.
pub struct CollabDoc {
    replica: Replica,
    text: String,
}

impl CollabDoc {
    /// A fresh document with `id` as this replica's identity and `initial` as
    /// its starting contents. Every replica of the same document must start
    /// from the same text (bootstrap it once, then exchange [`Op`]s).
    pub fn new(id: u64, initial: &str) -> Self {
        Self {
            replica: Replica::new(id, initial.len()),
            text: initial.to_string(),
        }
    }

    /// A second replica of this document for a new peer `id`, sharing the
    /// current contents and edit history so their future ops integrate.
    pub fn fork(&self, id: u64) -> Self {
        Self {
            replica: self.replica.fork(id),
            text: self.text.clone(),
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Apply a local insertion at byte offset `at` and return the op to
    /// broadcast to the other replicas.
    pub fn local_insert(&mut self, at: usize, s: &str) -> Op {
        self.text.insert_str(at, s);
        let insertion = self.replica.inserted(at, s.len());
        Op::Insert {
            insertion,
            text: s.to_string(),
        }
    }

    /// Apply a local deletion of byte range `at..at+len` and return the op.
    pub fn local_delete(&mut self, at: usize, len: usize) -> Op {
        let deletion = self.replica.deleted(at..at + len);
        self.text.replace_range(at..at + len, "");
        Op::Delete { deletion }
    }

    /// Integrate an op from another replica into this document's text at the
    /// position cola resolves against any concurrent local edits.
    pub fn apply_remote(&mut self, op: &Op) {
        match op {
            Op::Insert { insertion, text } => {
                if let Some(offset) = self.replica.integrate_insertion(insertion) {
                    self.text.insert_str(offset, text);
                }
            }
            Op::Delete { deletion } => {
                // Reverse order so each range's offsets stay valid as we splice.
                let mut ranges = self.replica.integrate_deletion(deletion);
                ranges.sort_by_key(|r| std::cmp::Reverse(r.start));
                for range in ranges {
                    self.text.replace_range(range, "");
                }
            }
        }
    }
}

/// Byte offset of the char-indexed position `(row, col)` within the text
/// formed by joining `lines` with `'\n'` — the linear coordinate CollabDoc and
/// cola operate in. croft's editor addresses the buffer as `(row, char-column)`
/// (`cursor_col` is a char index throughout src/widgets/editor.rs); cola
/// addresses it as one byte offset, so every editor edit converts through here.
pub fn byte_offset(lines: &[String], row: usize, col: usize) -> usize {
    let mut offset = 0;
    for line in lines.iter().take(row) {
        offset += line.len() + 1; // +1 for the '\n' separator
    }
    if let Some(line) = lines.get(row) {
        offset += line
            .char_indices()
            .nth(col)
            .map(|(b, _)| b)
            .unwrap_or(line.len());
    }
    offset
}

/// Inverse of [`byte_offset`]: the `(row, char-column)` of a byte `offset`.
/// `offset` is assumed char-aligned (every offset cola or [`byte_offset`]
/// produces is); a mid-char offset would clamp at the enclosing char boundary
/// via `chars().count()`. Past-the-end clamps to the end of the last line.
pub fn position(lines: &[String], offset: usize) -> (usize, usize) {
    let mut remaining = offset;
    for (row, line) in lines.iter().enumerate() {
        let line_bytes = line.len();
        if remaining <= line_bytes {
            let end = line
                .char_indices()
                .map(|(b, _)| b)
                .chain(std::iter::once(line_bytes))
                .take_while(|&b| b <= remaining)
                .count()
                .saturating_sub(1);
            return (row, end);
        }
        remaining -= line_bytes + 1; // consume the line and its '\n'
    }
    let row = lines.len().saturating_sub(1);
    let col = lines.get(row).map(|l| l.chars().count()).unwrap_or(0);
    (row, col)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The defining CRDT property: two replicas that make *concurrent* edits
    /// (each unaware of the other's) and then exchange ops must arrive at the
    /// identical text. A naive "apply the remote edit at its original offset"
    /// scheme diverges here (each side's earlier local edit shifted the other's
    /// intended position); cola resolving positions is what makes it converge.
    #[test]
    fn concurrent_edits_converge_on_both_replicas() {
        let mut a = CollabDoc::new(1, "abc");
        let mut b = a.fork(2);

        // Concurrent, position-conflicting inserts at opposite ends.
        let op_a = a.local_insert(0, "X"); // a: "Xabc"
        let op_b = b.local_insert(3, "Y"); // b: "abcY"

        a.apply_remote(&op_b);
        b.apply_remote(&op_a);

        assert_eq!(a.text(), b.text(), "replicas must converge");
        assert_eq!(a.text(), "XabcY");
    }

    /// Concurrent insert and delete over overlapping regions still converge.
    #[test]
    fn concurrent_insert_and_delete_converge() {
        let mut a = CollabDoc::new(1, "hello world");
        let mut b = a.fork(2);

        let op_a = a.local_insert(5, " dear"); // a: "hello dear world"
        let op_b = b.local_delete(6, 5); // b deletes "world" -> "hello "

        a.apply_remote(&op_b);
        b.apply_remote(&op_a);

        assert_eq!(a.text(), b.text(), "replicas must converge");
    }

    /// An op serializes and deserializes across the wire (the control channel
    /// will carry these), and integrating the decoded op matches integrating
    /// the original.
    #[test]
    fn ops_round_trip_through_serde() {
        let mut a = CollabDoc::new(1, "abc");
        let mut b = a.fork(2);

        let op = a.local_insert(1, "ZZ"); // a: "aZZbc"
        let bytes = serde_json::to_vec(&op).expect("serialize op");
        let decoded: Op = serde_json::from_slice(&bytes).expect("deserialize op");

        b.apply_remote(&decoded);
        assert_eq!(b.text(), a.text());
        assert_eq!(b.text(), "aZZbc");
    }

    #[test]
    fn char_column_maps_to_byte_offset_across_lines_and_multibyte() {
        // "héllo" is 5 chars / 6 bytes (é is 2 bytes); "wörld" likewise.
        let lines = vec![String::from("héllo"), String::from("wörld")];

        // Within the first line: char col 3 is the second 'l', at byte 4.
        assert_eq!(byte_offset(&lines, 0, 3), 4);
        // End of the first line: byte 6 (before the '\n').
        assert_eq!(byte_offset(&lines, 0, 5), 6);
        // Second line, char col 2 ('r'): 6 bytes + '\n' + "wö" (3 bytes) = 10.
        assert_eq!(byte_offset(&lines, 1, 2), 10);

        // Round-trips both directions for every valid position.
        for (row, line) in lines.iter().enumerate() {
            for col in 0..=line.chars().count() {
                let off = byte_offset(&lines, row, col);
                assert_eq!(position(&lines, off), (row, col), "row {row} col {col}");
            }
        }
    }

    /// The join text `byte_offset` addresses is exactly what `CollabDoc` holds,
    /// so an editor edit expressed in (row, col) integrates at the right place.
    #[test]
    fn editor_position_edit_integrates_into_collabdoc() {
        let lines = vec![String::from("foo"), String::from("bar")];
        let joined = lines.join("\n");
        let mut a = CollabDoc::new(1, &joined);
        let mut b = a.fork(2);

        // Insert "X" at editor position (row 1, col 0) = start of "bar".
        let at = byte_offset(&lines, 1, 0);
        let op = a.local_insert(at, "X");
        b.apply_remote(&op);

        assert_eq!(a.text(), "foo\nXbar");
        assert_eq!(b.text(), a.text());
    }
}
