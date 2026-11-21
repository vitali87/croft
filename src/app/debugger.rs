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
mod launch;
mod python;
mod view;
mod watches;
pub(super) use adapters::lldb_dap_missing_message;
pub(super) use adapters::lldb_dap_path;
#[cfg(test)]
pub(super) use adapters::lldb_dap_rank;
use adapters::*;
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

impl App {
    /// Rebuild the inline-value trailers (#135) from the current stop's
    /// already-fetched VARIABLES data, on every editor showing the stopped
    /// file (inactive split groups render too).
    pub(super) fn refresh_inline_values(&mut self) {
        use crate::dap::session::SessionPhase;
        if !self.inline_values_enabled {
            return;
        }
        let Some(session) = self.debug_sessions.focused() else {
            return;
        };
        if session.phase != SessionPhase::Stopped {
            return;
        }
        let Some((path, line)) = session.current_location.clone() else {
            return;
        };
        let locals = crate::dap::session::inline_locals(&session.scopes, &session.variables);
        for ed in self.editor.editors.iter_mut() {
            if ed.path.as_deref() == Some(path.as_path()) {
                ed.set_inline_values_from_locals(line, &locals);
            }
        }
        for group in self.editor_layout.inactive_groups_mut() {
            for ed in group.editors.iter_mut() {
                if ed.path.as_deref() == Some(path.as_path()) {
                    ed.set_inline_values_from_locals(line, &locals);
                }
            }
        }
    }

    /// Drop every inline-value trailer, in every editor of every group —
    /// called wherever the stop arrow clears (resume, step, terminate).
    pub(super) fn clear_inline_values(&mut self) {
        for ed in self.editor.editors.iter_mut() {
            ed.inline_values.clear();
        }
        for group in self.editor_layout.inactive_groups_mut() {
            for ed in group.editors.iter_mut() {
                ed.inline_values.clear();
            }
        }
    }

    pub(super) fn toggle_inline_values(&mut self) {
        self.inline_values_enabled = !self.inline_values_enabled;
        if self.inline_values_enabled {
            self.refresh_inline_values();
        } else {
            self.clear_inline_values();
        }
        self.status = format!(
            "Debug Inline Values: {}",
            if self.inline_values_enabled {
                "on"
            } else {
                "off"
            }
        );
        if !cfg!(test) {
            self.report_unsaved_setting(crate::prefs::save_inline_values(
                self.inline_values_enabled,
            ));
        }
    }
}
