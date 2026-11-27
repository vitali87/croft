//! A program croft waits for, to a time limit (#2062, card 12 step 4).
//!
//! [`Bounded`] states everything about how croft runs a program it waits
//! for: what it keeps of each output stream, whether the program runs apart
//! from croft's terminal, how long it may take, and whether a moment of
//! "Text file busy" is waited out. Every field is written at each call, so no
//! call inherits the standard library's defaults, which differ between
//! `output()` and `spawn()`. Its stdin is always empty: a program croft waits
//! on never reads croft's own input.
//!
//! Both outputs are read in the loop that waits for the program, with no
//! thread and without ever waiting on a pipe (#2756). croft wakes as soon as
//! either has output, so a program is read as fast as it writes and never
//! stalls on a full pipe, and a helper it leaves running with a pipe still
//! open cannot hold croft past the program's own exit. At the limit the
//! program is killed, together with everything it started when it runs
//! apart.
//!
//! croft builds only for unix (several modules import `std::os::unix`
//! unconditionally), so this reads its pipes through unix descriptors.

use std::fs::File;
use std::io::Read;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// The longest croft waits between checks of the program and the clock.
const LONGEST_WAIT: Duration = Duration::from_millis(50);

/// The most one read of a stream takes before croft checks the program and
/// the clock again, so a program that never stops writing is still killed at
/// its limit: past it the program waits on the pipe, not croft on the
/// program. It is also the largest a pipe grows by default on Linux
/// (`/proc/sys/fs/pipe-max-size`), so the one read after the program exits
/// takes all it left there.
const DRAIN_LIMIT: usize = 1024 * 1024;

/// What croft keeps of one of a program's output streams.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Keep {
    /// None of it: the stream goes to `/dev/null`.
    Nothing,
    /// All of it, up to `cap` bytes. A program that writes more is killed and
    /// the run refused with [`RunError::TooLong`]: croft never acts on part of
    /// an answer as if it were the whole.
    All { cap: usize },
    /// Its last `n` bytes, as for an error message, whose end says why.
    Tail(usize),
}

impl Keep {
    fn stdio(self) -> Stdio {
        match self {
            Keep::Nothing => Stdio::null(),
            Keep::All { .. } | Keep::Tail(_) => Stdio::piped(),
        }
    }
}

/// How croft runs a program it waits for.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Bounded {
    pub stdout: Keep,
    pub stderr: Keep,
    /// Run in a session of its own, apart from croft's terminal: it can
    /// neither read from nor write to it, and at the limit everything it
    /// started is killed with it.
    pub detached: bool,
    /// How long it may run, counted from before the first start attempt.
    pub limit: Duration,
    /// Wait out a moment of "Text file busy" while starting, within `limit`
    /// ([`super::retry_while_busy`]).
    pub retry_busy: bool,
}

/// One of a program's output streams, to say which a refusal is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stream {
    Stdout,
    Stderr,
}

/// A program that ran to its exit.
#[derive(Debug)]
pub(crate) struct Ran {
    pub status: ExitStatus,
    pub stdout: Kept,
    pub stderr: Kept,
}

/// Why a program did not run to its exit.
#[derive(Debug)]
pub(crate) enum RunError {
    /// It could not be started.
    Spawn(std::io::Error),
    /// It was still running at the limit, and was killed.
    TimedOut,
    /// It wrote more than `cap` bytes to `stream`, kept whole, and was killed.
    TooLong { stream: Stream, cap: usize },
    /// Asking whether it had exited failed, and it was killed.
    Wait(std::io::Error),
    /// One of its outputs could not be switched to reads that never wait,
    /// and it was killed: croft reads a program only that way, so a helper
    /// holding a stream open can never hold croft past the limit.
    Pipe(std::io::Error),
}

/// What croft kept of one stream.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Kept {
    bytes: Vec<u8>,
    cut: bool,
}

impl Kept {
    /// Whether the front was dropped to keep a [`Keep::Tail`].
    pub(crate) fn cut(&self) -> bool {
        self.cut
    }

    /// The kept bytes as text, an invalid byte shown as U+FFFD. When the
    /// front was dropped, it starts at the first whole character.
    pub(crate) fn text(&self) -> String {
        let start = if self.cut {
            self.bytes
                .iter()
                .position(|b| b & 0xC0 != 0x80)
                .unwrap_or(self.bytes.len())
        } else {
            0
        };
        String::from_utf8_lossy(&self.bytes[start..]).into_owned()
    }

