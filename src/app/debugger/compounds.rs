//! Compounds and the tasks around a launch: launching a compound's members, parking a
//! launch behind its `preLaunchTask` until the task ends, `postDebugTask`, a member
//! ending, and switching between the running sessions.

use super::*;

/// Whether a finished pane command settles the pending preLaunchTask: same
/// pane, and the captured command line is the task's (the shell may report
/// it with surrounding whitespace). Pure, for tests.
pub(in crate::app) fn finished_settles_pending(
    pane: u64,
    cmd: &str,
    pending_pane: u64,
    pending_cmd: &str,
) -> bool {
    pane == pending_pane && cmd.trim() == pending_cmd.trim()
}

/// The FinishedCommand match is the settling signal for a parked launch, but
/// it depends on OSC 133 shell-integration marks. This is the fallback for
/// panes that will never produce one: the pane vanished, or its foreground
/// returned to a plain shell (the task is over) and the grace period passed
/// without a mark. Returns the abort reason, or `None` to keep waiting. Pure.
pub(in crate::app) fn pending_abort_reason(
    pane_exists: bool,
    foreground_is_shell: bool,
    waited: std::time::Duration,
) -> Option<&'static str> {
    if !pane_exists {
        return Some("its terminal pane closed");
    }
    if foreground_is_shell && waited > PRELAUNCH_MARK_GRACE {
        return Some("the task produced no command mark (shell integration missing in that pane?)");
    }
    None
}

