//! The pane's processes: the shell and the foreground process, their names, command lines
//! and working directories, and changing directory.

use super::*;

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

impl PtyTerminal {
    /// The shell's live cwd per its latest OSC 7 report, when shell
    /// integration is active. Fresher than sampling `cwd_of_pid`.
    pub fn shell_cwd(&self) -> Option<std::path::PathBuf> {
        self.osc7_cwd.lock().unwrap().clone()
    }

    /// The kernel-reported cwd of the pane's shell process. Unlike
    /// [`Self::shell_cwd`] it needs no shell integration, so it answers for
    /// any local pane; `None` when the pid or the kernel query is
    /// unavailable (android, a dead shell, a remote pane's ssh process).
    pub fn kernel_shell_cwd(&self) -> Option<std::path::PathBuf> {
        cwd_of_pid(u32::try_from(self.shell_pid?).ok()?)
    }

    /// The pane's foreground process group leader pid (what owns the tty now):
    /// the shell at a prompt, or a running command. `None` if unavailable.
    pub fn foreground_pid(&self) -> Option<i32> {
        self.backend.process_group_leader()
    }

    /// The shell's own pid, stable for this pane's lifetime (the key the app
    /// uses to match async label lookups back to the right pane).
    pub fn shell_pid(&self) -> Option<i32> {
        self.shell_pid
    }

    /// [`Self::shell_cwd`], trusted only when the report provably applies
    /// to the LOCAL shell: the reporting host must be this machine (empty,
    /// "localhost", or the local hostname — RFC 8089 semantics via the
    /// command history's canonicalizer) AND the claimed path must name the
    /// directory the kernel says the shell is in ([`cwd_of_pid`]). PTY
    /// bytes carry no author — any foreground job can print a
    /// local-looking claim and die before the bytes are even parsed — so
    /// no tty-ownership sample at any moment establishes provenance.
    /// Matching the kernel's ground truth does: a forged claim equal to
    /// the truth grants nothing, and one that differs is rejected. The
    /// cost is that platforms without [`cwd_of_pid`] never trust a claim.
    pub fn local_shell_cwd(&self) -> Option<std::path::PathBuf> {
        let host = self.shell_host().unwrap_or_default();
        if !crate::command_history::is_local_host(&host) {
            return None;
        }
        let claim = self.shell_cwd()?;
        let real = cwd_of_pid(self.pid()?)?;
        // Symlink-tolerant equality (macOS reports /private/tmp for a
        // shell sitting in /tmp): equal canonical paths name the same
        // directory, so a claim passing this check cannot mislead.
        (claim.canonicalize().ok()? == real.canonicalize().ok()?).then_some(claim)
    }

    /// Process ID of the shell running inside this terminal, when the
    /// platform exposes one. Used to look up the live cwd so a new split
    /// inherits the directory the user has `cd`'d into. None once the shell
    /// has exited: a reaped pid may already name another process.
    pub fn pid(&self) -> Option<u32> {
        self.backend.pid().filter(|_| self.exit.is_none())
    }

    /// Seed a `cd <path>` into the embedded shell so the terminal
    /// follows an Explorer-side workspace-root change. Sent verbatim to
    /// the PTY input - the shell parses it like a typed command. See
    /// `format_cd_command` for the line-clearing prefix and the path
    /// quoting strategy.
    pub fn change_cwd(&mut self, path: &std::path::Path) {
        self.write_input(&format_cd_command(path));
    }

    /// True when the embedded shell is sitting at its prompt rather than
    /// running a foreground application (an editor, a pager, or an agent
    /// like Claude Code). Compares the PTY's foreground process group
    /// against the shell's own pid: a shell at its prompt is its own
    /// process-group leader, so `tcgetpgrp(master) == shell_pid`. Once
    /// the shell forks a command, that command owns a new foreground
    /// group and the two diverge. The only case that must suppress a
    /// `cd` seed is a foreground group that exists AND is not the shell,
    /// because that is an app reading the tty in raw mode; a missing
    /// foreground group (the brief window after spawn, before the shell
    /// claims the tty) means nothing has grabbed input, so a `cd` is
    /// still safe. `tcgetpgrp` is identical on macOS and Linux. A shell
    /// that has exited (#912) owns nothing.
    pub fn foreground_is_shell(&self) -> bool {
        if self.exit.is_some() {
            return false;
        }
        match (self.backend.process_group_leader(), self.shell_pid) {
            (Some(fg), Some(pid)) => fg == pid,
            _ if self.input_seen => {
                // A pane that has received input CAN be running a launched
                // app, so an UNRESOLVABLE sample (failed tcgetpgrp, or a
                // spawn that never reported a shell pid) must not read as
                // "shell owns it" — under scheduler load the sample
                // transiently fails while a foreground command runs, and
                // the old blanket `true` let a cd seed reach that app
                // (#155). Retry once (the failure window is brief), then
                // answer FALSE: every consumer fails safe on false (seed
                // suppressed, task pane not reused, no prompt arrows).
                std::thread::sleep(std::time::Duration::from_millis(5));
                match (self.backend.process_group_leader(), self.shell_pid) {
                    (Some(fg), Some(pid)) => fg == pid,
                    _ => false,
                }
            }
            // Startup window: no input has ever been written, so nothing
            // user-facing can own the tty yet — a missing group is the
            // shell's own rc still claiming it, and a seed queues as
            // type-ahead (#94).
            _ => true,
        }
    }

