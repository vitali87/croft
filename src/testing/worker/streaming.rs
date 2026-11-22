//! Streaming a runner's output: each line tagged with its request's epoch and runner,
//! stderr teed to OUTPUT, and the exit code.

use super::*;

impl<'a> EpochTx<'a> {
    pub(super) fn send(&self, resp: TestResponse) {
        if let (Some(owners), Some(runner), TestResponse::Case(case)) =
            (self.owners, self.tag, &resp)
        {
            owners.borrow_mut().insert(case.name.clone(), runner);
        }
        let _ = self.tx.send((self.epoch, resp));
    }

    /// This sender, recording what it carries as `runner`'s.
    pub(super) fn tagged(&self, runner: Runner) -> EpochTx<'a> {
        EpochTx {
            tag: Some(runner),
            ..*self
        }
    }

    /// The runner a run of `name` goes to, among `runners`.
    pub(super) fn choose(
        &self,
        runners: &[Runner],
        name: &str,
        file: Option<&Path>,
    ) -> Option<Runner> {
        let empty = BTreeMap::new();
        match self.owners {
            Some(owners) => choose_runner(runners, &owners.borrow(), name, file),
            None => choose_runner(runners, &empty, name, file),
        }
    }

    /// An owned clone for the stderr tee thread.
    pub(super) fn to_owned(&self) -> (Sender<(u64, TestResponse)>, u64) {
        (self.tx.clone(), self.epoch)
    }
}

/// Adapt a one-line-one-case parser to [`run_streaming`]'s many-cases shape
/// (jest's `--json` yields every case from a single stdout line, so the
/// streaming contract is a `Vec` per line).
pub(super) fn one(parse: fn(&str) -> Option<TestCase>) -> impl Fn(&str) -> Vec<TestCase> {
    move |line| parse(line).into_iter().collect()
}

/// Spawn `cmd`, tee stderr (compile diagnostics) to the OUTPUT channel, and run
/// each stdout line through `parse` — every matched [`TestCase`] is streamed as
/// it arrives. Returns the child's exit success, or `None` if it never spawned.
pub(super) fn run_streaming(
    tx: &EpochTx,
    cmd: Command,
    parse: impl Fn(&str) -> Vec<TestCase>,
) -> Option<bool> {
    run_streaming_shown(tx, cmd, |line| Some(line.to_string()), parse)
}

/// [`run_streaming`], with `show` choosing what of each stdout line the
/// OUTPUT channel gets (`None` hides it): `go test -json` wraps the tests'
/// own text in events.
pub(super) fn run_streaming_shown(
    tx: &EpochTx,
    cmd: Command,
    show: impl Fn(&str) -> Option<String>,
    parse: impl Fn(&str) -> Vec<TestCase>,
) -> Option<bool> {
    run_streaming_exit(tx, cmd, show, parse).map(|code| code == Some(0))
}

/// [`run_streaming_shown`], answering the child's exit code rather than
/// its success, for a caller that reads a nonzero code as something other
/// than failure. `Some(None)` when it died to a signal or could not be
/// waited on, `None` if it never spawned.
pub(super) fn run_streaming_exit(
    tx: &EpochTx,
    mut cmd: Command,
    show: impl Fn(&str) -> Option<String>,
    parse: impl Fn(&str) -> Vec<TestCase>,
) -> Option<Option<i32>> {
    let mut child = match tx.control.spawn(tx.request, &mut cmd) {
        // Cancelled before it started: it ended without a verdict.
        None => return Some(None),
        Some(Ok(c)) => c,
        Some(Err(e)) => {
            output::push(
                output::CHANNEL_TESTS,
                OutputLevel::Error,
                &format!("failed to spawn the test runner: {e}"),
            );
            return None;
        }
    };
    let stderr_handle = child.stderr.take().map(|err| {
        let (tx, epoch) = tx.to_owned();
        std::thread::spawn(move || {
            for line in BufReader::new(err).lines().map_while(Result::ok) {
                output::push(output::CHANNEL_TESTS, OutputLevel::Info, &line);
                if let Some(p) = cargo_progress(&line) {
                    let _ = tx.send((epoch, TestResponse::Progress(p)));
                }
            }
        })
    });
    if let Some(out) = child.stdout.take() {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            if let Some(text) = show(&line) {
                output::push(output::CHANNEL_TESTS, OutputLevel::Info, &text);
            }
            for case in parse(&line) {
                tx.send(TestResponse::Case(case));
            }
        }
    }
    if let Some(h) = stderr_handle {
        let _ = h.join();
    }
    let code = child.wait().ok().and_then(|s| s.code());
    tx.control.finished();
    Some(code)
}
