//! Pull-request review tabs (fetching with gh, viewed marks, checks, per-file diffs,
//! worktree checkout) and the comment boxes in the editor: review threads, navigator
//! notes and sticky notes.

use super::*;

/// How often an open review tab with running checks re-reads them (#365).
pub(super) const PR_CHECKS_POLL: std::time::Duration = std::time::Duration::from_secs(30);

/// Where GitHub's viewed state for a review tab arrives (#365): the PR's
/// node id and each file's state, or why it could not be read.
pub(super) type PrViewedRx = std::sync::mpsc::Receiver<
    Result<
        (
            String,
            std::collections::BTreeMap<String, crate::pr_review::ViewedState>,
        ),
        String,
    >,
>;

/// Where `gh`'s answer to one viewed mutation arrives (#365).
pub(super) type PrViewedWriteRx = std::sync::mpsc::Receiver<Result<String, String>>;

/// Where a pull request checkout's worktree path and commit, or git's
/// refusal, arrive (#365).
pub(super) type PrCheckoutRx =
    std::sync::mpsc::Receiver<Result<(PathBuf, crate::git::PrWorktree), String>>;

/// What a pull request review tab's `gh` call is fetching (#365).
pub(super) enum PrGhJob {
    /// The PR itself (`gh pr view`), by number or URL.
    View { selector: String },
    /// The whole patch, then the section for `path` opens.
    Diff { path: String },
    /// A failing check's log, for OUTPUT > PR Checks.
    Log { name: String },
    /// The open PRs, for the Review Pull Request picker.
    List,
    /// A background re-read of the PR while its checks run: only the
    /// checks of the open tab for `url` are replaced.
    Checks { url: String },
}

