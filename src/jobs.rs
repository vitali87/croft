//! Background jobs: work that runs off the UI thread and hands back one
//! result, which the UI collects on a later tick with [`Job::poll`] (#2062).
//!
//! Every job is started with a [`Start`] chosen on purpose, so work the first
//! frame does not need waits for it (`startup_gate`) and a launch draws first.
//! A job runs on a thread named after it, so the crash notice says which job
//! crashed (#2214). And a job whose thread ends without a result reads as
//! [`Poll::Died`], never as still running: a hand-rolled channel whose sender
//! the `App` also kept could not tell the two apart, and whatever it showed
//! as in progress stayed that way until a restart (#2562).

use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

/// When a job's work may start. There is no default: each caller decides
/// whether the first frame waits for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Start {
    /// At once: someone is waiting on the result.
    Now,
    /// Once the first frame is on screen, which outside a launch is at once:
    /// work the first frame does not show, like Help: Update croft's check.
    AfterFirstFrame,
}

/// What a job has to report on this tick.
#[derive(Debug)]
pub enum Poll<T> {
    /// Still working.
    Pending,
    /// Finished, with its result.
    Done(T),
    /// Its thread ended without a result: the work panicked. The crash
    /// notice has already said so; the caller settles what it showed as in
    /// progress.
    Died,
}

/// One piece of background work and the result it will hand back.
pub struct Job<T> {
    rx: Receiver<T>,
}

impl<T: Send + 'static> Job<T> {
    /// Run `work` on a thread named `name`, starting when `start` says.
    /// Fails only when no thread can be started at all; the caller reports
    /// that at once instead of showing work in progress that never began.
    pub fn spawn(
        name: &str,
        start: Start,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> std::io::Result<Self> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name(name.to_string())
            .spawn(move || {
                if start == Start::AfterFirstFrame {
                    crate::startup_gate::wait();
                }
                let _ = tx.send(work());
            })?;
        Ok(Self { rx })
    }

    /// A job that has already finished with `value`: for work that failed
    /// before it could start (voice input whose dialog would not open), and
    /// for tests that hand the UI a result without running the work.
    pub fn finished(value: T) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = tx.send(value);
        Self { rx }
    }
}

impl<T> Job<T> {
    /// Wait for the job to settle: its result, or `None` when its thread
    /// ended without one. For a caller that has asked the work to stop and
    /// must not go on until it has, like a search scan cut short.
    pub fn wait(&self) -> Option<T> {
        self.rx.recv().ok()
    }

    /// What the job has to report now. After [`Poll::Done`] or
    /// [`Poll::Died`] it has nothing more to say, so the caller drops it.
    pub fn poll(&self) -> Poll<T> {
        match self.rx.try_recv() {
            Ok(value) => Poll::Done(value),
            Err(TryRecvError::Empty) => Poll::Pending,
            Err(TryRecvError::Disconnected) => Poll::Died,
        }
    }
}

/// What a [`Stream`] has to report on this tick.
#[derive(Debug)]
pub enum StreamPoll<T> {
    /// One of its updates, in the order they were sent.
    Update(T),
    /// Still working, with nothing new to say.
    Pending,
    /// Its work returned: every update has been read.
    Ended,
    /// Its thread ended before its work returned: the work panicked. The
    /// crash notice has already said so; the caller settles what it showed
    /// as in progress.
    Died,
}

/// What travels from a [`Stream`]'s work to its reader.
enum StreamItem<T> {
    Update(T),
    End,
}

/// Background work that sends several updates and then ends, like a staged
/// update's build (#2062). It runs on a thread named after it, and a thread
/// that ends before its work returns reads as [`StreamPoll::Died`], never as
/// work still going: a hand-rolled channel read "gone" as "nothing new yet",
/// and whatever it showed as in progress stayed that way until a restart.
pub struct Stream<T> {
    rx: Receiver<StreamItem<T>>,
    work: StreamWork,
    /// Holds a test's stream open, as a thread still at work would.
    #[cfg(test)]
    _open: Option<Sender<StreamItem<T>>>,
}

