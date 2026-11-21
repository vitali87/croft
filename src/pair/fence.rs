//! The fenced edit protocol's parser: the fence markers, the events the machine resolves
//! from streamed text, the machine itself and its header parsers.

/// Start of an edit-fence header line.
pub(super) const EDIT_MARKER: &str = "<<<EDIT ";

/// Start of a note-fence header line (anchored commentary, never applied).
pub(super) const NOTE_MARKER: &str = "<<<NOTE ";

/// The whole terminator line.
pub(super) const END_MARKER: &str = "<<<END>>>";

/// What the fence machine resolved from a run of streamed text deltas, in
/// stream order. Only text between a well-formed `EditStart` and its
/// `EditEnd` ever touches a buffer.
#[derive(Debug)]
pub enum FenceEvent {
    /// Model prose outside any fence (including malformed fences): printed
    /// to the pilot's terminal, never applied.
    Commentary(String),
    /// A well-formed header opened an edit: replace the 0-based char range
    /// `start..end` of `file` with the body that streams next.
    EditStart {
        file: String,
        start: (usize, usize),
        end: (usize, usize),
    },
    /// The next fragment of the current fence's body, in stream order.
    EditBody(String),
    /// The fence closed cleanly; the streamed body is the whole replacement.
    EditEnd,
    /// The turn ended mid-fence (risk R2): the pilot reverts whatever body
    /// already streamed in.
    EditAbort,
    /// A well-formed header opened a note anchored to the 0-based `row` of
    /// `file`: commentary tied to a line, accumulated and surfaced whole at
    /// `NoteEnd`, never applied to any buffer.
    NoteStart { file: String, row: usize },
    /// The next fragment of the current note's body, in stream order.
    NoteBody(String),
    /// The note closed cleanly; its accumulated body may anchor.
    NoteEnd,
    /// The turn ended mid-note: nothing anchors.
    NoteAbort,
}

/// Where the machine is between pushes.
pub(super) enum FenceState {
    /// Outside a fence: complete lines classify as header or commentary.
    Outside,
    /// Inside a fence body. `held_newline`: a body '\n' was consumed but not
    /// emitted (it may turn out to be the newline that separates the body
    /// from `<<<END>>>`, which is stripped). `at_line_start`: the unprocessed
    /// tail starts a fresh line, so it could still become the END marker.
    /// `is_note`: the fence opened with NOTE, so body/end/abort events use
    /// the note variants (same scanner, different consumer semantics).
    Body {
        held_newline: bool,
        at_line_start: bool,
        is_note: bool,
    },
}

/// Incremental parser for the fenced edit protocol. Fed `text_delta`
/// fragments split at arbitrary points; emits [`FenceEvent`]s as soon as
/// they are unambiguous, so a fence body streams token by token (the whole
/// point of `croft pair`). Call [`finish`](Self::finish) at end of turn.
pub struct FenceMachine {
    buf: String,
    state: FenceState,
}

impl FenceMachine {
    pub fn new() -> Self {
        Self {
            buf: String::new(),
            state: FenceState::Outside,
        }
    }

    /// Feed one streamed fragment; returns every event it completed.
    pub fn push(&mut self, delta: &str) -> Vec<FenceEvent> {
        self.buf.push_str(delta);
        let mut events = Vec::new();
        loop {
            match self.state {
                FenceState::Outside => {
                    let Some(nl) = self.buf.find('\n') else {
                        break;
                    };
                    let line: String = self.buf.drain(..=nl).collect();
                    let trimmed = &line[..line.len() - 1];
                    if let Some(start) = parse_header(trimmed) {
                        events.push(start);
                        self.state = FenceState::Body {
                            held_newline: false,
                            at_line_start: true,
                            is_note: false,
                        };
                    } else if let Some(start) = parse_note_header(trimmed) {
                        events.push(start);
                        self.state = FenceState::Body {
                            held_newline: false,
                            at_line_start: true,
                            is_note: true,
                        };
                    } else {
                        events.push(FenceEvent::Commentary(line));
                    }
                }
                FenceState::Body {
                    ref mut held_newline,
                    ref mut at_line_start,
                    is_note,
                } => {
                    let body_event = if is_note {
                        FenceEvent::NoteBody
                    } else {
                        FenceEvent::EditBody
                    };
                    if *at_line_start {
                        match self.buf.find('\n') {
                            Some(nl) => {
                                if &self.buf[..nl] == END_MARKER {
                                    // The held newline separated body from
                                    // the marker: stripped, not body.
                                    self.buf.drain(..=nl);
                                    events.push(if is_note {
                                        FenceEvent::NoteEnd
                                    } else {
                                        FenceEvent::EditEnd
                                    });
                                    self.state = FenceState::Outside;
                                } else {
                                    // A complete body line: the held newline
                                    // is confirmed body, the line's own
                                    // newline is held in its place.
                                    let mut body = String::new();
                                    if *held_newline {
                                        body.push('\n');
                                    }
                                    body.push_str(&self.buf[..nl]);
                                    self.buf.drain(..=nl);
                                    events.push(body_event(body));
                                    *held_newline = true;
                                }
                            }
                            None => {
                                if END_MARKER.starts_with(self.buf.as_str()) {
                                    // Could still become the terminator (or
                                    // is it, pending its newline): wait.
                                    break;
                                }
                                // Provably not the marker: stream it now.
                                let mut body = String::new();
                                if *held_newline {
                                    body.push('\n');
                                    *held_newline = false;
                                }
                                body.push_str(&self.buf);
                                self.buf.clear();
                                events.push(body_event(body));
                                *at_line_start = false;
                                break;
                            }
                        }
                    } else {
                        match self.buf.find('\n') {
                            Some(nl) => {
                                if nl > 0 {
                                    events.push(body_event(self.buf[..nl].to_string()));
                                }
                                self.buf.drain(..=nl);
                                *held_newline = true;
                                *at_line_start = true;
                            }
                            None => {
                                if !self.buf.is_empty() {
                                    events.push(body_event(std::mem::take(&mut self.buf)));
                                }
                                break;
                            }
                        }
                    }
                }
            }
        }
        events
    }