impl App {
    /// Keys while a comment box's reply field has focus: type into the
    /// draft, Enter sends the reply turn, Esc leaves the box (its draft
    /// survives until the box goes), F4 hops to the next box, Shift+F4
    /// ignores this one.
    pub(super) fn handle_comment_focus_key(&mut self, key: KeyEvent) {
        if matches!(key.code, KeyCode::F(4)) {
            if key.modifiers.contains(KeyModifiers::SHIFT) {
                self.ignore_focused_comment();
            } else {
                self.cycle_comment_box();
            }
            return;
        }
        match key.code {
            KeyCode::Esc => {
                self.editor.comment_focus = None;
                return;
            }
            KeyCode::Enter => {
                self.submit_comment_reply();
                return;
            }
            _ => {}
        }
        let Some(focus) = self.editor.comment_focus.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Backspace if focus.cursor > 0 => {
                let idx = focus
                    .reply
                    .char_indices()
                    .nth(focus.cursor - 1)
                    .map(|(i, _)| i)
                    .unwrap_or(0);
                focus.reply.remove(idx);
                focus.cursor -= 1;
            }
            KeyCode::Left => focus.cursor = focus.cursor.saturating_sub(1),
            KeyCode::Right => {
                focus.cursor = (focus.cursor + 1).min(focus.reply.chars().count());
            }
            KeyCode::Char(c)
                if !key.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) =>
            {
                let idx = focus
                    .reply
                    .char_indices()
                    .nth(focus.cursor)
                    .map(|(i, _)| i)
                    .unwrap_or(focus.reply.len());
                focus.reply.insert(idx, c);
                focus.cursor += 1;
            }
            _ => {}
        }
    }

    /// Enter in the focused box: append the reply to the box (the running
    /// conversation stays visible) and send it to the navigator as a reply
    /// turn carrying the note's context, which may edit when it asks for a
    /// change (#1965).
    pub(super) fn submit_comment_reply(&mut self) {
        let Some(focus) = self.editor.comment_focus.clone() else {
            return;
        };
        let reply = focus.reply.trim().to_string();
        if reply.is_empty() {
            return;
        }
        if self.reply_to_review_thread(focus.id, &reply) {
            return;
        }
        let me = collab_display_name();
        if self.change_note_box(focus.id, |n| {
            n.replies.push(crate::sticky_notes::Reply {
                author: me,
                body: reply.clone(),
            })
        }) {
            self.editor.comment_focus = None;
            self.status = String::from("Reply added to the note");
            return;
        }
        if Self::pending_review_index(focus.id).is_some() {
            self.status =
                String::from("A pending comment has no thread yet; submit the review first");
            return;
        }
        let Some(host) = &self.pair_host else {
            self.status =
                String::from("Navigator is not active (run croft pair in this workspace)");
            return;
        };
        if host.is_busy() {
            self.status = format!("{} is mid-turn; wait for it to finish", host.name());
            return;
        }
        let Some(file) = self
            .editor
            .path
            .as_ref()
            .and_then(|p| collab_file_key(&self.tree.root, p))
        else {
            self.status = String::from("Reply: no active workspace file");
            return;
        };
        let Some((_, row, note_body)) = self
            .navigator_notes
            .get(&file)
            .and_then(|notes| notes.iter().find(|(id, ..)| *id == focus.id))
            .cloned()
        else {
            self.status = String::from("That comment is gone");
            self.editor.comment_focus = None;
            return;
        };
        let content = self.editor.lines.join("\n");
        match host.send_reply_turn(&file, row, &note_body, &reply, &content) {
            Ok(()) => {
                // Only after the send: a failed send keeps the draft for a
                // retry, and appending first would duplicate the reply in
                // the box on every attempt.
                host.append_to_note(focus.id, &format!("you: {reply}"));
                self.pair_turn_origin = Some((file, row));
                self.status = format!("Replied to {}", host.name());
                self.editor.comment_focus = None;
            }
            Err(e) => self.status = format!("Reply failed: {e}"),
        }
    }

    /// The workspace-relative key a note on the open file uses.
    pub(super) fn note_file_key(&self) -> Option<String> {
        let path = self.editor.path.as_deref()?;
        repo_relative(&self.tree.root, path)
    }

    /// Notes: Add Note (Cmd+K N) (#367).
    pub(super) fn open_sticky_note_prompt(&mut self) {
        if self.note_file_key().is_none() {
            self.status = String::from("Notes go on files inside the workspace");
            return;
        }
        self.prompt = Some(Prompt {
            label: format!("Note on line {}", self.editor.cursor_row + 1),
            buffer: String::new(),
            caret: usize::MAX,
            kind: PromptKind::StickyNote,
            target_dir: self.tree.root.clone(),
            error: None,
        });
    }

    pub(super) fn add_sticky_note(&mut self, body: &str) {
        let Some(file) = self.note_file_key() else {
            return;
        };
        if body.is_empty() {
            return;
        }
        let row = self.editor.cursor_row;
        let anchor = self.editor.lines.get(row).cloned().unwrap_or_default();
        let note = self
            .notes
            .add(&collab_display_name(), &file, row, &anchor, body);
        self.publish_note(&note);
        self.status = String::from("Note added");
    }

    /// Send a changed note to the session and keep the store current.
    pub(super) fn publish_note(&mut self, note: &crate::sticky_notes::Note) {
        let mut session = self.collab.take();
        if let Some(s) = session.as_mut() {
            s.send_note(note);
        }
        self.save_notes(session.as_ref());
        self.collab = session;
    }

    /// The owner (or a lone croft) writes the notes; a guest's copy lives
    /// with the owner.
    pub(super) fn save_notes(&mut self, session: Option<&crate::collab::CollabSession>) {
        if session.is_some_and(|s| s.role == crate::collab::CollabRole::Guest) {
            return;
        }
        if let Some(path) = &self.notes_path {
            let _ = self.notes.save(path);
        }
    }

    /// A note's comment-box id: high in the id space, clear of navigator
    /// notes (from 1), GitHub ids (~4e9) and pending review comments (top).
    pub(super) fn note_box_id(id: &str) -> u64 {
        let mut h: u64 = 0xcbf29ce484222325;
        for b in id.bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x100000001b3);
        }
        (1 << 62) | (h & ((1 << 61) - 1))
    }

    /// Change the note behind box `id`, if it is one. Returns whether it was.
    pub(super) fn change_note_box(
        &mut self,
        id: u64,
        change: impl FnOnce(&mut crate::sticky_notes::Note),
    ) -> bool {
        let Some(note_id) = self.note_box_ids.get(&id).cloned() else {
            return false;
        };
        if let Some(note) = self.notes.update(&note_id, change) {
            self.publish_note(&note);
        }
        true
    }

    /// Notes: Delete Note: the focused note, else the one on the caret's
    /// line. A note further down is never reached for: a tombstone can't be
    /// undone, so the command deletes only a note the user is on (#1131).
    pub(super) fn delete_sticky_note_here(&mut self) {
        let id = self
            .editor
            .comment_focus
            .as_ref()
            .map(|f| f.id)
            .filter(|id| self.note_box_ids.contains_key(id))
            .or_else(|| {
                let file = self.note_file_key()?;
                let lines = &self.editor.lines;
                self.notes
                    .on_file(&file)
                    .find(|n| n.place(lines) == self.editor.cursor_row)
                    .map(|n| Self::note_box_id(&n.id))
            });
        let Some(note) = id
            .and_then(|id| self.note_box_ids.get(&id))
            .and_then(|note_id| self.notes.get(note_id))
        else {
            self.status = String::from("No note on this line");
            return;
        };
        let line = note.place(&self.editor.lines) + 1;
        let first = note.body.lines().next().unwrap_or_default();
        // The first words, cut at a space so the status doesn't end mid-word.
        let mut gist = truncate_for_display(first, 40);
        if gist.len() < first.len()
            && let Some(cut) = gist.rfind(' ')
        {
            gist.truncate(cut);
        }
        if id.is_some_and(|id| self.change_note_box(id, |n| n.deleted = true)) {
            self.status = format!("Note deleted on line {line}: {gist}");
        }
    }

    /// Box ids for pending comments count down from the top of the id
    /// space, clear of navigator notes (from 1) and GitHub ids (~4e9).
    pub(super) const PENDING_REVIEW_ID: u64 = u64::MAX - 1_000_000;

    /// The pending comment a box id names, as an index.
    pub(super) fn pending_review_index(id: u64) -> Option<usize> {
        id.checked_sub(Self::PENDING_REVIEW_ID).map(|i| i as usize)
    }

    /// Review: Add Comment on This Line (#366).
    pub(super) fn open_review_comment_prompt(&mut self) {
        let Some((_, _, rel, _)) = self.review_context() else {
            return;
        };
        self.prompt = Some(Prompt {
            label: format!("Comment on {rel}:{}", self.editor.cursor_row + 1),
            buffer: String::new(),
            caret: usize::MAX,
            kind: PromptKind::ReviewComment,
            // Carries the repo-relative path, so the comment lands on the
            // file it was asked about.
            target_dir: PathBuf::from(rel),
            error: None,
        });
    }

    pub(super) fn add_pending_review_comment(&mut self, body: &str, rel: PathBuf) {
        if body.is_empty() {
            return;
        }
        let Some((root, number)) = self.review_pr.clone() else {
            return;
        };
        if !self.pending_is_for(&root, &number) {
            return;
        }
        self.review_pending_pr = Some((root, number));
        self.review_pending
            .push(crate::review_threads::PendingComment {
                path: rel.to_string_lossy().into_owned(),
                line: self.editor.cursor_row,
                body: body.to_string(),
            });
        self.status = format!(
            "{} pending comment(s): Review: Submit Review sends them",
            self.review_pending.len()
        );
    }

    /// Whether the pending comments (if any) were written on PR `number` in
    /// `root`. Comments written on another PR (another branch, another
    /// repository of the workspace) must never post here; the status says
    /// which PR they belong to.
    pub(super) fn pending_is_for(&mut self, root: &Path, number: &str) -> bool {
        match &self.review_pending_pr {
            Some((r, n)) if !self.review_pending.is_empty() && (r != root || n != number) => {
                self.status = format!(
                    "{} pending comment(s) belong to PR #{n} in {}: submit or discard them there first",
                    self.review_pending.len(),
                    r.display()
                );
                false
            }
            _ => true,
        }
    }

    /// Review: Submit Review: pick the verdict, then type the summary.
    pub(super) fn open_review_submit(&mut self) {
        if self.review_context().is_none() {
            return;
        }
        let rows = [
            ("COMMENT", "Comment"),
            ("APPROVE", "Approve"),
            ("REQUEST_CHANGES", "Request Changes"),
        ]
        .into_iter()
        .map(|(id, label)| crate::widgets::list_picker::ListRow {
            id: id.to_string(),
            label: format!("{label} ({} pending comment(s))", self.review_pending.len()),
        })
        .collect();
        self.open_list_picker(
            crate::widgets::list_picker::ListPicker::new(
                crate::widgets::list_picker::ListPurpose::ReviewVerdict,
                "Submit Review",
                rows,
            ),
            "",
        );
    }

    pub(super) fn submit_review(&mut self, summary: String) {
        use crate::review_threads::ReviewEvent;
        let event = self.review_verdict.take().unwrap_or(ReviewEvent::Comment);
        let Some((root, number)) = self.review_pr.clone() else {
            return;
        };
        if !self.pending_is_for(&root, &number) {
            return;
        }
        if self.review_submitting {
            self.status = String::from("A review is already being submitted");
            return;
        }
        if event == ReviewEvent::Comment && summary.is_empty() && self.review_pending.is_empty() {
            self.status = String::from("Nothing to submit: add a comment or a summary");
            return;
        }
        crate::review_ops::spawn(
            self.review_gh.clone(),
            root,
            crate::review_ops::Job::Submit {
                number,
                event,
                summary,
                pending: self.review_pending.clone(),
                settles: crate::review_ops::Settles {
                    pending: self.review_pending.clone(),
                    ..Default::default()
                },
            },
            self.review_tx.clone(),
        );
        self.review_submitting = true;
        self.status = String::from("Submitting review…");
    }

    /// Review: Resolve or Unresolve Thread: the focused box, else the next
    /// one from the caret.
    pub(super) fn toggle_review_thread_resolved(&mut self) {
        let id = self
            .editor
            .comment_focus
            .as_ref()
            .map(|f| f.id)
            .or_else(|| self.next_comment_from_caret().map(|(id, _)| id));
        let Some(thread) = id.and_then(|id| {
            self.review_boxes
                .as_ref()
                .and_then(|(_, ts)| ts.iter().find(|t| t.id == id).cloned())
        }) else {
            self.status = String::from("No review thread here");
            return;
        };
        let (Some(node), Some((root, _))) = (
            self.review_nodes.get(&thread.id).cloned(),
            self.review_pr.clone(),
        ) else {
            self.status =
                String::from("GitHub didn't report this thread's id; reload the comments");
            return;
        };
        crate::review_ops::spawn(
            self.review_gh.clone(),
            root,
            crate::review_ops::Job::Resolve {
                thread: thread.id,
                node,
                resolve: !thread.resolved,
            },
            self.review_tx.clone(),
        );
    }

    /// A reply typed into a review thread's box goes to GitHub. Returns
    /// false when the focused box is not a review thread.
    pub(super) fn reply_to_review_thread(&mut self, id: u64, text: &str) -> bool {
        let is_thread = self
            .review_boxes
            .as_ref()
            .is_some_and(|(_, ts)| ts.iter().any(|t| t.id == id));
        if !is_thread {
            return false;
        }
        let Some((root, number)) = self.review_pr.clone() else {
            return false;
        };
        crate::review_ops::spawn(
            self.review_gh.clone(),
            root,
            crate::review_ops::Job::Reply {
                number,
                thread: id,
                text: text.to_string(),
            },
            self.review_tx.clone(),
        );
        self.editor.comment_focus = None;
        self.status = String::from("Replying…");
        true
    }

    /// Apply finished review jobs.
    pub fn drain_review_ops(&mut self) -> bool {
        use crate::review_ops::Outcome;
        let mut changed = false;
        while let Ok(out) = self.review_rx.try_recv() {
            changed = true;
            match out {
                Outcome::Replied { thread, text } => {
                    if let Some((_, ts)) = &mut self.review_boxes
                        && let Some(t) = ts.iter_mut().find(|t| t.id == thread)
                    {
                        t.body.push_str(&format!("\n\nyou: {text}"));
                    }
                    self.status = String::from("Reply posted");
                }
                Outcome::Resolved { thread, resolved } => {
                    if let Some((_, ts)) = &mut self.review_boxes
                        && let Some(t) = ts.iter_mut().find(|t| t.id == thread)
                    {
                        t.resolved = resolved;
                    }
                    self.status = String::from(if resolved {
                        "Thread resolved"
                    } else {
                        "Thread reopened"
                    });
                }
                Outcome::Submitted {
                    inline,
                    folded,
                    settles,
                } => {
                    self.review_submitting = false;
                    self.review_pending.retain(|c| !settles.pending.contains(c));
                    if self.review_pending.is_empty() {
                        self.review_pending_pr = None;
                    }
                    // Exported navigator notes now live on GitHub; reload
                    // shows them there, as threads, rather than twice.
                    // Exported sticky notes are settled: resolved for everyone.
                    for id in settles.sticky {
                        if let Some(n) = self.notes.update(&id, |n| n.resolved = true) {
                            self.publish_note(&n);
                        }
                    }
                    let exported = settles.notes;
                    if !exported.is_empty() {
                        for id in &exported {
                            if let Some(host) = &self.pair_host {
                                host.remove_note(*id);
                            }
                        }
                        for list in self.navigator_notes.values_mut() {
                            list.retain(|(id, ..)| !exported.contains(id));
                        }
                        if self.editor.path.is_some() {
                            self.load_review_threads();
                        }
                    }
                    self.status = match folded {
                        0 => format!("Review submitted with {inline} comment(s)"),
                        n => format!(
                            "Review submitted: {inline} inline, {n} outside the diff moved into the summary"
                        ),
                    };
                }
                Outcome::Threads {
                    root,
                    number,
                    path,
                    rel,
                    threads,
                    states,
                } => self.show_review_threads(root, number, path, rel, threads, states),
                Outcome::ExportReady {
                    root,
                    number,
                    comments,
                    notes,
                    inline,
                } => self.show_export_ready(root, number, comments, notes, inline),
                Outcome::NoPr => {
                    self.status = String::from("No PR for this branch — check it out first");
                }
                Outcome::SubmitFailed(e) => {
                    self.review_submitting = false;
                    self.status = e;
                }
                // A failed reply or load leaves a submission in flight alone.
                Outcome::Failed(e) => self.status = e,
            }
        }
        changed
    }

    /// Follow the open file to its new path so its review boxes survive a
    /// rename or an explorer cut/paste (#366).
    ///
    /// Both guards — the render merge and the F4 walk — compare the stored
    /// path against `editor.path`. A rename repoints the tab and would
    /// otherwise leave the field naming a path nothing matches, so the boxes
    /// vanish from the buffer the user is still editing and the field holds
    /// unreachable garbage.
    pub(super) fn rename_review_boxes_path(&mut self, old: &Path, new: &Path) {
        if let Some((p, _)) = &mut self.review_boxes
            && p == old
        {
            *p = new.to_path_buf();
        }
    }

    /// Dismiss one comment box: drop the note from the pilot's state and
    /// from the local snapshot (the next poll would resurrect it otherwise
    /// only if the pilot still had it).
    pub(super) fn ignore_comment_box(&mut self, id: u64) {
        // A note's ✕ resolves it (and a resolved note's reopens it): notes
        // are shared, so dismissing one is a decision for everyone.
        if self.change_note_box(id, |n| n.resolved = !n.resolved) {
            self.status = String::from("Note resolved or reopened");
            return;
        }
        if let Some(host) = &self.pair_host {
            host.remove_note(id);
        }
        for notes in self.navigator_notes.values_mut() {
            notes.retain(|(nid, ..)| *nid != id);
        }
        // Review boxes share this footer and this hit-test, so without
        // retaining here the button would report "Comment ignored" and the
        // box would return on the very next frame (#366).
        //
        // Matching on the id alone is safe because the two sources cannot
        // collide in practice: a navigator note's id counts up from 1 (see
        // `pair.rs`'s `take_note_id`, over `next_note_id`), while a review
        // thread's is GitHub's own comment id, currently around 3.9e9. If
        // notes ever gain a wider allocator, this needs a tag rather than a
        // bare id.
        if let Some((_, threads)) = &mut self.review_boxes {
            threads.retain(|t| t.id != id);
        }
        if let Some(i) = Self::pending_review_index(id)
            && i < self.review_pending.len()
        {
            self.review_pending.remove(i);
        }
        if self
            .editor
            .comment_focus
            .as_ref()
            .is_some_and(|f| f.id == id)
        {
            self.editor.comment_focus = None;
        }
        self.status = String::from("Comment ignored");
    }

    /// Shift+F4 / palette: ignore the focused box, or the next one from the
    /// caret when none is focused.
    pub(super) fn ignore_focused_comment(&mut self) {
        let id = self
            .editor
            .comment_focus
            .as_ref()
            .map(|f| f.id)
            .or_else(|| self.next_comment_from_caret().map(|(id, _)| id));
        match id {
            Some(id) => self.ignore_comment_box(id),
            None => self.status = String::from("No navigator comments in this file"),
        }
    }

    /// The active file's next comment from the caret (by row, wrapping):
    /// (id, row).
    pub(super) fn next_comment_from_caret(&self) -> Option<(u64, usize)> {
        // Built from the same two SOURCES the render merges, in the same
        // order, rather than from `editor.comment_boxes` itself: that field
        // is a render output, so reading it would make F4 depend on a frame
        // having been drawn first, and the walk would find nothing before
        // the first render. Review threads are included so F4 reaches them
        // too (#366) — reading only the navigator's notes left review boxes
        // on screen but unreachable by keyboard, reported as "No navigator
        // comments in this file" while the user was looking at several.
        let mut notes: Vec<(u64, usize, String)> = self
            .editor
            .path
            .as_ref()
            .and_then(|p| collab_file_key(&self.tree.root, p))
            .and_then(|file| self.navigator_notes.get(&file))
            .cloned()
            .unwrap_or_default();
        if let Some((path, threads)) = &self.review_boxes
            && self.editor.path.as_deref() == Some(path.as_path())
        {
            let lines = self.editor.lines.len();
            notes.extend(
                threads
                    .iter()
                    .map(|t| (t.id, t.box_line(lines), t.body.clone())),
            );
        }
        // Unresolved sticky notes (#367) too: F4 is how they are walked.
        if let Some(file) = self.note_file_key() {
            let lines = &self.editor.lines;
            notes.extend(
                self.notes
                    .on_file(&file)
                    .filter(|n| !n.resolved)
                    .map(|n| (Self::note_box_id(&n.id), n.place(lines), n.body.clone())),
            );
            notes.sort_by_key(|(_, row, _)| *row);
        }
        if notes.is_empty() {
            return None;
        }
        let here = self
            .editor
            .comment_focus
            .as_ref()
            .and_then(|f| notes.iter().position(|(id, ..)| *id == f.id))
            .map(|i| (notes[i].1, i))
            .unwrap_or((self.editor.cursor_row, usize::MAX));
        let idx = next_note_by_row(&notes, here);
        Some((notes[idx].0, notes[idx].1))
    }

    /// F4: focus the active file's next comment box (by row, wrapping),
    /// jumping the caret to its anchor line so the box scrolls into view.
    pub(super) fn cycle_comment_box(&mut self) {
        let Some((id, row)) = self.next_comment_from_caret() else {
            self.status = String::from("No navigator comments in this file");
            return;
        };
        let row = row.min(self.editor.lines.len().saturating_sub(1));
        let path = self.editor.path.clone().expect("comments imply a file");
        match self.open_at(&path, row, 0) {
            Ok(()) => {
                self.editor.comment_focus = Some(crate::widgets::editor::CommentFocus {
                    id,
                    reply: String::new(),
                    cursor: 0,
                });
            }
            Err(e) => self.status = format!("Comment jump failed: {e}"),
        }
    }

    /// The open file's review context (#366): (repo root the `gh` calls run
    /// in, the file, its repo-relative path, the branch's PR number), or
    /// `None` with the reason in the status bar.
    pub(super) fn review_context(&mut self) -> Option<(PathBuf, PathBuf, String, String)> {
        let (root, path, rel) = self.review_file_context()?;
        let number = self.review_pr_number(&root)?;
        Some((root, path, rel, number))
    }

    /// [`Self::review_context`] without the PR number, which costs a `gh`
    /// call: the root, the file and its repo-relative path.
    pub(super) fn review_file_context(&mut self) -> Option<(PathBuf, PathBuf, String)> {
        let Some(path) = self.editor.path.clone() else {
            self.status = String::from("Open a file from the PR first");
            return None;
        };
        // The root that OWNS the open file, not the primary. In a
        // multi-root workspace the two differ, and using the primary runs
        // `gh` in the wrong repository — which answers about a different
        // PR entirely rather than failing.
        let root = self.active_workspace_root();
        // Relative to the REPOSITORY TOPLEVEL, not the workspace root. The
        // API's `path` is repo-relative, and the two coincide only when the
        // workspace root IS the toplevel — open croft on `repo/src` and
        // every comparison fails silently, so a file with review comments
        // reports having none. That is #139's rule; this is the same trap in
        // a new place.
        let Some(repo_root) = self.git_worker_for_root(&root).status().repo_root.clone() else {
            self.status = String::from("This folder is not inside a git repository");
            return None;
        };
        let Ok(rel) = path.strip_prefix(&repo_root) else {
            self.status = format!(
                "{} is outside the repository, so it has no review comments",
                path.display()
            );
            return None;
        };
        // Forward slashes: the API always uses them, and on Windows a
        // `to_string_lossy` here yields backslashes that match nothing.
        let rel = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        Some((root, path, rel))
    }

    /// Remember the PR under review, with the branch it belongs to.
    pub(super) fn set_review_pr(&mut self, root: PathBuf, number: String) {
        self.review_pr_branch = self.git_worker_for_root(&root).status().branch.clone();
        self.review_pr = Some((root, number));
    }

    /// The PR number for `root`'s branch, remembered for the write side, or
    /// `None` with the reason in the status bar.
    pub(super) fn review_pr_number(&mut self, root: &Path) -> Option<String> {
        // The PR already loaded for this root, when there is one: this runs
        // on the UI thread for every comment and submit, and asking GitHub
        // each time froze croft on a slow network or a gh auth prompt.
        let branch = self.git_worker_for_root(root).status().branch.clone();
        if let Some((r, number)) = &self.review_pr
            && r == root
            && branch.is_some()
            && branch == self.review_pr_branch
        {
            return Some(number.clone());
        }
        let out = bounded_output(
            std::process::Command::new(&self.review_gh)
                .args(["pr", "view", "--json", "number", "--jq", ".number"])
                .current_dir(root),
            std::time::Duration::from_secs(5),
        );
        let number = match out {
            Some((status, stdout)) if status.success() => {
                String::from_utf8_lossy(&stdout).trim().to_string()
            }
            // A gh that is missing or signed out says how to fix it, as the
            // review tab does (#917), not that the branch has no PR.
            failed => {
                let why = match failed {
                    Some((status, _)) => {
                        crate::gh::signed_out(status).then(|| String::from(crate::gh::SIGNED_OUT))
                    }
                    // No output: gh did not start, or ran past the budget.
                    None => std::process::Command::new(&self.review_gh)
                        .arg("--version")
                        .stdin(std::process::Stdio::null())
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .status()
                        .err()
                        .map(|e| crate::gh::spawn_error(&e, Some(root))),
                };
                self.status = match why {
                    Some(why) => format!("Could not find this branch's PR: {why}"),
                    None => String::from("No PR for this branch — check it out first"),
                };
                return None;
            }
        };
        self.set_review_pr(root.to_path_buf(), number.clone());
        Some(number)
    }

    /// Source Control: Export Comments to Pull Request (#368): the
    /// navigator's notes (marked as AI-authored) and your pending comments,
    /// previewed as a list before anything is posted.
    pub(super) fn open_export_comments(&mut self) {
        let root = self.active_workspace_root();
        let Some(repo_root) = self.git_worker_for_root(&root).status().repo_root.clone() else {
            self.status = String::from("This folder is not inside a git repository");
            return;
        };
        let prefix = self.review_ai_prefix.clone();
        let mut comments = self.review_pending.clone();
        let mut notes = Vec::new();
        for (file, list) in &self.navigator_notes {
            let Some(rel) = repo_relative(&repo_root, &self.tree.root.join(file)) else {
                continue;
            };
            for (id, row, body) in list {
                comments.push(crate::review_threads::PendingComment {
                    path: rel.clone(),
                    line: *row,
                    body: format!("{prefix}{body}"),
                });
                notes.push(*id);
            }
        }
        // Unresolved sticky notes (#367), each with its author and replies.
        let mut exported_notes = Vec::new();
        for n in self.notes.live().filter(|n| !n.resolved) {
            let Some(rel) = repo_relative(&repo_root, &self.tree.root.join(&n.file)) else {
                continue;
            };
            let lines = std::fs::read_to_string(self.tree.root.join(&n.file))
                .map(|t| t.lines().map(str::to_string).collect::<Vec<_>>())
                .unwrap_or_default();
            comments.push(crate::review_threads::PendingComment {
                path: rel,
                line: n.place(&lines),
                body: format!("{}: {}", n.author, n.text()),
            });
            exported_notes.push(n.id.clone());
        }
        self.review_export_sticky = exported_notes;
        if comments.is_empty() {
            self.status = String::from("No comments to export");
            return;
        }
        comments.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
        // Finding the PR and reading its diff are `gh` calls, which can take
        // as long as the network does: off the frame loop, the preview
        // opens when they answer.
        crate::review_ops::spawn(
            self.review_gh.clone(),
            root,
            crate::review_ops::Job::ExportPreview { comments, notes },
            self.review_tx.clone(),
        );
        self.status = String::from("Preparing the export\u{2026}");
    }

    /// Open the export preview once its PR and diff are known.
    pub(super) fn show_export_ready(
        &mut self,
        root: PathBuf,
        number: String,
        comments: Vec<crate::review_threads::PendingComment>,
        notes: Vec<u64>,
        inline: usize,
    ) {
        self.set_review_pr(root.clone(), number.clone());
        self.review_export_pr = Some((root, number.clone()));
        let mut rows = vec![crate::widgets::list_picker::ListRow {
            id: String::from("post"),
            label: format!(
                "Post to PR #{number}: {inline} inline, {} in the summary",
                comments.len() - inline
            ),
        }];
        rows.extend(comments.iter().enumerate().map(|(i, c)| {
            crate::widgets::list_picker::ListRow {
                id: format!("c{i}"),
                label: format!(
                    "  {}:{}  {}",
                    c.path,
                    c.line + 1,
                    c.body.lines().next().unwrap_or_default()
                ),
            }
        }));
        self.review_export = Some((comments, notes, rows));
        self.show_export_preview();
    }

    pub(super) fn show_export_preview(&mut self) {
        let Some((_, _, rows)) = &self.review_export else {
            return;
        };
        let rows = rows.clone();
        self.open_list_picker(
            crate::widgets::list_picker::ListPicker::new(
                crate::widgets::list_picker::ListPurpose::ExportComments,
                "Export Comments to Pull Request",
                rows,
            ),
            "",
        );
    }

    /// Post the previewed export as a Comment review.
    pub(super) fn post_exported_comments(&mut self) {
        let Some((comments, notes, _)) = self.review_export.take() else {
            return;
        };
        let Some((root, number)) = self.review_export_pr.take() else {
            return;
        };
        let sticky = std::mem::take(&mut self.review_export_sticky);
        if !self.pending_is_for(&root, &number) {
            return;
        }
        if self.review_submitting {
            self.status = String::from("A review is already being submitted");
            return;
        }
        let pending: Vec<_> = self
            .review_pending
            .iter()
            .filter(|c| comments.contains(c))
            .cloned()
            .collect();
        crate::review_ops::spawn(
            self.review_gh.clone(),
            root,
            crate::review_ops::Job::Submit {
                number,
                event: crate::review_threads::ReviewEvent::Comment,
                summary: String::new(),
                pending: comments,
                settles: crate::review_ops::Settles {
                    notes,
                    sticky,
                    pending,
                },
            },
            self.review_tx.clone(),
        );
        self.review_submitting = true;
        self.status = String::from("Posting comments…");
    }

    /// Load this PR's review threads onto the open file as comment boxes
    /// (#366).
    ///
    /// Only threads for THIS file. A review's comments span the whole PR,
    /// and hanging another file's objections off this buffer's line numbers
    /// would put them against unrelated code — the same failure as placing
    /// an outdated thread silently, arriving through the other axis.
    pub(super) fn load_review_threads(&mut self) {
        let Some((root, path, rel)) = self.review_file_context() else {
            return;
        };
        // `gh` over the network, several calls for a big PR: off the frame
        // loop. The threads come back keyed to `path`, the file asked about.
        crate::review_ops::spawn(
            self.review_gh.clone(),
            root,
            crate::review_ops::Job::LoadThreads { path, rel },
            self.review_tx.clone(),
        );
        self.status = String::from("Loading review comments\u{2026}");
    }

    /// Show threads read by [`Self::load_review_threads`].
    pub(super) fn show_review_threads(
        &mut self,
        root: PathBuf,
        number: String,
        path: PathBuf,
        rel: String,
        mut threads: Vec<crate::review_threads::Thread>,
        states: std::collections::HashMap<u64, (String, bool)>,
    ) {
        self.set_review_pr(root, number);
        for t in &mut threads {
            if let Some((node, resolved)) = states.get(&t.id) {
                t.resolved = *resolved;
                self.review_nodes.insert(t.id, node.clone());
            }
        }
        if threads.is_empty() {
            self.status = format!("No review comments on {rel}");
            return;
        }
        let outdated = threads
            .iter()
            .filter(|t| matches!(t.anchor, crate::review_threads::Anchor::Outdated(_)))
            .count();
        // Stored against the file they describe; the render derives each
        // box's row and title from the buffer as it is at that moment.
        // `path` is the file the `gh` query was about, carried through the
        // worker, not whatever `editor.path` says now: the user may have
        // switched tabs while it ran.
        self.review_boxes = Some((path, threads.clone()));
        self.status = match outdated {
            0 => format!("{} review comments on {rel}", threads.len()),
            n => format!(
                "{} review comments on {rel} \u{b7} {n} outdated, shown where they WERE",
                threads.len()
            ),
        };
    }

    pub(super) fn pr_viewed_path() -> PathBuf {
        croft_cache_dir().join("pr-viewed.json")
    }

    /// Open pull request `pr` for review in its own tab (#365), with the
    /// viewed marks remembered for `key` (`owner/repo#n`).
    pub fn open_pr_review(&mut self, pr: crate::pr_review::PrInfo, key: String) {
        let store = crate::pr_review::ViewedStore::load(&Self::pr_viewed_path());
        let viewed = store.viewed(&key);
        let n = pr.number;
        self.editor
            .open_pr_review(crate::widgets::pr_review::PrReviewView::new(
                pr, key, viewed,
            ));
        self.focus_pane(Pane::Editor);
        self.status = format!("Reviewing PR #{n}");
    }

    /// Ask for a pull request by number or URL (#365).
    pub(super) fn open_pr_number_prompt(&mut self) {
        use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
        self.open_input_prompt(InputPrompt::new(
            InputPurpose::PullRequestNumber,
            String::from("Review Pull Request"),
            String::from("number, #number, or the PR's URL"),
        ));
    }

    /// The Review Pull Request prompt's answer.
    pub(super) fn submit_pr_number(&mut self, input: &str) {
        match crate::pr_review::parse_pr_selector(input) {
            Some(selector) => self.start_pr_review(selector),
            None => {
                self.status = format!("{:?} is not a pull request number", input.trim());
            }
        }
    }

    /// Fetch the pull request `selector` names (a number, or its URL) with
    /// `gh` and open it for review once it arrives.
    pub(super) fn start_pr_review(&mut self, selector: String) {
        self.status = format!("Loading PR {selector}…");
        let args = crate::pr_review::view_args(&selector);
        self.spawn_pr_gh(PrGhJob::View { selector }, args);
    }

    /// Run one `gh` call for the review tab on a worker (#365): a slow
    /// network or a large log never stalls input or redraws, and the call
    /// is killed at [`crate::pr_review::GH_TIMEOUT`]. A newer call replaces
    /// one still running, whose answer is then dropped.
    pub(super) fn spawn_pr_gh(&mut self, job: PrGhJob, args: Vec<String>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let gh = self.review_gh.clone();
        let root = self.workspace_root().to_path_buf();
        std::thread::spawn(move || {
            let _ = tx.send(crate::pr_review::run_gh(
                &gh,
                &args,
                &root,
                crate::pr_review::GH_TIMEOUT,
            ));
        });
        self.pr_gh = Some((job, rx));
    }

    /// Read GitHub's viewed state for the open review tab on a worker
    /// (#365). A PR whose key names no GitHub repository reads nothing, and
    /// its marks stay local.
    pub(super) fn start_pr_viewed_fetch(&mut self) {
        let Some(view) = self.editor.pr_review.as_ref() else {
            return;
        };
        let key = view.key.clone();
        let number = view.pr.number;
        let slug = key.split('#').next().unwrap_or_default().to_string();
        if crate::pr_review::viewed_query_args(&slug, number, None).is_none() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let gh = self.review_gh.clone();
        let root = self.workspace_root().to_path_buf();
        std::thread::spawn(move || {
            let _ = tx.send(crate::pr_review::fetch_viewed(
                &gh,
                &slug,
                number,
                &root,
                crate::pr_review::GH_TIMEOUT,
            ));
        });
        self.pr_viewed_fetch = Some((key, self.pr_viewed_seq, rx));
    }

    /// Queue a viewed mark for GitHub (#365), stamping its file so a
    /// viewed-state read already under way cannot undo it.
    pub(super) fn queue_pr_viewed_write(&mut self, write: PrViewedWrite) {
        self.touch_pr_viewed(&write.key, &write.path);
        self.pr_viewed_queue.push_back(write);
    }

    /// Stamp `(key, path)` with the next [`Self::pr_viewed_seq`].
    pub(super) fn touch_pr_viewed(&mut self, key: &str, path: &str) {
        self.pr_viewed_seq += 1;
        self.pr_viewed_touched
            .insert((key.to_string(), path.to_string()), self.pr_viewed_seq);
    }

    /// Whether GitHub's answer from a read that began at `since` may be
    /// older than a viewed write of `path` in the tab keyed `key`: one
    /// still queued or in flight, or one that settled after the read began.
    pub(super) fn pr_viewed_fenced(&self, key: &str, path: &str, since: u64) -> bool {
        let pending = self
            .pr_viewed_queue
            .iter()
            .any(|w| w.key == key && w.path == path)
            || self
                .pr_viewed_write
                .as_ref()
                .is_some_and(|(k, p, _)| k == key && p == path);
        pending
            || self
                .pr_viewed_touched
                .get(&(key.to_string(), path.to_string()))
                .is_some_and(|&stamp| stamp > since)
    }

    /// Apply GitHub's viewed state when it arrives, report a failed viewed
    /// mutation once per tab, and send the next queued one (#365). Returns
    /// true when it changed what is on screen.
    pub(super) fn poll_pr_viewed(&mut self) -> bool {
        use std::sync::mpsc::TryRecvError;
        let mut changed = false;
        if let Some((key, since, rx)) = self.pr_viewed_fetch.as_ref() {
            let got = rx.try_recv();
            if !matches!(got, Err(TryRecvError::Empty)) {
                let (key, since) = (key.clone(), *since);
                self.pr_viewed_fetch = None;
                // gh missing, signed out or refused: the marks stay local,
                // exactly as before, and nothing is said about it.
                if let Ok(Ok((pr_id, states))) = got {
                    changed |= self.apply_github_viewed(&key, since, pr_id, states);
                }
            }
        }
        if let Some((key, path, rx)) = self.pr_viewed_write.as_ref() {
            let got = rx.try_recv();
            if !matches!(got, Err(TryRecvError::Empty)) {
                let (key, path) = (key.clone(), path.clone());
                self.pr_viewed_write = None;
                // Settled after any read already under way began: that
                // read may predate it.
                self.touch_pr_viewed(&key, &path);
                let why = match got {
                    Ok(Ok(_)) => None,
                    Ok(Err(why)) => Some(why),
                    Err(_) => Some(String::from("gh stopped without answering")),
                };
                if let Some(why) = why {
                    let warned = self
                        .editor
                        .pr_review
                        .as_mut()
                        .filter(|v| v.key == key)
                        .is_some_and(|v| std::mem::replace(&mut v.sync_warned, true));
                    if !warned {
                        self.status = format!(
                            "Viewed state saved locally; GitHub could not be updated: {why}"
                        );
                        changed = true;
                    }
                }
            }
        }
        if self.pr_viewed_write.is_none()
            && let Some(write) = self.pr_viewed_queue.pop_front()
        {
            let args = crate::pr_review::mark_viewed_args(&write.pr_id, &write.path, write.viewed);
            let (tx, rx) = std::sync::mpsc::channel();
            let gh = self.review_gh.clone();
            let root = self.workspace_root().to_path_buf();
            std::thread::spawn(move || {
                let _ = tx.send(crate::pr_review::run_gh(
                    &gh,
                    &args,
                    &root,
                    crate::pr_review::GH_TIMEOUT,
                ));
            });
            self.pr_viewed_write = Some((write.key, write.path, rx));
        }
        changed
    }

    /// GitHub's viewed state for the tab keyed `key`, read from `since`,
    /// becomes the truth (#365): the checkboxes and the local store follow
    /// it, except for a file toggled while the answer was on its way, whose
    /// newer mark is sent to GitHub instead, and a file whose write had not
    /// settled when the read began, which keeps its mark. Files GitHub did
    /// not list keep their local mark.
    pub(super) fn apply_github_viewed(
        &mut self,
        key: &str,
        since: u64,
        pr_id: String,
        states: std::collections::BTreeMap<String, crate::pr_review::ViewedState>,
    ) -> bool {
        if self.editor.pr_review.as_ref().is_none_or(|v| v.key != key) {
            return false;
        }
        let states: Vec<_> = states
            .into_iter()
            .filter(|(path, _)| !self.pr_viewed_fenced(key, path, since))
            .collect();
        let Some(view) = self.editor.pr_review.as_mut() else {
            return false;
        };
        let store_path = Self::pr_viewed_path();
        let mut store = crate::pr_review::ViewedStore::load(&store_path);
        let unsynced = std::mem::take(&mut view.unsynced);
        let mut resend = Vec::new();
        for (path, state) in states {
            let local = view.viewed.contains(&path);
            if unsynced.contains(&path) {
                if local != state.is_viewed() {
                    resend.push(PrViewedWrite {
                        key: key.to_string(),
                        pr_id: pr_id.clone(),
                        path,
                        viewed: local,
                    });
                }
                continue;
            }
            store.set(key, &path, state.is_viewed());
            if state.is_viewed() {
                view.viewed.insert(path);
            } else {
                view.viewed.remove(&path);
            }
        }
        view.github_id = Some(pr_id);
        // GitHub holds the marks now; a cache that cannot be written only
        // costs the offline copy.
        let _ = store.save(&store_path);
        for write in resend {
            self.queue_pr_viewed_write(write);
        }
        true
    }

    /// While the open review tab has checks still running, re-read the PR
    /// every [`PR_CHECKS_POLL`] so they turn green or red on their own
    /// (#365). Never while another review call is in flight, which it would
    /// otherwise replace.
    pub(super) fn refresh_pending_pr_checks(&mut self) {
        if self.pr_gh.is_some() {
            return;
        }
        let Some(view) = self.editor.pr_review.as_ref() else {
            return;
        };
        if !view.has_pending_checks() || view.pr.url.is_empty() {
            return;
        }
        let now = std::time::Instant::now();
        match self.pr_checks_polled {
            Some(at) if now.duration_since(at) < PR_CHECKS_POLL => return,
            // The first sight of a pending tab starts the clock.
            None => {
                self.pr_checks_polled = Some(now);
                return;
            }
            Some(_) => {}
        }
        self.pr_checks_polled = Some(now);
        let url = view.pr.url.clone();
        let args = crate::pr_review::view_args(&url);
        self.spawn_pr_gh(PrGhJob::Checks { url }, args);
    }

    /// Apply a finished review `gh` call. Returns true when it changed what
    /// is on screen.
    pub(super) fn poll_pr_gh(&mut self) -> bool {
        let synced = self.poll_pr_viewed();
        self.refresh_pending_pr_checks();
        let Some((_, rx)) = self.pr_gh.as_ref() else {
            return synced;
        };
        let result = match rx.try_recv() {
            Ok(r) => r,
            Err(std::sync::mpsc::TryRecvError::Empty) => return synced,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err(String::from("gh stopped without answering"))
            }
        };
        let Some((job, _)) = self.pr_gh.take() else {
            return synced;
        };
        match job {
            PrGhJob::View { selector } => {
                match result.and_then(|json| crate::pr_review::parse_pr(&json)) {
                    Ok(pr) => {
                        let key = crate::pr_review::review_key(&pr);
                        self.open_pr_review(pr, key);
                        self.start_pr_viewed_fetch();
                    }
                    Err(why) => self.status = format!("Could not load PR {selector}: {why}"),
                }
            }
            PrGhJob::Diff { path } => match result {
                Ok(patch) => {
                    let Some(view) = self.editor.pr_review.as_mut() else {
                        return true;
                    };
                    view.diff = Some(crate::pr_review::split_diff_by_file(&patch));
                    self.open_pr_diff_section(&path);
                }
                Err(why) => self.status = format!("gh pr diff failed: {why}"),
            },
            PrGhJob::List => {
                use crate::widgets::list_picker::{ListPicker, ListPurpose, ListRow, PR_BY_NUMBER};
                let items = result.and_then(|json| crate::pr_review::parse_pr_list(&json));
                let items = match items {
                    Ok(items) if !items.is_empty() => items,
                    other => {
                        self.open_pr_number_prompt();
                        if let Err(why) = other {
                            self.status = format!("Could not list pull requests: {why}");
                        }
                        return true;
                    }
                };
                let mut rows: Vec<ListRow> = items
                    .into_iter()
                    .map(|p| ListRow {
                        id: p.url,
                        label: format!(
                            "#{:<5} {}{}  ({})",
                            p.number,
                            if p.draft { "[draft] " } else { "" },
                            p.title,
                            p.author
                        ),
                    })
                    .collect();
                rows.push(ListRow {
                    id: PR_BY_NUMBER.to_string(),
                    label: String::from("Another pull request by number or URL…"),
                });
                self.status.clear();
                self.list_picker = Some(ListPicker::new(
                    ListPurpose::ReviewPullRequest,
                    String::from("Review Pull Request"),
                    rows,
                ));
            }
            PrGhJob::Checks { url } => {
                // A failed re-read changes nothing: the next one retries.
                let Ok(pr) = result.and_then(|json| crate::pr_review::parse_pr(&json)) else {
                    return false;
                };
                let Some(view) = self.editor.pr_review.as_mut().filter(|v| v.pr.url == url) else {
                    return false;
                };
                let was_pending = view.has_pending_checks();
                view.pr.checks = pr.checks;
                view.move_selection(0);
                if was_pending && !view.has_pending_checks() {
                    self.status = format!("PR #{}: every check has finished", view.pr.number);
                }
            }
            PrGhJob::Log { name } => {
                let channel = "PR Checks";
                crate::output::clear(channel);
                // A failed fetch's reason goes where the log would have.
                let text = result.unwrap_or_else(|why| why);
                for line in text.lines() {
                    crate::output::push(channel, crate::output::OutputLevel::Info, line);
                }
                self.status = format!("{name}: log in OUTPUT > {channel}");
            }
        }
        true
    }

    /// PULL REQUEST tab keys (#365).
    pub(super) fn handle_pr_review_key(&mut self, key: KeyEvent) {
        let Some(view) = self.editor.pr_review.as_mut() else {
            return;
        };
        let page = (view.rows_visible as isize).max(1);
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => view.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => view.move_selection(-1),
            KeyCode::PageDown => view.move_selection(page),
            KeyCode::PageUp => view.move_selection(-page),
            KeyCode::Home => view.selected = 0,
            KeyCode::End => view.move_selection(isize::MAX / 2),
            KeyCode::Char(' ') => {
                let Some(path) = view.selected_file().map(|f| f.path.clone()) else {
                    return;
                };
                let store_path = Self::pr_viewed_path();
                let mut store = crate::pr_review::ViewedStore::load(&store_path);
                let now = store.toggle(&view.key, &path);
                if let Err(e) = store.save(&store_path) {
                    self.status = format!("Could not save the viewed mark: {e}");
                }
                // The mark flips at once; GitHub hears about it off the UI
                // thread, or once its viewed state has loaded.
                match view.github_id.clone() {
                    Some(pr_id) => {
                        let write = PrViewedWrite {
                            key: view.key.clone(),
                            pr_id,
                            path: path.clone(),
                            viewed: now,
                        };
                        // The fields directly, as `view` still borrows the
                        // editor: what `queue_pr_viewed_write` does.
                        self.pr_viewed_seq += 1;
                        self.pr_viewed_touched
                            .insert((write.key.clone(), write.path.clone()), self.pr_viewed_seq);
                        self.pr_viewed_queue.push_back(write);
                    }
                    None if self
                        .pr_viewed_fetch
                        .as_ref()
                        .is_some_and(|(k, _, _)| *k == view.key) =>
                    {
                        view.unsynced.insert(path.clone());
                    }
                    None => {}
                }
                if now {
                    view.viewed.insert(path);
                } else {
                    view.viewed.remove(&path);
                }
                view.move_selection(1);
            }
            KeyCode::Enter => {
                if view.selected_check().is_some() {
                    self.show_pr_check_log();
                } else {
                    self.open_pr_file_diff();
                }
            }
            KeyCode::Char('l') => self.show_pr_check_log(),
            KeyCode::Char('c') => self.checkout_pr_head(),
            KeyCode::Char('r') => {
                // By URL, so a PR opened from another repository refreshes
                // as itself.
                let selector = if view.pr.url.is_empty() {
                    view.pr.number.to_string()
                } else {
                    view.pr.url.clone()
                };
                self.start_pr_review(selector);
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                let n = view.pr.number;
                self.editor.close_active();
                self.status = format!("Left review of PR #{n}");
                self.leave_pr_checkout();
            }
            _ => {}
        }
    }

    /// Check the reviewed pull request's head out into a sibling worktree
    /// and add it to the workspace (#365), so its code has the language
    /// server, go-to-definition and tests. Fetching runs off the UI thread.
    pub(super) fn checkout_pr_head(&mut self) {
        let Some(view) = self.editor.pr_review.as_ref() else {
            return;
        };
        if self.pr_checkout_rx.is_some() {
            self.status = String::from("A pull request checkout is already fetching");
            return;
        }
        let number = view.pr.number;
        let slug = view.key.split('#').next().unwrap_or_default().to_string();
        // What review mode last checked out there, the one HEAD a refresh
        // may move away from without stranding commits.
        let previous = self
            .pr_checkout
            .as_ref()
            .filter(|c| c.number == number)
            .map(|c| c.oid.clone());
        let repo = self.workspace_root().to_path_buf();
        let Some(path) = crate::git::pr_worktree_path(&repo, number) else {
            self.status = format!(
                "No place beside {} to check PR #{number} out",
                repo.display()
            );
            return;
        };
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let remote = crate::git::remote_for_slug(&repo, &slug);
            // Canonical, like the workspace roots it is added to and later
            // removed from.
            let result = crate::git::checkout_pr_worktree(
                &repo,
                &remote,
                number,
                &path,
                previous.as_deref(),
            )
            .map(|wt| (path.canonicalize().unwrap_or(path), wt));
            let _ = tx.send(result);
        });
        self.pr_checkout_rx = Some((number, rx));
        self.status = format!("Fetching PR #{number}…");
    }

    /// Apply a finished pull request checkout. Returns true when it changed
    /// what is on screen.
    pub(super) fn poll_pr_checkout(&mut self) -> bool {
        let Some((number, rx)) = self.pr_checkout_rx.as_ref() else {
            return false;
        };
        let number = *number;
        let result = match rx.try_recv() {
            Ok(r) => r,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err(String::from("the checkout stopped without answering"))
            }
        };
        self.pr_checkout_rx = None;
        match result {
            Ok((path, wt)) => {
                let mut fresh = PrCheckout {
                    number,
                    path,
                    oid: wt.oid,
                    created: wt.created,
                };
                // Review closed, or another PR opened, while this fetched:
                // no review owns it, so it is settled like a left one.
                if self.editor.pr_review.as_ref().map(|v| v.pr.number) != Some(number) {
                    let outcome = self.settle_pr_checkout(fresh);
                    self.status = format!(
                        "PR #{number}'s checkout finished after its review closed; {outcome}"
                    );
                    return true;
                }
                match self.pr_checkout.take() {
                    // A refresh of the same checkout: still review mode's
                    // if review mode made it.
                    Some(old) if old.number == number => fresh.created |= old.created,
                    // Another PR's: settled before this one replaces it.
                    Some(old) => {
                        let _ = self.settle_pr_checkout(old);
                    }
                    None => {}
                }
                if !self.roots.iter().any(|r| r == fresh.path) {
                    self.add_workspace_folder(fresh.path.clone());
                }
                let short: String = fresh.oid.chars().take(8).collect();
                self.status = if fresh.created {
                    format!(
                        "PR #{number} checked out at {short} in {} — leaving review removes it if nothing changed",
                        fresh.path.display()
                    )
                } else {
                    format!(
                        "PR #{number} checked out at {short} in the existing {}, which leaving review keeps",
                        fresh.path.display()
                    )
                };
                self.pr_checkout = Some(fresh);
            }
            Err(why) => self.status = format!("Could not check out PR #{number}: {why}"),
        }
        true
    }

    /// Remove a pull request checkout review mode no longer needs, with its
    /// workspace folder and panes, unless something in it is worth keeping:
    /// a worktree that existed before review, an unsaved editor tab under
    /// it, edits or build output on disk, or new commits. Says which.
    pub(super) fn settle_pr_checkout(&mut self, checkout: PrCheckout) -> String {
        let PrCheckout {
            path, oid, created, ..
        } = checkout;
        if !path.exists() {
            return String::from("its checkout is already gone");
        }
        let kept = |why: &str| format!("kept its checkout at {}: {why}", path.display());
        if !created {
            return kept("it existed before review");
        }
        if let Some(file) = self.unsaved_file_under(&path) {
            return kept(&format!("{file} has unsaved changes"));
        }
        if let Some(why) = crate::git::pr_worktree_keep_reason(&path, &oid) {
            return kept(&why);
        }
        match crate::git::remove_worktree_lane(&path) {
            Ok(()) => {
                self.close_lane_panes(&path);
                if self.roots.iter().any(|r| r == path) {
                    self.remove_workspace_folder(path);
                }
                String::from("removed its checkout")
            }
            Err(why) => format!("could not remove {}: {why}", path.display()),
        }
    }

    /// Leaving review mode (#365): remove the pull request's checkout and
    /// its workspace folder, unless it holds work — edits, build output or
    /// new commits — which stay, with the reason in the status.
    pub(super) fn leave_pr_checkout(&mut self) {
        let Some(checkout) = self.pr_checkout.take() else {
            return;
        };
        let number = checkout.number;
        let outcome = self.settle_pr_checkout(checkout);
        self.status = format!("Left review of PR #{number}; {outcome}");
    }

    /// The selected file's section of the PR's patch, side by side. The
    /// patch is fetched once per tab with `gh pr diff`, off the UI thread.
    pub(super) fn open_pr_file_diff(&mut self) {
        let Some(view) = self.editor.pr_review.as_ref() else {
            return;
        };
        let Some(path) = view.selected_file().map(|f| f.path.clone()) else {
            return;
        };
        if view.diff.is_some() {
            self.open_pr_diff_section(&path);
            return;
        }
        let selector = if view.pr.url.is_empty() {
            view.pr.number.to_string()
        } else {
            view.pr.url.clone()
        };
        self.status = format!("Fetching the diff of PR #{}…", view.pr.number);
        self.spawn_pr_gh(
            PrGhJob::Diff { path },
            vec![String::from("pr"), String::from("diff"), selector],
        );
    }

    /// Open `path`'s section of the fetched patch in a diff tab.
    pub(super) fn open_pr_diff_section(&mut self, path: &str) {
        let Some(view) = self.editor.pr_review.as_ref() else {
            return;
        };
        let n = view.pr.number;
        let Some(section) = view.diff.as_ref().and_then(|d| d.get(path)).cloned() else {
            self.status = format!("{path} has no textual diff (binary or too large)");
            return;
        };
        let label = PathBuf::from(format!("PR #{n}: {path}"));
        match self.editor.open_git_diff_side_by_side(&label, &section) {
            Ok(()) => self.status = format!("PR #{n}: {path}"),
            Err(e) => self.status = format!("Could not open the diff: {e}"),
        }
    }

    /// Put the selected (or first failing) check's failing-step log in
    /// OUTPUT > PR Checks.
    pub(super) fn show_pr_check_log(&mut self) {
        let Some(view) = self.editor.pr_review.as_ref() else {
            return;
        };
        let check = view.selected_check().or_else(|| {
            view.pr
                .checks
                .iter()
                .find(|c| c.state == crate::pr_review::CheckState::Fail)
        });
        let Some(check) = check.cloned() else {
            self.status = String::from("No failing check");
            return;
        };
        let Some((run, job)) = check.url.as_deref().and_then(crate::pr_review::run_and_job) else {
            self.status = format!(
                "{} is not a GitHub Actions check; its page: {}",
                check.name,
                check.url.unwrap_or_default()
            );
            return;
        };
        self.status = format!("Fetching the log of {}…", check.name);
        self.spawn_pr_gh(
            PrGhJob::Log { name: check.name },
            crate::pr_review::log_args(run, job),
        );
    }

    /// Run `gh` with `args` in the active workspace root. Its stdout, or
    /// the first line of what it said went wrong: GitHub's own message when
    /// the API sent one, else gh's; or that gh is missing or signed out.
    pub(super) fn run_gh(&self, args: &[String]) -> Result<String, String> {
        let root = self.active_workspace_root();
        let mut cmd = std::process::Command::new(&self.gh_program);
        cmd.args(args).current_dir(&root);
        let out = crate::review_ops::output_retrying_busy(&mut cmd)
            .map_err(|e| crate::gh::spawn_error(&e, Some(root.as_path())))?;
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        if out.status.success() {
            return Ok(stdout);
        }
        if crate::gh::signed_out(out.status) {
            return Err(String::from(crate::gh::SIGNED_OUT));
        }
        let api_message = serde_json::from_str::<serde_json::Value>(&stdout)
            .ok()
            .and_then(|v| v.get("message")?.as_str().map(str::to_string));
        let stderr = String::from_utf8_lossy(&out.stderr);
        let first = |s: &str| s.trim().lines().next().map(str::to_string);
        Err(api_message
            .or_else(|| {
                let s = stderr.trim();
                serde_json::from_str::<serde_json::Value>(s)
                    .ok()
                    .and_then(|v| v.get("message")?.as_str().map(str::to_string))
                    .or_else(|| first(s))
            })
            .unwrap_or_else(|| String::from("gh failed")))
    }
}

/// Run `cmd` for at most `budget`, collecting stdout; None when it could not
/// start or ran out of time (it is then killed and reaped). For the few
/// short commands that still run on the UI thread.
pub(super) fn bounded_output(
    cmd: &mut std::process::Command,
    budget: std::time::Duration,
) -> Option<(std::process::ExitStatus, Vec<u8>)> {
    use std::io::Read;
    let deadline = std::time::Instant::now() + budget;
    let mut child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    let bytes = rx.recv_timeout(budget).ok();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return bytes.map(|b| (status, b)),
            Ok(None) if bytes.is_some() && std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}
