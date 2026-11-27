//! Preparing a debugger off the UI thread (#2803): the Python debug venv
//! and the JavaScript debugger (node and the js-debug server) are made ready
//! on a job, and the launches that need them wait, parked, and start when
//! the job answers. A launch whose tooling is ready starts at once, as
//! before.

use super::*;
use crate::dap::install::{Ready, Tooling};

/// A debug launch parked until its debugger is ready, with what it needs to
/// start again exactly as asked.
pub(in crate::app) enum ParkedLaunch {
    /// F5 on a Python file.
    PythonFile(PathBuf),
    /// F5 on a JavaScript or TypeScript file.
    JsFile(PathBuf),
    /// A launch.json configuration, and where it goes.
    Resolved(Box<crate::dap::configs::ResolvedConfig>, LaunchSlot),
    /// Debug Test, for the test `name` in `file`.
    Test { name: String, file: Option<PathBuf> },
}

impl ParkedLaunch {
    /// Whether it joins the running set, as a compound member does, rather
    /// than replacing what runs and what waits.
    fn joins(&self) -> bool {
        matches!(self, ParkedLaunch::Resolved(_, LaunchSlot::Add))
    }
}

/// A debugger being prepared on a job, and the launches waiting for it.
pub(in crate::app) struct DebugSetup {
    tooling: Tooling,
    job: crate::jobs::Job<Result<(), String>>,
    waiting: Vec<ParkedLaunch>,
}

impl App {
    /// The tooling `tooling` names, when it is ready; else it is prepared on
    /// a job, `then` waits for it, and `None` says the launch is parked.
    pub(in crate::app) fn debugger_or_park(
        &mut self,
        tooling: Tooling,
        then: impl FnOnce() -> ParkedLaunch,
    ) -> Option<Ready> {
        if let Some(ready) = self.debug_tooling.ready(tooling) {
            return Some(ready);
        }
        let then = then();
        if !then.joins() {
            self.forget_parked_setup_launches();
        }
        if let Some(setup) = self.debug_setups.iter_mut().find(|s| s.tooling == tooling) {
            setup.waiting.push(then);
        } else {
            let tools = std::sync::Arc::clone(&self.debug_tooling);
            match crate::jobs::Job::spawn("debug-setup", crate::jobs::Start::Now, move || {
                tools.prepare(tooling)
            }) {
                Ok(job) => self.debug_setups.push(DebugSetup {
                    tooling,
                    job,
                    waiting: vec![then],
                }),
                Err(e) => {
                    self.debug_error(format!("Debugger setup failed: {e}"));
                    return None;
                }
            }
        }
        self.status = String::from(tooling.preparing());
        self.run_debug.feedback = Some(String::from(tooling.preparing()));
        self.run_debug.feedback_is_error = false;
        None
    }

    /// The Python debug venv's interpreter, or `None` with the launch
    /// parked until it is built.
    pub(in crate::app) fn debug_python_or_park(
        &mut self,
        then: impl FnOnce() -> ParkedLaunch,
    ) -> Option<PathBuf> {
        match self.debugger_or_park(Tooling::Python, then)? {
            Ready::Python(py) => Some(py),
            Ready::Js { .. } => None,
        }
    }

    /// `node` and the js-debug server, or `None` with the launch parked
    /// until both are ready.
    pub(in crate::app) fn debug_js_or_park(
        &mut self,
        then: impl FnOnce() -> ParkedLaunch,
    ) -> Option<(String, PathBuf)> {
        match self.debugger_or_park(Tooling::Js, then)? {
            Ready::Js { node, server } => Some((node, server)),
            Ready::Python(_) => None,
        }
    }

    /// Drop every launch still waiting for a debugger: a newer launch
    /// supersedes them, as it does one parked behind its task (#567). The
    /// jobs go on; what they prepare is there for the next launch.
    pub(in crate::app) fn forget_parked_setup_launches(&mut self) {
        for setup in &mut self.debug_setups {
            setup.waiting.clear();
        }
    }

    /// How many launches wait for a debugger.
    pub(in crate::app) fn parked_setup_launches(&self) -> usize {
        self.debug_setups.iter().map(|s| s.waiting.len()).sum()
    }

    /// Settle the debugger jobs that have answered: start what waited on
    /// one that made its tooling ready, and say why for one that could not.
    /// Whether anything changed.
    pub(in crate::app) fn poll_debug_setups(&mut self) -> bool {
        let mut settled = Vec::new();
        let mut i = 0;
        while i < self.debug_setups.len() {
            let answer = match self.debug_setups[i].job.poll() {
                crate::jobs::Poll::Pending => {
                    i += 1;
                    continue;
                }
                crate::jobs::Poll::Done(answer) => answer,
                crate::jobs::Poll::Died => Err(String::from(
                    "Debugger setup failed: it stopped before it finished",
                )),
            };
            let setup = self.debug_setups.remove(i);
            settled.push((setup.waiting, answer));
        }
        let changed = !settled.is_empty();
        for (waiting, answer) in settled {
            match answer {
                Ok(()) => {
                    for launch in waiting {
                        self.resume_parked_launch(launch);
                    }
                }
                Err(why) if !waiting.is_empty() => self.debug_error(why),
                Err(_) => {}
            }
        }
        changed
    }

    /// Start `launch` again, now its debugger is ready.
    fn resume_parked_launch(&mut self, launch: ParkedLaunch) {
        match launch {
            ParkedLaunch::PythonFile(path) => self.start_python_debug_session(&path),
            ParkedLaunch::JsFile(path) => self.start_js_debug_session(&path),
            ParkedLaunch::Resolved(rc, slot) => self.launch_resolved_in(*rc, slot),
            ParkedLaunch::Test { name, file } => self.debug_test_in(name, file),
        }
    }
}
