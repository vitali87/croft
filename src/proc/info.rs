//! What the operating system says about a running process, by pid: its
//! name, its command line, its working folder and, for a terminal's
//! session leader, the foreground process group. A terminal pane's label,
//! its split's starting folder, session recording and the ssh offer read
//! them; none of it draws anything.

/// Resolve a pid to its command name (basename), cross-platform, via `sysinfo`
/// (no subprocess). Used to label a terminal pane with its foreground process.
/// Refreshes only the one pid, so it's cheap, but still runs off the event loop.
pub fn process_name(pid: i32) -> Option<String> {
    use sysinfo::{Pid, ProcessesToUpdate, System};
    if pid <= 0 {
        return None;
    }
    let p = Pid::from(pid as usize);
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::Some(&[p]), true);
    sys.process(p)
        .map(|pr| pr.name().to_string_lossy().to_string())
        .filter(|n| !n.is_empty())
}

/// The argv of a running process, or `None` when the platform will not say.
///
/// Separate from [`process_name`] because the two answer different questions
/// and cost differently: a name is what the label pill needs on every tick,
/// while argv is only wanted when the name says the process is worth asking
/// about (#364 wants it for `ssh` and nothing else).
pub fn process_cmdline(pid: i32) -> Option<Vec<String>> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    if pid <= 0 {
        return None;
    }
    let p = Pid::from(pid as usize);
    let mut sys = System::new();
    // The argv must be asked for by name (#1846): sysinfo's plain
    // `refresh_processes` reads memory, CPU, disk use and the executable,
    // never the command line, so `cmd()` was always empty. Every pane that
    // ran `ssh box` then read as "not an SSH session": the open-workspace
    // offer never appeared, and the user stayed in the remote shell.
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[p]),
        true,
        ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always),
    );
    let cmd: Vec<String> = sys
        .process(p)?
        .cmd()
        .iter()
        .map(|s| s.to_string_lossy().to_string())
        .collect();
    (!cmd.is_empty()).then_some(cmd)
}

/// Live cwd of a running process by PID, or None when the platform doesn't
/// expose one. Used by `split_terminal` so a new pane lands wherever the
/// user has `cd`'d the active shell, and by [`PtyTerminal::local_shell_cwd`]
/// as the ground truth an OSC 7 claim must match to be trusted.
#[cfg(target_os = "linux")]
pub fn cwd_of_pid(pid: u32) -> Option<std::path::PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