/// How a [`Stream`] knows its work's thread has ended. The channel alone
/// cannot say: a helper thread of the work's own may hold a clone of the
/// sink, as a remote install's log reader does while its build runs on, and
/// that keeps the channel open after the work's thread has crashed (#2605).
enum StreamWork {
    /// Its thread, watched until it ends.
    Thread(std::thread::JoinHandle<()>),
    /// Its thread ended and was joined.
    Ended,
    /// No thread of its own (a test's stream): it ends when its sender is
    /// dropped.
    #[cfg(test)]
    Held,
}

/// Where a [`Stream`]'s work sends its updates. A clone may go to a helper
/// thread of the work's own; an update a helper sends after the work has
/// returned may never be read, so work waits for its helpers before it
/// returns.
pub struct Sink<T> {
    tx: Sender<StreamItem<T>>,
}

// Not derived: that would ask `T: Clone`, and a sink is cloned for a helper
// thread of the work's own, never its updates.
impl<T> Clone for Sink<T> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
        }
    }
}

impl<T> Sink<T> {
    /// Send one update. False once the reader has gone, so long work can
    /// stop early.
    pub fn send(&self, update: T) -> bool {
        self.tx.send(StreamItem::Update(update)).is_ok()
    }
}

impl<T: Send + 'static> Stream<T> {
    /// Run `work` on a thread named `name`, starting when `start` says.
    /// Fails only when no thread can be started at all; the caller reports
    /// that at once instead of showing work in progress that never began.
    pub fn spawn(
        name: &str,
        start: Start,
        work: impl FnOnce(&Sink<T>) + Send + 'static,
    ) -> std::io::Result<Self> {
        let (tx, rx) = std::sync::mpsc::channel();
        let thread = std::thread::Builder::new()
            .name(name.to_string())
            .spawn(move || {
                if start == Start::AfterFirstFrame {
                    crate::startup_gate::wait();
                }
                let sink = Sink { tx };
                work(&sink);
                let _ = sink.tx.send(StreamItem::End);
            })?;
        Ok(Self {
            rx,
            work: StreamWork::Thread(thread),
            #[cfg(test)]
            _open: None,
        })
    }

    /// A stream whose work has sent `updates` and is still running, for
    /// tests that hand the UI updates without running the work.
    #[cfg(test)]
    pub fn running(updates: impl IntoIterator<Item = T>) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        for update in updates {
            let _ = tx.send(StreamItem::Update(update));
        }
        Self {
            rx,
            work: StreamWork::Held,
            _open: Some(tx),
        }
    }
}

impl<T> Stream<T> {
    /// What the stream has to report now: one update at a time, so a caller
    /// drains it in a loop until [`StreamPoll::Pending`]. After
    /// [`StreamPoll::Ended`] or [`StreamPoll::Died`] it has nothing more to
    /// say, so the caller drops it.
    pub fn poll(&mut self) -> StreamPoll<T> {
        match self.rx.try_recv() {
            Ok(StreamItem::Update(update)) => return StreamPoll::Update(update),
            Ok(StreamItem::End) => return StreamPoll::Ended,
            Err(TryRecvError::Disconnected) => return StreamPoll::Died,
            Err(TryRecvError::Empty) if !self.work_ended() => return StreamPoll::Pending,
            Err(TryRecvError::Empty) => {}
        }
        // Its thread has ended, so all it sent is in the channel: read again,
        // as it may have sent its last since the read above.
        match self.rx.try_recv() {
            Ok(StreamItem::Update(update)) => StreamPoll::Update(update),
            Ok(StreamItem::End) => StreamPoll::Ended,
            Err(_) => StreamPoll::Died,
        }
    }

    /// Whether its work's thread has ended, joining it once it has.
    fn work_ended(&mut self) -> bool {
        match &self.work {
            StreamWork::Thread(thread) if !thread.is_finished() => false,
            StreamWork::Thread(_) => {
                if let StreamWork::Thread(thread) =
                    std::mem::replace(&mut self.work, StreamWork::Ended)
                {
                    let _ = thread.join();
                }
                true
            }
            StreamWork::Ended => true,
            #[cfg(test)]
            StreamWork::Held => false,
        }
    }
}

/// How soon after a restart a second death stops a [`Worker`] instead of
/// starting it again, so a crash that repeats on the same input cannot loop.
pub const RESTART_WINDOW: Duration = Duration::from_secs(60);

