//! The per-frame poll: draining what the adapters sent, the handshake, each message's
//! effect on the session, and which connection and capabilities user-facing requests
//! go to.

use super::*;

impl DapSession {
    /// The transport that user-facing operations (breakpoints, stepping, stack,
    /// variables, evaluate) target: the js-debug child once it exists, otherwise
    /// the parent. For stdio adapters there is only the parent.
    pub(super) fn active(&self) -> &DapTransport {
        self.child.as_ref().unwrap_or(&self.transport)
    }

    /// Drain everything the adapter(s) have sent, advance the handshake, and
    /// return the user-facing events. Non-blocking. For js-debug both the parent
    /// and the child connections are drained; `from_child` records which one a
    /// message came from so reverse requests are answered on the right socket.
    pub fn poll(&mut self) -> Vec<DapEvent> {
        let mut out = Vec::new();
        // Drain the parent first so a `startDebugging` request can open the child
        // before we try to drain it this same poll.
        while let Ok(msg) = self.transport.incoming.try_recv() {
            self.handle_message(msg, false, &mut out);
        }
        // Drain the child into a buffer first (handling a message borrows `self`
        // mutably, which can't coexist with holding the child's receiver).
        let mut child_msgs = Vec::new();
        if let Some(child) = self.child.as_ref() {
            while let Ok(msg) = child.incoming.try_recv() {
                child_msgs.push(msg);
            }
        }
        for msg in child_msgs {
            self.handle_message(msg, true, &mut out);
        }
        out
    }