    /// End of turn: flush leftovers. Outside a fence the tail is commentary;
    /// a bare END marker with no trailing newline still closes the fence;
    /// anything else mid-fence aborts it (the pilot reverts). The machine is
    /// reset for the next turn either way.
    pub fn finish(&mut self) -> Vec<FenceEvent> {
        let mut events = Vec::new();
        match self.state {
            FenceState::Outside => {
                if !self.buf.is_empty() {
                    let mut tail = std::mem::take(&mut self.buf);
                    // Only the unterminated LAST line can be a fence header.
                    // A VALID one is protocol residue (a fence the turn cut
                    // off before its newline) and is dropped like an
                    // unterminated body; malformed ones stay commentary,
                    // the malformed-header rule.
                    let last_start = tail.rfind('\n').map_or(0, |i| i + 1);
                    let line = tail[last_start..].trim_end_matches('\r');
                    if parse_header(line).is_some() || parse_note_header(line).is_some() {
                        tail.truncate(last_start);
                    }
                    if !tail.is_empty() {
                        events.push(FenceEvent::Commentary(tail));
                    }
                }
            }
            FenceState::Body {
                at_line_start,
                is_note,
                ..
            } => {
                if at_line_start && self.buf == END_MARKER {
                    events.push(if is_note {
                        FenceEvent::NoteEnd
                    } else {
                        FenceEvent::EditEnd
                    });
                } else if is_note {
                    events.push(FenceEvent::NoteAbort);
                } else {
                    events.push(FenceEvent::EditAbort);
                }
                self.buf.clear();
            }
        }
        self.state = FenceState::Outside;
        events
    }
}

/// Parse an EDIT header into its [`FenceEvent::EditStart`]. Two forms:
/// four integers `<file>:SR:SC-ER:EC` (character range), or two integers
/// `<file>:SR-ER` (whole rows, inclusive — start column 0, end column
/// `usize::MAX`, which [`crate::collab::byte_offset`] clamps to the end of
/// row ER). Coordinates bind rightmost so the file name may itself contain
/// ':' or '-'. None = not a well-formed header (the line degrades to
/// commentary).
pub(super) fn parse_header(line: &str) -> Option<FenceEvent> {
    let inner = line.strip_prefix(EDIT_MARKER)?.strip_suffix(">>>")?;
    parse_char_range_header(inner).or_else(|| parse_whole_line_header(inner))
}

/// The four-int form: `<file>:SR:SC-ER:EC`.
pub(super) fn parse_char_range_header(inner: &str) -> Option<FenceEvent> {
    let (rest, ec) = inner.rsplit_once(':')?;
    let (rest, mid) = rest.rsplit_once(':')?;
    let (sc, er) = mid.split_once('-')?;
    let (file, sr) = rest.rsplit_once(':')?;
    if file.is_empty() {
        return None;
    }
    Some(FenceEvent::EditStart {
        file: file.to_string(),
        start: (sr.parse().ok()?, sc.parse().ok()?),
        end: (er.parse().ok()?, ec.parse().ok()?),
    })
}

/// The two-int whole-line form: `<file>:SR-ER`, both rows inclusive. A file
/// part that itself ends in `:<digits>` is a TRUNCATED four-int header
/// (`file:SR:SC-ER` missing its end column), not a path — rejected so it
/// degrades to commentary instead of editing a bogus file.
pub(super) fn parse_whole_line_header(inner: &str) -> Option<FenceEvent> {
    let (file, range) = inner.rsplit_once(':')?;
    let (sr, er) = range.split_once('-')?;
    if file.is_empty() {
        return None;
    }
    if let Some((_, tail)) = file.rsplit_once(':')
        && !tail.is_empty()
        && tail.chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    Some(FenceEvent::EditStart {
        file: file.to_string(),
        start: (sr.parse().ok()?, 0),
        end: (er.parse().ok()?, usize::MAX),
    })
}

/// Parse `<<<NOTE <file>:<row>>>>` into its [`FenceEvent::NoteStart`]. The
/// row binds rightmost so the file name may itself contain ':'. None = not
/// a well-formed header (the line degrades to commentary).
pub(super) fn parse_note_header(line: &str) -> Option<FenceEvent> {
    let inner = line.strip_prefix(NOTE_MARKER)?.strip_suffix(">>>")?;
    let (file, row) = inner.rsplit_once(':')?;
    if file.is_empty() {
        return None;
    }
    Some(FenceEvent::NoteStart {
        file: file.to_string(),
        row: row.parse().ok()?,
    })
}
