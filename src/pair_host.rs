//! In-process host for the `croft pair` pilot — the resident navigator.
//!
//! The App spawns the claude child and the pilot threads itself (the way it
//! hosts LSP servers) and drains their events once per tick; the pilot's
//! collab guest seat streams fence edits through the workspace relay exactly
//! as the standalone REPL did (src/pair.rs, 0.1.634). Nothing here touches
//! stdout/stderr: seated inside croft, the pilot's voice is [`PairEvent`]s.

use std::path::Path;
use std::process::Command;
use std::sync::mpsc::{Receiver, channel};

use anyhow::{Context, Result};

use crate::collab::position;
use crate::pair::{
    PairConfig, Pilot, Provider, claude_command, compose_ask_turn, compose_yield_turn,
    lock_briefly, seat_local, seat_pilot, send_turn, write_user_turn,
};

/// What the pilot surfaces to the App, drained by `poll` once per tick.
#[derive(Debug)]
pub enum PairEvent {
    /// An anchored note landed: `row` is 0-based, computed against the
    /// replica at land time (later edits move it; re-read via
    /// [`PairHost::notes_snapshot`]).
    NoteAdded {
        file: String,
        row: usize,
        body: String,
    },
    /// A turn finished. `failed` carries claude's error text when the turn
    /// errored for a reason other than a cancel.
    TurnDone {
        cancelled: bool,
        failed: Option<String>,
    },
    /// Model prose outside fences, plus pilot and claude diagnostics.
    Commentary(String),
    /// The claude child hung up: the conversation is gone and the host is
    /// unusable (the App surfaces this and drops the host; no auto-respawn,
    /// matching the LSP precedent).
    Died(String),
}

/// The seated navigator: owns the pilot (threads + claude child) and the
/// event stream out of it. Dropping the host tears the seat down (revert,
/// hang up, grace-kill).
pub struct PairHost {
    pilot: Option<Pilot>,
    events: Receiver<PairEvent>,
    name: String,
    /// The local model this seat rides (None on the claude backend, whose
    /// child owns model selection); shown in the idle badge via [`Self::title`].
    model_label: Option<String>,
    died: bool,
    /// True from a successful send until [`Self::poll`] delivers the
    /// turn's `TurnDone`: the pilot's reader clears `turn_active` BEFORE
    /// it queues `TurnEnd`, so without this gate a tick landing in that
    /// window saw an idle seat and started a new turn that stole the
    /// finished turn's origin and note counters.
    turn_pending: std::sync::atomic::AtomicBool,
    /// Last successful [`Self::caret_sites`] answer, served when the pilot
    /// mutex is contended (the UI thread never waits on it).
    sites_cache: std::sync::Mutex<Vec<u64>>,
    /// Last successful [`Self::notes_snapshot`] per file, same reason.
    notes_cache: std::sync::Mutex<NotesByFile>,
}

/// Per-file note snapshots: `(id, 0-based row, body)` per note.
type NotesByFile = std::collections::HashMap<String, Vec<(u64, usize, String)>>;

impl PairHost {
    /// Seat the navigator for a workspace on its configured provider: the
    /// real claude CLI, or a childless seat over a local Anthropic-compatible
    /// endpoint.
    pub fn spawn(cfg: PairConfig) -> Result<Self> {
        match &cfg.provider {
            Provider::Claude => {
                let cmd = claude_command(&cfg)?;
                Self::spawn_cmd(&cfg.socket, &cfg.name, cfg.task.as_deref(), cmd)
            }
            Provider::Local { base_url } => {
                let model = cfg
                    .model
                    .as_deref()
                    .context("a local provider needs --model (there is no CLI default)")?;
                let (tx, rx) = channel();
                let pilot = seat_local(&cfg.socket, &cfg.name, base_url, model, Some(tx))?;
                let mut host = Self::seated(pilot, rx, &cfg.name, cfg.task.as_deref())?;
                host.model_label = Some(model.to_string());
                Ok(host)
            }
        }
    }

    /// Seat the navigator on an explicit command (the e2e tests drive this
    /// with a scripted fake instead of the real claude CLI).
    pub fn spawn_cmd(socket: &Path, name: &str, task: Option<&str>, cmd: Command) -> Result<Self> {
        let (tx, rx) = channel();
        let pilot = seat_pilot(socket, name, cmd, Some(tx))?;
        Self::seated(pilot, rx, name, task)
    }

