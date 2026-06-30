//! Test Runner subsystem.
//!
//! Mirrors the worker/drain shape of [`crate::app::git_worker`] and the DAP
//! client in [`crate::dap`]: a background thread spawns the project's test tool
//! (`cargo test` for M1), streams its output back over an mpsc channel, and the
//! app drains results into the Testing panel each tick. Test-tool output is
//! parsed into [`model::TestCase`]s; the suite tree and the failing-count badge
//! are derived in the UI layer.

pub mod model;
pub mod parse;
pub mod worker;
