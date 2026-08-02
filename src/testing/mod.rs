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