/// What a [`Worker`] has to report on this tick.
#[derive(Debug)]
pub enum WorkerPoll<T> {
    /// One of its answers.
    Answer(T),
    /// Nothing more for now.
    Idle,
    /// Its thread had ended, and a fresh one has been started: what was
    /// sent to the old one is lost, so the caller sends again whatever the
    /// new thread needs. The crash notice has already said what happened.
    Restarted,
    /// Its thread ended again within [`RESTART_WINDOW`] of the last
    /// restart, or could not be started: it stays stopped until
    /// [`Worker::restart`]. Said once: after that the worker reads as
    /// `Idle`, so a caller that says so on the status line says it once.
    Stopped,
}

/// The body of a [`Worker`]'s thread: it reads requests until the channel
/// closes and sends its answers.
type WorkerRun<Req, Resp> = Arc<dyn Fn(Receiver<Req>, Sender<Resp>) + Send + Sync>;

/// A long-lived thread croft sends requests to and reads answers from, like
/// the git status worker (#2062). It runs on a thread named after it, and a
/// thread that ended reads as [`WorkerPoll::Restarted`] or
/// [`WorkerPoll::Stopped`], never as a quiet worker: a hand-rolled channel
/// read "gone" as "no answer yet", and whatever it fed froze until a
/// restart (#2589).
pub struct Worker<Req, Resp> {
    name: String,
    start: Start,
    run: WorkerRun<Req, Resp>,
    tx: Sender<Req>,
    rx: Receiver<Resp>,
    thread: Liveness,
    restarted_at: Option<Instant>,
    stopped: bool,
    /// [`WorkerPoll::Stopped`] has been reported for the current stop.
    stop_reported: bool,
}

/// How a [`Worker`] knows its thread has ended. The answer channel alone
/// cannot say: the thread may hand a sender to a thread of its own, as the
/// test runner does to the reader of a runner's error output, and that
/// sender keeps the channel open after the worker's thread has crashed
/// (#2592).
enum Liveness {
    /// Its thread, watched until it ends.
    Thread(std::thread::JoinHandle<()>),
    /// Its thread ended and was joined.
    Ended,
    /// No thread of its own (a test's channels): it ends when the other
    /// end of its answer channel is dropped.
    #[cfg(test)]
    ChannelOnly,
}

impl<Req: Send + 'static, Resp: Send + 'static> Worker<Req, Resp> {
    /// Run `run` on a thread named `name`, starting when `start` says. It
    /// is run again on a fresh thread each time the worker restarts. Fails
    /// only when no thread can be started at all.
    pub fn spawn(
        name: &str,
        start: Start,
        run: impl Fn(Receiver<Req>, Sender<Resp>) + Send + Sync + 'static,
    ) -> std::io::Result<Self> {
        let run: WorkerRun<Req, Resp> = Arc::new(run);
        let (tx, rx, thread) = start_worker_thread(name, start, &run)?;
        Ok(Self {
            name: name.to_string(),
            start,
            run,
            tx,
            rx,
            thread: Liveness::Thread(thread),
            restarted_at: None,
            stopped: false,
            stop_reported: false,
        })
    }

    /// Send `req`. To a thread that has ended it is lost, and the next
    /// [`Self::poll`] says so.
    pub fn send(&self, req: Req) {
        let _ = self.tx.send(req);
    }

    /// What the worker has to report now: one answer at a time, so a caller
    /// drains it in a loop until [`WorkerPoll::Idle`].
    pub fn poll(&mut self) -> WorkerPoll<Resp> {
        if self.stopped {
            if std::mem::replace(&mut self.stop_reported, true) {
                return WorkerPoll::Idle;
            }
            return WorkerPoll::Stopped;
        }
        match self.rx.try_recv() {
            Ok(answer) => return WorkerPoll::Answer(answer),
            Err(TryRecvError::Disconnected) => {}
            Err(TryRecvError::Empty) => {
                if !self.thread_ended() {
                    return WorkerPoll::Idle;
                }
                // Joined, so what it sent before it ended is all in the
                // channel: that is read before it is started again.
                if let Ok(answer) = self.rx.try_recv() {
                    return WorkerPoll::Answer(answer);
                }
            }
        }
        let again = self
            .restarted_at
            .is_some_and(|at| at.elapsed() < RESTART_WINDOW);
        if again || self.restart().is_err() {
            self.stopped = true;
            self.stop_reported = true;
            return WorkerPoll::Stopped;
        }
        WorkerPoll::Restarted
    }

