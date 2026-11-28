//! Whether a thread is in a UI turn (#2062, card 12 step 11): a key press,
//! click, paste, command or frame, which must never wait on an outside
//! program. Croft's own helpers ([`super::Bounded`],
//! [`super::output_retrying_busy`] and what is built on them) tell
//! [`about_to_wait`] before they wait. In a test build, a wait during a UI
//! turn fails the test, naming the program, unless the place that asked for
//! it says why it may with [`ui_wait_allowed`]. Every build keeps the same
//! count, a thread-local write per turn, so a test runs the code that ships.
//!
//! Work on a job runs on another thread, outside any turn.

use std::cell::Cell;

thread_local! {
    /// How many UI turns this thread is in: a command run from a key press
    /// is a turn inside a turn.
    static TURNS: Cell<u32> = const { Cell::new(0) };
    /// How many [`ui_wait_allowed`] calls this thread is in.
    static ALLOWED: Cell<u32> = const { Cell::new(0) };
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

/// Run `wait`, which waits on a program during a UI turn, for the reason
/// `why`: a short, bounded answer the turn cannot go on without. Each such
/// place is counted, and the count may only fall
/// (`proc::ui_turn::tests::ui_waits_only_fall`).
pub(crate) fn ui_wait_allowed<T>(why: &'static str, wait: impl FnOnce() -> T) -> T {
    let _ = why;
    struct Allowed;
    impl Drop for Allowed {
        fn drop(&mut self) {
            ALLOWED.with(|allowed| allowed.set(allowed.get().saturating_sub(1)));
        }
    }
    ALLOWED.with(|allowed| allowed.set(allowed.get() + 1));
    let _allowed = Allowed;
    wait()
}

/// Croft's helpers call this just before they wait on `program`. In a test
/// build, a UI turn that waits without saying why fails here, naming the
/// program; in a release build it does nothing.
#[track_caller]
pub(crate) fn about_to_wait(program: &std::ffi::OsStr) {
    #[cfg(test)]
    if TURNS.with(Cell::get) > 0 && ALLOWED.with(Cell::get) == 0 {
        panic!(
            "a UI turn waits on `{}` (from {}): run it on a job, or say why it may wait with \
             proc::ui_wait_allowed",
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

    /// A wait during a UI turn fails unless its place says why it may; one
    /// outside a turn, or on another thread, is never flagged.
    #[test]
    fn a_ui_turn_may_not_wait_unless_its_place_says_why() {
        let program = std::ffi::OsStr::new("gh");
        about_to_wait(program);
        let turn = UiTurn::enter();
        let flagged = std::panic::catch_unwind(|| about_to_wait(program));
        assert!(flagged.is_err(), "a wait during a turn fails");
        ui_wait_allowed("a bounded answer", || about_to_wait(program));
        std::thread::spawn(move || about_to_wait(program))
            .join()
            .expect("a job's thread is outside the turn");
        drop(turn);
        about_to_wait(program);
    }

    /// The places allowed to wait during a UI turn may only fall in number:
    /// a new one is a wait that should have gone on a job.
    #[test]
    fn ui_waits_only_fall() {
        // The PDF viewer's page count and link reads (#2824).
        const MOST: usize = 2;
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut sites = Vec::new();
        let mut dirs = vec![src];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    dirs.push(path);
                } else if path.extension().is_some_and(|e| e == "rs")
                    && !path.ends_with("proc/ui_turn.rs")
                {
                    let text = std::fs::read_to_string(&path).unwrap();
                    for (n, line) in text.lines().enumerate() {
                        if line.contains("ui_wait_allowed(") {
                            sites.push(format!("{}:{}", path.display(), n + 1));
                        }
                    }
                }
            }
        }
        assert!(
            sites.len() <= MOST,
            "{} places may wait during a UI turn, more than the {MOST} allowed; run the new \
             one on a job:\n{}",
            sites.len(),
            sites.join("\n")
        );
        assert_eq!(
            sites.len(),
            MOST,
            "fewer places wait during a UI turn now: lower MOST to {} so the count holds",
            sites.len()
        );
    }
}
