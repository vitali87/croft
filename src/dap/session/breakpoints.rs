//! Breakpoints: line, function and data breakpoints and logpoints, what the adapter
//! reports back about each, pushing them mid-session, and Run to Cursor's one-stop
//! breakpoint.

use super::*;

/// One breakpoint's binding status as reported by the adapter: the source file,
/// the (possibly adjusted) 1-based line, and whether the adapter could actually
/// place it. An unverified breakpoint never pauses execution, so surfacing it is
/// what stops the "I set a breakpoint and it just ran" confusion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BreakpointReport {
    pub path: PathBuf,
    pub line: usize,
    pub verified: bool,
}

/// Extract breakpoint statuses from either a `setBreakpoints` response
/// (`body.breakpoints[]`) or a `breakpoint` event (`body.breakpoint`). Entries
/// without a source path or line are skipped (debugpy occasionally reports
/// `line: 0` for a not-yet-resolved breakpoint). Pure.
pub fn breakpoint_reports(msg: &Value) -> Vec<BreakpointReport> {
    let body = match msg.get("body") {
        Some(b) => b,
        None => return Vec::new(),
    };
    let mut raw: Vec<&Value> = Vec::new();
    if let Some(arr) = body.get("breakpoints").and_then(Value::as_array) {
        raw.extend(arr.iter());
    }
    if let Some(one) = body.get("breakpoint") {
        raw.push(one);
    }
    raw.into_iter()
        .filter_map(|bp| {
            let path = bp.get("source")?.get("path")?.as_str()?;
            let line = bp.get("line").and_then(Value::as_i64).unwrap_or(0);
            if line <= 0 {
                return None;
            }
            Some(BreakpointReport {
                path: PathBuf::from(path),
                line: line as usize,
                verified: bp.get("verified").and_then(Value::as_bool).unwrap_or(false),
            })
        })
        .collect()
}

/// Build a `setBreakpoints` request body for one source file, carrying any
/// per-breakpoint `condition` and `logMessage`.
pub fn set_breakpoints_request(path: &Path, breakpoints: &[SourceBreakpoint]) -> Value {
    let bps: Vec<Value> = breakpoints
        .iter()
        .map(|b| {
            let mut bp = json!({ "line": b.line });
            if let Some(c) = &b.condition {
                bp["condition"] = json!(c);
            }
            if let Some(m) = &b.log_message {
                bp["logMessage"] = json!(m);
            }
            if let Some(h) = &b.hit_condition {
                bp["hitCondition"] = json!(h);
            }
            bp
        })
        .collect();
    let lines: Vec<u32> = breakpoints.iter().map(|b| b.line).collect();
    json!({
        "type": "request",
        "command": "setBreakpoints",
        "arguments": {
            "source": { "path": path.to_string_lossy() },
            "breakpoints": bps,
            "lines": lines
        }
    })
}

/// Build a `setFunctionBreakpoints` request (#611): pause on entry to each
/// named function. The list replaces the adapter's whole set.
pub fn set_function_breakpoints_request(names: &[String]) -> Value {
    let bps: Vec<Value> = names.iter().map(|n| json!({ "name": n })).collect();
    json!({
        "type": "request",
        "command": "setFunctionBreakpoints",
        "arguments": { "breakpoints": bps }
    })
}

/// Build a `dataBreakpointInfo` request (#611): ask whether the variable
/// `name` under `variables_reference` can carry a data breakpoint, and for
/// the id to set it with.
pub fn data_breakpoint_info_request(variables_reference: i64, name: &str) -> Value {
    json!({
        "type": "request",
        "command": "dataBreakpointInfo",
        "arguments": { "variablesReference": variables_reference, "name": name }
    })
}

/// Build a `setDataBreakpoints` request (#611): pause when any of these data
/// ids is written. Replaces the adapter's whole set.
pub fn set_data_breakpoints_request(data_ids: &[String]) -> Value {
    let bps: Vec<Value> = data_ids
        .iter()
        .map(|id| json!({ "dataId": id, "accessType": "write" }))
        .collect();
    json!({
        "type": "request",
        "command": "setDataBreakpoints",
        "arguments": { "breakpoints": bps }
    })
}

/// Apply `breakpoint` event reports to the unverified-line map, one line at
/// a time, returning `true` if anything changed. An event carries a single
/// breakpoint, unlike a `setBreakpoints` response, so it must not replace
/// its file's set: every other unverified line there would be dropped and
/// drawn as verified. Pure.
pub fn update_breakpoint_lines(
    map: &mut BTreeMap<PathBuf, std::collections::BTreeSet<usize>>,
    reports: &[BreakpointReport],
) -> bool {
    let mut changed = false;
    for r in reports {
        let set = map.entry(r.path.clone()).or_default();
        changed |= if r.verified {
            set.remove(&r.line)
        } else {
            set.insert(r.line)
        };
        if set.is_empty() {
            map.remove(&r.path);
        }
    }
    changed
}

