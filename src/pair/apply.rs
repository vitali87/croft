//! Applying the stream to the collab seat: waiting for a file to go live, applying each
//! fence event, opening and reverting the streamed region, and the pump tick that keeps it
//! true against the relay.

use super::*;

/// How long an EditStart waits for its file to bootstrap, a hair past the
/// session's own 3s deadline (same reasoning as the collab agent's).
pub(super) const LIVE_TIMEOUT: Duration = Duration::from_secs(4);

/// The fence currently streaming into a buffer: byte offsets into the
/// replica text (kept fresh against concurrent remote edits by
/// [`shift_offset`]) plus the slice the fence's range deleted, for revert.
pub(super) struct StreamRegion {
    file: String,
    start: usize,
    anchor: usize,
    original: String,
    /// Notes whose anchors sat at or inside the fence's range when it
    /// opened, as `(note id, offset - start)`: the delete collapses them
    /// and the streamed inserts drag them along, so the revert can only
    /// restore them from these recorded deltas (`start` keeps tracking
    /// remote edits, so `start + delta` stays exact in the restored text).
    displaced: Vec<(u64, usize)>,
}

/// Bounded wait for a requested file's bootstrap ([`LIVE_TIMEOUT`]). True
/// when the file went live; false when nobody answered (no owner).
pub(super) fn wait_live(state: &Mutex<PairState>, file: &str) -> bool {
    let deadline = Instant::now() + LIVE_TIMEOUT;
    loop {
        let st = state.lock().unwrap();
        if st.session.is_live(file) {
            return true;
        }
        if !st.session.is_bootstrapping(file) || Instant::now() >= deadline {
            return false;
        }
        drop(st);
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Apply one fence event to the collab seat. Commentary prints; edits apply
/// at the tracked anchor; abort reverts. Never holds the state lock across
/// the EditStart bootstrap wait.
pub(super) fn apply_fence_event(state: &Mutex<PairState>, event: FenceEvent) {
    match event {
        FenceEvent::Commentary(text) => {
            say(state, &text);
        }
        FenceEvent::EditStart { file, start, end } => {
            {
                let mut st = state.lock().unwrap();
                if st.cancelled {
                    st.discarding = true;
                    return;
                }
                if st.comment_only {
                    // The host gate, not just a prompt rule: a yielded turn
                    // may never touch a buffer, whatever the model says.
                    st.discarding = true;
                    drop(st);
                    diag(
                        state,
                        crate::output::OutputLevel::Warn,
                        "[pair] edit suppressed: this is a yielded, comment-only turn",
                    );
                    return;
                }
                st.target_file = Some(file.clone());
                st.session.request_file(&file);
            }
            if wait_live(state, &file) {
                let mut st = state.lock().unwrap();
                open_region(&mut st, &file, start, end);
            } else {
                state.lock().unwrap().discarding = true;
                diag(
                    state,
                    crate::output::OutputLevel::Warn,
                    &format!(
                        "[pair] no live croft session serves {file}; edit dropped \
                         (start croft in this workspace first)"
                    ),
                );
            }
        }
        FenceEvent::EditBody(delta) => {
            let mut st = state.lock().unwrap();
            if st.cancelled || st.discarding {
                return;
            }
            let Some((file, anchor)) = st.region.as_ref().map(|r| (r.file.clone(), r.anchor))
            else {
                return;
            };
            let Some(doc) = st.session.doc_text(&file).map(str::to_string) else {
                return;
            };
            let anchor = anchor.min(doc.len());
            let new = format!("{}{}{}", &doc[..anchor], delta, &doc[anchor..]);
            st.session.local_change(&file, &new);
            // Our own insert moves any note anchored below it in this file
            // (notes and edits can both land in one ask turn).
            let span = ResolvedSpan {
                at: anchor,
                deleted: 0,
                inserted: delta.clone(),
            };
            shift_notes(&mut st.notes, &file, std::slice::from_ref(&span));
            let next = anchor + delta.len();
            if let Some(r) = st.region.as_mut() {
                r.anchor = next;
            }
            let lines: Vec<String> = new.split('\n').map(String::from).collect();
            let (row, col) = position(&lines, next);
            st.caret_now(&file, row, col);
        }
        FenceEvent::EditEnd => {
            let mut st = state.lock().unwrap();
            st.discarding = false;
            if st.cancelled {
                return; // already reverted by the cancel
            }
            if let Some(r) = st.region.take() {
                st.session.send_stream_state(&r.file, false);
            }
        }
        FenceEvent::EditAbort => {
            let mut st = state.lock().unwrap();
            st.discarding = false;
            if st.cancelled {
                st.region = None;
                return;
            }
            revert_region(&mut st);
        }
        FenceEvent::NoteStart { file, row } => {
            let mut st = state.lock().unwrap();
            if st.cancelled {
                return;
            }
            // Non-blocking: the offset resolves lazily at NoteEnd, so a
            // bootstrap kicked off here has the body's stream time to land.
            st.session.request_file(&file);
            st.note_in_flight = Some((file, row, String::new()));
        }
        FenceEvent::NoteBody(delta) => {
            let mut st = state.lock().unwrap();
            if let Some((_, _, body)) = st.note_in_flight.as_mut() {
                body.push_str(&delta);
            }
        }
        FenceEvent::NoteEnd => {
            let pending = {
                let mut st = state.lock().unwrap();
                let pending = st.note_in_flight.take();
                if st.cancelled { None } else { pending }
            };
            let Some((file, row, body)) = pending else {
                return;
            };
            // NoteStart's request_file may still be bootstrapping (a fast
            // fence outruns the 25ms pump); give it the same bounded wait
            // an edit gets. Still no live doc = the owner never served the
            // file: nothing to anchor to (mirrors the edit-drop path).
            if !wait_live(state, &file) {
                diag(
                    state,
                    crate::output::OutputLevel::Warn,
                    &format!(
                        "[pair] no live croft session serves {file}; note dropped \
                         (start croft in this workspace first)"
                    ),
                );
                return;
            }
            let mut st = state.lock().unwrap();
            let Some(lines) = st.doc_lines(&file) else {
                // The file left the live session between wait_live releasing
                // the lock and this reacquisition: same outcome, same warning.
                drop(st);
                diag(
                    state,
                    crate::output::OutputLevel::Warn,
                    &format!(
                        "[pair] no live croft session serves {file}; note dropped \
                         (start croft in this workspace first)"
                    ),
                );
                return;
            };
            let offset = note_offset(&lines, row);
            let row_now = position(&lines, offset).0;
            // The AI is visibly "looking" where it just commented.
            st.caret_now(&file, row_now, 0);
            let id = st.take_note_id();
            let sink = st.events.clone();
            // Pushed under the SAME lock as the offset computation: pushing
            // after the REPL's say() left a window where a concurrent
            // remote edit shifted every note EXCEPT this not-yet-pushed
            // one, stranding its anchor below the edit.
            st.notes.push(Note {
                id,
                file: file.clone(),
                offset,
                body: body.clone(),
            });
            match sink {
                Some(tx) => {
                    let _ = tx.send(crate::pair_host::PairEvent::NoteAdded {
                        file,
                        row: row_now,
                        body,
                    });
                }
                None => {
                    // REPL driver: no event channel, so print the note or it
                    // would be silently swallowed (say re-locks internally,
                    // hence the drop).
                    drop(st);
                    say(state, &format!("[note {file}:{}] {body}\n", row_now + 1));
                }
            }
        }
        FenceEvent::NoteAbort => {
            state.lock().unwrap().note_in_flight = None;
        }
    }
}

/// EditStart's second half, once the file is live: delete the fence's range
/// in one replica change and open the streamed region at its start.
pub(super) fn open_region(
    st: &mut PairState,
    file: &str,
    start: (usize, usize),
    end: (usize, usize),
) {
    let Some(doc) = st.session.doc_text(file).map(str::to_string) else {
        st.discarding = true;
        return;
    };
    let lines: Vec<String> = doc.split('\n').map(String::from).collect();
    let (s, e) = range_bytes(&lines, start, end);
    let (s, e) = (s.min(doc.len()), e.min(doc.len()));
    if s > e {
        st.discarding = true;
        // The OUTPUT channel, never stderr: the resident navigator runs
        // in-process, so an eprintln! lands on the alternate screen and
        // corrupts the render (the pdftoppm/DAP capture class). The
        // whole-line header form makes an inverted range a one-token slip
        // for a weak local model.
        crate::output::push(
            "Navigator",
            crate::output::OutputLevel::Warn,
            "fence range is inverted; edit dropped",
        );
        return;
    }
    let original = doc[s..e].to_string();
    let new = format!("{}{}", &doc[..s], &doc[e..]);
    // Announce the stream before its first edit, the region's delete: the
    // host groups the stream's edits into one undo step from the moment it
    // sees the announcement, so a delete sent first was a step of its own
    // (#1207).
    st.session.send_stream_state(file, true);
    st.session.local_change(file, &new);
    // Anchors at or inside the range are about to be collapsed by the
    // delete (and then dragged along by the streamed inserts): record
    // their pre-edit deltas so a revert can put them back exactly.
    let displaced: Vec<(u64, usize)> = st
        .notes
        .iter()
        .filter(|n| n.file == file && (s..e).contains(&n.offset))
        .map(|n| (n.id, n.offset - s))
        .collect();
    // Our own delete moves any note anchored below the cut in this file.
    let span = ResolvedSpan {
        at: s,
        deleted: e - s,
        inserted: String::new(),
    };
    shift_notes(&mut st.notes, file, std::slice::from_ref(&span));
    st.region = Some(StreamRegion {
        file: file.to_string(),
        start: s,
        anchor: s,
        original,
        displaced,
    });
    st.discarding = false;
    let lines: Vec<String> = new.split('\n').map(String::from).collect();
    let (row, col) = position(&lines, s);
    st.caret_now(file, row, col);
}

/// Put the streamed region's original slice back (cancel, abort, or exit)
/// and broadcast the stream as inactive. No-op without an open region.
pub(super) fn revert_region(st: &mut PairState) {
    let Some(r) = st.region.take() else {
        return;
    };
    if let Some(doc) = st.session.doc_text(&r.file).map(str::to_string) {
        let start = r.start.min(doc.len());
        let anchor = r.anchor.clamp(start, doc.len());
        let new = format!("{}{}{}", &doc[..start], r.original, &doc[anchor..]);
        st.session.local_change(&r.file, &new);
        // The inverse of the shifts open_region and EditBody applied: the
        // revert deletes the streamed bytes and puts the original back, so
        // notes the edit moved must move back with it.
        let span = ResolvedSpan {
            at: start,
            deleted: anchor - start,
            inserted: r.original.clone(),
        };
        shift_notes(&mut st.notes, &r.file, std::slice::from_ref(&span));
        // Anchors that sat inside the range cannot be un-shifted (the
        // delete collapsed them); restore them from the deltas recorded at
        // open. `start` tracked remote edits all along, so `start + delta`
        // lands exactly where the anchor sat in the restored slice.
        for (id, delta) in &r.displaced {
            if let Some(n) = st
                .notes
                .iter_mut()
                .find(|n| n.id == *id && n.file == r.file)
            {
                n.offset = start + delta;
            }
        }
    }
    st.session.send_stream_state(&r.file, false);
}

/// One pump tick: drain the relay; remote spans shift the streamed region's
/// offsets (concurrent human edits), StreamCancel interrupts and reverts.
pub(super) fn pump_session(state: &Mutex<PairState>, sink: &TurnSink, req_id: &AtomicU64) {
    let mut interrupt = false;
    {
        let mut st = state.lock().unwrap();
        let events = st.session.poll(|_| None);
        for event in events {
            match event {
                CollabEvent::RemoteEdit { file, spans, .. } => {
                    if let Some(r) = st.region.as_mut()
                        && r.file == file
                    {
                        for span in &spans {
                            r.start = shift_offset(r.start, span);
                            r.anchor = shift_offset(r.anchor, span);
                        }
                    }
                    shift_notes(&mut st.notes, &file, &spans);
                }
                CollabEvent::StreamCancel => {
                    if !st.turn_active || st.cancelled {
                        continue;
                    }
                    st.cancelled = true;
                    st.pending_note = Some(
                        "Note: your previous streamed edit was cancelled and reverted by a \
                         participant. Stop that approach and ask what they want instead."
                            .to_string(),
                    );
                    revert_region(&mut st);
                    interrupt = st.can_interrupt;
                    match st.events.clone() {
                        Some(tx) => {
                            let _ = tx.send(crate::pair_host::PairEvent::Commentary(
                                "[stream cancelled by a participant; reverted]".to_string(),
                            ));
                        }
                        None => println!("\n[stream cancelled by a participant; reverted]"),
                    }
                }
                _ => {}
            }
        }
        st.resolve_pending_caret();
    }
    // The claude write happens outside the state lock (lock order: state
    // then writer, same as send_turn; never both held). Only the claude
    // backend can interrupt mid-turn; can_interrupt never arms elsewhere.
    if interrupt {
        let TurnSink::Claude(writer) = sink else {
            return; // can_interrupt only ever arms on the claude transport
        };
        if let Some(w) = writer.lock().unwrap().as_mut() {
            let id = req_id.fetch_add(1, Ordering::Relaxed);
            let msg = json!({
                "type": "control_request",
                "request_id": format!("croft-pair-{id}"),
                "request": { "subtype": "interrupt" },
            });
            let _ = writeln!(w, "{msg}");
            let _ = w.flush();
        }
    }
}
