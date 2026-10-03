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

use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

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
/// Skips croft's own process, anything that is not a CPython interpreter, a
/// `-m pdb -p` attach client (croft's own, from an earlier attach), any
/// process whose interpreter version cannot be resolved, and any interpreter
/// older than 3.14.
pub fn attachable_python_targets() -> Vec<PyTarget> {
    let mut sys = System::new();
    // The plain `refresh_processes` reads no argv, so every row's command
    // line was empty (#868). Threads are left out: on Linux each would be
    // listed as a process of its own, one row per thread.
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .without_tasks()
            .with_exe(UpdateKind::OnlyIfNotSet)
            .with_cmd(UpdateKind::OnlyIfNotSet),
    );
    let self_pid = std::process::id();

    let rows: Vec<(u32, PyVersion, PathBuf, String)> = sys
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
            if is_pdb_attach_client(proc_.cmd()) {
                return None;
            }
            let version = interpreter_version(exe)?;
            if !version.supports_remote_attach() {
                return None;
            }
            Some((pid, version, exe.to_path_buf(), summarize_cmd(proc_.cmd())))
        })
        .collect();
    labelled_targets(rows)
}

/// The picker's targets for `(pid, version, exe, command line)` rows, by
/// pid, each label's PID column padded to the widest PID listed (#868): 4-
/// and 5-digit PIDs in one list otherwise push the version and command line
/// out of line from row to row.
fn labelled_targets(mut rows: Vec<(u32, PyVersion, PathBuf, String)>) -> Vec<PyTarget> {
    rows.sort_by_key(|(pid, ..)| *pid);
    let pid_width = rows
        .iter()
        .map(|(pid, ..)| pid.to_string().len())
        .max()
        .unwrap_or(0);
    rows.into_iter()
        .map(|(pid, version, exe, summary)| PyTarget {
            pid,
            version,
            exe,
            label: target_label(pid, pid_width, version, &summary),
        })
        .collect()
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

/// Whether `cmd` is a `python -m pdb -p <pid>` attach client, the process an
/// earlier attach left running (#868). It is a debugger, not a program to
/// debug, and listing it beside its target offered to attach to itself.
fn is_pdb_attach_client(cmd: &[OsString]) -> bool {
    let args: Vec<_> = cmd.iter().skip(1).map(|a| a.to_string_lossy()).collect();
    let Some(module_at) = pdb_arguments_at(&args) else {
        return false;
    };
    // pdb's own options, up to the script it debugs: a `-p` after that is
    // the script's argument, and `python -m pdb app.py` is a real target.
    let mut rest = args[module_at..].iter();
    while let Some(a) = rest.next() {
        match a.as_ref() {
            "-p" | "--pid" => return true,
            a if a.starts_with("--pid=") => return true,
            "-c" | "--command" => {
                rest.next();
            }
            a if a.starts_with('-') => {}
            _ => return false,
        }
    }
    false
}

/// Where pdb's own arguments start in `args` (an interpreter's argv without
/// argv[0]) when it runs `-m pdb`, else None (#868). Only the interpreter's
/// options are read, the way CPython reads them: they end at the script (the
/// first non-option, or `-` for stdin, or whatever follows `--`), at `-c`,
/// whose code and everything after it are the program's, and at `-m`, whose
/// module decides. Short options combine (`-um pdb`), and `-W`, `-X` and
/// `--check-hash-based-pycs` take a value, from the rest of their group or
/// the next argument, which is never the script.
fn pdb_arguments_at(args: &[std::borrow::Cow<'_, str>]) -> Option<usize> {
    let mut next = 0;
    while let Some(arg) = args.get(next) {
        next += 1;
        if let Some(long) = arg.strip_prefix("--") {
            match long {
                "" => return None,
                "check-hash-based-pycs" => next += 1,
                _ => {}
            }
            continue;
        }
        // Not an option: the script (`-` is stdin's), where options end.
        let group = arg.strip_prefix('-').filter(|g| !g.is_empty())?;
        for (at, flag) in group.char_indices() {
            let value = &group[at + flag.len_utf8()..];
            match flag {
                'c' => return None,
                'm' => {
                    let module = if value.is_empty() {
                        next += 1;
                        args.get(next - 1)?.as_ref()
                    } else {
                        value
                    };
                    return (module == "pdb").then_some(next);
                }
                'W' | 'X' => {
                    if value.is_empty() {
                        next += 1;
                    }
                    break;
                }
                _ => {}
            }
        }
    }
    None
}

/// Build the picker row label: pid (left-aligned in `pid_width` columns),
/// interpreter version, then the command line.
fn target_label(pid: u32, pid_width: usize, version: PyVersion, cmd_summary: &str) -> String {
    format!(
        "PID {pid:<pid_width$}  ·  Python {}.{}.{}  ·  {cmd_summary}",
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
        let label = target_label(4321, 4, v(3, 14, 2), "app.py --serve");
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

    fn argv(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    /// #868: an attach client (croft's `python -m pdb -p <pid>`) is not a
    /// target; a program merely run under pdb, or taking a `-p` of its own,
    /// still is.
    #[test]
    fn a_pdb_attach_client_is_not_offered_as_a_target() {
        for client in [
            &["/usr/bin/python3.14", "-m", "pdb", "-p", "4321"][..],
            &["python3", "-mpdb", "--pid=4321"],
            &["python3", "-m", "pdb", "-c", "continue", "--pid", "9"],
        ] {
            assert!(is_pdb_attach_client(&argv(client)), "{client:?}");
        }
        for target in [
            &["python3", "-m", "pdb", "app.py", "-p", "80"][..],
            &["python3", "serve.py", "-p", "8000"],
            &["python3", "-m", "http.server", "-p"],
            &["python3"],
        ] {
            assert!(!is_pdb_attach_client(&argv(target)), "{target:?}");
        }
    }

    /// #868: only the interpreter's own options are read for `-m pdb`.
    /// What follows the script, or `-c code`, is the program's own argv, so
    /// `python3 tool.py -m pdb -p 80` is a program to debug, not pdb.
    #[test]
    fn a_programs_own_dash_m_pdb_arguments_are_not_an_attach_client() {
        let hidden = [
            &["python3", "tool.py", "-m", "pdb", "-p", "80"][..],
            &["python3", "-c", "code", "-m", "pdb", "-p", "5"],
            &["python3", "-X", "dev", "tool.py", "-m", "pdb", "-p", "1"],
            &["python3", "-", "-m", "pdb", "-p", "1"],
            &["python3", "--", "tool.py", "-m", "pdb", "-p", "1"],
        ]
        .into_iter()
        .filter(|target| is_pdb_attach_client(&argv(target)))
        .collect::<Vec<_>>();
        assert!(hidden.is_empty(), "taken for pdb clients: {hidden:?}");
    }

    /// #868 guard: interpreter options before `-m pdb` are read through,
    /// the ones that take a value included, whose value (`dev`, `ignore`,
    /// `never`) is never taken for the script. A script after such an option
    /// is still a script.
    #[test]
    fn interpreter_options_before_dash_m_pdb_are_read_through() {
        for client in [
            &["python3", "-m", "pdb", "-p", "123"][..],
            &["python3", "-X", "dev", "-m", "pdb", "-p", "123"],
            &[
                "python3", "-Xdev", "-W", "ignore", "-u", "-m", "pdb", "--pid", "1",
            ],
            &[
                "python3",
                "--check-hash-based-pycs",
                "never",
                "-m",
                "pdb",
                "-p",
                "1",
            ],
        ] {
            assert!(is_pdb_attach_client(&argv(client)), "{client:?}");
        }
        for target in [
            &["python3", "-W", "ignore", "tool.py"][..],
            &["python3", "-W", "ignore", "tool.py", "-p", "1"],
        ] {
            assert!(!is_pdb_attach_client(&argv(target)), "{target:?}");
        }
    }

    /// #868: short options combine as CPython reads them, the last of a
    /// group taking its value from the next argument: `-um pdb` runs pdb,
    /// `-uX dev` sets `-X dev`, and `-uc code` ends the options there.
    #[test]
    fn combined_short_options_are_read_as_cpython_reads_them() {
        for client in [
            &["python3", "-IB", "-um", "pdb", "-p", "1"][..],
            &["python3", "-uX", "dev", "-mpdb", "-p", "1"],
        ] {
            assert!(is_pdb_attach_client(&argv(client)), "{client:?}");
        }
        let code_then_args = ["python3", "-uc", "import pdb", "-m", "pdb", "-p", "1"];
        assert!(!is_pdb_attach_client(&argv(&code_then_args)));
    }

    /// #868 guard: only pdb itself is an attach client. A module that merely
    /// starts with `pdb`, or pdb with no `-p`, is left in the list.
    #[test]
    fn a_module_only_named_like_pdb_is_still_a_target() {
        for target in [
            &["python3", "-m", "pdbpp", "-p", "1"][..],
            &["python3", "-mpdbx", "--pid=1"],
            &["python3", "-m", "pdb"],
            &["python3", "-m", "pdb", "-c", "continue"],
        ] {
            assert!(!is_pdb_attach_client(&argv(target)), "{target:?}");
        }
    }

    /// #868 guard: the refresh that reads argv leaves threads out. A 3.14
    /// process running several threads is one row, not one per thread.
    #[test]
    fn a_threaded_target_is_listed_once() {
        let Some(python) = python_314() else {
            eprintln!("skipping: no CPython 3.14+ found");
            return;
        };
        // The marker leads the command line, inside the label's cap.
        let marker = format!("threaded{}", std::process::id());
        let script = format!(
            "{marker} = 1\n\
             import threading, time\n\
             for _ in range(3): threading.Thread(target=time.sleep, args=(30,), daemon=True).start()\n\
             time.sleep(30)\n"
        );
        let target = Reap(
            Command::new(&python)
                .args(["-c", &script])
                .spawn()
                .expect("spawn the target"),
        );
        let pid = target.0.id();
        let listed = || -> Vec<u32> {
            attachable_python_targets()
                .into_iter()
                .filter(|t| t.label.contains(&marker))
                .map(|t| t.pid)
                .collect()
        };
        crate::test_budget::await_spawned(
            std::time::Duration::from_millis(500),
            "the threaded python to be listed",
            || !listed().is_empty(),
        );
        // Give the threads time to start, then look again.
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert_eq!(listed(), vec![pid]);
    }

    /// A CPython 3.14+ to run a target under, or None: attaching needs one,
    /// so the listing test skips without it.
    fn python_314() -> Option<PathBuf> {
        let mut candidates: Vec<PathBuf> = ["python3.14", "python3.15", "python3"]
            .into_iter()
            .map(PathBuf::from)
            .collect();
        if let Some(home) = std::env::var_os("HOME") {
            candidates.push(PathBuf::from(home).join(".croft/debug-venv/bin/python"));
        }
        if let Ok(out) = Command::new("uv").args(["python", "find", "3.14"]).output() {
            let found = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !found.is_empty() {
                candidates.push(PathBuf::from(found));
            }
        }
        candidates
            .into_iter()
            .find(|p| interpreter_version(p).is_some_and(|v| v.supports_remote_attach()))
    }

    /// Kills the spawned target however the test ends.
    struct Reap(std::process::Child);

    impl Drop for Reap {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// #868: every row read `PID n · Python 3.14.7 ·` with nothing after,
    /// because the process refresh never read argv. A real 3.14 process is
    /// listed with its command line.
    #[test]
    fn a_listed_target_shows_its_command_line() {
        let Some(python) = python_314() else {
            eprintln!("skipping: no CPython 3.14+ found");
            return;
        };
        let target = Reap(
            Command::new(&python)
                .args(["-c", "import time; time.sleep(30)"])
                .spawn()
                .expect("spawn the target"),
        );
        let pid = target.0.id();
        let mut label = None;
        crate::test_budget::await_spawned(
            std::time::Duration::from_millis(500),
            "the sleeping python to be listed",
            || {
                label = attachable_python_targets()
                    .into_iter()
                    .find(|t| t.pid == pid)
                    .map(|t| t.label);
                label.is_some()
            },
        );
        let label = label.unwrap();
        assert!(
            label.contains("-c import time"),
            "the row names the command line: {label}"
        );
    }

    /// Where a label's version column starts, in characters.
    fn version_column(label: &str) -> usize {
        label[..label.find("Python").expect("a version")]
            .chars()
            .count()
    }

    /// #868: the issue's screenshot has 4- and 5-digit PIDs in one list,
    /// pushing the columns after them out of line. The PID column is padded
    /// to the widest PID listed, so every row's version and command line
    /// start at one column. Rows come out by PID.
    #[test]
    fn the_pid_column_is_padded_to_the_widest_pid() {
        let exe = PathBuf::from("/usr/bin/python3.14");
        let rows = labelled_targets(vec![
            (
                51234,
                v(3, 14, 2),
                exe.clone(),
                String::from("python3.14 worker.py"),
            ),
            (
                812,
                v(3, 14, 2),
                exe.clone(),
                String::from("python3.14 a.py"),
            ),
            (
                4321,
                v(3, 14, 2),
                exe.clone(),
                String::from("python3.14 -m app"),
            ),
        ]);
        assert_eq!(
            rows.iter().map(|t| t.pid).collect::<Vec<_>>(),
            vec![812, 4321, 51234]
        );
        let columns: Vec<usize> = rows.iter().map(|t| version_column(&t.label)).collect();
        assert!(columns.iter().all(|c| *c == columns[0]), "{rows:#?}");
        assert!(
            rows[0].label.starts_with("PID 812    ·"),
            "{}",
            rows[0].label
        );
        assert!(
            rows[2].label.ends_with("·  python3.14 worker.py"),
            "{}",
            rows[2].label
        );
    }

    /// #868 guard: padding is to the widest PID present, not a fixed width.
    /// A lone target, or PIDs all as wide, read exactly as before, and an
    /// empty list stays empty.
    #[test]
    fn same_width_pids_are_not_padded() {
        let exe = PathBuf::from("/usr/bin/python3.14");
        let one = labelled_targets(vec![(
            4321,
            v(3, 14, 2),
            exe.clone(),
            String::from("app.py --serve"),
        )]);
        assert_eq!(
            one[0].label,
            "PID 4321  ·  Python 3.14.2  ·  app.py --serve"
        );
        let same = labelled_targets(vec![
            (4321, v(3, 14, 2), exe.clone(), String::from("a.py")),
            (1234, v(3, 14, 2), exe.clone(), String::from("b.py")),
        ]);
        assert!(
            same.iter()
                .all(|t| t.label.starts_with("PID ") && !t.label.contains("   ·"))
        );
        assert!(labelled_targets(Vec::new()).is_empty());
    }
}
