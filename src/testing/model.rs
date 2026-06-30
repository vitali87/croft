//! Shared data types for the Test Runner: the per-test status and case record
//! the parser produces and the worker streams to the app.

/// What the worker is currently doing, surfaced as the panel's busy state.
/// `Discovering` lists tests without running them (`cargo test -- --list`);
/// `Running` executes them. Both compile the test binary first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activity {
    Idle,
    Discovering,
    Running,
}

/// Lifecycle of one test, mirroring VS Code's test states. `NotRun` is the
/// discovered-but-unexecuted state; `Running` is set while a run is in flight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TestStatus {
    NotRun,
    Running,
    Passed,
    Failed,
    Skipped,
}

/// One test case keyed by its full path as the runner prints it (for cargo,
/// `module::submodule::name`). The suite grouping in the panel is derived from
/// this path, so the parser need not know about the tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestCase {
    pub name: String,
    pub status: TestStatus,
}

impl TestCase {
    /// Split `module::submodule::name` into (suite, leaf): everything before the
    /// last `::` is the suite, the last segment is the leaf. A path with no `::`
    /// (a crate-root test) has no suite.
    pub fn suite_and_leaf(&self) -> (Option<&str>, &str) {
        match self.name.rsplit_once("::") {
            Some((suite, leaf)) => (Some(suite), leaf),
            None => (None, self.name.as_str()),
        }
    }
}
