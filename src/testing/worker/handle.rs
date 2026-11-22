//! The app's side of the worker: queueing each kind of run and discovery, cancelling,
//! re-rooting, and draining the streamed results into the panel.

use super::*;

impl TestWorker {
    pub fn spawn(workspace_root: PathBuf) -> Self {
        Self::spawn_with(workspace_root, worker_loop)
    }

    /// A worker whose thread runs `work`, the real loop outside tests.
    pub(super) fn spawn_with(
        workspace_root: PathBuf,
        work: impl Fn(PathBuf, Receiver<Sent>, Sender<(u64, TestResponse)>, &RunControl)
        + Send
        + Sync
        + 'static,
    ) -> Self {
        let control = RunControl::default();
        let thread_control = control.clone();
        let first_root = workspace_root.clone();
        let worker = Worker::spawn("test-runner", Start::AfterFirstFrame, move |rx, tx| {
            work(first_root.clone(), rx, tx, &thread_control)
        })
        .expect("start the test runner");
        Self::around(worker, control, workspace_root)
    }

    fn around(
        worker: Worker<Sent, (u64, TestResponse)>,
        control: RunControl,
        root: PathBuf,
    ) -> Self {
        Self {
            worker,
            control,
            sent: 0,
            cancel_pending: false,
            discover_queued: false,
            expected_epoch: 0,
            root,
            codeql: None,
            extensions: None,
            stopped_news: false,
            stop_seen: false,
        }
    }

    /// Build a worker around hand-made channels (no thread) so tests can
    /// inject tagged responses straight into the drain.
    #[cfg(test)]
    pub(super) fn for_test() -> (Self, Sender<(u64, TestResponse)>) {
        let (request_tx, _request_rx) = std::sync::mpsc::channel::<Sent>();
        let (response_tx, response_rx) = std::sync::mpsc::channel::<(u64, TestResponse)>();
        let worker = Worker::from_channels(request_tx, response_rx);
        (
            Self::around(worker, RunControl::default(), PathBuf::new()),
            response_tx,
        )
    }

    /// The real worker, whose first `crashes` threads panic on their first
    /// request (#2592).
    #[cfg(test)]
    pub(crate) fn crashing(root: PathBuf, crashes: usize) -> Self {
        let started = Arc::new(AtomicU64::new(0));
        Self::spawn_with(root, move |root, rx, tx, control| {
            if started.fetch_add(1, Ordering::SeqCst) < crashes as u64 {
                let _ = rx.recv();
                panic!("the test runner crashed (on purpose, in a test)");
            }
            worker_loop(root, rx, tx, control);
        })
    }

    /// Queue `request`, numbered and tagged with the current root.
    pub(super) fn send(&mut self, request: TestRequest) {
        self.sent += 1;
        self.worker.send(Sent {
            number: self.sent,
            epoch: self.expected_epoch,
            request,
        });
    }

    /// Queue a run or discovery. A worker that stopped after crashing
    /// twice starts again here: the user asked for tests (#2592).
    fn ask(&mut self, request: TestRequest) {
        if self.worker.is_stopped() && self.worker.restart().is_ok() {
            self.stop_seen = false;
            self.resend_settings();
        }
        self.send(request);
    }

    /// Give a fresh thread the root and settings its crashed one had
    /// (#2592), under the same epoch: nothing about the root changed.
    fn resend_settings(&mut self) {
        self.send(TestRequest::SetRoot(self.root.clone()));
        if let Some(program) = self.codeql.clone() {
            self.send(TestRequest::SetCodeqlProgram(program));
        }
        if let Some(dir) = self.extensions.clone() {
            self.send(TestRequest::SetExtensionsDir(dir));
        }
    }

    pub fn run_coverage(&mut self) {
        self.ask(TestRequest::RunCoverage(None));
    }

    /// Run only the tests `scope` names with coverage on (#263).
    pub fn run_coverage_scoped(&mut self, scope: CoverageScope) {
        self.ask(TestRequest::RunCoverage(Some(scope)));
    }

    pub fn run_all(&mut self) {
        self.ask(TestRequest::RunAll);
    }

    pub fn run_one(&mut self, name: String) {
        self.ask(TestRequest::RunOne(name));
    }

    pub fn run_many(&mut self, names: Vec<String>) {
        self.ask(TestRequest::RunMany(names));
    }

    /// Run the tests matching `pattern`, with the runner for `file`'s
    /// language when the run came from one (#1408).
    pub fn run_filter(&mut self, pattern: String, file: Option<PathBuf>) {
        self.ask(TestRequest::RunFilter(pattern, file));
    }

    pub fn run_suite(&mut self, suite: String) {
        self.ask(TestRequest::RunSuite(suite));
    }

    pub fn discover(&mut self) {
        self.ask(TestRequest::Discover { quiet: false });
    }

    /// Discover from the Testing view (#1841): queued without looking for a
    /// runner first, so opening the view never waits on that search. The
    /// worker finds the runner and stays silent when there is none.
    pub fn discover_quietly(&mut self) {
        self.discover_queued = true;
        self.ask(TestRequest::Discover { quiet: true });
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
        self.codeql = Some(program.clone());
        self.send(TestRequest::SetCodeqlProgram(program));
    }

    pub fn set_extensions_dir(&mut self, dir: PathBuf) {
        self.extensions = Some(dir.clone());
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
        self.root = root.clone();
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
        loop {
            let (epoch, resp) = match self.worker.poll() {
                WorkerPoll::Answer(answer) => answer,
                WorkerPoll::Idle => break,
                // Its thread crashed, and the crash notice said so. A
                // fresh one has the same root and settings; the run it
                // was on is not sent again, as a run can have side
                // effects (#2592).
                WorkerPoll::Restarted => {
                    changed |= self.thread_ended(panel);
                    self.resend_settings();
                    continue;
                }
                WorkerPoll::Stopped => {
                    if !std::mem::replace(&mut self.stop_seen, true) {
                        self.thread_ended(panel);
                        self.stopped_news = true;
                        changed = true;
                    }
                    break;
                }
            };
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

    /// The worker's thread ended (#2592). What was sent to it went with
    /// it: the runner it was reading from is killed, a run or discovery
    /// in flight ends as stopped, and a queued quiet discovery is
    /// forgotten so the Testing view can queue another. True when the
    /// panel changed.
    fn thread_ended(&mut self, panel: &mut TestingPanel) -> bool {
        self.control.abandon(self.sent);
        self.discover_queued = false;
        self.cancel_pending = false;
        if !panel.is_busy() {
            return false;
        }
        panel.on_stopped();
        true
    }

    /// True once when the worker has stopped after crashing again soon
    /// after a restart (#2592), so the app can say so.
    pub fn take_stopped(&mut self) -> bool {
        std::mem::take(&mut self.stopped_news)
    }
}
