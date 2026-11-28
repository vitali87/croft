//! Run and Debug: launch configs and Add Configuration, compounds and preLaunchTask waits,
//! adapters, debug builds, attach pickers, the paused view, watches, the console,
//! stepping, every kind of breakpoint, Debug Test and inline values.

use super::*;

mod active_file;
mod adapters;
mod attach;
mod breakpoint_list;
mod breakpoints;
mod builds;
mod compounds;
mod configs;
mod console;
mod controls;
mod debug_test;
mod events;
mod inline_values;
mod launch;
mod python;
mod setup;
mod view;
mod watches;
pub(super) use adapters::lldb_dap_missing_message;
pub(super) use adapters::lldb_dap_path;
#[cfg(test)]
pub(super) use adapters::lldb_dap_rank;
use breakpoint_list::*;
pub(super) use compounds::finished_settles_pending;
pub(super) use compounds::pending_abort_reason;
use console::*;
#[cfg(test)]
pub(super) use events::debug_end_message;
#[cfg(test)]
pub(super) use events::index_after_removals;
#[cfg(test)]
pub(crate) use launch::no_display;
#[cfg(test)]
pub(super) use launch::resolve_lldb_program;
pub(super) use python::config_project_python;
pub(super) use python::debug_test_python;
pub(super) use python::debuggee_python;
pub(super) use python::debuggee_python_note;
pub(super) use python::launch_request_python;
pub(super) use python::project_python_for;
use python::*;
pub(super) use setup::DebugSetup;
use setup::ParkedLaunch;

/// Run and Debug's own state outside its view: the debug sessions and the
/// configs, compounds and setups they start from, the builds and tasks a
/// launch waits on, the console, and what a stop last reported.
pub(super) struct DebugState {
    /// Why the focused debug session last stopped, until the stop's
    /// location is known and the pause can be announced (#1547).
    pub(super) pause_reason: Option<String>,
    /// A breakpoint croft set itself at the assertion the last run failed
    /// on (#373), as `(file, 1-based line)`. It is NOT one of the user's
    /// breakpoints: it is added to the launch set, rendered hollow-red like
    /// any unverified one, and removed when the session ends — a debugger
    /// that silently left breakpoints behind would be worse than one that
    /// set none.
    pub(super) temp_breakpoint: Option<(PathBuf, usize)>,
    /// Human-readable note about that breakpoint, folded into whichever
    /// launch message the user actually ends up seeing.
    pub(super) temp_note: Option<String>,
    /// Active debug session: a debugpy DAP launch. None when not debugging.
    /// Polled each frame by [`App::poll_dap`], which mirrors the paused location
    /// into `editor.stop_line`.
    /// The debug sessions in flight, one FOCUSED (#310). Was a bare
    /// `Option`, which made "the session" the only thing a call site could
    /// mean; a compound needs several, so every site now says whether it
    /// means the focused one or all of them.
    pub(super) sessions: crate::dap::session::DebugSessions,
    /// launch.json configurations discovered for the picker (#250); refreshed
    /// on every picker open so edits are picked up without a restart.
    pub(super) configs: Vec<crate::dap::configs::DebugConfig>,
    /// The configuration **Debug: Add Configuration…** is building (#250),
    /// between its steps.
    pub(super) config_draft: Option<crate::dap::configs::ConfigDraft>,
    /// Compounds declared beside those configurations. Listed in the picker so
    /// a workspace's compounds are visible; launching one needs the
    /// multi-session model tracked in #310.
    pub(super) compounds: Vec<crate::dap::configs::Compound>,
    /// The launched compound asked for `stopAll: true` (#567): the first
    /// member to end stops the rest. Set on every compound launch, so a
    /// single-config launch (one session, no siblings) never reads a stale
    /// value in a way that matters.
    pub(super) stop_all: bool,
    /// The `postDebugTask`s of the configurations launched into the current
    /// run (#250), run once each when the run ends: the user stops it, or
    /// the last session terminates. A new launch starts a fresh list.
    pub(super) post_tasks: Vec<String>,
    /// The running compound's name, kept past launch so the report rebuilt
    /// when one member ends can still say which compound is live (#567).
    pub(super) compound: Option<String>,
    /// Debuggers being prepared off the UI thread, and the launches that
    /// wait for them (#2803).
    pub(super) setups: Vec<debugger::DebugSetup>,
    /// What prepares a debugger on this machine; a test puts a stand-in here.
    pub(super) tooling: std::sync::Arc<dyn crate::dap::install::DebugTooling>,
    /// F5's C/C++ build running off the UI thread (#1359): the source it
    /// debugs and where the build's result arrives. `poll_debug_build`
    /// launches lldb-dap on the program once it is built.
    pub(super) build: Option<(
        PathBuf,
        std::sync::mpsc::Receiver<Result<crate::cpp_build::Built, crate::cpp_build::Failed>>,
    )>,
    /// Variable `variablesReference`s the user has expanded in the Variables
    /// panel. Persisted across polls so re-fetched variables stay open.
    pub(super) expanded: std::collections::HashSet<i64>,
    /// The latest Copy Value waiting on the adapter's `clipboard` evaluate
    /// (#1546): the expression asked for and the row's shown value, copied
    /// instead if the evaluate fails. A response for any other expression
    /// answers an earlier Copy Value the user has since replaced, and is
    /// dropped.
    pub(super) copy_pending: Option<(String, String)>,
    /// Debug console log: program stdout/stderr (DAP output events) plus REPL
    /// echoes and results. Capped; the tail is shown in the Run & Debug panel.
    /// Each line is escape-free text with the colour spans its SGR sequences
    /// set (#1501), so search and copy see `FAILED`, not `[31mFAILED[0m`.
    pub(super) console: Vec<crate::ansi_text::AnsiLine>,
    /// How many lines have left the front of `console`, to the cap or
    /// to a new run's clear, so a stream's open line keeps its place by
    /// counting from the first line the console ever held (#932).
    pub(super) console_dropped: usize,
    /// True once the current/just-ended debug session stopped at least once
    /// (breakpoint / step / exception). Drives the "exited without hitting a
    /// breakpoint" end message. Reset when a new session starts.
    pub(super) ever_stopped: bool,
}

