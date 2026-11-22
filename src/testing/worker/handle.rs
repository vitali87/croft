//! The app's side of the worker: queueing each kind of run and discovery, cancelling,
//! re-rooting, and draining the streamed results into the panel.

use super::*;

impl TestWorker {
    pub fn spawn(workspace_root: PathBuf) -> Self {
        let (request_tx, request_rx) = std::sync::mpsc::channel::<TestRequest>();
        let (response_tx, response_rx) = std::sync::mpsc::channel::<(u64, TestResponse)>();
        #[cfg(test)]
        let root = workspace_root.clone();
        let control = RunControl::default();
        let worker_control = control.clone();
        std::thread::spawn(move || {
            crate::startup_gate::wait();
            worker_loop(workspace_root, request_rx, response_tx, &worker_control)
        });
        Self {
            request_tx,
            control,
            sent: 0,
            cancel_pending: false,
            response_rx,
            discover_queued: false,
            expected_epoch: 0,
            #[cfg(test)]
            root,
        }
    }

    /// Build a worker around hand-made channels (no thread) so tests can
    /// inject tagged responses straight into the drain.
    #[cfg(test)]
    pub(super) fn for_test() -> (Self, Sender<(u64, TestResponse)>) {
        let (request_tx, _request_rx) = std::sync::mpsc::channel::<TestRequest>();
        let (response_tx, response_rx) = std::sync::mpsc::channel::<(u64, TestResponse)>();
        (
            Self {
                request_tx,
                control: RunControl::default(),
                sent: 0,
                cancel_pending: false,
                response_rx,
                discover_queued: false,
                expected_epoch: 0,
                root: PathBuf::new(),
            },
            response_tx,
        )
    }

    /// Queue `req`, numbered as the worker will number it.
    pub(super) fn send(&mut self, req: TestRequest) {
        self.sent += 1;
        let _ = self.request_tx.send(req);
    }

    pub fn run_coverage(&mut self) {
        self.send(TestRequest::RunCoverage(None));
    }

    /// Run only the tests `scope` names with coverage on (#263).
    pub fn run_coverage_scoped(&mut self, scope: CoverageScope) {
        self.send(TestRequest::RunCoverage(Some(scope)));
    }

    pub fn run_all(&mut self) {
        self.send(TestRequest::RunAll);
    }

    pub fn run_one(&mut self, name: String) {
        self.send(TestRequest::RunOne(name));
    }

    pub fn run_many(&mut self, names: Vec<String>) {
        self.send(TestRequest::RunMany(names));
    }

    /// Run the tests matching `pattern`, with the runner for `file`'s
    /// language when the run came from one (#1408).
    pub fn run_filter(&mut self, pattern: String, file: Option<PathBuf>) {
        self.send(TestRequest::RunFilter(pattern, file));
    }

    pub fn run_suite(&mut self, suite: String) {
        self.send(TestRequest::RunSuite(suite));
    }

    pub fn discover(&mut self) {
        self.send(TestRequest::Discover { quiet: false });
    }

    /// Discover from the Testing view (#1841): queued without looking for a
    /// runner first, so opening the view never waits on that search. The
    /// worker finds the runner and stays silent when there is none.
    pub fn discover_quietly(&mut self) {
        self.discover_queued = true;
        self.send(TestRequest::Discover { quiet: true });
    }

    /// A quiet discovery is queued and has not started yet.
    pub fn discover_queued(&self) -> bool {
        self.discover_queued
    }

    /// How many requests have been queued, for tests that count them.
    #[cfg(test)]
    pub fn requests_sent(&self) -> u64 {
        self.sent
    }

    pub fn set_codeql_program(&mut self, program: PathBuf) {
        self.send(TestRequest::SetCodeqlProgram(program));
    }

    pub fn set_extensions_dir(&mut self, dir: PathBuf) {
        self.send(TestRequest::SetExtensionsDir(dir));
    }

    /// Stop the run or discovery in flight (#1534): its runner's process
    /// group is killed, and its end reaches the panel as cancelled. Also
    /// called on quit, so no runner outlives croft.
    pub fn cancel(&mut self) {
        self.control.cancel(self.sent);
        self.cancel_pending = true;
    }

    /// Rebind the worker to a new workspace root after an Explorer re-root.
    /// Everything still streaming for the old root carries the old epoch and
    /// is dropped by [`Self::drain`].
    pub fn set_root(&mut self, root: PathBuf) {
        #[cfg(test)]
        {
            self.root = root.clone();
        }
        self.expected_epoch += 1;
        // A discovery queued for the old root reports under the old epoch,
        // which the drain drops: nothing would ever clear it.
        self.discover_queued = false;
        // Every request a pending cancel covers was sent before this
        // re-root, so its end carries the old epoch and is dropped below:
        // the cancel must not outlive it and swallow the new root's next
        // verdict.
        self.cancel_pending = false;
        self.send(TestRequest::SetRoot(root));
    }

    #[cfg(test)]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Drain streamed results into the panel. Returns true iff anything was
    /// applied, so the main loop only redraws on a real update. Responses
    /// tagged with an epoch older than the last `set_root` belong to the
    /// previous project and are dropped.
    pub fn drain(&mut self, panel: &mut TestingPanel) -> bool {
        let mut changed = false;
        while let Ok((epoch, resp)) = self.response_rx.try_recv() {
            if epoch != self.expected_epoch {
                continue;
            }
            match resp {
                TestResponse::QuietDiscovery => {
                    self.discover_queued = false;
                    continue;
                }
                TestResponse::Started(activity) => panel.on_busy_started(activity),
                TestResponse::Case(case) => panel.apply_case(case),
                TestResponse::Progress(line) => panel.set_progress(line),
                TestResponse::Finished { .. } | TestResponse::Refused
                    if std::mem::take(&mut self.cancel_pending) =>
                {
                    panel.on_cancelled()
                }
                TestResponse::Finished { ok } => panel.on_finished(ok),
                TestResponse::Refused => panel.on_refused(),
                // A cancelled coverage run's killed child wrote no report:
                // that error is not news, and would overwrite the cancel.
                TestResponse::Coverage(_) if self.cancel_pending => continue,
                TestResponse::Coverage(result) => panel.on_coverage(result),
            }
            changed = true;
        }
        changed
    }
}