    /// Wrap a seated pilot and send its start task, if any.
    fn seated(
        pilot: Pilot,
        events: Receiver<PairEvent>,
        name: &str,
        task: Option<&str>,
    ) -> Result<Self> {
        let host = Self {
            pilot: Some(pilot),
            events,
            name: name.to_string(),
            model_label: None,
            died: false,
            turn_pending: std::sync::atomic::AtomicBool::new(false),
            sites_cache: std::sync::Mutex::new(Vec::new()),
            notes_cache: std::sync::Mutex::new(std::collections::HashMap::new()),
        };
        if let Some(task) = task {
            host.send_task(task)?;
        }
        Ok(host)
    }

    /// The caret name the navigator sits under.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Test seam: the pilot's state mutex, for pinning that UI-facing
    /// reads never block on it.
    #[cfg(test)]
    pub(crate) fn state_for_tests(
        &self,
    ) -> Option<std::sync::Arc<std::sync::Mutex<crate::pair::PairState>>> {
        self.pilot.as_ref().map(|p| std::sync::Arc::clone(&p.state))
    }

    /// This seat's site id in every live file: the navigator's wire
    /// identity. The App paints these carets in the navigator accent and
    /// clears exactly them on unseat — display names are neither unique (a
    /// human may pick the same one) nor length-stable (carets truncate
    /// their name to 24 chars at ingest).
    pub fn caret_sites(&self) -> Vec<u64> {
        let Some(p) = self.pilot.as_ref() else {
            return Vec::new();
        };
        // try_lock: this runs on every render, and the pilot's threads hold
        // the state mutex across BLOCKING relay writes that only complete
        // once the App drains the relay — waiting here closes a circular
        // wait and hangs the editor. Contended, serve the last answer.
        match p.state.try_lock() {
            Ok(st) => {
                let sites = st.my_site_ids();
                *self.sites_cache.lock().unwrap() = sites.clone();
                sites
            }
            Err(_) => self.sites_cache.lock().unwrap().clone(),
        }
    }

    /// What the navigator last saw of `file` (None = it never looked
    /// there). The proactive trigger compares the live buffer against this
    /// to decide whether a NEW construct completed.
    pub fn last_seen(&self, file: &str) -> Option<String> {
        // Bounded lock (see lock_briefly): the proactive trigger DECIDES on
        // this value, so a spurious None under microsecond pump contention
        // would silently swallow a look. A wedged pilot still answers None.
        self.pilot
            .as_ref()
            .and_then(|p| lock_briefly(&p.state).and_then(|st| st.last_seen_of(file)))
    }

    /// The badge label: the caret name, plus the local model when this seat
    /// rides a local endpoint (so the badge says who is actually typing).
    pub fn title(&self) -> String {
        match &self.model_label {
            Some(model) => format!("{} ({model})", self.name),
            None => self.name.clone(),
        }
    }

    /// True while a turn is streaming OR finished but not yet consumed via
    /// [`Self::poll`]: a new ask/yield must wait for it, else the shared
    /// comment-only flag gets clobbered mid-stream (and a turn started in
    /// the finish window would steal the previous turn's origin).
    pub fn is_busy(&self) -> bool {
        if self.turn_pending.load(std::sync::atomic::Ordering::Relaxed) {
            return true;
        }
        // Bounded lock (see lock_briefly): this gates asks, yields and the
        // proactive trigger, so an instant try_lock misreported a quiet
        // seat as busy whenever the pump's microsecond hold collided with
        // the check. Only a genuinely wedged pilot answers busy here.
        self.pilot.as_ref().is_some_and(|p| {
            lock_briefly(&p.state)
                .map(|st| st.turn_active())
                .unwrap_or(true)
        })
    }

    /// Everything the pilot produced since the last tick: commentary and
    /// notes from its channel, turn ends from the reader, and a one-shot
    /// Died when claude's stdout closed.
    pub fn poll(&mut self) -> Vec<PairEvent> {
        let mut out: Vec<PairEvent> = self.events.try_iter().collect();
        if let Some(p) = &self.pilot {
            while let Ok(end) = p.turn_rx.try_recv() {
                // The App has now SEEN the turn end: only here does the
                // busy gate release (see `turn_pending`).
                self.turn_pending
                    .store(false, std::sync::atomic::Ordering::Relaxed);
                out.push(PairEvent::TurnDone {
                    cancelled: end.cancelled,
                    failed: (end.is_error && !end.cancelled).then(|| end.text.clone()),
                });
            }
            if !self.died && p.reader_finished() {
                self.died = true;
                out.push(PairEvent::Died(String::from(
                    "claude exited; re-activate the navigator to reseat it",
                )));
            }
        }
        out
    }

