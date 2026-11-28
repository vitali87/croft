//! Whether a thread is in a UI turn (#2062, card 12 step 11): a key press,
//! click, paste, command or frame, which must never wait on an outside
//! program. Croft's own helpers ([`super::Bounded`],
//! [`super::output_retrying_busy`] and what is built on them) tell
//! [`about_to_wait`] before they wait. In a test build, a wait during a UI
//! turn fails the test, naming the program: the work belongs on a job. Every
//! build keeps the same count, a thread-local write per turn, so a test runs
//! the code that ships.
//!
//! Work on a job runs on another thread, outside any turn.

use std::cell::Cell;

thread_local! {
    /// How many UI turns this thread is in: a command run from a key press
    /// is a turn inside a turn.
    static TURNS: Cell<u32> = const { Cell::new(0) };
}

/// A UI turn on this thread for as long as it lives.
pub(crate) struct UiTurn(());

impl UiTurn {
    /// Start one: a key press, click, paste, command or frame.
    pub(crate) fn enter() -> Self {
        TURNS.with(|turns| turns.set(turns.get() + 1));
        Self(())
    }
}

impl Drop for UiTurn {
    fn drop(&mut self) {
        TURNS.with(|turns| turns.set(turns.get().saturating_sub(1)));
    }
}

/// Croft's helpers call this just before they wait on `program`. In a test
/// build, a UI turn that waits fails here, naming the program; in a release
/// build it does nothing.
#[track_caller]
pub(crate) fn about_to_wait(program: &std::ffi::OsStr) {
    #[cfg(test)]
    if TURNS.with(Cell::get) > 0 {
        panic!(
            "a UI turn waits on `{}` (from {}): run it on a job",
            program.to_string_lossy(),
            std::panic::Location::caller(),
        );
    }
    #[cfg(not(test))]
    let _ = program;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A wait during a UI turn fails; one outside a turn, or on another
    /// thread, is never flagged.
    #[test]
    fn a_ui_turn_may_not_wait() {
        let program = std::ffi::OsStr::new("gh");
        about_to_wait(program);
        let turn = UiTurn::enter();
        let flagged = std::panic::catch_unwind(|| about_to_wait(program));
        assert!(flagged.is_err(), "a wait during a turn fails");
        std::thread::spawn(move || about_to_wait(program))
            .join()
            .expect("a job's thread is outside the turn");
        drop(turn);
        about_to_wait(program);
    }
}
