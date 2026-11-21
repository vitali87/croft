//! The navigator's anchored notes: listing them, dropping one or all, replying in one, and
//! landing one from the app side.

use super::*;

/// One anchored navigator note: a byte offset into `file`'s replica text
/// (kept fresh against concurrent edits exactly like the stream region)
/// plus the accumulated body.
pub struct Note {
    /// Stable identity for the note's comment box: Ignore removes exactly
    /// this note, a reply appends to exactly this note. Monotonic per seat.
    pub id: u64,
    pub file: String,
    pub offset: usize,
    pub body: String,
}

impl PairState {
    /// The anchored notes sitting in `file`, in landing order.
    pub(crate) fn notes_in<'a>(&'a self, file: &'a str) -> impl Iterator<Item = &'a Note> {
        self.notes.iter().filter(move |n| n.file == file)
    }

    /// Drop every note in every file (the user asked for a clean slate).
    pub(crate) fn clear_all_notes(&mut self) {
        self.notes.clear();
    }

    /// Drop exactly one note (the driver ignored its comment box).
    pub(crate) fn remove_note(&mut self, id: u64) {
        self.notes.retain(|n| n.id != id);
    }

    /// Append one line to a note's body (the driver replied in its box, so
    /// the box keeps the running conversation).
    pub(crate) fn append_to_note(&mut self, id: u64, line: &str) {
        if let Some(n) = self.notes.iter_mut().find(|n| n.id == id) {
            n.body.push('\n');
            n.body.push_str(line);
        }
    }

    /// The next note id (monotonic within the seat).
    pub(super) fn take_note_id(&mut self) -> u64 {
        let id = self.next_note_id;
        self.next_note_id += 1;
        id
    }
}

/// Anchor a note into `file` at `row` from the app side (commentary landing
/// as a comment box). Mirrors the NoteEnd path: request the file, give the
/// bootstrap the same bounded wait, then anchor by byte offset. None = the
/// owner never served the file.
/// Test-only: the e2e suite anchors seed notes right after a spawn, before
/// the bootstrap settles; production (the App tick thread) must never block
/// and uses [`inject_note_now`].
#[cfg(test)]
pub(crate) fn inject_note(
    state: &Mutex<PairState>,
    file: &str,
    row: usize,
    body: &str,
) -> Option<u64> {
    state.lock().unwrap().session.request_file(file);
    if !wait_live(state, file) {
        return None;
    }
    inject_note_now(state, file, row, body)
}

/// Non-blocking [`inject_note`]: anchors only when `file` is already live.
/// The App's tick thread lands turn commentary through this — it must never
/// sit out a bootstrap wait.
pub(crate) fn inject_note_now(
    state: &Mutex<PairState>,
    file: &str,
    row: usize,
    body: &str,
) -> Option<u64> {
    // Bounded lock (see lock_briefly): the App's tick thread must never
    // wait indefinitely on the pilot mutex (held across blocking relay
    // writes). None sends the caller to its OUTPUT fallback.
    let mut st = lock_briefly(state)?;
    if !st.session.is_live(file) {
        return None;
    }
    let lines = st.doc_lines(file)?;
    let offset = note_offset(&lines, row);
    // The AI is visibly "looking" where it just commented.
    st.caret_now(file, position(&lines, offset).0, 0);
    let id = st.take_note_id();
    st.notes.push(Note {
        id,
        file: file.to_string(),
        offset,
        body: body.to_string(),
    });
    Some(id)
}