impl App {
    /// Launch a launch.json compound: resolve its members against the
    /// un-deduped config list, refuse what croft cannot honour, then start
    /// one or several sessions. Shared by the picker and by F5/restart once
    /// the compound is the selected target (#567).
    pub(in crate::app) fn launch_compound(&mut self, compound: &crate::dap::configs::Compound) {
        // Resolve first: a compound naming a configuration no
        // launch.json declares is a config error worth
        // reporting now, separately from the unbuilt feature.
        // The un-deduped list: `debug_configs` is the picker's
        // display list and drops a lower-precedence duplicate by
        // name, which is the very entry a `.vscode` compound
        // naming that name has to bind.
        let all = crate::dap::configs::discover_configs_all(&self.active_workspace_root());
        let folders = self.workspace_folders();
        // Every `${input:…}` the members and the compound's task use is asked
        // once, up front, and shared (#1424).
        if let Ok(members) = crate::dap::configs::resolve_compound(compound, &all, &folders) {
            let mut refs = Vec::new();
            let mut defs = Vec::new();
            for m in &members {
                for r in m.input_refs() {
                    if !refs.contains(&r) {
                        refs.push(r);
                    }
                }
                defs.extend(m.inputs.iter().cloned());
            }
            if let Some(label) = &compound.pre_launch_task {
                self.task_inputs(label, &mut refs, &mut defs);
            }
            let then = InputThen::Compound(Box::new(compound.clone()));
            match self.ask_inputs(&refs, &defs, &compound.answers, then) {
                Ok(true) => {}
                Ok(false) => return,
                Err(e) => {
                    self.debug_error(format!("compound \"{}\": {e}", compound.name));
                    return;
                }
            }
        }
        // #310 defers compounds that need SEVERAL sessions at
        // once. A compound naming ONE configuration needs one,
        // which croft has run since #250 — so the arity has to
        // be READ here rather than assumed. Reporting the
        // multi-session limit for a one-member compound cites
        // a limitation that does not apply and tells the user
        // something false about their own launch.json.
        match crate::dap::configs::resolve_compound(compound, &all, &folders) {
            Err(e) => self.debug_error(e),
            // A one-member compound carrying keys croft does
            // not honour is REFUSED rather than launched.
            // Since #318 completed, only VALUES croft cannot
            // READ remain: a `presentation` that is not an
            // object, or a `hidden`/`group`/`order` that is
            // not a bool/string/number. Every key that says
            // something croft can act on is honoured -
            // `preLaunchTask` by the branch below, `hidden`,
            // `group` and `order` by the picker's sort.
            // Launching while ignoring what the compound asked
            // for is the "silently debug something other than
            // what was asked for" outcome every guard here
            // exists to prevent.
            // Asked of BOTH arities (#310). It sat on the
            // one-member arm because the other refused anyway;
            // now that both launch, a malformed key would be
            // refused at one member and ignored at two - the
            // "silently debug something other than what was
            // asked for" outcome this guard exists to prevent.
            Ok(members) if !compound.unsupported_keys.is_empty() => {
                // #318, NOT #310: this compound needs ONE
                // session, which croft runs. What it cannot do
                // is honour the key. Citing the multi-session
                // limit here would be the same false claim the
                // launching branch below exists to stop making.
                //
                // The remedy names both ways out: fix the
                // value, or run the member directly. Running
                // it directly DOES skip the compound's own
                // preLaunchTask, which the message leaves
                // unsaid on purpose - a test pins that the
                // status never mentions preLaunchTask, since
                // naming it here would read as advice to
                // accept the skip.
                // Up to three keys can be listed now, so
                // "a and b and c" would read as one run-on;
                // the last pair keeps the "and".
                let keys = match compound.unsupported_keys.split_last() {
                    Some((last, [])) => (*last).to_string(),
                    Some((last, rest)) => {
                        format!("{} and {last}", rest.join(", "))
                    }
                    None => String::new(),
                };
                let plural = if compound.unsupported_keys.len() > 1 {
                    "those keys"
                } else {
                    "that key"
                };
                // Name EVERY member, not `members[0]`. The
                // guard now covers both arities, and telling
                // the user of a three-member compound to run
                // the first one directly is advice to launch
                // a SUBSET - the precise outcome this arm
                // refuses to produce itself.
                let all = members
                    .iter()
                    .map(|m| format!("\"{}\"", m.name))
                    .collect::<Vec<_>>()
                    .join(", ");
                let run_them = if members.len() > 1 {
                    format!("or run {all} directly, in that order")
                } else {
                    format!("or run {all} directly")
                };
                self.debug_error(format!(
                    "compound \"{}\" sets {}, which croft cannot read — fix {} to launch {} through the compound, {}",
                    compound.name, keys, plural, all, run_them
                ));
            }
            Ok(members) if members.len() == 1 => {
                let mut cfg = members[0].clone();
                cfg.answers.extend(compound.answers.clone());
                // One member has no siblings to stop, so its `stopAll` is set
                // false rather than inherited from whatever ran before.
                self.debug_stop_all = false;
                match compound.pre_launch_task.clone() {
                    // #318: the compound's own task runs
                    // first, and the member starts only if it
                    // exits 0 - the same contract a config's
                    // own task already gets. Chained rather
                    // than merged: the member may declare a
                    // task too, and VS Code runs both.
                    Some(task) => self.launch_after_compound_task(&task, cfg),
                    None => self.launch_debug_config(&cfg),
                }
            }
            Ok(members) => {
                // Several sessions at once: what #310 built
                // the set for. Members start in DECLARATION
                // order, which is the order the file asked
                // for and the order a client-then-server
                // compound depends on.
                let label = compound.name.clone();
                // Each member carries the answers asked once for
                // the whole compound (#1424).
                let members: Vec<_> = members
                    .into_iter()
                    .map(|mut m| {
                        m.answers.extend(compound.answers.clone());
                        m
                    })
                    .collect();
                match compound.pre_launch_task.clone() {
                    Some(task) => self.launch_after_compound_task_all(
                        &task,
                        members,
                        label,
                        compound.stop_all,
                    ),
                    None => self.launch_compound_members(members, label, compound.stop_all),
                }
            }
        }
    }