impl DebugState {
    /// No session, config or console line yet.
    pub(super) fn new() -> Self {
        Self {
            pause_reason: None,
            temp_breakpoint: None,
            temp_note: None,
            sessions: Default::default(),
            configs: Vec::new(),
            config_draft: None,
            compounds: Vec::new(),
            stop_all: false,
            post_tasks: Vec::new(),
            compound: None,
            setups: Vec::new(),
            tooling: std::sync::Arc::new(crate::dap::install::MachineTooling),
            build: None,
            expanded: std::collections::HashSet::new(),
            copy_pending: None,
            console: Vec::new(),
            console_dropped: 0,
            ever_stopped: false,
        }
    }
}

/// Grace before the post-teardown js-debug orphan sweep runs, letting a
/// just-killed server's detached watchdog reparent to init so the sweep can
/// recognise it as an orphan. See [`crate::dap::reaper`].
pub(super) const REAPER_TEARDOWN_GRACE_MS: u64 = 250;

/// What a parked launch does once its task exits 0.
///
/// Two shapes, because a compound's task and a config's task are different
/// steps (#318). A config's own task is the LAST thing before the adapter
/// starts, so the config is already resolved. A compound's task runs BEFORE
/// its member is considered at all, and the member may carry a task of its
/// own - VS Code runs both - so that continuation re-enters the normal launch
/// path rather than skipping to the adapter.
pub(super) enum PendingLaunchStep {
    /// The config's own task just finished: start it. Boxed like the other
    /// variant so neither dominates the enum's size - a resolved config is
    /// several hundred bytes, and this type is held in an `Option` on `App`.
    Resolved(Box<crate::dap::configs::ResolvedConfig>),
    /// A compound's task just finished: launch its member from the top, which
    /// parks again if the member has a `preLaunchTask` too.
    Member(Box<crate::dap::configs::DebugConfig>),
    /// A MULTI-member compound's task just finished: launch every member
    /// (#310). Separate from `Member` because the set has to be started
    /// together - running them through the single-member path would stop each
    /// previous one.
    /// Boxed like its siblings: the doc above says they are boxed so none
    /// dominates the enum's size, and a list plus a name inline would have
    /// dominated it six times over.
    Members(Box<CompoundLaunch>),
}

/// What became of one compound member (#310).
#[derive(Clone, Copy, PartialEq)]
pub(super) enum MemberOutcome {
    /// Started, and now in the running set.
    Launched,
    /// Its `cargo` build is running (#1392); it joins the set when the
    /// build lands.
    Building,
    /// Its debugger is being set up on a job (#2803); it joins the set when
    /// that is done.
    Preparing,
    /// Not started: it declares a `preLaunchTask` of its own, and croft's
    /// parking is one-at-a-time, so the task cannot be run as part of a set.
    /// Launching regardless would debug whatever the last build left behind.
    TaskUnrun,
    /// Tried and failed; the reason was already reported against its name.
    Failed,
}

/// Whether a launch replaces the running set or joins it (#310).
///
/// Starting a configuration has always terminated whatever was running, and
/// that stays true; a compound MEMBER is the one caller that must not, or the
/// second member would kill the first.
#[derive(Clone, Copy)]
pub(super) enum LaunchSlot {
    Replace,
    Add,
}

/// What an [`InputRun`] does once every question is answered (#1424).
pub(super) enum InputThen {
    Launch(Box<crate::dap::configs::DebugConfig>),
    Compound(Box<crate::dap::configs::Compound>),
    Task(Box<crate::tasks::Task>),
}

/// How a launch waits on its `preLaunchTask` (#1541), as VS Code does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PreLaunchWait {
    /// An ordinary task: until it exits, and only exit 0 launches.
    Exit,
    /// A background task with a watch matcher: until its first build
    /// cycle ends, and only a cycle with no errors launches.
    FirstBuild,
    /// A background task with no matcher to say when a build ends: it is
    /// never going to exit, so the launch goes ahead at once.
    Untracked,
}

impl PreLaunchWait {
    pub(super) fn of(task: &crate::tasks::Task) -> Self {
        if !task.is_background {
            return Self::Exit;
        }
        let watches = task
            .problem_matcher
            .as_ref()
            .and_then(crate::problem_matchers::from_tasks_json)
            .is_some_and(|m| m.background.is_some());
        if watches {
            Self::FirstBuild
        } else {
            Self::Untracked
        }
    }
}

/// How long a parked launch waits before concluding the pane will never
/// report a command mark. Only consulted while the pane's foreground is back
/// to a plain shell — a long build keeps a non-shell foreground and waits
/// indefinitely, exactly like the FinishedCommand path.
pub(super) const PRELAUNCH_MARK_GRACE: std::time::Duration = std::time::Duration::from_secs(15);
