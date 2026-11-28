//! The pane's processes: the shell and the foreground process, their names, command lines
//! and working directories, and changing directory.

use super::*;

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
