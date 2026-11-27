//! Starting outside programs (#2062, card 12).
//!
//! Every git, gh, ssh, rsync and scp call names its program through
//! [`program`], or starts at [`command`]. A test can put a stand-in in a
//! program's place for its own thread with [`stand_in`]. In a release build
//! the name passes straight through, so croft runs exactly the program it
//! named.

use std::ffi::OsString;
use std::process::{Command, Stdio};

#[cfg(test)]
pub(crate) mod record;

/// The program croft starts for `name`: `name` itself, or in a test the
/// stand-in this thread put in its place.
pub fn program(name: &str) -> OsString {
    #[cfg(test)]
    if let Some(path) = STAND_INS.with(|s| s.borrow().get(name).cloned()) {
        return path.into_os_string();
    }
    OsString::from(name)
}

/// A [`Command`] for [`program`]`(name)`.
pub fn command(name: &str) -> Command {
    Command::new(program(name))
}

/// Spawn `cmd`, trying again for a moment while its program is "Text file
/// busy" (ETXTBSY). A file just written is busy while any process forked
/// meanwhile still holds the write descriptor, until that child execs,
/// which takes microseconds. A freshly installed or updated `gh` can hit it,
/// and a test's stand-in script hits it whenever another test forks.
pub(crate) fn spawn_retrying_busy(cmd: &mut Command) -> std::io::Result<std::process::Child> {
    retry_while_busy(|| cmd.spawn(), |r| r.as_ref().is_err_and(text_file_busy))
}

/// [`Command::output`] through [`spawn_retrying_busy`]: stdin closed, stdout
/// and stderr captured, waiting out a moment of "Text file busy".
pub(crate) fn output_retrying_busy(cmd: &mut Command) -> std::io::Result<std::process::Output> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    spawn_retrying_busy(cmd)?.wait_with_output()
}

/// Whether a spawn failed because its program is "Text file busy" (ETXTBSY).
pub(crate) fn text_file_busy(e: &std::io::Error) -> bool {
    const ETXTBSY: i32 = 26;
    e.raw_os_error() == Some(ETXTBSY)
}

/// Call `attempt` again, 10 ms apart and at most 50 more times, while `busy`
/// says its result is a moment of "Text file busy". Nothing `attempt` takes
/// is held across the pause, so a caller that spawns under a lock lets
/// others in between tries.
pub(crate) fn retry_while_busy<R>(mut attempt: impl FnMut() -> R, busy: impl Fn(&R) -> bool) -> R {
    let mut tries = 0;
    loop {
        let result = attempt();
        if tries >= 50 || !busy(&result) {
            return result;
        }
        tries += 1;
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// `O_NONBLOCK` onto one descriptor, reporting whether it is set now.
///
/// This is the whole of `set_nonblocking`'s unix arm, which does nothing but
/// hand it a descriptor. It exists as a seam because provoking the fcntl
/// failure through a live `ChildStderr` means closing the fd under the
/// handle, which trips Rust's IO-safety check and aborts the process instead
/// of failing an assertion - so the test drives the failure path here and the
/// success path through `set_nonblocking`, and between them every line of
/// both is executed.
#[cfg(unix)]
fn set_nonblocking_on(fd: std::os::fd::RawFd) -> bool {
    // `EINTR` is a signal arriving mid-call, not a verdict about the
    // descriptor: retrying is the documented response, and treating it as
    // permanent would disable the in-loop drain over a transient the next
    // call resolves - reintroducing the pipe-full stall this change removes.
    // Bounded rather than unbounded, so a pathological signal storm cannot
    // spin here instead of getting on with the render.
    fn retry_on_eintr(mut call: impl FnMut() -> libc::c_int) -> libc::c_int {
        for _ in 0..4 {
            let rc = call();
            if rc >= 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                return rc;
            }
        }
        -1
    }

    // SAFETY: both calls only read and set the status flags of `fd`, and
    // neither takes ownership of it. An invalid `fd` is reported as EBADF
    // rather than being undefined.
    unsafe {
        let flags = retry_on_eintr(|| libc::fcntl(fd, libc::F_GETFL));
        if flags < 0 {
            return false;
        }
        retry_on_eintr(|| libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK)) >= 0
    }
}

