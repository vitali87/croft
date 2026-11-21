//! Byte offsets into a file's replica text: a fence's range and a note's row as offsets,
//! and keeping a tracked offset true through other participants' edits.

use super::*;

/// A fence range as byte offsets into the joined ('\n'-separated) text of
/// `lines`, via the shared char-coordinate bridge (risk R6: columns are
/// chars, offsets are bytes; this is the only conversion path).
pub fn range_bytes(lines: &[String], start: (usize, usize), end: (usize, usize)) -> (usize, usize) {
    (
        crate::collab::byte_offset(lines, start.0, start.1),
        crate::collab::byte_offset(lines, end.0, end.1),
    )
}

/// Byte offset of the start of `row` in `lines`, clamped to the last line
/// (models sometimes cite a row just past EOF).
pub(super) fn note_offset(lines: &[String], row: usize) -> usize {
    let row = row.min(lines.len().saturating_sub(1));
    crate::collab::byte_offset(lines, row, 0)
}

/// Replay remote-edit spans over every note anchored in `file`, keeping
/// their offsets true while other participants type.
pub(super) fn shift_notes(notes: &mut [Note], file: &str, spans: &[ResolvedSpan]) {
    for n in notes.iter_mut().filter(|n| n.file == file) {
        for span in spans {
            n.offset = shift_offset(n.offset, span);
        }
    }
}

/// Transform a tracked byte offset through one remote edit span, the same
/// sequential replay the buffers do: a span entirely before the offset
/// shifts it by the size delta (an insert exactly at the offset counts as
/// before, pushing it right); a span straddling it clamps to the span's new
/// end; a span after it leaves it alone.
pub fn shift_offset(x: usize, span: &ResolvedSpan) -> usize {
    if span.at + span.deleted <= x {
        // No underflow: span.deleted <= x on this branch.
        x + span.inserted.len() - span.deleted
    } else if span.at < x {
        span.at + span.inserted.len()
    } else {
        x
    }
}