    /// Launch one member INTO the running set, leaving its siblings alone.
    ///
    /// `launch_debug_config` opens by stopping whatever is running, which is
    /// right for a configuration the user picked and fatal for a compound's
    /// second member. This is the same path with that first step removed and
    /// the session added rather than substituted.
    ///
    /// A member with its own `preLaunchTask` is NOT parked here: parking is a
    /// one-at-a-time mechanism, and a compound whose members each park would
    /// interleave unpredictably. Its task is skipped with a status saying so,
    /// which is honest, and #310 is where the general form belongs.
    pub(in crate::app) fn launch_member_into_set(
        &mut self,
        cfg: &crate::dap::configs::DebugConfig,
    ) -> MemberOutcome {
        // A member from another workspace folder resolves there (#1425).
        let root = cfg
            .root
            .clone()
            .unwrap_or_else(|| self.active_workspace_root());
        let ctx = crate::dap::configs::SubstCtx {
            workspace_folder: root.clone(),
            file: self.editor.path.clone(),
            inputs: cfg.answers.clone(),
            folders: self.workspace_folders(),
        };
        let mut rc = match crate::dap::configs::resolve(cfg, &ctx) {
            Ok(rc) => rc,
            Err(e) => {
                self.debug_error(format!("config \"{}\": {e}", cfg.name));
                return MemberOutcome::Failed;
            }
        };
        // js-debug's source-map defaults follow the folder the member was
        // resolved against (#1414), not the one the active file is in.
        if rc.kind == crate::dap::session::AdapterKind::JsDebug {
            rc.extra
                .entry("__workspaceFolder")
                .or_insert_with(|| serde_json::json!(root.to_string_lossy()));
        }
        self.record_post_debug_task(&rc);
        if rc.pre_launch_task.is_some() {
            // NOT launched. Running it anyway would debug whatever the last
            // build left behind, which is exactly what the single-member path
            // refuses to do for the same hazard - and a status message would
            // not have helped, since the caller overwrites it before a frame
            // is drawn. The caller collects the name and says so.
            return MemberOutcome::TaskUnrun;
        }
        // Ask the SET whether a session appeared, rather than trusting the
        // call to have made one: `launch_resolved_into_set` returns `()` on
        // every validation, spawn and attach failure alike, so reporting
        // `Launched` unconditionally counted members that never started and
        // made "N of M started" name an N the user does not have.
        let before = self.debug_sessions.len();
        let building = self.pending_cargo_launches.len();
        let preparing = self.parked_setup_launches();
        self.launch_resolved_into_set(rc);
        if self.debug_sessions.len() > before {
            MemberOutcome::Launched
        } else if self.pending_cargo_launches.len() > building {
            MemberOutcome::Building
        } else if self.parked_setup_launches() > preparing {
            MemberOutcome::Preparing
        } else {
            MemberOutcome::Failed
        }
    }

    /// Park a whole compound behind its own `preLaunchTask` (#310).
    ///
    /// The single-member form parks one config; this parks the list, because
    /// the members have to start together once the task exits 0. Running them
    /// through the single-member path in turn would stop each previous one.
    pub(in crate::app) fn launch_after_compound_task_all(
        &mut self,
        task_label: &str,
        members: Vec<crate::dap::configs::DebugConfig>,
        compound: String,
        stop_all: bool,
    ) {
        self.debug_stop();
        self.supersede_waiting_launches();
        let root = self.active_workspace_root();
        let tasks = crate::tasks::discover_tasks(&root);
        let task = match crate::tasks::find_task(tasks, task_label) {
            Ok(task) => task,
            Err(near) => {
                self.debug_error(format!(
                    "compound preLaunchTask \"{task_label}\" not found{} — Tasks: Run Task lists what the workspace declares",
                    did_you_mean(near)
                ));
                return;
            }
        };
        let answers = members
            .first()
            .map(|m| m.answers.clone())
            .unwrap_or_default();
        let task = with_answers(task, &answers);
        let command = self
            .task_command_line(&task)
            .unwrap_or_else(|_| task.command.clone());
        let step = PendingLaunchStep::Members(Box::new(CompoundLaunch {
            members,
            compound,
            stop_all,
        }));
        let folder = self.active_workspace_root();
        self.park_launch_behind(
            task,
            folder,
            command,
            step,
            "compound preLaunchTask",
            "its sessions start",
        );
    }

