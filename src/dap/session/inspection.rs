//! Inspecting a stopped program: threads, the call stack, scopes and variables,
//! expensive scopes, evaluate and the hover's lookup, and the locals inline values
//! draw from.

use super::*;

/// Build a `threads` request: every thread the debuggee has.
pub fn threads_request() -> Value {
    json!({ "type": "request", "command": "threads" })
}

/// The `(id, name)` pairs of a `threads` response.
pub fn parse_threads(msg: &Value) -> Vec<(i64, String)> {
    msg.pointer("/body/threads")
        .and_then(Value::as_array)
        .map(|ts| {
            ts.iter()
                .filter_map(|t| {
                    Some((
                        t.get("id")?.as_i64()?,
                        t.get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Build a `stackTrace` request for the full call stack of `thread_id`. Omitting
/// `levels` asks the adapter for every frame (debugpy returns the whole stack),
/// which powers the Call Stack panel; the top frame still drives the stop-line.
pub fn stack_trace_request(thread_id: i64) -> Value {
    json!({
        "type": "request",
        "command": "stackTrace",
        "arguments": { "threadId": thread_id, "startFrame": 0 }
    })
}

/// Build an `evaluate` request: run `expression` in the context of `frame_id`
/// (None evaluates in the global/REPL context). `context` is `repl`, `watch`,
/// or `hover` and tells the adapter how to format the result.
pub fn evaluate_request(expression: &str, frame_id: Option<i64>, context: &str) -> Value {
    let mut args = json!({ "expression": expression, "context": context });
    if let Some(fid) = frame_id {
        args["frameId"] = Value::from(fid);
    }
    json!({ "type": "request", "command": "evaluate", "arguments": args })
}

/// Extract an `evaluate` response's `result` string, or None. Pure.
pub fn parse_evaluate_result(response: &Value) -> Option<String> {
    response
        .get("body")?
        .get("result")?
        .as_str()
        .map(str::to_string)
}

/// Build a `scopes` request for one stack frame (locals / globals containers).
pub fn scopes_request(frame_id: i64) -> Value {
    json!({
        "type": "request",
        "command": "scopes",
        "arguments": { "frameId": frame_id }
    })
}

/// Build a `variables` request for a `variablesReference` (a scope's container or
/// an expandable variable's children).
pub fn variables_request(variables_reference: i64) -> Value {
    json!({
        "type": "request",
        "command": "variables",
        "arguments": { "variablesReference": variables_reference }
    })
}

/// Parse the full call stack from a `stackTrace` response. Pure.
pub fn parse_stack_frames(stack_trace_response: &Value) -> Vec<StackFrame> {
    stack_trace_response
        .get("body")
        .and_then(|b| b.get("stackFrames"))
        .and_then(Value::as_array)
        .map(|frames| {
            frames
                .iter()
                .filter_map(|f| {
                    Some(StackFrame {
                        id: f.get("id")?.as_i64()?,
                        name: f
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        path: f
                            .get("source")
                            .and_then(|s| s.get("path"))
                            .and_then(Value::as_str)
                            .map(PathBuf::from),
                        line: f.get("line").and_then(Value::as_i64).unwrap_or(0) as usize,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Parse scopes from a `scopes` response. Pure.
pub fn parse_scopes(scopes_response: &Value) -> Vec<Scope> {
    scopes_response
        .get("body")
        .and_then(|b| b.get("scopes"))
        .and_then(Value::as_array)
        .map(|scopes| {
            scopes
                .iter()
                .filter_map(|s| {
                    Some(Scope {
                        name: s.get("name")?.as_str()?.to_string(),
                        variables_ref: s.get("variablesReference")?.as_i64()?,
                        expensive: s.get("expensive").and_then(Value::as_bool) == Some(true),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Parse variables from a `variables` response. Pure.
pub fn parse_variables(variables_response: &Value) -> Vec<Variable> {
    variables_response
        .get("body")
        .and_then(|b| b.get("variables"))
        .and_then(Value::as_array)
        .map(|vars| {
            vars.iter()
                .filter_map(|v| {
                    Some(Variable {
                        name: v.get("name")?.as_str()?.to_string(),
                        value: v
                            .get("value")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        type_name: v
                            .get("type")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        variables_ref: v
                            .get("variablesReference")
                            .and_then(Value::as_i64)
                            .unwrap_or(0),
                        evaluate_name: v
                            .get("evaluateName")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

impl DapSession {
    /// Evaluate `expression` in the selected frame (or globally if no frame is
    /// selected). The result arrives on a later poll as [`DapEvent::Evaluated`].
    /// `context` is typically `repl` or `watch`.
    pub fn evaluate(&mut self, expression: &str, context: &str) {
        let req = evaluate_request(expression, self.selected_frame, context);
        if let Ok(seq) = self.active().send(req) {
            self.pending_evals
                .insert(seq, (context.to_string(), expression.to_string()));
        }
    }

    /// Clear all inspection state (call stack, scopes, variables). Done on every
    /// resume/step/terminate since frame ids and variable references are only
    /// valid for the current stop.
    pub(super) fn clear_inspection(&mut self) {
        self.stack_frames.clear();
        self.scopes.clear();
        self.variables.clear();
        self.selected_frame = None;
        self.pending_var_refs.clear();
    }

    /// Show another thread's stack (#264): a goroutine other than the one
    /// that hit the breakpoint. Stepping then applies to it, as in VS Code.
    /// Only while stopped: a running thread has no stack to show.
    pub fn select_thread(&mut self, thread_id: i64) {
        if self.phase != SessionPhase::Stopped || self.stopped_thread == Some(thread_id) {
            return;
        }
        self.stopped_thread = Some(thread_id);
        self.known_thread = Some(thread_id);
        self.clear_inspection();
        let _ = self.active().send(stack_trace_request(thread_id));
    }

    /// Load scopes for `frame_id` and fetch each scope's variables. Called when a
    /// stop resolves its stack (top frame) or the user selects another frame.
    pub fn load_frame(&mut self, frame_id: i64) {
        self.selected_frame = Some(frame_id);
        self.scopes.clear();
        self.variables.clear();
        self.pending_var_refs.clear();
        let _ = self.active().send(scopes_request(frame_id));
    }

    /// Find a top-level variable by name across the selected frame's loaded
    /// scopes (Locals first, then Globals), for hover-to-evaluate. Returns the
    /// first match. None while running or before variables arrive.
    pub fn lookup_local(&self, name: &str) -> Option<&Variable> {
        self.scopes
            .iter()
            .filter_map(|s| self.variables.get(&s.variables_ref))
            .flatten()
            .find(|v| v.name == name)
    }

    /// The persistent identity of the scope at `index`: its name and how
    /// many earlier scopes share that name (#1421). A `variablesReference`
    /// changes at every stop, so it cannot keep a scope open across stops.
    pub(super) fn scope_key(&self, index: usize) -> Option<(String, usize)> {
        let scope = self.scopes.get(index)?;
        let nth = self.scopes[..index]
            .iter()
            .filter(|s| s.name == scope.name)
            .count();
        Some((scope.name.clone(), nth))
    }

    /// Whether the variables of the scope at `index` are shown: always,
    /// unless it is an expensive one the user has not opened (#1421).
    pub fn scope_open(&self, index: usize) -> bool {
        match self.scopes.get(index) {
            Some(scope) if scope.expensive => self
                .scope_key(index)
                .is_some_and(|k| self.open_expensive_scopes.contains(&k)),
            Some(_) => true,
            None => false,
        }
    }

    /// Open or close the expensive scope at `index` (#1421), fetching its
    /// variables when it opens.
    pub fn toggle_expensive_scope(&mut self, index: usize) {
        let Some(key) = self.scope_key(index) else {
            return;
        };
        if !self.open_expensive_scopes.remove(&key) {
            self.open_expensive_scopes.insert(key);
            let r = self.scopes[index].variables_ref;
            self.expand_variable(r);
        }
    }

    /// Request the children of an expandable variable (its `variablesReference`)
    /// unless already loaded; the response files them under that reference.
    pub fn expand_variable(&mut self, variables_reference: i64) {
        if variables_reference > 0 && !self.variables.contains_key(&variables_reference) {
            self.request_variables(variables_reference);
        }
    }

    /// Send a `variables` request and remember which reference it was for.
    pub(super) fn request_variables(&mut self, variables_reference: i64) {
        if let Ok(seq) = self.active().send(variables_request(variables_reference)) {
            self.pending_var_refs.insert(seq, variables_reference);
        }
    }
}

/// The `(name, value)` pairs inline values draw from (#135): every variable
/// of the "Locals"-named scopes, or of the first scope when no scope carries
/// that name (adapters differ — debugpy says "Locals", js-debug "Local").
/// Pure over already-fetched data; no adapter round-trip.
pub fn inline_locals(
    scopes: &[Scope],
    variables: &std::collections::BTreeMap<i64, Vec<Variable>>,
) -> Vec<(String, String)> {
    let local_refs: Vec<i64> = scopes
        .iter()
        .filter(|s| s.name.to_lowercase().contains("local"))
        .map(|s| s.variables_ref)
        .collect();
    let refs: Vec<i64> = if local_refs.is_empty() {
        scopes
            .first()
            .map(|s| s.variables_ref)
            .into_iter()
            .collect()
    } else {
        local_refs
    };
    refs.iter()
        .filter_map(|r| variables.get(r))
        .flatten()
        .map(|v| (v.name.clone(), v.value.clone()))
        .collect()
}
