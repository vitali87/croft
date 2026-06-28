//! Discovery of attachable Python processes.
//!
//! The enumeration itself is impure (it snapshots the process table via
//! `sysinfo` and runs each candidate interpreter with `--version`), but it is
//! built entirely from the pure, tested helpers in
//! [`super::remote_attach`]. Only processes running CPython >= 3.14 (those with
//! `sys.remote_exec`) are returned, so the picker never offers a target that
//! would fail to attach.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use sysinfo::{ProcessesToUpdate, System};

use super::remote_attach::{PyVersion, is_python_process};

/// `--version` flag, understood by every CPython.
const VERSION_FLAG: &str = "--version";
/// Cap on the command-line summary shown in the picker so one long argv can't
/// blow out the row width.
const CMD_SUMMARY_MAX: usize = 80;

/// A running CPython process that can be attached to (>= 3.14).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PyTarget {
    pub pid: u32,
    pub version: PyVersion,
    /// The interpreter binary, reused as the pdb *client* (`-m pdb -p`).
    pub exe: PathBuf,
    /// One-line, picker-ready label.
    pub label: String,
}

/// Enumerate attachable CPython processes, newest-looking first by pid.
///
/// Skips croft's own process, anything that is not a CPython interpreter, any
/// process whose interpreter version cannot be resolved, and any interpreter
/// older than 3.14.
pub fn attachable_python_targets() -> Vec<PyTarget> {
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::All, true);
    let self_pid = std::process::id();

    let mut targets: Vec<PyTarget> = sys
        .processes()
        .iter()
        .filter_map(|(pid, proc_)| {
            let pid = pid.as_u32();
            if pid == self_pid {
                return None;
            }
            let name = proc_.name().to_string_lossy();
            let exe = proc_.exe();
            if !is_python_process(&name, exe) {
                return None;
            }
            let exe = exe?;
            let version = interpreter_version(exe)?;
            if !version.supports_remote_attach() {
                return None;
            }
            let summary = summarize_cmd(proc_.cmd());
            Some(PyTarget {
                pid,
                version,
                exe: exe.to_path_buf(),
                label: target_label(pid, version, &summary),
            })
        })
        .collect();

    targets.sort_by_key(|t| t.pid);
    targets
}

/// Run `<exe> --version` and parse the reported CPython version. Older
/// interpreters print to stderr, newer ones to stdout, so both are consulted.
/// Returns `None` if the process cannot be run or the output is unparseable.
fn interpreter_version(exe: &Path) -> Option<PyVersion> {
    let out = Command::new(exe).arg(VERSION_FLAG).output().ok()?;
    PyVersion::parse(&String::from_utf8_lossy(&out.stdout))
        .or_else(|| PyVersion::parse(&String::from_utf8_lossy(&out.stderr)))
}

/// Collapse a process argv into a single bounded line for display.
fn summarize_cmd(cmd: &[OsString]) -> String {
    let joined = cmd
        .iter()
        .map(|s| s.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");
    if joined.chars().count() > CMD_SUMMARY_MAX {
        // Cut to the cap with no trailing ellipsis marker.
        joined.chars().take(CMD_SUMMARY_MAX).collect()
    } else {
        joined
    }
}

/// Build the picker row label: pid, interpreter version, then the command line.
fn target_label(pid: u32, version: PyVersion, cmd_summary: &str) -> String {
    format!(
        "PID {pid}  ·  Python {}.{}.{}  ·  {cmd_summary}",
        version.major, version.minor, version.patch
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(major: u32, minor: u32, patch: u32) -> PyVersion {
        PyVersion {
            major,
            minor,
            patch,
        }
    }

    #[test]
    fn label_includes_pid_version_and_command() {
        let label = target_label(4321, v(3, 14, 2), "app.py --serve");
        assert!(label.contains("PID 4321"));
        assert!(label.contains("3.14.2"));
        assert!(label.contains("app.py --serve"));
    }

    #[test]
    fn summarize_joins_argv_with_spaces() {
        let cmd = vec![
            OsString::from("python3.14"),
            OsString::from("app.py"),
            OsString::from("--port=8000"),
        ];
        assert_eq!(summarize_cmd(&cmd), "python3.14 app.py --port=8000");
    }

    #[test]
    fn summarize_truncates_overlong_argv_plainly() {
        let long = "x".repeat(200);
        let cmd = vec![OsString::from(long)];
        let out = summarize_cmd(&cmd);
        // Cut to the cap with no trailing ellipsis marker.
        assert!(out.chars().count() <= CMD_SUMMARY_MAX);
        assert!(!out.contains('…'));
        assert!(out.chars().all(|c| c == 'x'));
    }

    #[test]
    fn summarize_empty_argv_is_empty() {
        assert_eq!(summarize_cmd(&[]), "");
    }
}