    /// Whether its thread has ended, joining it once it has.
    fn thread_ended(&mut self) -> bool {
        match &self.thread {
            Liveness::Thread(thread) if !thread.is_finished() => false,
            #[cfg(test)]
            Liveness::ChannelOnly => false,
            Liveness::Ended => true,
            Liveness::Thread(_) => {
                if let Liveness::Thread(thread) =
                    std::mem::replace(&mut self.thread, Liveness::Ended)
                {
                    let _ = thread.join();
                }
                true
            }
        }
    }

    /// Start a fresh thread now, as after a death: for a caller retrying a
    /// stopped worker at the user's request.
    pub fn restart(&mut self) -> std::io::Result<()> {
        let (tx, rx, thread) = start_worker_thread(&self.name, self.start, &self.run)?;
        self.tx = tx;
        self.rx = rx;
        self.thread = Liveness::Thread(thread);
        self.restarted_at = Some(Instant::now());
        self.stopped = false;
        self.stop_reported = false;
        Ok(())
    }

    /// Whether the worker stopped and waits for [`Self::restart`].
    pub fn is_stopped(&self) -> bool {
        self.stopped
    }

    /// Start it again now if it stopped, or if its thread has ended though
    /// no [`Self::poll`] has seen it yet: for a caller acting on the user's
    /// request, which the next poll would otherwise answer with a stop.
    /// Whatever the old thread sent that was not read goes with it. True
    /// when a fresh thread started.
    pub fn restart_if_down(&mut self) -> std::io::Result<bool> {
        if !self.stopped && !self.thread_ended() {
            return Ok(false);
        }
        self.restart()?;
        Ok(true)
    }

    /// Whether its thread has ended, for a test that must see a crash no
    /// poll has seen.
    #[cfg(test)]
    pub fn has_ended(&mut self) -> bool {
        self.thread_ended()
    }
}

#[cfg(test)]
impl<Req, Resp> Worker<Req, Resp> {
    /// A worker over channels a test holds the other ends of: it answers
    /// what the test sends, and ends when the test drops its sender. A
    /// restart starts a thread that ends at once.
    pub fn from_channels(tx: Sender<Req>, rx: Receiver<Resp>) -> Self {
        Self {
            name: String::from("test-worker"),
            start: Start::Now,
            run: Arc::new(|_, _| {}),
            tx,
            rx,
            thread: Liveness::ChannelOnly,
            restarted_at: None,
            stopped: false,
            stop_reported: false,
        }
    }
}

type Started<Req, Resp> = (Sender<Req>, Receiver<Resp>, std::thread::JoinHandle<()>);