    /// Run `task` in `folder` and park `step` until it is done, as its [`PreLaunchWait`]
    /// says (#250, #1541). `what` names the task in the status ("preLaunchTask",
    /// "compound preLaunchTask") and `starts` says what follows it.
    pub(in crate::app) fn park_launch_behind(
        &mut self,
        task: crate::tasks::Task,
        folder: PathBuf,
        command: String,
        step: PendingLaunchStep,
        what: &str,
        starts: &str,
    ) {
        let task_label = task.label.clone();
        let wait = PreLaunchWait::of(&task);
        let Some(pane) = self.run_project_task_in(task, folder) else {
            // run_project_task already reported why the pane failed.
            return;
        };
        if wait == PreLaunchWait::Untracked {
            // VS Code warns that such a task cannot be tracked; waiting for
            // an exit that never comes is the hang this avoids.
            crate::output::push(
                "Tasks",
                crate::output::OutputLevel::Warn,
                &format!(
                    "{what} \"{task_label}\" is a background task with no problem matcher that marks when its build ends, so debugging started without waiting for it"
                ),
            );
            self.resume_pending_launch(Some(0), step);
            return;
        }
        self.pending_debug_launch = Some(PendingDebugLaunch {
            pane,
            command,
            step,
            started: std::time::Instant::now(),
            watch: wait == PreLaunchWait::FirstBuild,
        });
        self.run_debug.feedback = Some(format!("{what} \"{task_label}\" running…"));
        self.run_debug.feedback_is_error = false;
        let when = if wait == PreLaunchWait::FirstBuild {
            "when its first build ends"
        } else {
            "when it exits 0"
        };
        self.status = format!("{what} \"{task_label}\" running — {starts} {when}");
    }