    /// True when seeding a `cd` into the PTY cannot reach a user-facing
    /// foreground application. Either the shell owns the foreground group
    /// (`foreground_is_shell`), or the pane is still in its startup
    /// window: nothing has ever been written toward the child's stdin and
    /// shell integration has recorded no prompt mark, so a foreground
    /// group that differs from the shell can only be the shell's own rc
    /// startup. A seed written then is ordinary type-ahead — the bytes
    /// wait in the tty input queue for the first prompt — because a pane
    /// that has never received input cannot be running a launched app.
    /// Without the carve-out, a Make Root landing inside that window
    /// silently skipped the seed and left the shell behind the Explorer
    /// (#94; under full-suite load the window stretches past the call).
    pub fn cwd_seed_is_safe(&self) -> bool {
        self.foreground_is_shell() || (!self.input_seen && self.marks.lock().unwrap().is_empty())
    }

    /// Test-only: the RESOLVED foreground sample — `Some(fg == shell)`
    /// when both sides are known, `None` otherwise. `foreground_is_shell`
    /// folds policy fallbacks (the startup carve-out, the fail-closed
    /// retry) into its answer, so a test precondition polling it can be
    /// satisfied by a fallback and race the state it believes it proved
    /// (#186); gate preconditions on `Some(..)` instead.
    #[cfg(test)]
    pub fn foreground_resolved_for_test(&self) -> Option<bool> {
        match (self.backend.process_group_leader(), self.shell_pid) {
            (Some(fg), Some(pid)) => Some(fg == pid),
            _ => None,
        }
    }
}

/// The foreground process group of the terminal `pid` (a session leader)
/// controls: field 8 (`tpgid`) of `/proc/<pid>/stat` on Linux, the BSD
/// info's `e_tpgid` from `proc_pidinfo` on macOS. Hosted panes run only
/// where one of these answers.
#[cfg(target_os = "linux")]
pub(super) fn foreground_group_of(pid: i32) -> Option<i32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    parse_tpgid(&stat)
}

#[cfg(target_os = "macos")]
pub(super) fn foreground_group_of(pid: i32) -> Option<i32> {
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
pub(super) fn foreground_group_of(_pid: i32) -> Option<i32> {
    None
}

/// `tpgid` from a `/proc/<pid>/stat` line. The command name is in
/// parentheses and may hold spaces or parentheses itself, so the fields
/// are counted from the LAST `)`.
#[cfg(any(target_os = "linux", test))]
pub(super) fn parse_tpgid(stat: &str) -> Option<i32> {
    let rest = &stat[stat.rfind(')')? + 1..];
    // state ppid pgrp session tty_nr tpgid
    let tpgid: i32 = rest.split_whitespace().nth(5)?.parse().ok()?;
    (tpgid > 0).then_some(tpgid)
}

/// What croft types ahead of a command it puts into a pane's shell, so a
/// half-typed command at the prompt cannot concatenate with it: Ctrl-E to
/// the end of the line, a letter, then Ctrl-U to kill the line backward.
/// The letter keeps the line from being empty when Ctrl-U runs: bash's line
/// editor rings the bell for Ctrl-U on an empty line, and croft reported
/// that bell over the "Running …" status of the task it had just typed. A
/// letter, not a space: a space expands a fish abbreviation, and one set up
/// with `--set-cursor` leaves the cursor before its own suffix, which
/// Ctrl-U then keeps and croft's command runs into. No shell expands
/// anything on a letter. Before the shell's line editor is up, the
/// terminal's own line discipline takes Ctrl-U as its kill character, to
/// the same effect.
pub const CLEAR_TYPED_LINE: &str = "\x05x\x15";

/// Build the byte sequence that retargets a POSIX shell's working
/// directory to `path`. Prefixed with [`CLEAR_TYPED_LINE`] so a
/// half-typed command at the prompt does not concatenate with our `cd`.
/// Path is single-quoted with embedded
/// quotes escaped via the standard `'\''` sleight-of-hand so paths
/// containing spaces or apostrophes round-trip safely.
pub fn format_cd_command(path: &std::path::Path) -> Vec<u8> {
    let s = path.to_string_lossy();
    let mut out = Vec::with_capacity(s.len() + 8);
    out.extend_from_slice(CLEAR_TYPED_LINE.as_bytes());
    out.extend_from_slice(b"cd '");
    for c in s.chars() {
        if c == '\'' {
            out.extend_from_slice(b"'\\''");
        } else {
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
    }
    out.extend_from_slice(b"'\n");
    out
}
