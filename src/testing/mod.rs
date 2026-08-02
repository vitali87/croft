//! Test Runner subsystem.
//!
//! Mirrors the worker/drain shape of [`crate::app::git_worker`] and the DAP
//! client in [`crate::dap`]: a background thread spawns the project's test tool
//! (cargo / pytest / vitest / jest — which one a workspace uses is resolved by
//! [`registry`] from `[[test_runners]]` manifest data), streams its output back
//! over an mpsc channel, and the app drains results into the Testing panel each
//! tick. Test-tool output is parsed into [`model::TestCase`]s; the suite tree
//! and the failing-count badge are derived in the UI layer.

pub mod locate;
pub mod model;
pub mod parse;
pub mod registry;
pub mod worker;

/// Status-bar message for a run gesture refused because no enabled runner
/// claims the workspace. One string for both refusal sites: the app's
/// entry-point check and the worker's queued-request refusal.
pub const NO_RUNNER_STATUS: &str =
    "No test runner detected in this workspace (is its extension enabled?)";

/// The run pattern for a whole suite. cargo's positional filter and the
/// panel's marking are substring matches, so a bare `parse` would also sweep
/// `parse_utils::b`; anchoring with the `::` separator selects exactly the
/// suite's own cases. One helper so the panel and the worker cannot drift.
pub fn suite_pattern(suite: &str) -> String {
    format!("{suite}::")
}

/// Escape a string for literal use inside a regex (test titles are arbitrary;
/// an unbalanced bracket would abort the matcher). Shared by the JS runner
/// argv builders and the source locator.
pub(crate) fn regex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if "\\^$.|?*+()[]{}".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}