    /// Launch every member of a compound, in declaration order (#310).
    ///
    /// The first member REPLACES whatever was running, matching what starting
    /// any configuration has always done; the rest are pushed beside it. That
    /// asymmetry is the point - `launch_debug_config` stops the world before
    /// it starts, so calling it per member would leave only the last one
    /// alive, which is the bug this path exists to avoid.
    ///
    /// A member that fails to start does not abort the ones already running:
    /// they are real processes, and killing them because a later sibling
    /// could not resolve would be a second failure caused by the first. The
    /// error names the member, and the survivors stay.
    pub(in crate::app) fn launch_compound_members(
        &mut self,
        members: Vec<crate::dap::configs::DebugConfig>,
        compound: String,
        stop_all: bool,
    ) {
        self.debug_stop();
        self.debug_post_tasks.clear();
        // Set only here, once the old set is gone: a compound refused before
        // this point leaves the running set untouched, and must leave its
        // `stopAll` out of it too (#567).
        self.debug_stop_all = stop_all;
        // A launch supersedes any older one still parked behind its task:
        // left in place, that task's exit would launch its config and stop
        // this set (#567). The task-gated path reaches here via `take()`, so
        // this only ever drops a STALE parking.
        self.supersede_waiting_launches();
        self.debug_build = None;
        let total = members.len();
        let mut started = 0usize;
        let mut building: Vec<String> = Vec::new();
        let mut preparing: Vec<String> = Vec::new();
        let mut unrun: Vec<String> = Vec::new();
        for cfg in members {
            match self.launch_member_into_set(&cfg) {
                MemberOutcome::Launched => started += 1,
                MemberOutcome::Building => building.push(cfg.name.clone()),
                MemberOutcome::Preparing => preparing.push(cfg.name.clone()),
                MemberOutcome::TaskUnrun => unrun.push(cfg.name.clone()),
                MemberOutcome::Failed => {}
            }
        }
        if started == 0 && building.is_empty() && !preparing.is_empty() {
            // Every member that could start waits for its debugger to be
            // set up (#2803): each joins the set when that is done.
            self.debug_compound = Some(compound.clone());
            let waiting = format!(
                "setting up the debugger for {} \u{2014} sessions start when it is ready",
                preparing.join(", ")
            );
            self.run_debug.feedback = Some(format!("compound {compound}: {waiting}"));
            self.run_debug.feedback_is_error = preparing.len() != total;
            self.status = format!("compound \"{compound}\": {waiting}");
            return;
        }
        if started == 0 && !building.is_empty() {
            // Every member that could start is still building with cargo
            // (#1392): each joins the set when its build lands.
            self.debug_compound = Some(compound.clone());
            // Members waiting for their debugger to be set up (#2803) join
            // the same way, and are named with them.
            let also = if preparing.is_empty() {
                String::new()
            } else {
                format!("; setting up the debugger for {}", preparing.join(", "))
            };
            self.run_debug.feedback = Some(format!(
                "compound {compound}: building {}{also} \u{2014} sessions start when cargo finishes",
                building.join(", ")
            ));
            self.run_debug.feedback_is_error = building.len() + preparing.len() != total;
            self.status = format!(
                "compound \"{compound}\": building {}{also} \u{2014} sessions start when cargo finishes",
                building.join(", ")
            );
            return;
        }
        if started == 0 {
            // Nothing started. Each member has already said why, so this names
            // the compound and, if that is the reason, the members croft
            // declined to launch against a build it could not run.
            let why = if unrun.is_empty() {
                String::new()
            } else {
                format!(
                    " — {} declares a preLaunchTask croft cannot run as part of a set, and starting it anyway would debug the last build",
                    unrun.join(", ")
                )
            };
            self.debug_error(format!("compound \"{compound}\" started no sessions{why}"));
            return;
        }
        // The FIRST member takes focus, not the last: a compound is usually
        // written server-first, and landing the user on whichever happened to
        // start last would put them in the wrong one.
        self.debug_sessions.focus(0);
        self.debug_compound = Some(compound.clone());
        // Reveal FIRST, then write the feedback: `reveal_debug_view` runs
        // `refresh_run_debug`, which clears `feedback` unconditionally, so
        // doing it afterwards wiped every line built below - including the
        // only place a skipped member is ever named - before it was drawn.
        //
        // Once for the compound, not once per member: each member's launch
        // reveals the view too, and doing it per member clears the console
        // and re-focuses the pane for every one of them.
        self.reveal_debug_view();
        // Name them. With one session "the debugger" was unambiguous; with
        // several the user has to be told which are running and which one the
        // call stack and variables belong to, or the view describes a session
        // they cannot identify.
        let mut running = self.debug_sessions.names().join(", ");
        if !building.is_empty() {
            // Not failed: they join the set when their cargo builds land.
            running = format!("{running}; building {}", building.join(", "));
        }
        if !preparing.is_empty() {
            // Not failed either: they join it once their debugger is set up.
            running = format!(
                "{running}; setting up the debugger for {}",
                preparing.join(", ")
            );
        }
        let started = started + building.len() + preparing.len();
        self.run_debug.feedback = Some(if started == total {
            format!("Debugging compound {compound}: {running}")
        } else if unrun.is_empty() {
            format!("Debugging compound {compound}: {running} ({started} of {total} started)")
        } else {
            // Named, not silent: the member is absent because croft would
            // otherwise debug an unbuilt binary, and the user has to know
            // which one to run themselves.
            format!(
                "Debugging compound {compound}: {running} ({started} of {total}; {} needs its preLaunchTask run first)",
                unrun.join(", ")
            )
        });
        self.run_debug.feedback_is_error = started != total;
        let focused = self
            .debug_sessions
            .focused_name()
            .unwrap_or("?")
            .to_string();
        self.status = format!(
            "Debugging compound {compound} — {started} session(s), showing {focused} · Shift+F5 stops all"
        );
    }

