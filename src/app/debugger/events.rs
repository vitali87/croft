//! The debug sessions' events, drained every frame: stops and the paused line, output, a
//! session ending (and which one, in a compound) and the message it leaves.

use super::*;

/// Where the session at `index` sits once every index in `removed` has been
/// taken out of the set: `None` if it was removed itself, otherwise shifted
/// down by one for each removal below it (#567).
pub(in crate::app) fn index_after_removals(index: usize, removed: &[usize]) -> Option<usize> {
    (!removed.contains(&index)).then(|| index - removed.iter().filter(|&&r| r < index).count())
}

/// The end-of-session feedback line. When breakpoints were set but the program
/// exited without ever stopping, say so explicitly — that is the common "I set a
/// breakpoint and nothing happened" confusion (e.g. running a library module
/// whose breakpointed code is never called). Otherwise a plain end message.
pub(in crate::app) fn debug_end_message(had_breakpoints: bool, ever_stopped: bool) -> &'static str {
    if had_breakpoints && !ever_stopped {
        "Program exited without hitting a breakpoint"
    } else {
        "Debug session ended"
    }
}

impl App {
    /// Drain the debug session each frame. Returns true if anything changed
    /// (so the frame loop redraws). Mirrors the paused location into the
    /// editor's stop-line, surfaces program output + stop reasons, and tears
    /// the session down when the debuggee exits.
    pub fn poll_dap(&mut self) -> bool {
        use crate::dap::session::{DapEvent, SessionPhase};
        if self.debug_sessions.is_empty() {
            return false;
        }
        let mut changed_background = false;
        // EVERY session is polled, not just the focused one (#310). `poll` is
        // the protocol state machine, not a passive read: it is where
        // `initialized` triggers the breakpoint push and `configurationDone`,
        // and where js-debug's reverse requests are answered. An unpolled
        // member never leaves the gate - its debuggee never runs, its
        // breakpoints never bind, and a js-debug parent blocks forever on a
        // reverse request nobody answered. It would be started, named in the
        // status, and dead.
        //
        // Only the FOCUSED session's events drive the UI. A background
        // member's output still reaches the console, tagged with its name,
        // and everything else it reports is queued on it and replayed when it
        // is focused (#567): dropping its `stopped` left a breakpoint hit that
        // nothing showed, so the member looked hung.
        const BACKLOG_CAP: usize = 256;
        let focused = self.debug_sessions.focused_index();
        // A session focused since the last poll first replays what it did
        // while in the background.
        let mut events = self.debug_sessions.take_focused_backlog();
        let mut background_ended: Vec<usize> = Vec::new();
        // (session index, category, text) of each background output event.
        let mut background_output: Vec<(usize, String, String)> = Vec::new();
        // By index, not name: a compound can list one configuration twice,
        // so two members can share a name.
        let mut background_stopped: Option<usize> = None;
        for (index, named) in self.debug_sessions.iter_named_mut_indexed() {
            let drained = named.session.poll();
            if index == focused {
                events.extend(drained);
                continue;
            }
            changed_background |= !drained.is_empty();
            for ev in drained {
                match ev {
                    // `exited` and `terminated` both end a member and often
                    // arrive together: name it once, or the removal below
                    // takes a live sibling for the second copy.
                    DapEvent::Terminated => {
                        if background_ended.last() != Some(&index) {
                            background_ended.push(index);
                        }
                    }
                    DapEvent::Output { category, text }
                        if crate::dap::session::output_is_user_visible(&category) =>
                    {
                        background_output.push((index, category, text));
                    }
                    DapEvent::Output { .. } => {}
                    other => {
                        if matches!(other, DapEvent::Stopped { .. }) {
                            background_stopped.get_or_insert(index);
                        }
                        named.backlog.push(other);
                        if named.backlog.len() > BACKLOG_CAP {
                            let overflow = named.backlog.len() - BACKLOG_CAP;
                            named.backlog.drain(..overflow);
                        }
                    }
                }
            }
        }
        for (index, category, text) in background_output {
            self.debug_console_push_output(index, &category, &text, true);
        }
        // Highest index first, so removing one does not move the next.
        let mut ended_names = Vec::new();
        // Where the stopped member sits once the ended ones are gone: gone
        // itself if it also ended, else shifted down past every removal below.
        let background_stopped =
            background_stopped.and_then(|i| index_after_removals(i, &background_ended));
        for index in background_ended.into_iter().rev() {
            if let Some(mut gone) = self.debug_sessions.remove(index) {
                mirror_open_console_lines(
                    &mut gone.console,
                    &self.debug_console,
                    self.debug_console_dropped,
                );
                gone.session.disconnect();
                ended_names.push(gone.name);
            }
        }
        // `stopAll: true` (#567): a background member ending takes the set
        // down with it, the same as the focused one ending does below.
        if self.debug_stop_all && !ended_names.is_empty() && !self.debug_sessions.is_empty() {
            self.debug_stop();
            self.status = format!(
                "{} ended — stopAll stopped the rest of the compound",
                ended_names.join(", ")
            );
            self.run_post_debug_tasks();
            return true;
        }
        if !ended_names.is_empty() && !self.debug_sessions.is_empty() {
            self.report_member_ended(&ended_names.join(", "));
        }
        // A background member stopped (a breakpoint, an exception, a pause).
        // If the member being looked at is still running, show the stopped
        // one, as VS Code does; otherwise say so rather than steal the view
        // from a stop the user is already inspecting.
        if let Some(index) = background_stopped
            && let Some(name) = self.debug_sessions.names().get(index).cloned()
        {
            let focused_is_stopped = self
                .debug_sessions
                .focused()
                .is_some_and(|s| s.phase == SessionPhase::Stopped)
                || events.iter().any(|e| matches!(e, DapEvent::Stopped { .. }));
            // A focused member that ended this tick is torn down below, and
            // the teardown reads the FOCUSED member: moving the view first
            // would carry the ending into its backlog and leave it running.
            let focused_ended = self
                .debug_sessions
                .focused()
                .is_some_and(|s| s.phase == SessionPhase::Terminated)
                || events.iter().any(|e| matches!(e, DapEvent::Terminated));
            if focused_is_stopped || focused_ended {
                self.status =
                    format!("{name} stopped in the background · Cmd+Opt+Shift+G switches to it");
            } else {
                self.refocus_debug_session(index, &mut events);
                self.status = format!("{name} stopped · now showing it");
            }
        }
        let mut changed = !events.is_empty() || changed_background;
        for ev in events {
            match ev {
                // Route program output to the debug console (a line per
                // newline, #932), but drop debugpy's own `telemetry` banner
                // (the `ptvsd` / `debugpy` noise).
                DapEvent::Output { category, text }
                    if crate::dap::session::output_is_user_visible(&category) =>
                {
                    let focused = self.debug_sessions.focused_index();
                    self.debug_console_push_output(focused, &category, &text, false);
                }
                // REPL submissions echo "❯ expr" then the result; watch/hover
                // results are consumed elsewhere.
                DapEvent::Evaluated {
                    context,
                    expression,
                    result,
                    ..
                } if context == "repl" => {
                    self.debug_console_push(format!("❯ {expression}"));
                    // A console line each (#932): a traceback is many lines,
                    // and as one entry only its first was ever visible.
                    for line in result.lines() {
                        self.debug_console_push(line.to_string());
                    }
                }
                DapEvent::Evaluated {
                    context,
                    expression,
                    result,
                    success,
                } if context == "watch" => {
                    self.watch_vals.insert(expression, (result, success));
                }
                // Copy Value's whole value (#1546), or, when the adapter
                // could not give it, the value the row showed.
                DapEvent::Evaluated {
                    context,
                    expression,
                    result,
                    success,
                } if context == "clipboard" => {
                    if self
                        .debug_copy_pending
                        .as_ref()
                        .is_some_and(|(expr, _)| *expr == expression)
                        && let Some((_, fallback)) = self.debug_copy_pending.take()
                    {
                        let text = if success { result } else { fallback };
                        self.status = self.copy_where_pasted(&text).status("value");
                    }
                }
                // Fresh stack + frame selection just landed: this is the
                // moment each watch expression re-evaluates against the new
                // top frame (#112).
                DapEvent::InspectionUpdated => {
                    self.refresh_watches();
                    self.refresh_inline_values();
                    changed = true;
                }
                DapEvent::Stopped { reason, .. } => {
                    self.run_debug.feedback = Some(format!("Paused ({reason})"));
                    self.run_debug.feedback_is_error = false;
                    self.watch_baseline_pending = true;
                    self.debug_pause_reason = Some(reason);
                }
                // What the exception stop threw (#864): the panel's pill and
                // the status line, which has the width the pill lacks.
                // Step Into Target (#1543): one call steps straight in, several
                // are offered by label, and a line with none steps in plainly.
                DapEvent::StepInTargets { targets } => {
                    self.offer_step_in_targets(targets);
                }
                DapEvent::ExceptionInfo { summary } => {
                    self.run_debug.feedback = Some(format!("Paused (exception): {summary}"));
                    self.status = format!("Paused on exception: {summary}");
                }
                DapEvent::BreakpointsUpdated => {
                    // Mirror the adapter's unverified set into the editor so the
                    // gutter can hollow out inert breakpoints, and warn once if
                    // any failed to bind (the "I set a breakpoint and it just
                    // ran" case).
                    let unverified: std::collections::HashMap<
                        PathBuf,
                        std::collections::BTreeSet<usize>,
                    > = self
                        .debug_sessions
                        .focused()
                        .map(|s| s.unverified_breakpoints.clone().into_iter().collect())
                        .unwrap_or_default();
                    let total: usize = unverified.values().map(|s| s.len()).sum();
                    self.editor.unverified_breakpoints = unverified;
                    if total > 0 {
                        self.status = format!(
                            "{total} breakpoint(s) could not bind (no executable code on that line)"
                        );
                    }
                }
                _ => {}
            }
        }
        let phase = self.debug_sessions.focused().map(|s| s.phase);
        match phase {
            Some(SessionPhase::Stopped) => {
                self.debug_ever_stopped = true;
                let loc = self
                    .debug_sessions
                    .focused()
                    .and_then(|s| s.current_location.clone());
                // Said once this stop's stack trace has placed it, so the
                // announcement names the file and line (#1547). The frames
                // are cleared at each stop and the location is not, so an
                // empty stack means the location is still the last stop's,
                // and so does a top frame without a source: that stop is
                // read without a file and line rather than with stale ones.
                let top = self
                    .debug_sessions
                    .focused()
                    .and_then(|s| s.stack_frames.first())
                    .map(|f| (f.name.clone(), f.path.is_some()));
                if let Some((frame, has_source)) = top
                    && let Some(reason) = self.debug_pause_reason.take()
                {
                    let at = if has_source { loc.as_ref() } else { None };
                    let said = self.debug_pause_announcement(&reason, at, &frame);
                    self.announce_event(said);
                    changed = true;
                }
                if let Some(loc) = loc
                    && self.editor.stop_line.as_ref() != Some(&loc)
                {
                    self.editor.stop_line = Some(loc);
                    // A re-stop at the same location can re-arm the arrow
                    // without a fresh InspectionUpdated on this tick; rebuild
                    // the trailers here too so arrow and values never split
                    // (#136 review). Idempotent over already-fetched data.
                    self.refresh_inline_values();
                    changed = true;
                }
            }
            Some(SessionPhase::Terminated) => {
                self.debug_pause_reason = None;
                self.editor.stop_line = None;
                self.editor.unverified_breakpoints.clear();
                // A session that ends on its own must take croft's own
                // breakpoint with it, exactly as Shift+F5 does (#373):
                // this is the path a passing test takes, so it is the
                // common one, not the exceptional one.
                self.disarm_failure_breakpoint();
                self.reset_watch_runtime();
                self.clear_inline_values();
                // Reply to the adapter's own `terminated`/`exited` with the
                // graceful `disconnect` handshake before dropping it. js-debug
                // only retires its detached watchdog on this handshake, never on
                // a SIGKILL of the server, so the natural-exit path used to leak
                // the watchdog by dropping straight to a kill.
                // The session that ENDED, not the whole set (#310). Dropping
                // the set reaches its siblings as a SIGKILL through the
                // transport's drop - exactly the kill this handshake exists to
                // avoid - and would make every compound behave as
                // `stopAll: true`, which is neither VS Code's default nor what
                // the file asked for.
                let ended = self.debug_sessions.focused_index();
                let ended_name = self
                    .debug_sessions
                    .focused_name()
                    .unwrap_or("the session")
                    .to_string();
                // The adapter refused to start the program (#264): its reason
                // is the message, not "Debug session ended".
                let launch_error = self
                    .debug_sessions
                    .focused()
                    .and_then(|s| s.launch_error.clone());
                if let Some(mut gone) = self.debug_sessions.remove(ended) {
                    mirror_open_console_lines(
                        &mut gone.console,
                        &self.debug_console,
                        self.debug_console_dropped,
                    );
                    gone.session.disconnect();
                }
                // `stopAll: true` (#567): the siblings go too, and the rest
                // of this arm reports the ended run as for a lone session.
                let stopped_all = self.debug_stop_all && !self.debug_sessions.is_empty();
                if stopped_all {
                    self.debug_stop();
                    self.status =
                        format!("{ended_name} ended — stopAll stopped the rest of the compound");
                }
                if !self.debug_sessions.is_empty() {
                    // Siblings are still running, so the debugger is not torn
                    // down: the view moves to one of them.
                    self.report_member_ended(&ended_name);
                    return true;
                }
                // The watchdog reparents to init the moment the server dies;
                // sweep it (and any tree a crashed prior croft left behind)
                // after a short grace so it has settled into the orphan state.
                crate::dap::reaper::reap_orphaned_js_debug_async(std::time::Duration::from_millis(
                    REAPER_TEARDOWN_GRACE_MS,
                ));
                let had_breakpoints = !self.editor.breakpoints.is_empty();
                match launch_error {
                    Some(reason) => {
                        let msg = format!("Could not start debugging: {reason}");
                        self.status = msg.clone();
                        self.run_debug.feedback = Some(msg);
                        self.run_debug.feedback_is_error = true;
                    }
                    None => {
                        let msg =
                            debug_end_message(had_breakpoints, self.debug_ever_stopped).to_string();
                        // The status bar still read "Debugging x — F5
                        // continue · F10 step over" after the end (#867).
                        // stopAll's own explanation above is left standing,
                        // and a postDebugTask below may report over this.
                        if !stopped_all {
                            self.status = msg.clone();
                        }
                        self.run_debug.feedback = Some(msg);
                        self.run_debug.feedback_is_error = false;
                    }
                }
                // Keep `debug_console` so its output stays visible after the run;
                // the panel renders it until the next session starts.
                self.run_debug.session_ended = true;
                self.run_post_debug_tasks();
                changed = true;
            }
            _ => {
                if self.editor.stop_line.take().is_some() {
                    // The trailers clear wherever the stop arrow clears
                    // (#136 review): this arm catches every phase change the
                    // four explicit resume sites don't, guarded the same way
                    // so ordinary Running ticks pay nothing.
                    self.clear_inline_values();
                    changed = true;
                }
            }
        }
        // Rebuild the Call Stack / Variables panel from the (now-updated) session.
        self.refresh_debug_panel();
        changed
    }
}