fn start_worker_thread<Req: Send + 'static, Resp: Send + 'static>(
    name: &str,
    start: Start,
    run: &WorkerRun<Req, Resp>,
) -> std::io::Result<Started<Req, Resp>> {
    let (req_tx, req_rx) = std::sync::mpsc::channel();
    let (resp_tx, resp_rx) = std::sync::mpsc::channel();
    let run = Arc::clone(run);
    let thread = std::thread::Builder::new()
        .name(name.to_string())
        .spawn(move || {
            if start == Start::AfterFirstFrame {
                crate::startup_gate::wait();
            }
            run(req_rx, resp_tx);
        })?;
    Ok((req_tx, resp_rx, thread))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Poll `job` until it has something other than `Pending` to say.
    fn settle<T>(job: &Job<T>) -> Poll<T> {
        let mut last = Poll::Pending;
        crate::test_budget::await_spawned(Duration::from_secs(1), "the job to settle", || {
            last = job.poll();
            !matches!(last, Poll::Pending)
        });
        last
    }

    #[test]
    fn a_job_hands_back_its_result_on_a_thread_named_after_it() {
        let job = Job::spawn("check-project", Start::Now, || {
            std::thread::current().name().map(String::from)
        })
        .unwrap();
        match settle(&job) {
            Poll::Done(name) => assert_eq!(name.as_deref(), Some("check-project")),
            _ => panic!("the job died"),
        }
    }

    /// #2562: a job whose work panics reads as dead, not as still running.
    #[test]
    fn a_job_whose_work_panics_reads_as_dead() {
        let job: Job<()> = Job::spawn("a-job-that-panics", Start::Now, || {
            panic!("the work crashed (on purpose, in a test)")
        })
        .unwrap();
        assert!(matches!(settle(&job), Poll::Died));
    }

    /// Outside a launch, as under test, a job held for the first frame
    /// starts at once.
    #[test]
    fn a_job_held_for_the_first_frame_runs_at_once_outside_a_launch() {
        let job = Job::spawn("after-first-frame", Start::AfterFirstFrame, || 42).unwrap();
        assert!(matches!(settle(&job), Poll::Done(42)));
    }

    /// Every update `stream` sends until it ends or dies, then how it
    /// finished: `true` when it ended.
    fn drain_stream<T>(stream: &mut Stream<T>) -> (Vec<T>, bool) {
        let mut updates = Vec::new();
        let mut ended = None;
        crate::test_budget::await_spawned(Duration::from_secs(1), "the stream to finish", || {
            loop {
                match stream.poll() {
                    StreamPoll::Update(update) => updates.push(update),
                    StreamPoll::Pending => return false,
                    StreamPoll::Ended => ended = Some(true),
                    StreamPoll::Died => ended = Some(false),
                }
                if ended.is_some() {
                    return true;
                }
            }
        });
        (updates, ended.expect("the stream finished"))
    }

    #[test]
    fn a_stream_sends_its_updates_in_order_then_ends_on_a_thread_named_after_it() {
        let mut stream = Stream::spawn("croft-update", Start::Now, |sink| {
            sink.send(std::thread::current().name().map(String::from));
            sink.send(None);
        })
        .unwrap();
        let (updates, ended) = drain_stream(&mut stream);
        assert_eq!(updates, vec![Some(String::from("croft-update")), None]);
        assert!(ended, "the stream ended");
    }

    /// A stream whose work panics reads as dead once its updates are read,
    /// not as work still going.
    #[test]
    fn a_stream_whose_work_panics_reads_as_dead_after_its_updates() {
        let mut stream: Stream<u32> = Stream::spawn("a-stream-that-panics", Start::Now, |sink| {
            sink.send(1);
            panic!("the work crashed (on purpose, in a test)")
        })
        .unwrap();
        let (updates, ended) = drain_stream(&mut stream);
        assert_eq!(updates, vec![1]);
        assert!(!ended, "the stream died");
    }

    /// #2605: a stream whose work crashed reads as dead while a helper
    /// thread of the work's own still holds a clone of its sink.
    #[test]
    fn a_stream_whose_work_crashed_reads_as_dead_while_a_helper_holds_its_sink() {
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let mut stream: Stream<u32> = Stream::spawn("a-stream-with-a-helper", Start::Now, |sink| {
            let helper = sink.clone();
            std::thread::spawn(move || {
                let _ = release_rx.recv();
                helper.send(2);
            });
            sink.send(1);
            panic!("the work crashed (on purpose, in a test)")
        })
        .unwrap();
        let (updates, ended) = drain_stream(&mut stream);
        assert_eq!(updates, vec![1]);
        assert!(!ended, "the stream died, its helper still running");
        let _ = release_tx.send(());
    }

    /// A test's running stream says what it was given, then nothing more.
    #[test]
    fn a_running_stream_stays_pending_after_its_updates() {
        let mut stream = Stream::running([1, 2]);
        assert!(matches!(stream.poll(), StreamPoll::Update(1)));
        assert!(matches!(stream.poll(), StreamPoll::Update(2)));
        assert!(matches!(stream.poll(), StreamPoll::Pending));
    }

    /// Poll `worker` until it has something other than `Idle` to say.
    fn next<Req: Send + 'static, Resp: Send + 'static>(
        worker: &mut Worker<Req, Resp>,
    ) -> WorkerPoll<Resp> {
        let mut last = WorkerPoll::Idle;
        crate::test_budget::await_spawned(Duration::from_secs(1), "the worker to report", || {
            last = worker.poll();
            !matches!(last, WorkerPoll::Idle)
        });
        last
    }

    /// An echo worker whose first `crashes` threads panic on their first
    /// request.
    fn echo_crashing(crashes: usize) -> Worker<u32, u32> {
        let started = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        Worker::spawn("echo", Start::Now, move |rx, tx| {
            let n = started.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            for req in rx {
                if n < crashes {
                    panic!("the worker crashed (on purpose, in a test)");
                }
                let _ = tx.send(req);
            }
        })
        .unwrap()
    }

    #[test]
    fn a_worker_answers_on_a_thread_named_after_it() {
        let mut worker: Worker<(), Option<String>> =
            Worker::spawn("git-status", Start::Now, |rx, tx| {
                for () in rx {
                    let _ = tx.send(std::thread::current().name().map(String::from));
                }
            })
            .unwrap();
        worker.send(());
        match next(&mut worker) {
            WorkerPoll::Answer(name) => assert_eq!(name.as_deref(), Some("git-status")),
            other => panic!("got {other:?}"),
        }
    }

    /// #2589: a worker whose thread crashed is started again once, and the
    /// fresh thread answers what is sent to it.
    #[test]
    fn a_worker_whose_thread_crashed_is_started_again() {
        let mut worker = echo_crashing(1);
        worker.send(1);
        assert!(matches!(next(&mut worker), WorkerPoll::Restarted));
        worker.send(2);
        assert!(matches!(next(&mut worker), WorkerPoll::Answer(2)));
    }

    /// #2592: a thread that hands a sender to a thread of its own and then
    /// crashes is seen to have ended while that sender is still held, as
    /// the test runner's reader of a runner's error output holds one.
    #[test]
    fn a_worker_whose_thread_crashed_while_its_helper_holds_a_sender_is_started_again() {
        let (let_go, held) = std::sync::mpsc::channel::<()>();
        let held = std::sync::Mutex::new(Some(held));
        let started = std::sync::atomic::AtomicUsize::new(0);
        let mut worker: Worker<u32, u32> = Worker::spawn("helper", Start::Now, move |rx, tx| {
            if started.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                let helper_tx = tx.clone();
                let held = held.lock().unwrap().take().unwrap();
                std::thread::spawn(move || {
                    let _ = held.recv();
                    drop(helper_tx);
                });
                let _ = rx.recv();
                panic!("the worker crashed (on purpose, in a test)");
            }
            while let Ok(n) = rx.recv() {
                let _ = tx.send(n);
            }
        })
        .unwrap();
        worker.send(1);
        assert!(
            matches!(next(&mut worker), WorkerPoll::Restarted),
            "its end is seen while the helper still holds a sender"
        );
        worker.send(2);
        assert!(matches!(next(&mut worker), WorkerPoll::Answer(2)));
        let _ = let_go.send(());
    }

    /// #2589: a second crash soon after a restart stops the worker instead
    /// of looping; an explicit restart starts it again.
    #[test]
    fn a_worker_that_crashes_again_stops_until_restarted() {
        let mut worker = echo_crashing(2);
        worker.send(1);
        assert!(matches!(next(&mut worker), WorkerPoll::Restarted));
        worker.send(2);
        assert!(matches!(next(&mut worker), WorkerPoll::Stopped));
        assert!(worker.is_stopped());
        assert!(matches!(worker.poll(), WorkerPoll::Idle), "it says so once");
        assert!(worker.is_stopped(), "and stays stopped");
        worker.restart().unwrap();
        worker.send(3);
        assert!(matches!(next(&mut worker), WorkerPoll::Answer(3)));
    }

    /// Codex's finding on #2600: a worker whose thread ended where no poll
    /// saw it is started again on request, and a running one is left be.
    #[test]
    fn a_worker_down_where_no_poll_saw_it_is_restarted_on_request() {
        let mut worker = echo_crashing(1);
        assert!(
            !worker.restart_if_down().unwrap(),
            "a running worker is left be"
        );
        worker.send(1);
        crate::test_budget::await_spawned(Duration::from_secs(1), "the thread to end", || {
            worker.has_ended()
        });
        assert!(worker.restart_if_down().unwrap());
        worker.send(2);
        assert!(matches!(next(&mut worker), WorkerPoll::Answer(2)));
    }

    #[test]
    fn a_finished_job_reports_its_value_once() {
        let job = Job::finished("done");
        assert!(matches!(job.poll(), Poll::Done("done")));
        assert!(matches!(job.poll(), Poll::Died));
    }
}