    /// Continue a parked launch whose `preLaunchTask` has finished.
    ///
    /// Split from the terminal sweep so it can be driven directly: reaching it
    /// through the sweep needs a pane emitting OSC 133 marks, and the branch
    /// that matters most - a compound's task completing, then its MEMBER
    /// starting (#318) - is invisible to every test that only checks the
    /// launch parked.
    pub(in crate::app) fn resume_pending_launch(
        &mut self,
        exit: Option<i32>,
        step: PendingLaunchStep,
    ) {
        if exit != Some(0) {
            let code = exit.map_or_else(|| String::from("?"), |c| c.to_string());
            self.debug_error(format!(
                "preLaunchTask exited {code} — debug launch aborted (its pane has the output)"
            ));
            return;
        }
        match step {
            PendingLaunchStep::Resolved(rc) => self.launch_resolved_config(*rc),
            // Back through the front door: the member may have a
            // `preLaunchTask` of its own, and VS Code runs the compound's AND
            // the member's. Calling the resolved-launch path here would
            // silently skip the member's.
            PendingLaunchStep::Member(cfg) => self.launch_debug_config(&cfg),
            // NOT back through the front door, unlike the single member above:
            // `launch_debug_config` stops the world first, so the second
            // member would kill the first (#310). The members' own
            // `preLaunchTask`s are skipped, with a status saying so per
            // member - parking is one-at-a-time and a set of parked members
            // would interleave.
            PendingLaunchStep::Members(launch) => {
                self.launch_compound_members(launch.members, launch.compound, launch.stop_all)
            }
        }
    }
}

impl App {
    /// Run a COMPOUND's own `preLaunchTask`, then launch its single member
    /// (#318).
    ///
    /// Deliberately not folded into [`Self::launch_debug_config`]: that runs
    /// the *config's* task as the last step before the adapter, while this one
    /// runs before the member is even resolved. Keeping them separate is what
    /// lets both fire for a compound whose member also declares a task, which
    /// is what VS Code does.
    ///
    /// Same failure contract as a config's task, because a user cannot tell
    /// the two apart from the outside: a task the workspace does not declare
    /// is an error rather than a silent skip, and a non-zero exit aborts the
    /// launch instead of debugging whatever the build left behind.
    pub(in crate::app) fn launch_after_compound_task(
        &mut self,
        task_label: &str,
        member: crate::dap::configs::DebugConfig,
    ) {
        if !self.debug_sessions.is_empty() {
            self.debug_stop();
        }
        self.supersede_waiting_launches();
        self.debug_build = None;
        let root = self.active_workspace_root();
        let tasks = crate::tasks::discover_tasks(&root);
        let task = match crate::tasks::find_task(tasks, task_label) {
            Ok(task) => task,
            Err(near) => {
                self.debug_error(format!(
                    "compound preLaunchTask \"{task_label}\" not found{} — Tasks: Run Task lists what the workspace declares",
                    did_you_mean(near)
                ));
                return;
            }
        };
        let task = with_answers(task, &member.answers);
        let command = self
            .task_command_line(&task)
            .unwrap_or_else(|_| task.command.clone());
        let folder = self.active_workspace_root();
        self.park_launch_behind(
            task,
            folder,
            command,
            PendingLaunchStep::Member(Box::new(member)),
            "compound preLaunchTask",
            "debug starts",
        );
    }

    /// A compound member ended and siblings survive (#567): rebuild the
    /// status and the panel line from the live set, naming what ended, which
    /// session the view now shows, and every one still running. Without this
    /// both kept naming the ended session.
    pub(in crate::app) fn report_member_ended(&mut self, ended: &str) {
        let running = self.debug_sessions.names().join(", ");
        let focused = self
            .debug_sessions
            .focused_name()
            .unwrap_or("?")
            .to_string();
        let what = self
            .debug_compound
            .as_deref()
            .map_or_else(String::new, |c| format!(" compound {c}"));
        self.run_debug.feedback = Some(format!("Debugging{what}: {running} ({ended} ended)"));
        self.run_debug.feedback_is_error = false;
        self.status =
            format!("{ended} ended — showing {focused}; running: {running} · Shift+F5 stops all");
    }

    /// A member of the set ended before the rest started: they wait for
    /// their debugger to be set up (#2803), and start when it is ready.
    pub(in crate::app) fn report_member_ended_before_the_rest(&mut self, ended: &str) {
        let what = self
            .debug_compound
            .as_deref()
            .map_or_else(String::new, |c| format!(" compound {c}"));
        self.run_debug.feedback = Some(format!(
            "Debugging{what}: {ended} ended; the rest start when their debugger is set up"
        ));
        self.run_debug.feedback_is_error = false;
        self.status = format!(
            "{ended} ended — the rest start when their debugger is set up · Shift+F5 stops all"
        );
    }