/// Put the pipe in non-blocking mode so a read returns what is there rather
/// than waiting for more (#553).
///
/// Reports whether the pipe is actually non-blocking now, because the answer
/// decides whether the caller may read BEFORE the child is reaped. A blocking
/// read there waits on a program that is still running, and the time limit
/// and the kill path both sit after it - so a failed `fcntl` would reinstate
/// the hang a read while it runs exists to remove. Callers must not drain a
/// pipe this returned `false` for until the child has exited.
///
/// With no pipe at all the answer is `true`: there is nothing to block on,
/// and every drain returns at once on `None`.
///
/// Off unix the fd type does not exist, so no pipe is ever made non-blocking
/// and a real pipe always answers `false` - the caller then keeps to the
/// single after-exit read.
pub(crate) fn set_nonblocking(pipe: Option<&std::process::ChildStderr>) -> bool {
    #[cfg(unix)]
    {
        let Some(pipe) = pipe else {
            // No pipe is vacuously safe to drain: a drain returns at once on
            // `None`, so it can never block.
            return true;
        };
        use std::os::fd::AsRawFd;
        set_nonblocking_on(pipe.as_raw_fd())
    }
    #[cfg(not(unix))]
    {
        // No fd type, so nothing is made non-blocking and a real pipe must
        // answer `false`. The caller then skips the in-loop drain, which
        // trades one hazard for a smaller one: a program writing more than a
        // pipeful before exiting can stall on a full buffer, where the unix
        // path would have drained it. The alternative is worse - a blocking
        // read BEFORE `try_wait` waits on a live program and takes the time
        // limit and the kill path down with it, so the stall would be
        // unbounded rather than bounded by the limit.
        //
        // Reachable only on a non-unix target, and croft has none: there is
        // no Windows job in ci.yml and no windows dependency in Cargo.toml,
        // and croft documents WSL2 - itself unix - as the Windows story.
        // Worth fixing with an overlapped/thread reader if that ever changes;
        // not worth a reader thread on every platform today.
        pipe.is_none()
    }
}

