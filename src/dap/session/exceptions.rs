//! Exception breakpoints: the filters an adapter offers, which of them mean "break on
//! raised exceptions", turning those on and off, and the one-line summary of what was
//! thrown.

use super::*;

/// The ids adapters give their "break where an exception is thrown, caught
/// or not" filter (#1426): debugpy `raised`, js-debug and netcoredbg `all`,
/// java `caught`. First offered wins.
pub(super) const RAISED_FILTER_IDS: &[&str] = &["raised", "all", "caught"];

/// lldb-dap and CodeLLDB split "break on throw" per language: C++
/// `cpp_throw`, Objective-C `objc_throw`, Swift `swift_throw`. Every one
/// offered is turned on, so Objective-C(++) `@throw` stops too.
pub(super) const THROW_FILTER_IDS: &[&str] = &["cpp_throw", "objc_throw", "swift_throw"];

/// The filter ids that mean "break on raised exceptions" among those an
/// adapter offers; `None` when it offers none of them.
pub(super) fn raised_filter_ids(offered: &[(String, bool)]) -> Option<Vec<String>> {
    let has = |id: &str| offered.iter().any(|(o, _)| o == id);
    if let Some(id) = RAISED_FILTER_IDS.iter().find(|id| has(id)) {
        return Some(vec![id.to_string()]);
    }
    let throws: Vec<String> = THROW_FILTER_IDS
        .iter()
        .filter(|id| has(id))
        .map(|id| id.to_string())
        .collect();
    (!throws.is_empty()).then_some(throws)
}

/// The exception filters an adapter's `initialize` reply offers, as
/// `(id, default)`. `None` when the reply lists none.
pub(super) fn offered_exception_filters(caps: &Value) -> Option<Vec<(String, bool)>> {
    let list = caps.get("exceptionBreakpointFilters")?.as_array()?;
    Some(
        list.iter()
            .filter_map(|f| {
                let id = f.get("filter")?.as_str()?.to_string();
                Some((id, f.get("default").and_then(Value::as_bool) == Some(true)))
            })
            .collect(),
    )
}

/// Build a `setExceptionBreakpoints` request. `filters` are adapter-defined ids;
/// debugpy supports `raised`, `uncaught`, and `userUnhandled`. An empty slice
/// disables exception breaking.
pub fn set_exception_breakpoints_request(filters: &[String]) -> Value {
    json!({
        "type": "request",
        "command": "setExceptionBreakpoints",
        "arguments": { "filters": filters }
    })
}

/// One line naming what an `exceptionInfo` response says was thrown (#864):
/// `exceptionId: description`, with only the description's first line (an
/// adapter may append the stack) and without repeating an id the
/// description already opens with. None when the response names nothing.
pub fn exception_summary(response: &Value) -> Option<String> {
    let body = response.get("body")?;
    let text = |pointer: &str| {
        body.pointer(pointer)
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("")
    };
    // debugpy pads the id with "(note: full exception trace is shown but
    // execution is paused at: …)" when the stop is outside user code.
    let id = text("/exceptionId")
        .split("(note:")
        .next()
        .unwrap_or("")
        .trim();
    let description = [text("/description"), text("/details/message")]
        .into_iter()
        .find(|s| !s.is_empty())
        .and_then(|s| s.lines().next())
        .unwrap_or("")
        .trim();
    match (id.is_empty(), description.is_empty()) {
        (true, true) => None,
        (false, true) => Some(id.to_string()),
        (true, false) => Some(description.to_string()),
        (false, false) if description.starts_with(id) => Some(description.to_string()),
        (false, false) => Some(format!("{id}: {description}")),
    }
}

impl DapSession {
    /// Replace the active exception-breakpoint filters, resending immediately so
    /// the change takes effect mid-session (debugpy accepts it at any time).
    pub fn set_exception_filters(&mut self, filters: Vec<String>) {
        self.exception_filters = filters;
        // The active connection: for js-debug that is the child session,
        // where the debuggee runs; the parent ignores exception filters.
        let _ = self
            .active()
            .send(set_exception_breakpoints_request(&self.exception_filters));
    }

    /// This adapter's ids for "break on raised exceptions" (#1426):
    /// debugpy's `raised`, js-debug's `all`, lldb-dap's `cpp_throw` and
    /// `objc_throw`. `None` before the adapter answers `initialize`, or when
    /// it offers no such filter (Delve).
    pub fn raised_exception_filters(&self) -> Option<Vec<String>> {
        raised_filter_ids(&offered_exception_filters(&self.capabilities)?)
    }

    /// Turn the adapter's raised-exceptions filters on or off, keeping the
    /// other active filters (`uncaught`), and resend. `Some(on)` says which
    /// way it went; `None` (nothing sent) when the adapter has no such filter.
    /// Before the adapter answers `initialize` the wanted state is kept and
    /// applied with its own ids once it does.
    pub fn toggle_raised_exceptions(&mut self) -> Option<bool> {
        if self.capabilities.is_null() {
            let on = !self.pending_raised.unwrap_or(false);
            self.pending_raised = Some(on);
            return Some(on);
        }
        let raised = self.raised_exception_filters()?;
        let on = !raised.iter().any(|r| self.has_exception_filter(r));
        let mut filters: Vec<String> = self
            .exception_filters
            .iter()
            .filter(|f| !raised.contains(f))
            .cloned()
            .collect();
        if on {
            filters.extend(raised);
        }
        self.set_exception_filters(filters);
        Some(on)
    }

    /// Whether an exception filter id is currently active.
    pub fn has_exception_filter(&self, filter: &str) -> bool {
        self.exception_filters.iter().any(|f| f == filter)
    }
}
