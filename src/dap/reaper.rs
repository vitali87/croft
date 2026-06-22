//! Reaper for orphaned vscode-js-debug processes.
//!
//! croft launches the js-debug DAP server (`dapDebugServer.js`) as a child, and
//! that server forks a `watchdog.js` which `setsid`s into its own session. When
//! croft exits *uncleanly* (a crash, a force-quit, or a killed `cargo test`
//! runner) its `Drop` never runs, so `DapTransport::kill` is never called and
//! the whole tree is reparented to launchd (pid 1), lingering forever. The
//! detached watchdog also outlives a plain group-kill of the server. Either way
//! the leftover process is something croft started and lost track of.
//!
//! This module sweeps those orphans. The reap rule is deliberately narrow: a
//! process is killed only when its command names a script under croft's own
//! `~/.croft/js-debug` install **and** its parent is pid 1. A live debug
//! session never matches: its server is parented to the running croft and its
//! watchdog to the live server, so neither shows pid 1 as a parent. That makes
//! the sweep safe to run at any time, even while a session is active.

use std::ffi::{OsStr, OsString};
use std::path::Path;

use sysinfo::{ProcessesToUpdate, System};

use super::install::js_debug_dir;

/// Pid of launchd/init. A process whose parent is this has been reparented,
/// i.e. its real parent (croft, or croft's js-debug server) has died.
const INIT_PID: u32 = 1;

/// The js-debug scripts croft spawns directly or transitively. Either can be
/// the process left behind, so both are reapable.
const JS_DEBUG_SCRIPTS: [&str; 2] = ["dapDebugServer.js", "watchdog.js"];

/// Whether `cmd` (a process argv) names a js-debug script living under croft's
/// own install at `js_debug_root`. Restricting to that root means a js-debug
/// the user runs from some *other* checkout is never touched.
fn cmd_is_croft_js_debug(cmd: &[OsString], js_debug_root: &Path) -> bool {
    cmd.iter().any(|arg| {
        let p = Path::new(arg);
        p.starts_with(js_debug_root)
            && p.file_name()
                .and_then(OsStr::to_str)
                .is_some_and(|name| JS_DEBUG_SCRIPTS.contains(&name))
    })
}

/// Whether a process with parent pid `ppid` running `cmd` is an orphaned
/// croft-owned js-debug process that should be reaped: croft's script, and
/// reparented to init because the launcher is gone.
fn is_orphaned_js_debug(ppid: Option<u32>, cmd: &[OsString], js_debug_root: &Path) -> bool {
    ppid == Some(INIT_PID) && cmd_is_croft_js_debug(cmd, js_debug_root)
}

/// SIGKILL every orphaned croft js-debug process. Returns the number signalled.
/// A no-op (returns 0) when `$HOME` is unset or nothing matches.
pub fn reap_orphaned_js_debug() -> usize {
    let Some(root) = js_debug_dir() else {
        return 0;
    };
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::All, true);
    let mut reaped = 0;
    for (pid, proc_) in sys.processes() {
        let ppid = proc_.parent().map(|p| p.as_u32());
        if is_orphaned_js_debug(ppid, proc_.cmd(), &root) && proc_.kill() {
            reaped += 1;
            super::log::log(&format!("reaped orphaned js-debug pid {}", pid.as_u32()));
        }
    }
    reaped
}

/// Run the sweep on a detached thread so the process scan never blocks the
/// caller. `delay` lets a post-teardown sweep wait for a just-killed server's
/// watchdog to reparent to init before looking for it; pass `Duration::ZERO`
/// for the startup sweep, which has no such race.
pub fn reap_orphaned_js_debug_async(delay: std::time::Duration) {
    let _ = std::thread::Builder::new()
        .name("js-debug-reaper".into())
        .spawn(move || {
            if !delay.is_zero() {
                std::thread::sleep(delay);
            }
            reap_orphaned_js_debug();
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> std::path::PathBuf {
        std::path::PathBuf::from("/home/u/.croft/js-debug")
    }

    fn argv(parts: &[&str]) -> Vec<OsString> {
        parts.iter().map(OsString::from).collect()
    }

    #[test]
    fn matches_the_dap_server_under_the_croft_install() {
        let cmd = argv(&[
            "node",
            "/home/u/.croft/js-debug/js-debug/src/dapDebugServer.js",
            "8123",
        ]);
        assert!(cmd_is_croft_js_debug(&cmd, &root()));
    }

    #[test]
    fn matches_the_detached_watchdog() {
        let cmd = argv(&["node", "/home/u/.croft/js-debug/js-debug/src/watchdog.js"]);
        assert!(cmd_is_croft_js_debug(&cmd, &root()));
    }

    #[test]
    fn ignores_a_same_named_script_outside_crofts_install() {
        let cmd = argv(&["node", "/somewhere/else/js-debug/src/dapDebugServer.js"]);
        assert!(
            !cmd_is_croft_js_debug(&cmd, &root()),
            "only croft's own install is ours to reap"
        );
    }

    #[test]
    fn ignores_an_unrelated_node_process() {
        let cmd = argv(&["node", "/home/u/project/server.js"]);
        assert!(!cmd_is_croft_js_debug(&cmd, &root()));
    }

    #[test]
    fn reaps_only_when_orphaned_to_init() {
        let cmd = argv(&["node", "/home/u/.croft/js-debug/js-debug/src/watchdog.js"]);
        assert!(
            is_orphaned_js_debug(Some(INIT_PID), &cmd, &root()),
            "reparented to init -> orphan"
        );
        assert!(
            !is_orphaned_js_debug(Some(4321), &cmd, &root()),
            "still parented to a live process -> part of a running session, leave it"
        );
        assert!(
            !is_orphaned_js_debug(None, &cmd, &root()),
            "unknown parent -> do not reap"
        );
    }
}
