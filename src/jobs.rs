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

use std::sync::mpsc::{Receiver, TryRecvError};

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

    /// A job that has already finished with `value`, for tests that hand
    /// the UI a result without running the work.
    #[cfg(test)]
    pub fn finished(value: T) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = tx.send(value);
        Self { rx }
    }
}

impl<T> Job<T> {
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

    #[test]
    fn a_finished_job_reports_its_value_once() {
        let job = Job::finished("done");
        assert!(matches!(job.poll(), Poll::Done("done")));
        assert!(matches!(job.poll(), Poll::Died));
    }
}