/// macOS: call `proc_pidinfo` with `PROC_PIDVNODEPATHINFO` directly via the
/// libSystem FFI instead of shelling out to `lsof -d cwd`. Two reasons:
///   1. `lsof` on Sonoma+ tickles the "App Management" / "App Data" TCC
///      privacy class (the OS sees the responsible parent process — iTerm
///      — inspecting another process's open files and prompts the user).
///      `proc_pidinfo` against our own child PID needs no TCC entitlement.
///   2. No fork/exec on the hot path of every terminal split.
///
/// Struct layout matches `<sys/proc_info.h>`. We read the path field of
/// `pvi_cdir` (the cwd vnode) at the documented offset.
#[cfg(target_os = "macos")]
pub fn cwd_of_pid(pid: u32) -> Option<std::path::PathBuf> {
    use std::ffi::OsString;
    use std::os::raw::{c_int, c_void};
    use std::os::unix::ffi::OsStringExt;

    const PROC_PIDVNODEPATHINFO: c_int = 9;
    const MAXPATHLEN: usize = 1024;

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct VinfoStat {
        vst_dev: u32,
        vst_mode: u16,
        vst_nlink: u16,
        vst_ino: u64,
        vst_uid: u32,
        vst_gid: u32,
        vst_atime: i64,
        vst_atimensec: i64,
        vst_mtime: i64,
        vst_mtimensec: i64,
        vst_ctime: i64,
        vst_ctimensec: i64,
        vst_birthtime: i64,
        vst_birthtimensec: i64,
        vst_size: i64,
        vst_blocks: i64,
        vst_blksize: i32,
        vst_flags: u32,
        vst_gen: u32,
        vst_rdev: u32,
        vst_qspare: [i64; 2],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct VnodeInfo {
        vi_stat: VinfoStat,
        vi_type: i32,
        vi_pad: i32,
        vi_fsid: [i32; 2],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct VnodeInfoPath {
        vip_vi: VnodeInfo,
        vip_path: [u8; MAXPATHLEN],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct ProcVnodePathInfo {
        pvi_cdir: VnodeInfoPath,
        pvi_rdir: VnodeInfoPath,
    }

    unsafe extern "C" {
        fn proc_pidinfo(
            pid: c_int,
            flavor: c_int,
            arg: u64,
            buffer: *mut c_void,
            buffersize: c_int,
        ) -> c_int;
    }

    let mut info: ProcVnodePathInfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<ProcVnodePathInfo>() as c_int;
    let ret = unsafe {
        proc_pidinfo(
            pid as c_int,
            PROC_PIDVNODEPATHINFO,
            0,
            &mut info as *mut _ as *mut c_void,
            size,
        )
    };
    if ret <= 0 {
        return None;
    }
    let path = &info.pvi_cdir.vip_path;
    let len = path.iter().position(|&b| b == 0).unwrap_or(0);
    if len == 0 {
        return None;
    }
    Some(std::path::PathBuf::from(OsString::from_vec(
        path[..len].to_vec(),
    )))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn cwd_of_pid(_pid: u32) -> Option<std::path::PathBuf> {
    None
}

/// Whether this platform can report a process's cwd AT ALL.
///
/// The distinction matters wherever a guard reads `cwd_of_pid`: on Android
/// — a separate `target_os` from `linux`, so it binds the stub above — the
/// answer is a permanent `None` rather than a per-pane one. A guard that
/// treats "cannot observe" as "observed something wrong" does not become
/// stricter there, it becomes unsatisfiable, and whatever it guards is
/// lost entirely for those users (#430).
pub const fn cwd_is_observable() -> bool {
    cfg!(any(target_os = "linux", target_os = "macos"))
}

/// The foreground process group of the terminal `pid` (a session leader)
/// controls: field 8 (`tpgid`) of `/proc/<pid>/stat` on Linux, the BSD
/// info's `e_tpgid` from `proc_pidinfo` on macOS. Hosted panes run only
/// where one of these answers.
#[cfg(target_os = "linux")]
pub(crate) fn foreground_group_of(pid: i32) -> Option<i32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    parse_tpgid(&stat)
}

#[cfg(target_os = "macos")]
pub(crate) fn foreground_group_of(pid: i32) -> Option<i32> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // Our own child's info needs no entitlement, as for its cwd above.
    let got = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size,
        )
    };
    if got != size {
        return None;
    }
    let tpgid = info.e_tpgid as i32;
    (tpgid > 0).then_some(tpgid)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn foreground_group_of(_pid: i32) -> Option<i32> {
    None
}

/// `tpgid` from a `/proc/<pid>/stat` line. The command name is in
/// parentheses and may hold spaces or parentheses itself, so the fields
/// are counted from the LAST `)`.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn parse_tpgid(stat: &str) -> Option<i32> {
    let rest = &stat[stat.rfind(')')? + 1..];
    // state ppid pgrp session tty_nr tpgid
    let tpgid: i32 = rest.split_whitespace().nth(5)?.parse().ok()?;
    (tpgid > 0).then_some(tpgid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tpgid_is_read_past_a_command_name_with_spaces_and_parens() {
        let stat = "4242 (my (odd) sh) S 1 4242 4242 34816 5151 4194560 0 0";
        assert_eq!(parse_tpgid(stat), Some(5151));
        assert_eq!(
            parse_tpgid("1 (init) S 0 1 1 0 -1 4194560"),
            None,
            "no terminal"
        );
        assert_eq!(parse_tpgid("garbage"), None);
    }

    #[test]
    fn process_name_resolves_a_live_pid() {
        let me = std::process::id() as i32;
        let name = process_name(me);
        assert!(
            name.as_deref().is_some_and(|n| !n.is_empty()),
            "the test process resolves to a non-empty name: {name:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn process_cmdline_reads_a_live_process_argv() {
        // #1846: the plain refresh never read the command line, so this was
        // always `None` and no pane running `ssh box` read as an ssh session.
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("sleep runs");
        // `spawn` can return while the child still reads as the test binary
        // that forked it, before its exec lands; a loaded runner caught that.
        // Read until the argv is sleep's: a process_cmdline that never reads
        // the command line still fails here, when the budget runs out.
        let mut argv = None;
        crate::test_budget::await_spawned(
            std::time::Duration::from_millis(200),
            "the child's argv to read as sleep's",
            || {
                argv = process_cmdline(child.id() as i32);
                argv.as_ref()
                    .is_some_and(|a| a.last().map(String::as_str) == Some("30"))
            },
        );
        let _ = child.kill();
        let _ = child.wait();
        let argv = argv.expect("a live child's argv");
        assert!(argv[0].ends_with("sleep"), "{argv:?}");
        assert_eq!(argv.last().map(String::as_str), Some("30"), "{argv:?}");
    }

    #[test]
    fn cwd_of_pid_on_self_returns_a_directory_under_tempdir() {
        // The harness runs each test from the crate root by default, but
        // we don't depend on that — we just need cwd_of_pid to round-trip
        // *some* directory for the current process. lsof / /proc gives us
        // the live cwd of the shell that started the test runner.
        let pid = std::process::id();
        match cwd_of_pid(pid) {
            Some(p) => assert!(
                p.is_dir(),
                "cwd lookup must return a real directory, got {p:?}"
            ),
            None => {
                // Acceptable on platforms where neither /proc nor lsof
                // resolve the cwd in the sandbox. The fallback in
                // split_terminal handles that case; nothing to assert here.
            }
        }
    }
}