    /// Handle one decoded message from the parent (`from_child = false`) or the
    /// js-debug child (`from_child = true`), pushing any user-facing events.
    pub(super) fn handle_message(&mut self, msg: Value, from_child: bool, out: &mut Vec<DapEvent>) {
        // Reverse requests (adapter -> client) MUST be answered or the adapter
        // blocks. vscode-js-debug's `startDebugging` asks us to open the child
        // session; everything else croft declines (it debugs in-process). So
        // is a `startDebugging` on a session with no child path (anything but
        // js-debug, see `initialize_request`): acknowledging it would promise
        // a subprocess session that never opens.
        if msg.get("type").and_then(Value::as_str) == Some("request") {
            if let Some(req) = start_debugging_request(&msg).filter(|_| self.js_server.is_some()) {
                let _ = self.transport.send(reverse_request_response(
                    req.request_seq,
                    "startDebugging",
                    true,
                ));
                self.open_child_session(&req.config, req.request.as_deref());
            } else if let (Some(seq), Some(command)) = (
                msg.get("seq").and_then(Value::as_i64),
                msg.get("command").and_then(Value::as_str),
            ) {
                let resp = reverse_request_response(seq, command, false);
                if from_child {
                    if let Some(c) = self.child.as_ref() {
                        let _ = c.send(resp);
                    }
                } else {
                    let _ = self.transport.send(resp);
                }
            }
            return;
        }
        // Responses: the only one we act on is `stackTrace`, which tells us the
        // file + line a `stopped` paused on.
        if msg.get("type").and_then(Value::as_str) == Some("response") {
            match msg.get("command").and_then(Value::as_str) {
                Some("stackTrace") => {
                    self.stack_frames = parse_stack_frames(&msg);
                    if let Some(top) = self.stack_frames.first() {
                        if let Some(path) = top.path.clone() {
                            self.current_location = Some((path, top.line));
                        }
                        let top_id = top.id;
                        self.load_frame(top_id);
                    }
                    out.push(DapEvent::InspectionUpdated);
                }
                Some("threads") => {
                    self.threads = parse_threads(&msg);
                    out.push(DapEvent::InspectionUpdated);
                }
                Some("stepInTargets") => {
                    let req_seq = msg.get("request_seq").and_then(Value::as_i64);
                    if req_seq.is_some() && req_seq == self.step_in_targets_seq {
                        self.step_in_targets_seq = None;
                        out.push(DapEvent::StepInTargets {
                            targets: parse_step_in_targets(&msg),
                        });
                    }
                }
                Some("exceptionInfo") => {
                    let req_seq = msg.get("request_seq").and_then(Value::as_i64);
                    if req_seq.is_some() && req_seq == self.exception_info_seq {
                        self.exception_info_seq = None;
                        if let Some(summary) = exception_summary(&msg) {
                            out.push(DapEvent::ExceptionInfo { summary });
                        }
                    }
                }
                Some("scopes") => {
                    self.scopes = parse_scopes(&msg);
                    // An expensive scope (a whole Global object) waits until
                    // the user opens it (#1421), or every step would wait
                    // for it.
                    let refs: Vec<i64> = (0..self.scopes.len())
                        .filter(|&i| self.scope_open(i))
                        .map(|i| self.scopes[i].variables_ref)
                        .collect();
                    for r in refs {
                        self.request_variables(r);
                    }
                    out.push(DapEvent::InspectionUpdated);
                }
                Some("variables") => {
                    let req_seq = msg.get("request_seq").and_then(Value::as_i64);
                    if let Some(reference) = req_seq.and_then(|s| self.pending_var_refs.remove(&s))
                    {
                        self.variables.insert(reference, parse_variables(&msg));
                        out.push(DapEvent::InspectionUpdated);
                    }
                }
                Some("evaluate") => {
                    let req_seq = msg.get("request_seq").and_then(Value::as_i64);
                    if let Some((context, expression)) =
                        req_seq.and_then(|s| self.pending_evals.remove(&s))
                    {
                        let success = msg.get("success").and_then(Value::as_bool).unwrap_or(false);
                        let result = parse_evaluate_result(&msg).unwrap_or_else(|| {
                            msg.get("message")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string()
                        });
                        out.push(DapEvent::Evaluated {
                            context,
                            expression,
                            result,
                            success,
                        });
                    }
                }
                // js-debug reports breakpoints `verified:false` ("provisional")
                // before binding them on the fly, so treating its reports as
                // unverified would wrongly render every js/ts breakpoint hollow.
                // Only stdio adapters' reports drive the hollow rendering; for a
                // js-debug session the guard fails and the arm falls through to
                // the no-op `_` below.
                Some("initialize") => {
                    if let Some(body) = msg.get("body") {
                        self.capabilities = body.clone();
                        // Keep only the filters this adapter knows (#1426),
                        // plus those it turns on by default.
                        if let Some(offered) = offered_exception_filters(body) {
                            let mut keep: Vec<String> = self
                                .exception_filters
                                .iter()
                                .filter(|f| offered.iter().any(|(id, _)| id == *f))
                                .cloned()
                                .collect();
                            for (id, default) in &offered {
                                if *default && !keep.contains(id) {
                                    keep.push(id.clone());
                                }
                            }
                            // A toggle made before this reply, in this
                            // adapter's own ids.
                            if self.pending_raised == Some(true) {
                                for id in raised_filter_ids(&offered).unwrap_or_default() {
                                    if !keep.contains(&id) {
                                        keep.push(id);
                                    }
                                }
                            }
                            self.exception_filters = keep;
                        }
                        self.pending_raised = None;
                    }
                }
                Some("launch" | "attach")
                    if msg.get("success").and_then(Value::as_bool) == Some(false) =>
                {
                    let reason = launch_failure_reason(&msg);
                    out.push(DapEvent::Output {
                        category: String::from("stderr"),
                        text: format!("{reason}\n"),
                    });
                    self.launch_error = Some(reason);
                    self.phase = SessionPhase::Terminated;
                    self.current_location = None;
                    out.push(DapEvent::Terminated);
                }
                Some("dataBreakpointInfo") => {
                    let req_seq = msg.get("request_seq").and_then(Value::as_i64);
                    if let Some(label) = req_seq.and_then(|s| self.pending_data_info.remove(&s)) {
                        let body = msg.get("body");
                        match body.and_then(|b| b.get("dataId")).and_then(Value::as_str) {
                            Some(id) => {
                                self.data_breakpoints.push((id.to_string(), label));
                                self.push_data_breakpoints();
                            }
                            None => {
                                let why = body
                                    .and_then(|b| b.get("description"))
                                    .and_then(Value::as_str)
                                    .unwrap_or("the adapter cannot watch it")
                                    .to_string();
                                out.push(DapEvent::Output {
                                    category: String::from("console"),
                                    text: format!("Cannot break when {label} changes: {why}\n"),
                                });
                            }
                        }
                    }
                }
                Some("setBreakpoints") if self.js_server.is_none() => {
                    let reports = breakpoint_reports(&msg);
                    if self.apply_breakpoint_reports(&reports) {
                        out.push(DapEvent::BreakpointsUpdated);
                    }
                }
                _ => {}
            }
            return;
        }
        // `thread` events announce thread ids; remember the latest so we can
        // target `pause` while the program is running.
        if msg.get("event").and_then(Value::as_str) == Some("thread") {
            if let Some(tid) = msg
                .get("body")
                .and_then(|b| b.get("threadId"))
                .and_then(Value::as_i64)
            {
                self.known_thread = Some(tid);
            }
            return;
        }
        // `breakpoint` events carry late-resolved verification updates.
        if msg.get("event").and_then(Value::as_str) == Some("breakpoint") {
            if self.js_server.is_none() {
                let reports = breakpoint_reports(&msg);
                if update_breakpoint_lines(&mut self.unverified_breakpoints, &reports) {
                    out.push(DapEvent::BreakpointsUpdated);
                }
            }
            return;
        }
        let Some(event) = classify_event(&msg) else {
            return;
        };
        match &event {
            DapEvent::Initialized => {
                // Push breakpoints + configurationDone to the connection that
                // just initialized (parent or child). `active()` already resolves
                // to the child once it exists, and to the parent before that.
                self.push_breakpoints();
                let t = self.active();
                let _ = t.send(set_exception_breakpoints_request(&self.exception_filters));
                let _ = t.send(configuration_done_request());
                self.phase = SessionPhase::Running;
            }
            DapEvent::Stopped { thread_id, reason } => {
                // Run to Cursor's breakpoint lives for one stop, whichever
                // breakpoint or step produced it.
                if let Some((path, _)) = self.run_to.take() {
                    let own = self.breakpoints.get(&path).cloned().unwrap_or_default();
                    let _ = self.active().send(set_breakpoints_request(&path, &own));
                }
                self.stopped_thread = Some(*thread_id);
                self.known_thread = Some(*thread_id);
                self.phase = SessionPhase::Stopped;
                self.clear_inspection();
                let _ = self.active().send(stack_trace_request(*thread_id));
                let _ = self.active().send(threads_request());
                // "Paused (exception)" alone does not say what was thrown
                // (#864); the answer arrives as `ExceptionInfo`, for this
                // stop only.
                self.exception_info_seq = None;
                if reason == "exception" && self.supports("supportsExceptionInfoRequest") {
                    self.exception_info_seq = self
                        .active()
                        .send(thread_request("exceptionInfo", *thread_id))
                        .ok();
                }
            }
            DapEvent::Continued => {
                self.phase = SessionPhase::Running;
                self.current_location = None;
                self.exception_info_seq = None;
                self.clear_inspection();
            }
            DapEvent::Terminated => {
                // A js-debug parent ("bootstrap") can end its own connection
                // while the child debuggee is still running; that must NOT tear
                // the session down. Only the child's exit (or a stdio adapter /
                // parent with no child) actually ends the run.
                if from_child || self.child.is_none() {
                    self.phase = SessionPhase::Terminated;
                    self.current_location = None;
                    self.exception_info_seq = None;
                    self.clear_inspection();
                }
            }
            DapEvent::Output { .. } => {}
            DapEvent::BreakpointsUpdated
            | DapEvent::InspectionUpdated
            | DapEvent::Evaluated { .. }
            | DapEvent::ExceptionInfo { .. }
            | DapEvent::StepInTargets { .. } => {}
        }
        // Suppress the parent's own `terminated` for js-debug while a child is
        // still live (the parent can wind down its bootstrap connection first).
        if from_child || !matches!(event, DapEvent::Terminated) || self.child.is_none() {
            out.push(event);
        }
    }

    /// Whether the adapter declared capability `name` (#611). An adapter
    /// that has not answered `initialize` yet counts as supporting it, so a
    /// request made during startup is sent rather than refused.
    pub fn supports(&self, name: &str) -> bool {
        match &self.capabilities {
            Value::Null => true,
            caps => caps.get(name).and_then(Value::as_bool).unwrap_or(false),
        }
    }
}