    /// Add `new` as `keep` says, or refuse it when it would take a stream
    /// kept whole past its cap.
    fn add(&mut self, new: &[u8], keep: Keep, stream: Stream) -> Result<(), RunError> {
        match keep {
            Keep::Nothing => {}
            Keep::All { cap } => {
                if self.bytes.len() + new.len() > cap {
                    return Err(RunError::TooLong { stream, cap });
                }
                self.bytes.extend_from_slice(new);
            }
            Keep::Tail(n) => {
                self.bytes.extend_from_slice(new);
                if let Some(over) = self.bytes.len().checked_sub(n).filter(|&o| o > 0) {
                    self.bytes.drain(..over);
                    self.cut = true;
                }
            }
        }
        Ok(())
    }
}

impl Bounded {
    /// Run `cmd` to its exit or the limit, whichever is first. `cmd`'s own
    /// stdin, stdout and stderr settings are replaced by this one's.
    ///
    /// What it kept is what the program wrote before it exited: a helper it
    /// leaves running may hold a stream open and write on, and croft does
    /// not wait for that.
    pub(crate) fn run(self, mut cmd: Command) -> Result<Ran, RunError> {
        let deadline = Instant::now() + self.limit;
        cmd.stdin(Stdio::null())
            .stdout(self.stdout.stdio())
            .stderr(self.stderr.stdio());
        if self.detached {
            // SAFETY: `setsid` is async-signal-safe, the only kind of call
            // allowed between fork and exec, and touches no memory of
            // croft's. It fails only for a process group leader, which a
            // freshly forked child is not.
            unsafe {
                cmd.pre_exec(|| {
                    libc::setsid();
                    Ok(())
                });
            }
        }
        let started = if self.retry_busy {
            super::retry_while_busy(
                || cmd.spawn(),
                |r| r.as_ref().is_err_and(super::text_file_busy) && Instant::now() < deadline,
            )
        } else {
            cmd.spawn()
        };
        let mut child = started.map_err(RunError::Spawn)?;
        let stdout = Pipe::new(
            child.stdout.take().map(OwnedFd::from),
            self.stdout,
            Stream::Stdout,
        );
        let stderr = Pipe::new(
            child.stderr.take().map(OwnedFd::from),
            self.stderr,
            Stream::Stderr,
        );
        let mut pipes = match (stdout, stderr) {
            (Ok(stdout), Ok(stderr)) => [stdout, stderr],
            (Err(e), _) | (_, Err(e)) => {
                stop(child, self.detached);
                return Err(RunError::Pipe(e));
            }
        };
        let mut quiet = 0u32;
        loop {
            let mut heard = false;
            for pipe in &mut pipes {
                match pipe.drain() {
                    Ok(took) => heard |= took,
                    Err(refused) => {
                        stop(child, self.detached);
                        return Err(refused);
                    }
                }
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    // All the program itself wrote is in its pipes now it has
                    // exited, so what is there is the whole of it.
                    for pipe in &mut pipes {
                        pipe.finish()?;
                    }
                    let [stdout, stderr] = pipes.map(|pipe| pipe.kept);
                    return Ok(Ran {
                        status,
                        stdout,
                        stderr,
                    });
                }
                Ok(None) => {
                    let now = Instant::now();
                    if now >= deadline {
                        stop(child, self.detached);
                        return Err(RunError::TimedOut);
                    }
                    quiet = if heard { 0 } else { quiet.saturating_add(1) };
                    wait_for_output(&pipes, pause(quiet).min(deadline - now));
                }
                Err(e) => {
                    stop(child, self.detached);
                    return Err(RunError::Wait(e));
                }
            }
        }
    }
}

/// How long to wait after `quiet` checks in a row heard nothing: 1 ms at
/// first, doubling to [`LONGEST_WAIT`]. A program that finishes at once is
/// seen to have finished within a millisecond or two, rather than a whole
/// long wait later, and a quiet long one costs croft a wake every 50 ms.
fn pause(quiet: u32) -> Duration {
    Duration::from_millis(1u64 << quiet.min(6)).min(LONGEST_WAIT)
}

