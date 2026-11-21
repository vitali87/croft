//! CodeQL: Run Tests and Accept Test Output: every CodeQL test run through the Testing
//! view's run-all, and a failing test's output made its expected output.

use super::*;

impl App {
    /// CodeQL: Run Tests (#578): every CodeQL test, through the Testing
    /// view's run-all, with the same `codeql` the side bar runs. Refused,
    /// with the reason, when the workspace's runner is not the CodeQL one.
    pub(in crate::app) fn run_codeql_tests(&mut self) {
        use crate::testing::worker::{Runner, runner_for};
        self.settle_followed_test_root();
        if runner_for(&self.extensions_dir(), &self.active_test_root) != Some(Runner::Codeql) {
            self.status = String::from(
                "No CodeQL tests here: a test pack's qlpack.yml declares tests: or extractor:",
            );
            return;
        }
        if self.refuse_test_run_while_busy() {
            return;
        }
        self.run_all_tests();
        self.status = String::from("Running CodeQL tests");
        self.await_test_run_outcome(String::from("CodeQL tests"));
    }

    /// CodeQL: Accept Test Output (#578): make a failing CodeQL test's
    /// actual results its expected ones, through `codeql test accept`. The
    /// test is the one the open file belongs to (its `.ql`, `.qlref`,
    /// `.expected` or `.actual`), else every CodeQL test the last run
    /// failed. Only tests a run left `.actual` output for are accepted.
    pub(in crate::app) fn accept_codeql_test_output(&mut self) {
        use crate::testing::codeqltest as ct;
        let tests: Vec<PathBuf> = match self.editor.path.as_deref().and_then(ct::test_for_file) {
            Some(test) => vec![test],
            None => self
                .testing
                .failed_names()
                .into_iter()
                .map(|id| self.active_test_root.join(ct::id_path(&id)))
                .filter(|t| t.is_file())
                .collect(),
        };
        if tests.is_empty() {
            self.status = String::from(
                "No CodeQL test to accept: open one of its files, or run the tests first",
            );
            return;
        }
        let ready: Vec<PathBuf> = tests
            .into_iter()
            .filter(|t| ct::actual_output(t).is_file())
            .collect();
        if ready.is_empty() {
            self.status = String::from(
                "No actual output to accept: run the test first, and accept it once it fails",
            );
            return;
        }
        let what = match ready.as_slice() {
            [one] => one
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
            many => format!("{} tests", many.len()),
        };
        self.start_codeql_pack_job(
            ct::accept_args(&ready),
            format!("Accepting the output of {what}\u{2026}"),
            format!("Accepted the output of {what} as expected"),
            format!("Could not accept the output of {what}"),
        );
    }
}