#[cfg(test)]
thread_local! {
    /// The stand-ins this test thread put in place of programs, by name.
    static STAND_INS: std::cell::RefCell<std::collections::HashMap<String, std::path::PathBuf>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// Start `path` wherever code on this thread starts `name`, for the rest of
/// the thread. Commands built on other threads still start `name`.
#[cfg(test)]
pub(crate) fn stand_in(name: &str, path: std::path::PathBuf) {
    STAND_INS.with(|s| s.borrow_mut().insert(name.to_string(), path));
}

/// A stand-in script for `name` on this thread that runs `body` with `sh`.
/// It is ready to run when this returns: a script just written stays "Text
/// file busy" while another test thread's forked child still holds its write
/// descriptor, so a dry run waits that out before any code under test starts
/// it, including code that starts a program without retrying.
#[cfg(all(test, unix))]
pub(crate) fn stand_in_script(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(
        &path,
        format!("#!/bin/sh\n[ \"$1\" = --stand-in-ready ] && exit 0\n{body}"),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let ready = retry_while_busy(
        || {
            Command::new(&path)
                .arg("--stand-in-ready")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
        },
        |r| r.as_ref().is_err_and(text_file_busy),
    );
    assert!(
        ready.as_ref().is_ok_and(|s| s.success()),
        "the stand-in for {name} runs: {ready:?}"
    );
    stand_in(name, path.clone());
    path
}

/// What a [`record_runs`] stand-in wrote down.
#[cfg(all(test, target_os = "linux"))]
pub(crate) struct Runs {
    log: std::path::PathBuf,
}

#[cfg(all(test, target_os = "linux"))]
impl Runs {
    /// One line per run: its arguments, then where its stdin, stdout and
    /// stderr went. `/dev/null` is nowhere; a pipe reads `pipe`, a socket
    /// `socket` and a terminal `terminal`, whichever one it was.
    pub(crate) fn lines(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(|line| {
                line.split(' ')
                    .map(|word| {
                        if word.starts_with("pipe:[") {
                            "pipe"
                        } else if word.starts_with("socket:[") {
                            "socket"
                        } else if word.starts_with("/dev/pts/") || word.starts_with("/dev/tty") {
                            "terminal"
                        } else {
                            word
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect()
    }
}

/// A stand-in for `name` on this thread that does nothing but write down
/// each run: its arguments, and what each of its three streams is connected
/// to, read from `/proc`. It exits 0.
#[cfg(all(test, target_os = "linux"))]
pub(crate) fn record_runs(dir: &std::path::Path, name: &str) -> Runs {
    let log = dir.join(format!("{name}.runs"));
    stand_in_script(
        dir,
        name,
        &format!(
            "i=$(readlink /proc/$$/fd/0); o=$(readlink /proc/$$/fd/1); e=$(readlink /proc/$$/fd/2)\n\
             printf '%s | stdin %s | stdout %s | stderr %s\\n' \"$*\" \"$i\" \"$o\" \"$e\" >> '{}'\n",
            log.display()
        ),
    );
    Runs { log }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_passes_through_until_a_stand_in_takes_its_place() {
        assert_eq!(program("git"), OsString::from("git"));
        stand_in("git", std::path::PathBuf::from("/stand-in/git"));
        assert_eq!(program("git"), OsString::from("/stand-in/git"));
        assert_eq!(
            command("git").get_program(),
            std::ffi::OsStr::new("/stand-in/git")
        );
        // Other names, and other threads, are untouched.
        assert_eq!(program("ssh"), OsString::from("ssh"));
        let elsewhere = std::thread::spawn(|| program("git")).join().unwrap();
        assert_eq!(elsewhere, OsString::from("git"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_recording_stand_in_writes_down_each_run_and_its_streams() {
        let dir = tempfile::tempdir().unwrap();
        let runs = record_runs(dir.path(), "tool");
        let quiet = command("tool")
            .args(["-x", "one"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(quiet.success());
        let piped = command("tool").arg("two").output().unwrap();
        assert!(piped.status.success());
        assert_eq!(
            runs.lines(),
            [
                "-x one | stdin /dev/null | stdout /dev/null | stderr /dev/null",
                "two | stdin /dev/null | stdout pipe | stderr pipe",
            ]
        );
    }

    /// `set_nonblocking` must REPORT the outcome, because the caller reads
    /// the pipe before reaping the child and only a non-blocking pipe makes
    /// that read safe. A bare `true` would pass the live-pipe case, so the
    /// bad-descriptor case is what gives the assertion teeth: the two
    /// together pin a return value that actually tracks the fcntl.
    ///
    /// The failure is provoked on a RAW fd rather than a real pipe, because
    /// closing one out from under a `ChildStderr` aborts the process under
    /// Rust's IO-safety checks - which would "detect" a regression by
    /// crashing rather than failing, and abort a passing run just as readily.
    #[cfg(unix)]
    #[test]
    fn set_nonblocking_reports_whether_the_pipe_is_now_non_blocking() {
        let mut child = std::process::Command::new("sh")
            .args(["-c", "exit 0"])
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn");
        let pipe = child.stderr.take().expect("piped stderr");

        // A live pipe: the fcntl pair succeeds and the answer is yes.
        assert!(
            set_nonblocking(Some(&pipe)),
            "a live pipe must report non-blocking"
        );

        // -1 is never a valid descriptor, so F_GETFL fails with EBADF - the
        // same branch a real fcntl failure takes. `set_nonblocking`'s unix
        // arm is a call to this and nothing else, so this IS its failure
        // path, reached without closing an fd under a live handle.
        assert!(
            !set_nonblocking_on(-1),
            "a bad descriptor must report failure, not be ignored"
        );

        // And the no-pipe case the doc promises, so the contract is pinned on
        // every input shape rather than only the one with a pipe.
        assert!(set_nonblocking(None), "no pipe means nothing to block on");

        let _ = child.wait();
    }
}
