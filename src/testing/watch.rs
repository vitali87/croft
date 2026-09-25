//! Test watch mode (#263): rerun a watched scope when a file under the
//! runner's root is saved, and tell the user only when a watched run turns
//! red. The policy lives here, apart from the App, so its timing and its
//! "newly red" rule are testable without a worker.

use std::path::Path;
use std::time::{Duration, Instant};

/// What a watch reruns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchScope {
    All,
    /// A suite, by its name in the tree (`parse` for `parse::a`).
    Suite(String),
    /// One test, by its full name.
    Test(String),
}

/// Saves closer together than this collapse into one rerun: a format-on-save
/// or a save-all fires several in a burst.
pub const DEBOUNCE: Duration = Duration::from_millis(300);

/// How a finished watched run should be reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchNotice {
    /// Not a watched run, or nothing worth saying (green, or still red).
    Quiet,
    /// The watched scope was green (or never ran) and is now red.
    NewlyRed,
}

#[derive(Debug, Default)]
pub struct TestWatch {
    scope: Option<WatchScope>,
    due_at: Option<Instant>,
    /// A rerun this watch started has not finished yet.
    awaiting: bool,
    /// The last watched run ended red.
    red: bool,
}

impl TestWatch {
    pub fn scope(&self) -> Option<&WatchScope> {
        self.scope.as_ref()
    }

    pub fn is_watching(&self, scope: &WatchScope) -> bool {
        self.scope.as_ref() == Some(scope)
    }

    /// Watch `scope`, or stop watching it if it already is. One scope at a
    /// time: watching another replaces it.
    pub fn toggle(&mut self, scope: WatchScope) -> bool {
        if self.is_watching(&scope) {
            self.clear();
            return false;
        }
        *self = Self {
            scope: Some(scope),
            ..Self::default()
        };
        true
    }

    /// Stop watching (the runner's root changed, so the scope's names no
    /// longer mean anything).
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// A file was saved. Inside `root` it schedules a rerun `DEBOUNCE`
    /// from now, pushing back one already scheduled.
    pub fn on_saved(&mut self, path: &Path, root: &Path, now: Instant) {
        if self.scope.is_some() && path.starts_with(root) {
            self.due_at = Some(now + DEBOUNCE);
        }
    }

    /// The scope to rerun now, if one is due. While `busy` (a run of any
    /// kind in flight) it stays due and fires once the runner is free,
    /// rather than being dropped.
    pub fn take_due(&mut self, now: Instant, busy: bool) -> Option<WatchScope> {
        if busy || self.due_at.is_none_or(|due| now < due) {
            return None;
        }
        self.due_at = None;
        self.awaiting = true;
        self.scope.clone()
    }

    /// A run finished with `ok` (the runner's exit success; `None` for a
    /// discovery). Only a run this watch started is judged.
    pub fn finished(&mut self, ok: Option<bool>) -> WatchNotice {
        if !std::mem::take(&mut self.awaiting) {
            return WatchNotice::Quiet;
        }
        let Some(ok) = ok else {
            return WatchNotice::Quiet;
        };
        let was_red = std::mem::replace(&mut self.red, !ok);
        if !ok && !was_red {
            WatchNotice::NewlyRed
        } else {
            WatchNotice::Quiet
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ms: u64) -> Instant {
        // A fixed base keeps the arithmetic readable.
        thread_local!(static BASE: Instant = Instant::now());
        BASE.with(|b| *b + Duration::from_millis(ms))
    }

    #[test]
    fn toggling_the_same_scope_turns_it_off_and_another_replaces_it() {
        let mut w = TestWatch::default();
        assert!(w.toggle(WatchScope::Test("a::b".into())));
        assert!(w.is_watching(&WatchScope::Test("a::b".into())));
        assert!(w.toggle(WatchScope::Suite("a".into())));
        assert_eq!(w.scope(), Some(&WatchScope::Suite("a".into())));
        assert!(!w.toggle(WatchScope::Suite("a".into())));
        assert_eq!(w.scope(), None);
    }

    #[test]
    fn a_save_under_the_root_reruns_once_after_the_debounce() {
        let root = Path::new("/w");
        let mut w = TestWatch::default();
        w.on_saved(Path::new("/w/src/a.rs"), root, at(0));
        assert_eq!(w.take_due(at(100), false), None, "not watching: nothing");
        w.toggle(WatchScope::All);
        w.on_saved(Path::new("/elsewhere/a.rs"), root, at(0));
        assert_eq!(w.take_due(at(1000), false), None, "outside the root");
        w.on_saved(Path::new("/w/src/a.rs"), root, at(0));
        w.on_saved(Path::new("/w/src/b.rs"), root, at(200));
        assert_eq!(w.take_due(at(400), false), None, "the burst pushed it back");
        assert_eq!(w.take_due(at(500), false), Some(WatchScope::All));
        assert_eq!(w.take_due(at(600), false), None, "one rerun per burst");
    }

    #[test]
    fn a_due_rerun_waits_for_a_busy_runner_instead_of_being_dropped() {
        let mut w = TestWatch::default();
        w.toggle(WatchScope::Test("t".into()));
        w.on_saved(Path::new("/w/a.rs"), Path::new("/w"), at(0));
        assert_eq!(w.take_due(at(500), true), None);
        assert_eq!(
            w.take_due(at(900), false),
            Some(WatchScope::Test("t".into()))
        );
    }

    #[test]
    fn only_a_watched_run_turning_red_is_reported() {
        let mut w = TestWatch::default();
        w.toggle(WatchScope::All);
        assert_eq!(
            w.finished(Some(false)),
            WatchNotice::Quiet,
            "a run the user started"
        );
        let rerun = |w: &mut TestWatch, ms: u64| {
            w.on_saved(Path::new("/w/a.rs"), Path::new("/w"), at(ms));
            w.take_due(at(ms + 400), false).unwrap();
        };
        rerun(&mut w, 0);
        assert_eq!(w.finished(Some(false)), WatchNotice::NewlyRed);
        rerun(&mut w, 1000);
        assert_eq!(
            w.finished(Some(false)),
            WatchNotice::Quiet,
            "still red: no repeat"
        );
        rerun(&mut w, 2000);
        assert_eq!(
            w.finished(Some(true)),
            WatchNotice::Quiet,
            "green is silent"
        );
        rerun(&mut w, 3000);
        assert_eq!(
            w.finished(Some(false)),
            WatchNotice::NewlyRed,
            "red again after green"
        );
        rerun(&mut w, 4000);
        assert_eq!(
            w.finished(None),
            WatchNotice::Quiet,
            "a discovery is not a verdict"
        );
    }
}
