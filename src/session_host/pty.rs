//! The PTY master and the inner command as the host holds them, whether it spawned them
//! itself or inherited them across a host swap: resizing the master and waiting for the
//! child.

use super::*;

/// The one master-side operation the host needs after startup (resize); a
/// fresh host holds the portable-pty master, a resumed host only the raw
/// master fd it inherited.
pub(super) enum MasterHandle {
    Spawned(Box<dyn MasterPty + Send>),
    Adopted(std::os::fd::RawFd),
}

impl MasterHandle {
    pub(super) fn resize(&self, size: PtySize) -> Result<()> {
        match self {
            Self::Spawned(master) => master.resize(size),
            Self::Adopted(fd) => {
                let ws = libc::winsize {
                    ws_row: size.rows,
                    ws_col: size.cols,
                    ws_xpixel: 0,
                    ws_ypixel: 0,
                };
                let rc = unsafe { libc::ioctl(*fd, libc::TIOCSWINSZ, &ws) };
                anyhow::ensure!(rc == 0, "TIOCSWINSZ({fd}) failed");
                Ok(())
            }
        }
    }

    pub(super) fn as_raw_fd(&self) -> Option<std::os::fd::RawFd> {
        match self {
            Self::Spawned(master) => master.as_raw_fd(),
            Self::Adopted(fd) => Some(*fd),
        }
    }
}

/// The inner command, waitable from either lifetime: the image that spawned
/// it (portable-pty child) or a successor image that inherited it (raw pid -
/// exec preserves the parent/child relationship, so waitpid still works).
pub(super) enum ChildHandle {
    Spawned(Box<dyn portable_pty::Child + Send + Sync>),
    Adopted(u32),
}

impl ChildHandle {
    pub(super) fn process_id(&self) -> Option<u32> {
        match self {
            Self::Spawned(child) => child.process_id(),
            Self::Adopted(pid) => Some(*pid),
        }
    }

    pub(super) fn wait_code(&mut self) -> Result<i32> {
        match self {
            Self::Spawned(child) => Ok(child
                .wait()
                .context("waiting on inner command")?
                .exit_code() as i32),
            Self::Adopted(pid) => {
                let pid = *pid as libc::pid_t;
                loop {
                    let mut status: libc::c_int = 0;
                    let rc = unsafe { libc::waitpid(pid, &mut status, 0) };
                    if rc == -1 {
                        let err = std::io::Error::last_os_error();
                        if err.raw_os_error() == Some(libc::EINTR) {
                            continue;
                        }
                        anyhow::bail!("waitpid({pid}): {err}");
                    }
                    if libc::WIFEXITED(status) {
                        return Ok(libc::WEXITSTATUS(status));
                    }
                    if libc::WIFSIGNALED(status) {
                        return Ok(128 + libc::WTERMSIG(status));
                    }
                }
            }
        }
    }
}