/// One output stream of a running program, read without ever waiting.
struct Pipe {
    /// Until the stream ends: every holder of its other end has closed it.
    file: Option<File>,
    keep: Keep,
    stream: Stream,
    kept: Kept,
}

impl Pipe {
    /// The stream on `fd`, switched to reads that return at once. A stream
    /// that cannot be switched is refused rather than read some other way:
    /// any read that can wait, even one after the program exits, waits on a
    /// helper still holding the stream, past the limit.
    fn new(fd: Option<OwnedFd>, keep: Keep, stream: Stream) -> std::io::Result<Self> {
        if let Some(fd) = &fd
            && !super::set_nonblocking_on(fd.as_raw_fd())
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self {
            file: fd.map(File::from),
            keep,
            stream,
            kept: Kept::default(),
        })
    }

    /// While the program runs: take what the stream holds now, up to
    /// [`DRAIN_LIMIT`], and let it go once it has ended. Reports whether it
    /// took anything.
    fn drain(&mut self) -> Result<bool, RunError> {
        let Some(file) = self.file.as_mut() else {
            return Ok(false);
        };
        let mut chunk = [0u8; 8192];
        let mut taken = 0;
        let mut ended = false;
        while taken < DRAIN_LIMIT {
            match file.read(&mut chunk) {
                Ok(0) => {
                    ended = true;
                    break;
                }
                Ok(n) => {
                    taken += n;
                    self.kept.add(&chunk[..n], self.keep, self.stream)?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                // A stream that fails to read will not recover, and waiting
                // on it would wake at once each time.
                Err(_) => {
                    ended = true;
                    break;
                }
            }
        }
        if ended {
            self.file = None;
        }
        Ok(taken > 0)
    }

    /// Once the program has exited: what it wrote, all of which is in the
    /// stream by now.
    fn finish(&mut self) -> Result<(), RunError> {
        self.drain().map(|_| ())
    }
}

/// Wait up to `wait`, waking as soon as a stream croft reads has output or
/// ends, so a program that writes fast is read as fast as it writes.
fn wait_for_output(pipes: &[Pipe], wait: Duration) {
    let mut ready: Vec<libc::pollfd> = pipes
        .iter()
        .filter_map(|pipe| pipe.file.as_ref())
        .map(|file| libc::pollfd {
            fd: file.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        })
        .collect();
    if ready.is_empty() {
        // No stream left to wake on: only the program's exit is awaited.
        std::thread::sleep(wait);
        return;
    }
    let ms = libc::c_int::try_from(wait.as_millis()).unwrap_or(libc::c_int::MAX);
    // SAFETY: `ready` holds `ready.len()` initialised `pollfd`s on descriptors
    // the pipes own, which outlive the call. An interrupted or failed poll only
    // ends this wait early; the caller checks the program and the clock either
    // way.
    unsafe { libc::poll(ready.as_mut_ptr(), ready.len() as libc::nfds_t, ms) };
}