/// Fold breakpoint reports into a per-path unverified-line map, returning `true`
/// if anything changed. Each reported path's unverified set is replaced
/// wholesale (a `setBreakpoints` response describes that file's full set); a path
/// whose breakpoints all verify is dropped from the map. Pure.
pub fn fold_breakpoint_reports(
    map: &mut BTreeMap<PathBuf, std::collections::BTreeSet<usize>>,
    reports: &[BreakpointReport],
) -> bool {
    use std::collections::{BTreeSet, btree_map::Entry};
    if reports.is_empty() {
        return false;
    }
    let mut by_path: BTreeMap<PathBuf, BTreeSet<usize>> = BTreeMap::new();
    for r in reports {
        let set = by_path.entry(r.path.clone()).or_default();
        if !r.verified {
            set.insert(r.line);
        }
    }
    let mut changed = false;
    for (path, unverified) in by_path {
        match map.entry(path) {
            Entry::Occupied(mut e) => {
                if *e.get() != unverified {
                    if unverified.is_empty() {
                        e.remove();
                    } else {
                        e.insert(unverified);
                    }
                    changed = true;
                }
            }
            Entry::Vacant(e) => {
                if !unverified.is_empty() {
                    e.insert(unverified);
                    changed = true;
                }
            }
        }
    }
    changed
}

impl DapSession {
    /// Fold a set of breakpoint reports into `unverified_breakpoints`, returning
    /// `true` if the set changed (so the caller can trigger a redraw).
    pub(super) fn apply_breakpoint_reports(&mut self, reports: &[BreakpointReport]) -> bool {
        fold_breakpoint_reports(&mut self.unverified_breakpoints, reports)
    }

    /// Push every stashed breakpoint set to the active connection (called on
    /// `initialized`).
    pub(super) fn push_breakpoints(&mut self) {
        let t = self.active();
        for (path, lines) in &self.breakpoints {
            let _ = t.send(set_breakpoints_request(path, lines));
        }
        if !self.function_breakpoints.is_empty() {
            let _ = t.send(set_function_breakpoints_request(&self.function_breakpoints));
        }
    }

    /// Replace the function breakpoints (#611), resending at once.
    pub fn set_function_breakpoints(&mut self, names: Vec<String>) {
        self.function_breakpoints = names;
        if !matches!(self.phase, SessionPhase::Initializing) {
            let _ = self
                .active()
                .send(set_function_breakpoints_request(&self.function_breakpoints));
        }
    }

    /// Run to Cursor (#611): add a one-stop breakpoint at 1-based `line` of
    /// `path` and resume.
    pub fn run_to_cursor(&mut self, path: &Path, line: u32) {
        // A second Run to Cursor before the first stopped withdraws the
        // first one's line, or it would stay behind with no gutter mark.
        if let Some((old, _)) = self.run_to.take()
            && old != path
        {
            let own = self.breakpoints.get(&old).cloned().unwrap_or_default();
            let _ = self.active().send(set_breakpoints_request(&old, &own));
        }
        self.run_to = Some((path.to_path_buf(), line));
        let own = self.breakpoints.get(path).cloned().unwrap_or_default();
        let set = self.with_run_to(path, &own);
        let _ = self.active().send(set_breakpoints_request(path, &set));
        self.continue_execution();
    }

    pub(super) fn with_run_to(
        &self,
        path: &Path,
        own: &[SourceBreakpoint],
    ) -> Vec<SourceBreakpoint> {
        with_run_to(self.run_to.as_ref(), path, own)
    }

    /// Break when a variable changes (#611): ask the adapter for the data id
    /// of `name` under `variables_reference`; the reply sets the breakpoint.
    pub fn break_on_change(&mut self, variables_reference: i64, name: &str) {
        if let Ok(seq) = self
            .active()
            .send(data_breakpoint_info_request(variables_reference, name))
        {
            self.pending_data_info.insert(seq, name.to_string());
        }
    }

    /// Drop every data breakpoint (#611).
    pub fn clear_data_breakpoints(&mut self) {
        self.data_breakpoints.clear();
        self.push_data_breakpoints();
    }

    pub(super) fn push_data_breakpoints(&mut self) {
        let ids: Vec<String> = self
            .data_breakpoints
            .iter()
            .map(|(id, _)| id.clone())
            .collect();
        let _ = self.active().send(set_data_breakpoints_request(&ids));
    }

    /// Push an updated breakpoint set for one file mid-session (debugpy accepts
    /// `setBreakpoints` at any time), so toggling a breakpoint while paused or
    /// running takes effect without a restart.
    pub fn update_breakpoints(&mut self, path: &Path, breakpoints: &[SourceBreakpoint]) {
        // Kept current so Run to Cursor can restore exactly this set.
        self.breakpoints
            .insert(path.to_path_buf(), breakpoints.to_vec());
        // A pending Run to Cursor in this file keeps its line.
        let set = self.with_run_to(path, breakpoints);
        let _ = self.active().send(set_breakpoints_request(path, &set));
    }
}

/// `own` plus a pending Run to Cursor line when it is in `path`, as a plain
/// breakpoint: a logpoint, condition or hit count already on that line would
/// let the program run straight past it.
pub(super) fn with_run_to(
    run_to: Option<&(PathBuf, u32)>,
    path: &Path,
    own: &[SourceBreakpoint],
) -> Vec<SourceBreakpoint> {
    let mut set = own.to_vec();
    if let Some((target, line)) = run_to
        && target == path
    {
        set.retain(|b| b.line != *line);
        set.push(SourceBreakpoint::plain(*line));
    }
    set
}
