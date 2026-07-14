//! `croft pair`: the AI pilot that streams a Claude conversation's tokens
//! straight into the collab seat, so a co-editing session sees the edit
//! arrive token by token instead of as one bulk insert (docs/MULTIPLAYER.md,
//! "croft pair").
//!
//! This module's pure core is the fence machine: the model is taught (via
//! the pair system prompt) to wrap edits in a fenced protocol inside its
//! streamed TEXT output:
//!
//! ```text
//! <<<EDIT file:START_ROW:START_COL-END_ROW:END_COL>>>
//! <replacement text>
//! <<<END>>>
//! ```
//!
//! Coordinates are 0-based CHARACTER positions against the buffer text the
//! pilot injected into the turn; byte offsets only ever come from
//! [`crate::collab::byte_offset`] (never treat a column as bytes). The
//! machine is fed raw `text_delta` fragments split at arbitrary points and
//! emits events; everything outside a well-formed fence is commentary and is
//! never applied to a buffer.

use crate::collab::ResolvedSpan;

/// Start of an edit-fence header line.
const EDIT_MARKER: &str = "<<<EDIT ";
/// The whole terminator line.
const END_MARKER: &str = "<<<END>>>";

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
}

/// Where the machine is between pushes.
enum FenceState {
    /// Outside a fence: complete lines classify as header or commentary.
    Outside,
    /// Inside a fence body. `held_newline`: a body '\n' was consumed but not
    /// emitted (it may turn out to be the newline that separates the body
    /// from `<<<END>>>`, which is stripped). `at_line_start`: the unprocessed
    /// tail starts a fresh line, so it could still become the END marker.
    Body {
        held_newline: bool,
        at_line_start: bool,
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
                        };
                    } else {
                        events.push(FenceEvent::Commentary(line));
                    }
                }
                FenceState::Body {
                    ref mut held_newline,
                    ref mut at_line_start,
                } => {
                    if *at_line_start {
                        match self.buf.find('\n') {
                            Some(nl) => {
                                if &self.buf[..nl] == END_MARKER {
                                    // The held newline separated body from
                                    // the marker: stripped, not body.
                                    self.buf.drain(..=nl);
                                    events.push(FenceEvent::EditEnd);
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
                                    events.push(FenceEvent::EditBody(body));
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
                                events.push(FenceEvent::EditBody(body));
                                *at_line_start = false;
                                break;
                            }
                        }
                    } else {
                        match self.buf.find('\n') {
                            Some(nl) => {
                                if nl > 0 {
                                    events.push(FenceEvent::EditBody(self.buf[..nl].to_string()));
                                }
                                self.buf.drain(..=nl);
                                *held_newline = true;
                                *at_line_start = true;
                            }
                            None => {
                                if !self.buf.is_empty() {
                                    events
                                        .push(FenceEvent::EditBody(std::mem::take(&mut self.buf)));
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
                    events.push(FenceEvent::Commentary(std::mem::take(&mut self.buf)));
                }
            }
            FenceState::Body { at_line_start, .. } => {
                if at_line_start && self.buf == END_MARKER {
                    events.push(FenceEvent::EditEnd);
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

/// Parse `<<<EDIT <file>:SR:SC-ER:EC>>>` into its [`FenceEvent::EditStart`].
/// Coordinates bind rightmost so the file name may itself contain ':' or
/// '-'. None = not a well-formed header (the line degrades to commentary).
fn parse_header(line: &str) -> Option<FenceEvent> {
    let inner = line.strip_prefix(EDIT_MARKER)?.strip_suffix(">>>")?;
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

/// A fence range as byte offsets into the joined ('\n'-separated) text of
/// `lines`, via the shared char-coordinate bridge (risk R6: columns are
/// chars, offsets are bytes; this is the only conversion path).
pub fn range_bytes(lines: &[String], start: (usize, usize), end: (usize, usize)) -> (usize, usize) {
    (
        crate::collab::byte_offset(lines, start.0, start.1),
        crate::collab::byte_offset(lines, end.0, end.1),
    )
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collab::ResolvedSpan;

    /// Feed `input` to a fresh machine in `chunk`-sized pieces plus finish,
    /// collecting every event.
    fn run_chunks(input: &str, chunk: usize) -> Vec<FenceEvent> {
        let mut m = FenceMachine::new();
        let mut events = Vec::new();
        let chars: Vec<char> = input.chars().collect();
        for piece in chars.chunks(chunk) {
            let s: String = piece.iter().collect();
            events.extend(m.push(&s));
        }
        events.extend(m.finish());
        events
    }

    /// Concatenated body text of every EditBody event.
    fn body_of(events: &[FenceEvent]) -> String {
        events
            .iter()
            .filter_map(|e| match e {
                FenceEvent::EditBody(s) => Some(s.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Concatenated commentary of every Commentary event.
    fn commentary_of(events: &[FenceEvent]) -> String {
        events
            .iter()
            .filter_map(|e| match e {
                FenceEvent::Commentary(s) => Some(s.as_str()),
                _ => None,
            })
            .collect()
    }

    /// The core streaming property: a full fenced edit parses identically no
    /// matter how the deltas split it, down to one char at a time, and the
    /// body arrives across multiple EditBody events (streaming), not one.
    #[test]
    fn fence_parses_identically_across_arbitrary_delta_splits() {
        let input = "I will fix the loop.\n\
                     <<<EDIT src/f.rs:3:0-5:10>>>\n\
                     for x in xs {\n    go(x);\n}\n\
                     <<<END>>>\n\
                     Done.\n";
        for chunk in [1, 2, 3, 7, input.len()] {
            let events = run_chunks(input, chunk);
            let start = events
                .iter()
                .find_map(|e| match e {
                    FenceEvent::EditStart { file, start, end } => {
                        Some((file.clone(), *start, *end))
                    }
                    _ => None,
                })
                .unwrap_or_else(|| panic!("no EditStart at chunk {chunk}"));
            assert_eq!(
                start,
                ("src/f.rs".to_string(), (3, 0), (5, 10)),
                "chunk {chunk}"
            );
            assert_eq!(
                body_of(&events),
                "for x in xs {\n    go(x);\n}",
                "chunk {chunk}"
            );
            assert_eq!(
                events
                    .iter()
                    .filter(|e| matches!(e, FenceEvent::EditEnd))
                    .count(),
                1,
                "chunk {chunk}"
            );
            let commentary = commentary_of(&events);
            assert!(
                commentary.contains("I will fix the loop."),
                "chunk {chunk}: {commentary:?}"
            );
            assert!(commentary.contains("Done."), "chunk {chunk}");
            assert!(
                !commentary.contains("<<<EDIT"),
                "header must not leak into commentary at chunk {chunk}"
            );
        }
        // Char-at-a-time must still stream the body incrementally.
        let events = run_chunks(input, 1);
        let bodies = events
            .iter()
            .filter(|e| matches!(e, FenceEvent::EditBody(_)))
            .count();
        assert!(bodies > 1, "body must stream, got {bodies} event(s)");
    }

    /// A header that fails to parse is commentary, never an edit.
    #[test]
    fn malformed_headers_become_commentary() {
        for bad in [
            "<<<EDIT src/f.rs:3:0-5>>>\nbody\n<<<END>>>\n",
            "<<<EDIT src/f.rs>>>\nbody\n<<<END>>>\n",
            "<<<EDIT src/f.rs:a:0-5:10>>>\nbody\n<<<END>>>\n",
            "<<<EDIT>>>\nbody\n<<<END>>>\n",
        ] {
            let events = run_chunks(bad, 5);
            assert!(
                !events
                    .iter()
                    .any(|e| matches!(e, FenceEvent::EditStart { .. })),
                "{bad:?} must not start an edit"
            );
            assert!(
                commentary_of(&events).contains("body"),
                "{bad:?}: the un-fenced body is commentary"
            );
        }
    }

    /// Exactly one trailing newline is stripped: the one that separates the
    /// body from the END marker. A body that ends in a blank line keeps it.
    #[test]
    fn end_marker_strips_exactly_one_trailing_newline() {
        let input = "<<<EDIT f:0:0-0:0>>>\nabc\n\n<<<END>>>\n";
        for chunk in [1, 4, input.len()] {
            assert_eq!(body_of(&run_chunks(input, chunk)), "abc\n", "chunk {chunk}");
        }
    }

    /// A body line that merely contains the END marker text with a suffix is
    /// body, not a terminator.
    #[test]
    fn end_marker_with_trailing_chars_is_body() {
        let input = "<<<EDIT f:0:0-0:0>>>\nkeep <<<END>>>ish\n<<<END>>>\n";
        for chunk in [1, 3, input.len()] {
            assert_eq!(
                body_of(&run_chunks(input, chunk)),
                "keep <<<END>>>ish",
                "chunk {chunk}"
            );
        }
    }

    /// Two fences in one turn each produce their own start/body/end, with
    /// the commentary between them intact.
    #[test]
    fn multiple_fences_in_one_turn() {
        let input = "<<<EDIT a:0:0-0:0>>>\nfirst\n<<<END>>>\n\
                     between\n\
                     <<<EDIT b:1:2-3:4>>>\nsecond\n<<<END>>>\n";
        let events = run_chunks(input, 2);
        let starts: Vec<String> = events
            .iter()
            .filter_map(|e| match e {
                FenceEvent::EditStart { file, .. } => Some(file.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(starts, ["a", "b"]);
        assert_eq!(body_of(&events), "firstsecond");
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, FenceEvent::EditEnd))
                .count(),
            2
        );
        assert!(commentary_of(&events).contains("between"));
    }

    /// The model stopping mid-fence (turn ends before <<<END>>>) aborts the
    /// edit so the pilot can revert what already streamed in.
    #[test]
    fn unterminated_fence_aborts_at_finish() {
        let mut m = FenceMachine::new();
        let mut events = m.push("<<<EDIT f:0:0-0:0>>>\npartial body");
        events.extend(m.finish());
        assert!(
            events.iter().any(|e| matches!(e, FenceEvent::EditAbort)),
            "{events:?}"
        );
        // The machine is reusable for the next turn afterwards.
        let events = {
            let mut e = m.push("plain text\n");
            e.extend(m.finish());
            e
        };
        assert!(commentary_of(&events).contains("plain text"));
    }

    /// An END marker arriving exactly at end-of-turn (no trailing newline)
    /// still closes the fence.
    #[test]
    fn end_marker_at_end_of_turn_closes_the_fence() {
        let mut m = FenceMachine::new();
        let mut events = m.push("<<<EDIT f:0:0-0:0>>>\nbody\n<<<END>>>");
        events.extend(m.finish());
        assert!(events.iter().any(|e| matches!(e, FenceEvent::EditEnd)));
        assert_eq!(body_of(&events), "body");
    }

    /// File names with colons and dashes still parse (coords bind rightmost).
    #[test]
    fn header_files_with_colons_and_dashes_parse() {
        let input = "<<<EDIT a-b:c.rs:10:2-11:0>>>\nx\n<<<END>>>\n";
        let events = run_chunks(input, input.len());
        match events
            .iter()
            .find(|e| matches!(e, FenceEvent::EditStart { .. }))
        {
            Some(FenceEvent::EditStart { file, start, end }) => {
                assert_eq!(file, "a-b:c.rs");
                assert_eq!((*start, *end), ((10, 2), (11, 0)));
            }
            other => panic!("expected EditStart, got {other:?}"),
        }
    }

    /// Anchor-shift math: spans entirely before an offset shift it by the
    /// span's size delta; spans straddling it clamp to the span's new end;
    /// spans after leave it alone.
    #[test]
    fn shift_offset_handles_before_straddle_and_after() {
        // Insert of 3 bytes at 2, entirely before offset 10.
        let ins = ResolvedSpan {
            at: 2,
            deleted: 0,
            inserted: "abc".into(),
        };
        assert_eq!(shift_offset(10, &ins), 13);
        // Delete of 4 bytes ending exactly at the offset: still "before".
        let del = ResolvedSpan {
            at: 6,
            deleted: 4,
            inserted: String::new(),
        };
        assert_eq!(shift_offset(10, &del), 6);
        // Straddle: delete [8, 14) around offset 10 with 1 byte inserted.
        let straddle = ResolvedSpan {
            at: 8,
            deleted: 6,
            inserted: "Z".into(),
        };
        assert_eq!(shift_offset(10, &straddle), 9);
        // Entirely after: change at the offset itself or beyond.
        let after = ResolvedSpan {
            at: 10,
            deleted: 3,
            inserted: "wxyz".into(),
        };
        assert_eq!(shift_offset(10, &after), 10);
        let insert_at = ResolvedSpan {
            at: 10,
            deleted: 0,
            inserted: "Q".into(),
        };
        // An insert exactly at the offset counts as before (pushes it right).
        assert_eq!(shift_offset(10, &insert_at), 11);
    }

    /// A sequence of spans replays through both region offsets like the
    /// pilot's pump does, keeping start <= anchor.
    #[test]
    fn shift_offset_sequences_track_a_region() {
        let mut start = 20usize;
        let mut anchor = 30usize;
        let spans = [
            // 5 bytes inserted at 0: both shift right.
            ResolvedSpan {
                at: 0,
                deleted: 0,
                inserted: "aaaaa".into(),
            },
            // Delete [40, 45): after both, no change.
            ResolvedSpan {
                at: 40,
                deleted: 5,
                inserted: String::new(),
            },
            // Delete [23, 28): straddles start (clamps it to the span's new
            // end, 23) and sits entirely before anchor (shifts it left 5).
            ResolvedSpan {
                at: 23,
                deleted: 5,
                inserted: String::new(),
            },
        ];
        for span in &spans {
            start = shift_offset(start, span);
            anchor = shift_offset(anchor, span);
        }
        assert_eq!(start, 23);
        assert_eq!(anchor, 30);
        assert!(start <= anchor);
    }

    /// The char-coordinate bridge: fence coords convert to byte offsets via
    /// collab::byte_offset, including multibyte lines.
    #[test]
    fn fence_range_converts_to_byte_offsets() {
        let lines: Vec<String> = ["let s = \"héllo\";", "next"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (start, end) = range_bytes(&lines, (0, 10), (1, 2));
        // 'é' is 2 bytes: char col 10 sits after the quote + h + é.
        assert_eq!(start, crate::collab::byte_offset(&lines, 0, 10));
        assert_eq!(end, crate::collab::byte_offset(&lines, 1, 2));
        assert!(start < end);
    }
}