    /// Move the UI to the session at `index` (#567). `events` holds what the
    /// previously focused session produced this tick: its output goes to the
    /// console now, and the rest goes back on its own backlog so it replays
    /// when that session is focused again, rather than being applied to the
    /// wrong one. `events` is then the new session's queued backlog, so the
    /// caller's event loop replays its stop as if it had been focused all
    /// along.
    pub(in crate::app) fn refocus_debug_session(
        &mut self,
        index: usize,
        events: &mut Vec<crate::dap::session::DapEvent>,
    ) {
        use crate::dap::session::DapEvent;
        let old = self.debug_sessions.focused_index();
        if index == old || !self.debug_sessions.focus(index) {
            return;
        }
        let mut keep = Vec::new();
        for ev in events.drain(..) {
            match ev {
                DapEvent::Output { category, text }
                    if crate::dap::session::output_is_user_visible(&category) =>
                {
                    self.debug_console_push_output(old, &category, &text, false);
                }
                DapEvent::Output { .. } => {}
                other => keep.push(other),
            }
        }
        if let Some((_, named)) = self
            .debug_sessions
            .iter_named_mut_indexed()
            .find(|(i, _)| *i == old)
        {
            keep.append(&mut named.backlog);
            named.backlog = keep;
        }
        *events = self.debug_sessions.take_focused_backlog();
        // The arrow belongs to the session that was focused; the phase pass
        // after the event loop redraws it for the new one.
        self.editor.stop_line = None;
    }

    /// Debug: Switch Session (#567): focus the next member of a compound, so
    /// one stopped in the background can be inspected. Its queued events
    /// replay on the next poll.
    pub(in crate::app) fn switch_debug_session(&mut self) {
        if self.debug_sessions.len() < 2 {
            self.status = String::from("Only one debug session is running");
            return;
        }
        let next = (self.debug_sessions.focused_index() + 1) % self.debug_sessions.len();
        let mut none = Vec::new();
        self.refocus_debug_session(next, &mut none);
        // Nothing drained this tick belongs to the old session here, so the
        // new session's backlog goes back to wait for the next poll.
        if let Some((_, named)) = self
            .debug_sessions
            .iter_named_mut_indexed()
            .find(|(i, _)| *i == next)
        {
            none.append(&mut named.backlog);
            named.backlog = none;
        }
        let name = self
            .debug_sessions
            .focused_name()
            .unwrap_or("?")
            .to_string();
        let running = self.debug_sessions.names().join(", ");
        self.status = format!("Debugging {name} · running: {running}");
        self.refresh_debug_panel();
    }

    /// Remember a launched configuration's `postDebugTask` for the end of
    /// this run (#250). A compound's members may name the same one; it runs
    /// once.
    pub(in crate::app) fn record_post_debug_task(
        &mut self,
        rc: &crate::dap::configs::ResolvedConfig,
    ) {
        if let Some(task) = &rc.post_debug_task
            && !self.debug_post_tasks.contains(task)
        {
            self.debug_post_tasks.push(task.clone());
        }
    }

    /// Run the finished run's `postDebugTask`s, each once, in launch order.
    /// A label no tasks.json declares is reported rather than skipped, the
    /// same contract `preLaunchTask` has.
    pub(in crate::app) fn run_post_debug_tasks(&mut self) {
        let labels = std::mem::take(&mut self.debug_post_tasks);
        if labels.is_empty() {
            return;
        }
        let tasks = crate::tasks::discover_tasks(&self.active_workspace_root());
        for label in labels {
            match crate::tasks::find_task(tasks.clone(), &label) {
                Ok(task) => {
                    self.run_project_task(task);
                }
                Err(near) => {
                    self.status = format!(
                        "postDebugTask \"{label}\" not found{} — Tasks: Run Task lists what the workspace declares",
                        did_you_mean(near)
                    );
                }
            }
        }
    }
}
