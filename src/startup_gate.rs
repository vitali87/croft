//! Holding startup's background work back until the first frame is up.
//!
//! A launch starts a dozen jobs the first frame never shows: the syntax
//! highlight prewarm (hundreds of milliseconds of CPU on its own), the
//! Cmd+P file index, git status, the file watcher, the extension index
//! download, the activity-bar icon bake. Started at once, they outnumbered
//! the cores and the UI thread spent most of its way to the first frame
//! runnable but not running: five milliseconds of work took tens. Each such
//! job now calls [`wait`] first, which returns at once except during a
//! launch [`arm`]ed by `app::run` whose first frame has not been flushed
//! yet; `main_loop` calls [`open`] when it has.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

/// The longest a held-back job waits: a first frame that never comes (a
/// start that fails, a frame loop that errors) must not strand the work.
const CAP: Duration = Duration::from_secs(1);

struct Gate {
    armed: AtomicBool,
    open: Mutex<bool>,
    opened: Condvar,
}

impl Gate {
    const fn new() -> Self {
        Self {
            armed: AtomicBool::new(false),
            open: Mutex::new(false),
            opened: Condvar::new(),
        }
    }

    fn arm(&self) {
        self.armed.store(true, Ordering::Release);
    }

    fn open(&self) {
        if let Ok(mut open) = self.open.lock() {
            *open = true;
        }
        self.opened.notify_all();
        self.armed.store(false, Ordering::Release);
    }

    fn wait(&self, cap: Duration) {
        if !self.armed.load(Ordering::Acquire) {
            return;
        }
        if let Ok(open) = self.open.lock() {
            let _ = self.opened.wait_timeout_while(open, cap, |open| !*open);
        }
    }
}

static GATE: Gate = Gate::new();

/// Hold background work back from now until [`open`]. `app::run` calls this
/// before it starts any; nothing else does, so outside a launch, and under
/// test, [`wait`] never waits.
pub fn arm() {
    GATE.arm();
}

/// The first frame is on screen: release everything waiting, and every
/// later [`wait`].
pub fn open() {
    GATE.open();
}

/// Wait until the first frame is up, if a launch is still drawing it. Call
/// at the top of a background job the first frame does not need.
pub fn wait() {
    GATE.wait(CAP);
}

/// `job` on a thread of its own, once the first frame is up.
pub fn spawn_after_first_frame<F>(job: F) -> std::thread::JoinHandle<()>
where
    F: FnOnce() + Send + 'static,
{
    std::thread::spawn(move || {
        wait();
        job();
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Instant;

    /// Unarmed, as everywhere but a launch's first frame, a job never waits.
    #[test]
    fn an_unarmed_gate_never_holds_a_job() {
        let gate = Gate::new();
        let started = Instant::now();
        gate.wait(Duration::from_secs(5));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn an_armed_gate_holds_a_job_until_it_opens() {
        let gate = Arc::new(Gate::new());
        gate.arm();
        let (tx, rx) = std::sync::mpsc::channel();
        let waiter = gate.clone();
        std::thread::spawn(move || {
            waiter.wait(Duration::from_secs(30));
            let _ = tx.send(());
        });
        assert!(
            rx.recv_timeout(Duration::from_millis(100)).is_err(),
            "the job ran before the first frame"
        );
        gate.open();
        assert!(rx.recv_timeout(Duration::from_secs(10)).is_ok());
        // Opened once, it never holds a job again.
        let started = Instant::now();
        gate.wait(Duration::from_secs(5));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    /// A first frame that never comes does not strand the work.
    #[test]
    fn an_armed_gate_lets_a_job_go_after_its_cap() {
        let gate = Gate::new();
        gate.arm();
        let started = Instant::now();
        gate.wait(Duration::from_millis(50));
        assert!(started.elapsed() >= Duration::from_millis(50));
    }
}