    /// Send one task line to the conversation (the `@<file> <task>` form
    /// focuses a buffer, as in the REPL). Non-blocking: completion arrives
    /// as a [`PairEvent::TurnDone`].
    pub fn send_task(&self, line: &str) -> Result<()> {
        let p = self.pilot.as_ref().context("navigator pilot is gone")?;
        if self.turn_pending.load(std::sync::atomic::Ordering::Relaxed) {
            anyhow::bail!("the navigator is mid-turn; wait for it to finish");
        }
        let sent = send_turn(&p.state, &p.sink, line);
        if sent.is_ok() {
            self.turn_pending
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
        sent
    }

    /// Ask turn: the navigator may edit. `range` is the invoked 0-based
    /// line span (a single line when both ends match), `selection` the
    /// selected text (may be empty), `content` the caller's current buffer.
    pub fn send_ask_turn(
        &self,
        file: &str,
        range: (usize, usize),
        selection: &str,
        instruction: &str,
        content: &str,
    ) -> Result<()> {
        let p = self.pilot.as_ref().context("navigator pilot is gone")?;
        let staged = {
            // Bounded lock (see lock_briefly): a wedged pilot cannot hang
            // the keypress, transient pump contention succeeds on retry.
            let Some(mut st) = lock_briefly(&p.state) else {
                anyhow::bail!("the navigator is mid-turn; wait for it to finish");
            };
            if st.turn_active() || self.turn_pending.load(std::sync::atomic::Ordering::Relaxed) {
                // turn_pending too: between the pilot finishing and poll()
                // consuming its TurnDone, turn_active is already false, and
                // a send admitted here would overwrite the unconsumed
                // turn's staging (origin, counters, target).
                anyhow::bail!("the navigator is mid-turn; wait for it to finish");
            }
            let staged = st.stage_turn(file, content, false);
            st.park_caret(file, range.0); // visible presence at the ask site
            staged
        };
        let body = compose_ask_turn(file, range, selection, instruction, content);
        self.commit_or_unstage(write_user_turn(&p.state, &p.sink, body), &p.state, staged)
    }

    /// Yield turn: the driver hands the navigator the floor. Comment-only,
    /// host-enforced; the turn carries the diff since its last look.
    pub fn send_yield_turn(&self, file: &str, content: &str) -> Result<()> {
        let p = self.pilot.as_ref().context("navigator pilot is gone")?;
        let staged = {
            // Bounded lock (see lock_briefly): a wedged pilot cannot hang
            // the keypress, transient pump contention succeeds on retry.
            let Some(mut st) = lock_briefly(&p.state) else {
                anyhow::bail!("the navigator is mid-turn; wait for it to finish");
            };
            if st.turn_active() || self.turn_pending.load(std::sync::atomic::Ordering::Relaxed) {
                // turn_pending too: between the pilot finishing and poll()
                // consuming its TurnDone, turn_active is already false, and
                // a send admitted here would overwrite the unconsumed
                // turn's staging (origin, counters, target).
                anyhow::bail!("the navigator is mid-turn; wait for it to finish");
            }
            st.stage_turn(file, content, true)
        };
        let diff = staged
            .prior_seen
            .as_ref()
            .filter(|o| o.as_str() != content)
            .map(|o| {
                similar::TextDiff::from_lines(o.as_str(), content)
                    .unified_diff()
                    .context_radius(2)
                    .to_string()
            });
        let body = compose_yield_turn(file, content, diff.as_deref());
        self.commit_or_unstage(write_user_turn(&p.state, &p.sink, body), &p.state, staged)
    }

    /// Drop every anchored note (the palette's "Navigator: Clear Notes").
    pub fn clear_notes(&self) {
        // Bounded lock (see lock_briefly): a clear that still cannot get
        // the mutex is skipped; the notes are there and the next press
        // works, and a wedged pilot costs at most the 100ms budget.
        if let Some(p) = &self.pilot
            && let Some(mut st) = lock_briefly(&p.state)
        {
            st.clear_all_notes();
        }
    }

    /// Test-only blocking variant of [`Self::add_note_now`]: waits out the
    /// file's bootstrap so an e2e can seed notes right after a spawn.
    #[cfg(test)]
    pub fn add_note(&self, file: &str, row: usize, body: &str) -> Option<u64> {
        let p = self.pilot.as_ref()?;
        crate::pair::inject_note(&p.state, file, row, body)
    }

    /// Anchor a note from the app side (turn commentary landing as a
    /// comment box) when the file is already live — no bootstrap wait, the
    /// App's tick thread must never block. None = not live, nothing to
    /// anchor to.
    pub fn add_note_now(&self, file: &str, row: usize, body: &str) -> Option<u64> {
        let p = self.pilot.as_ref()?;
        crate::pair::inject_note_now(&p.state, file, row, body)
    }

    /// Drop exactly one note (the box's Ignore button).
    pub fn remove_note(&self, id: u64) {
        // Bounded lock (see lock_briefly): an Ignore that still cannot
        // get the mutex is skipped; the box stays and the next press works.
        if let Some(p) = &self.pilot
            && let Some(mut st) = lock_briefly(&p.state)
        {
            st.remove_note(id);
        }
    }

    /// Append one line to a note's body (the driver replied in its box).
    pub fn append_to_note(&self, id: u64, line: &str) {
        // Bounded lock (see lock_briefly): losing the echo line in the box
        // when the mutex stays held is cosmetic; the reply itself already
        // went.
        if let Some(p) = &self.pilot
            && let Some(mut st) = lock_briefly(&p.state)
        {
            st.append_to_note(id, line);
        }
    }

    /// Reply turn: the driver answered a note inside its comment box.
    /// Comment-only, host-enforced, like a yield.
    pub fn send_reply_turn(
        &self,
        file: &str,
        row: usize,
        note_body: &str,
        reply: &str,
        content: &str,
    ) -> Result<()> {
        let p = self.pilot.as_ref().context("navigator pilot is gone")?;
        let staged = {
            // Bounded lock (see lock_briefly): a wedged pilot cannot hang
            // the keypress, transient pump contention succeeds on retry.
            let Some(mut st) = lock_briefly(&p.state) else {
                anyhow::bail!("the navigator is mid-turn; wait for it to finish");
            };
            if st.turn_active() || self.turn_pending.load(std::sync::atomic::Ordering::Relaxed) {
                // turn_pending too: between the pilot finishing and poll()
                // consuming its TurnDone, turn_active is already false, and
                // a send admitted here would overwrite the unconsumed
                // turn's staging (origin, counters, target).
                anyhow::bail!("the navigator is mid-turn; wait for it to finish");
            }
            let staged = st.stage_turn(file, content, true);
            st.park_caret(file, row); // visible presence at the answered note
            staged
        };
        let body = crate::pair::compose_reply_turn(file, row, note_body, reply, content);
        self.commit_or_unstage(write_user_turn(&p.state, &p.sink, body), &p.state, staged)
    }

    /// A sent turn arms the busy gate; a failed one undoes its staged look
    /// so the seat is exactly as it was before the attempt.
    fn commit_or_unstage(
        &self,
        sent: Result<()>,
        state: &std::sync::Arc<std::sync::Mutex<crate::pair::PairState>>,
        staged: crate::pair::StagedTurn,
    ) -> Result<()> {
        match &sent {
            Ok(()) => self
                .turn_pending
                .store(true, std::sync::atomic::Ordering::Relaxed),
            // Bounded (see lock_briefly): the keypress thread must not wait
            // on a wedged pilot, but the unstage must not be LOST either (a
            // stale look breaks the yield diff and latches comment_only) —
            // hand it to a thread that may wait.
            Err(_) => match lock_briefly(state) {
                Some(mut st) => st.unstage_turn(staged),
                None => {
                    let state = std::sync::Arc::clone(state);
                    std::thread::spawn(move || {
                        state.lock().unwrap().unstage_turn(staged);
                    });
                }
            },
        }
        sent
    }

    /// Tear the seat down synchronously (revert, hang up, grace-kill, join).
    /// Blocks up to the grace period, so call this only on an exit path where
    /// the child MUST be reaped before the process ends — never on the tick
    /// loop, where `Drop` detaches the same work instead.
    pub fn shutdown_blocking(mut self) {
        if let Some(p) = self.pilot.take() {
            p.shutdown();
        }
    }

    /// (id, 0-based row, body) of every note anchored in `file`, rows
    /// computed against the live replica so they track concurrent edits.
    pub fn notes_snapshot(&self, file: &str) -> Vec<(u64, usize, String)> {
        let Some(p) = &self.pilot else {
            return Vec::new();
        };
        // try_lock (UI tick path, see caret_sites): contended, serve the
        // last computed snapshot for this file.
        let Ok(st) = p.state.try_lock() else {
            return self
                .notes_cache
                .lock()
                .unwrap()
                .get(file)
                .cloned()
                .unwrap_or_default();
        };
        // Skip splitting the whole document (this runs every tick for the
        // active file) when it has no notes to place.
        let snap = if st.notes_in(file).next().is_none() {
            Vec::new()
        } else if let Some(lines) = st.doc_lines(file) {
            st.notes_in(file)
                .map(|n| (n.id, position(&lines, n.offset).0, n.body.clone()))
                .collect()
        } else {
            Vec::new()
        };
        self.notes_cache
            .lock()
            .unwrap()
            .insert(file.to_string(), snap.clone());
        snap
    }
}

/// Teardown threads detached by [`Drop`]: an unseat's grace-kill runs on
/// its own thread, and a thread that dies with the process leaves the
/// claude child running (orphaned, still holding its MCP subprocesses).
/// The exit path joins these so every detached teardown finishes first.
static TEARDOWNS: std::sync::Mutex<Vec<std::thread::JoinHandle<()>>> =
    std::sync::Mutex::new(Vec::new());

/// Join every teardown thread [`Drop`] detached. Called on the exit path
/// after the live host's own `shutdown_blocking`; bounded by the pilots'
/// grace-kill (about two seconds each, and stacked teardowns are rare).
pub fn join_teardowns() {
    let handles: Vec<_> = std::mem::take(&mut *TEARDOWNS.lock().unwrap());
    for h in handles {
        let _ = h.join();
    }
}

/// Pull the FINISHED handles out of the registry, leaving the running ones:
/// each unseat sweeps its predecessors so repeated unseat/reseat cycles do
/// not grow the registry without bound. Joining the returned handles is
/// instant (the threads have exited) but happens outside the lock anyway.
fn sweep_finished(reg: &mut Vec<std::thread::JoinHandle<()>>) -> Vec<std::thread::JoinHandle<()>> {
    let mut done = Vec::new();
    let mut i = 0;
    while i < reg.len() {
        if reg[i].is_finished() {
            done.push(reg.swap_remove(i));
        } else {
            i += 1;
        }
    }
    done
}

impl Drop for PairHost {
    fn drop(&mut self) {
        // Detach the teardown: the grace-kill blocks up to 2s (claude lingers
        // on stdin EOF), and this Drop runs on the App's tick thread when the
        // navigator is unseated (toggle off / disabled record). The detached
        // thread owns the pilot and reverts, hangs up, and grace-kills on its
        // own. Exit paths that must reap the child first use
        // `shutdown_blocking` instead, plus [`join_teardowns`] for drops
        // that happened shortly before exit.
        if let Some(p) = self.pilot.take() {
            let handle = std::thread::spawn(move || p.shutdown());
            let done = {
                let mut reg = TEARDOWNS.lock().unwrap();
                let done = sweep_finished(&mut reg);
                reg.push(handle);
                done
            };
            for h in done {
                let _ = h.join();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::sweep_finished;

    /// The registry sweep keeps only RUNNING teardowns: finished ones are
    /// pulled out (and joined by the caller), so repeated unseat/reseat
    /// cycles cannot grow the registry without bound.
    #[test]
    fn sweep_finished_pulls_only_the_finished_handles() {
        let (block_tx, block_rx) = std::sync::mpsc::channel::<()>();
        let finished = std::thread::spawn(|| {});
        // A finished thread may take a beat to be observably finished.
        while !finished.is_finished() {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let running = std::thread::spawn(move || {
            let _ = block_rx.recv();
        });
        let mut reg = vec![finished, running];
        let done = sweep_finished(&mut reg);
        assert_eq!(done.len(), 1, "exactly the finished handle is pulled");
        assert_eq!(reg.len(), 1, "the running teardown stays registered");
        assert!(!reg[0].is_finished(), "and it is the running one");
        block_tx.send(()).unwrap();
        for h in done.into_iter().chain(reg) {
            h.join().unwrap();
        }
    }
}