/// Kill a program croft no longer waits for, with everything it started when
/// it runs apart, and reap it on a thread of its own so croft never waits on
/// it (#2755).
fn stop(mut child: Child, detached: bool) {
    if detached && let Ok(pid) = i32::try_from(child.id()) {
        // SAFETY: a plain signal to the process group the program leads. It
        // is not reaped yet, so its id cannot have passed to another.
        unsafe { libc::kill(-pid, libc::SIGKILL) };
    }
    let _ = child.kill();
    std::thread::spawn(move || {
        let _ = child.wait();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str) -> Command {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", script]);
        cmd
    }

    fn bounded(stdout: Keep, stderr: Keep, limit: Duration) -> Bounded {
        Bounded {
            stdout,
            stderr,
            detached: false,
            limit,
            retry_busy: false,
        }
    }

    /// Whether `pid` is gone: no longer running, or only a zombie waiting for
    /// a reaper on another thread.
    fn gone(pid: i32) -> bool {
        // SAFETY: signal 0 only asks whether the process exists.
        let signalled = unsafe { libc::kill(pid, 0) };
        signalled != 0
            || std::fs::read_to_string(format!("/proc/{pid}/stat"))
                .ok()
                .and_then(|stat| {
                    let after = stat.rsplit_once(')')?.1;
                    after.split_whitespace().next().map(|state| state == "Z")
                })
                .unwrap_or(true)
    }

    fn wait_gone(pid: i32) -> bool {
        let until = Instant::now() + Duration::from_secs(5);
        while Instant::now() < until {
            if gone(pid) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        gone(pid)
    }

    #[test]
    fn a_program_is_run_to_its_exit_with_what_it_wrote() {
        let ran = bounded(
            Keep::All { cap: 1024 },
            Keep::All { cap: 1024 },
            Duration::from_secs(5),
        )
        .run(sh("echo out; echo err >&2; exit 3"))
        .unwrap();
        assert_eq!(ran.status.code(), Some(3));
        assert_eq!(ran.stdout.text(), "out\n");
        assert_eq!(ran.stderr.text(), "err\n");
    }

    /// A stream kept as nothing goes to `/dev/null`, and stdin is always
    /// empty, whatever the command said before.
    #[cfg(target_os = "linux")]
    #[test]
    fn its_input_and_an_unkept_stream_are_null() {
        let mut cmd =
            sh("i=$(readlink /proc/$$/fd/0); o=$(readlink /proc/$$/fd/1); echo \"$i $o\" >&2");
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped());
        let ran = bounded(
            Keep::Nothing,
            Keep::All { cap: 1024 },
            Duration::from_secs(5),
        )
        .run(cmd)
        .unwrap();
        assert_eq!(ran.stderr.text(), "/dev/null /dev/null\n");
        assert!(ran.stdout.text().is_empty());
    }

    #[test]
    fn a_program_that_cannot_start_is_reported_as_not_started() {
        let run = bounded(Keep::Nothing, Keep::Nothing, Duration::from_secs(5))
            .run(Command::new("/nonexistent/croft-test-program"));
        assert!(matches!(run, Err(RunError::Spawn(_))), "{run:?}");
    }

    /// A stream kept whole is refused past its cap, never cut, and the
    /// program is stopped rather than left writing.
    #[test]
    fn a_stream_kept_whole_is_refused_past_its_cap() {
        let started = Instant::now();
        let run = bounded(
            Keep::All { cap: 1000 },
            Keep::Nothing,
            Duration::from_secs(20),
        )
        .run(sh("exec yes x"));
        assert!(
            matches!(
                run,
                Err(RunError::TooLong {
                    stream: Stream::Stdout,
                    cap: 1000
                })
            ),
            "{run:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn a_stream_exactly_at_its_cap_is_kept() {
        let ran = bounded(Keep::All { cap: 6 }, Keep::Nothing, Duration::from_secs(5))
            .run(sh("printf 'abcdef'"))
            .unwrap();
        assert_eq!(ran.stdout.text(), "abcdef");
    }

    /// A tail keeps the end of a stream far longer than itself, and is read as
    /// fast as the program writes: a pipeful a 50 ms pause, 32 MB would take
    /// 25 s and be killed at the 10 s limit.
    #[test]
    fn a_tail_keeps_the_end_of_a_fast_flood() {
        let ran = bounded(Keep::Tail(64), Keep::Nothing, Duration::from_secs(10))
            .run(sh("head -c 32000000 /dev/zero | tr '\\0' x; echo done"))
            .unwrap();
        assert!(ran.stdout.cut());
        assert_eq!(ran.stdout.text().len(), 64);
        assert!(
            ran.stdout.text().ends_with("xdone\n"),
            "{}",
            ran.stdout.text()
        );
    }

    /// A helper the program leaves running with its output still open does
    /// not hold croft past the program's own exit.
    #[test]
    fn a_helper_holding_a_stream_does_not_hold_the_run() {
        let dir = tempfile::tempdir().unwrap();
        let pid = dir.path().join("helper.pid");
        let started = Instant::now();
        let ran = bounded(
            Keep::All { cap: 1024 },
            Keep::Nothing,
            Duration::from_secs(20),
        )
        .run(sh(&format!(
            "sleep 30 & echo $! > '{}'; echo hi",
            pid.display()
        )))
        .unwrap();
        let took = started.elapsed();
        let helper = std::fs::read_to_string(&pid).ok();
        if let Some(helper) = helper.and_then(|h| h.trim().parse::<i32>().ok()) {
            // SAFETY: a plain signal to the helper this test started.
            unsafe { libc::kill(helper, libc::SIGKILL) };
        }
        assert_eq!(ran.stdout.text(), "hi\n");
        assert!(took < Duration::from_secs(10), "took {took:?}");
    }

    /// A program that never stops writing is still killed at its limit.
    #[test]
    fn a_program_that_never_stops_writing_is_killed_at_its_limit() {
        let started = Instant::now();
        let run =
            bounded(Keep::Tail(64), Keep::Nothing, Duration::from_secs(1)).run(sh("exec yes x"));
        assert!(matches!(run, Err(RunError::TimedOut)), "{run:?}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    /// A program that finishes at once is seen to have finished within a
    /// millisecond or two, not a whole long wait later; a quiet one costs a
    /// wake every 50 ms at most.
    #[test]
    fn the_wait_between_checks_starts_short_and_grows_to_the_longest() {
        assert_eq!(pause(0), Duration::from_millis(1));
        assert_eq!(pause(1), Duration::from_millis(2));
        assert_eq!(pause(5), Duration::from_millis(32));
        assert_eq!(pause(6), LONGEST_WAIT);
        assert_eq!(pause(u32::MAX), LONGEST_WAIT);
    }

    /// Run apart, a program has a session of its own, and at its limit
    /// everything it started is killed with it.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_detached_program_runs_apart_and_is_killed_with_what_it_started() {
        let dir = tempfile::tempdir().unwrap();
        let pids = dir.path().join("pids");
        let run = Bounded {
            stdout: Keep::Nothing,
            stderr: Keep::Nothing,
            detached: true,
            limit: Duration::from_millis(500),
            retry_busy: false,
        }
        .run(sh(&format!(
            "sleep 30 & echo $$ $! $(cut -d' ' -f6 /proc/$$/stat) > '{}'; wait",
            pids.display()
        )));
        assert!(matches!(run, Err(RunError::TimedOut)), "{run:?}");
        let pids = std::fs::read_to_string(&pids).unwrap();
        let pids: Vec<i32> = pids
            .split_whitespace()
            .map(|p| p.parse().unwrap())
            .collect();
        let [shell, helper, session] = pids[..] else {
            panic!("{pids:?}");
        };
        assert_eq!(session, shell, "it leads a session of its own");
        assert!(wait_gone(shell), "the program is killed");
        assert!(wait_gone(helper), "and what it started with it");
    }

    /// Not run apart, a program stays in croft's session.
    #[cfg(target_os = "linux")]
    #[test]
    fn an_attached_program_stays_in_crofts_session() {
        let ran = bounded(Keep::All { cap: 64 }, Keep::Nothing, Duration::from_secs(5))
            .run(sh("cut -d' ' -f6 /proc/$$/stat"))
            .unwrap();
        // SAFETY: asks for this process's own session id.
        let ours = unsafe { libc::getsid(0) };
        assert_eq!(ran.stdout.text().trim(), ours.to_string());
    }

    /// A stream that cannot be switched to reads that never wait is refused,
    /// not read some other way: any read that can wait would wait on a helper
    /// still holding it. A path-only descriptor is a real one that refuses the
    /// switch.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_stream_that_cannot_read_without_waiting_is_refused() {
        use std::os::unix::fs::OpenOptionsExt;
        let path_only = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH)
            .open("/")
            .unwrap();
        let refused = Pipe::new(
            Some(OwnedFd::from(path_only)),
            Keep::Tail(64),
            Stream::Stdout,
        );
        assert!(refused.is_err());
        assert!(Pipe::new(None, Keep::Nothing, Stream::Stdout).is_ok());
    }

    /// Cutting the front of a tail lands at a whole character, so its text
    /// never opens on a broken one.
    #[test]
    fn a_cut_tail_starts_at_a_whole_character() {
        let mut kept = Kept::default();
        let tail = Keep::Tail(64);
        kept.add("é".repeat(32).as_bytes(), tail, Stream::Stderr)
            .unwrap();
        kept.add(b"z", tail, Stream::Stderr).unwrap();
        let text = kept.text();
        assert!(kept.cut());
        assert!(text.starts_with('é'), "{text}");
        assert!(text.ends_with("éz"));
        assert!(!text.contains('\u{fffd}'));
    }
}
