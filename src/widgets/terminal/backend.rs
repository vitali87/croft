//! The process behind a pane: the local pty or the session host, waiting on its output,
//! resizing it, noticing it exit, and shutting it down.

use super::*;

impl PtyTerminal {
    /// Notice that the pane's shell has exited (#912): `exit`, Ctrl+D, a
    /// task that ran `exit 3`. The shell is reaped (it stayed a zombie for
    /// as long as the pane lived), and once the reader has painted all it
    /// wrote, the exit goes under its output and the pane is marked
    /// exited. Returns the exit code the first time only.
    pub fn poll_exit(&mut self) -> Option<Option<i32>> {
        if self.exit.is_some() {
            return None;
        }
        let output_closed = self.reader_thread.as_ref().is_none_or(|h| h.is_finished());
        let code = self.backend.try_exit(output_closed)?;
        if !output_closed {
            let since = *self.reaped_at.get_or_insert_with(std::time::Instant::now);
            if since.elapsed() < EXIT_OUTPUT_GRACE {
                return None;
            }
        }
        self.exit = Some(code);
        // A run pane's reader paints its own footer.
        if !self.run_pane {
            let footer = match code {
                Some(c) => format!("\r\n\x1b[2m[Process exited with code {c}]\x1b[22m\r\n"),
                None => String::from("\r\n\x1b[2m[Process exited]\x1b[22m\r\n"),
            };
            Processor::<StdSyncHandler>::new().advance(&mut *self.term.lock(), footer.as_bytes());
            self.pty_dirty.store(true, Ordering::Release);
        }
        Some(code)
    }

    /// Whether [`Self::poll_exit`] has found the shell exited: the pane
    /// then takes no input.
    pub fn has_exited(&self) -> bool {
        self.exit.is_some()
    }

    /// Whether this is a `new_running` program pane rather than a shell.
    pub fn is_run_pane(&self) -> bool {
        self.run_pane
    }

    /// The pane host holding this pane's shell, when one does (#694): what
    /// a saved session records so the next croft can reattach.
    pub fn host_socket(&self) -> Option<&std::path::Path> {
        match &self.backend {
            PaneBackend::Hosted { socket, .. } => Some(socket),
            PaneBackend::Local { .. } => None,
        }
    }
}

/// Where a pane's shell comes from.
pub(super) enum PaneSource {
    /// Start `cmd`: on a PTY croft holds, or in a new pane host.
    Spawn { cmd: CommandBuilder, hosted: bool },
    /// Reattach to the pane host at this socket (#694).
    Attach(std::path::PathBuf),
}

/// Who holds a pane's PTY.
pub(super) enum PaneBackend {
    /// croft: the PTY dies with croft.
    Local {
        master: Box<dyn MasterPty + Send>,
        child: Box<dyn portable_pty::Child + Send + Sync>,
    },
    /// A pane host (#694): the shell outlives croft. `conn` carries input,
    /// resizes and the kill; the pid comes from the host's greeting, and
    /// `exit` from its notice that the shell ended.
    Hosted {
        conn: Arc<std::sync::Mutex<std::os::unix::net::UnixStream>>,
        socket: std::path::PathBuf,
        pid: Option<u32>,
        exit: Arc<std::sync::Mutex<Option<i32>>>,
    },
}

impl PaneBackend {
    pub(super) fn pid(&self) -> Option<u32> {
        match self {
            PaneBackend::Local { child, .. } => child.process_id(),
            PaneBackend::Hosted { pid, .. } => *pid,
        }
    }

    /// The terminal's foreground process group: `tcgetpgrp` on a PTY croft
    /// holds, the shell's `tpgid` from the process table for a hosted one
    /// (the same value, read without the master).
    pub(super) fn process_group_leader(&self) -> Option<i32> {
        match self {
            PaneBackend::Local { master, .. } => master.process_group_leader(),
            PaneBackend::Hosted { pid, .. } => pid.and_then(|p| foreground_group_of(p as i32)),
        }
    }

    pub(super) fn resize(&self, cols: u16, rows: u16) {
        match self {
            PaneBackend::Local { master, .. } => {
                let _ = master.resize(PtySize {
                    cols,
                    rows,
                    pixel_width: 0,
                    pixel_height: 0,
                });
            }
            PaneBackend::Hosted { conn, .. } => {
                let frame = crate::session_host::encode_control_frame(
                    &crate::session_host::Control::Resize { cols, rows },
                );
                let _ = conn.lock().unwrap().write_all(&frame);
            }
        }
    }

    /// The shell's exit code once it has ended, reaping it on a PTY croft
    /// holds (#912). A pane host reaps its own shell and sends the code
    /// before the pane's output closes, so it is read once
    /// `output_closed`; a host gone without sending one gives `Some(None)`.
    pub(super) fn try_exit(&mut self, output_closed: bool) -> Option<Option<i32>> {
        match self {
            PaneBackend::Local { child, .. } => {
                let status = child.try_wait().ok()??;
                Some(Some(status.exit_code() as i32))
            }
            PaneBackend::Hosted { exit, .. } => output_closed.then(|| *exit.lock().unwrap()),
        }
    }

    /// End the shell: kill and reap it, or ask its host to (which ends the
    /// host too), then drop the connection so the demux thread ends.
    pub(super) fn end(&mut self) {
        match self {
            PaneBackend::Local { child, .. } => {
                // A shell already reaped is never signalled: its pid may
                // name another process by now.
                if matches!(child.try_wait(), Ok(Some(_))) {
                    return;
                }
                let _ = child.kill();
                let _ = child.wait();
            }
            PaneBackend::Hosted { conn, .. } => {
                let frame =
                    crate::session_host::encode_control_frame(&crate::session_host::Control::Kill);
                let conn = conn.lock().unwrap();
                let _ = (&*conn).write_all(&frame);
                let _ = conn.shutdown(std::net::Shutdown::Both);
            }
        }
    }
}

/// Input for a hosted pane: each write is one bytes frame.
pub(super) struct HostInput(Arc<std::sync::Mutex<std::os::unix::net::UnixStream>>);

impl Write for HostInput {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let frame = crate::session_host::encode_bytes_frame(buf);
        (&*self.0.lock().unwrap()).write_all(&frame)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl PaneLink {
    pub(super) fn open(source: PaneSource, cols: u16, rows: u16) -> Result<Self> {
        match source {
            PaneSource::Spawn { cmd, hosted: true } => {
                let spec = crate::pane_host::PaneSpec {
                    socket: crate::pane_host::socket_for(&crate::pane_host::new_pane_id()),
                    cwd: cmd.get_cwd().map(std::path::PathBuf::from),
                    env: cmd
                        .iter_full_env_as_str()
                        .map(|(k, v)| (k.to_string(), v.to_string()))
                        .collect(),
                    cols,
                    rows,
                    argv: cmd
                        .get_argv()
                        .iter()
                        .map(|a| a.to_string_lossy().into_owned())
                        .collect(),
                };
                // A host that cannot start (no socket dir, a sandbox) must
                // not cost the user a terminal: the pane is croft's instead.
                match crate::pane_host::spawn(&spec)
                    .and_then(|()| Self::attach(&spec.socket, cols, rows))
                {
                    Ok(link) => Ok(link),
                    // Nothing to print from here: a write to stderr would
                    // land on croft's own screen (#537).
                    Err(_) => Self::local(cmd, cols, rows),
                }
            }
            PaneSource::Spawn { cmd, hosted: false } => Self::local(cmd, cols, rows),
            PaneSource::Attach(socket) => Self::attach(&socket, cols, rows),
        }
    }

    pub(super) fn local(cmd: CommandBuilder, cols: u16, rows: u16) -> Result<Self> {
        let pair = native_pty_system()
            .openpty(PtySize {
                cols,
                rows,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("openpty")?;
        let child = pair.slave.spawn_command(cmd).context("spawn child")?;
        let shell_pid = child.process_id().map(|p| p as i32);
        drop(pair.slave);
        let writer = pair.master.take_writer().context("take writer")?;
        let reader = pair.master.try_clone_reader().context("clone reader")?;
        let poll_fd = pair
            .master
            .as_raw_fd()
            .context("pty master has no raw fd")?;
        Ok(Self {
            writer,
            reader,
            poll_fd,
            shell_pid,
            backend: PaneBackend::Local {
                master: pair.master,
                child,
            },
        })
    }

    /// Attach to a pane host: read its greeting for the shell's pid, then
    /// hand the pane a pipe a demux thread fills with the shell's bytes
    /// (the redraw first), so the reader reads it exactly as it reads a
    /// PTY. The pipe closes when the host says the shell exited or the
    /// connection ends, which the reader sees as the PTY's EOF.
    pub(super) fn attach(socket: &std::path::Path, cols: u16, rows: u16) -> Result<Self> {
        use crate::session_host::{Control, Frame, FrameReader};
        use std::os::fd::AsRawFd;
        let stream = crate::pane_host::PaneClient::connect(socket, cols, rows)?.into_stream();
        let mut rd = stream
            .try_clone()
            .context("cloning the pane host connection")?;
        rd.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
        let mut frames = FrameReader::new();
        let mut pending: Vec<u8> = Vec::new();
        let mut pid = None;
        // A shell that exits while it attaches can have its exit arrive in
        // the same read as the greeting (#912): kept, not dropped with the
        // frames the greeting has no use for.
        let mut greeting_exit = None;
        let mut buf = [0u8; 65536];
        while pid.is_none() {
            let n = rd
                .read(&mut buf)
                .context("reading the pane host's greeting")?;
            anyhow::ensure!(n > 0, "the pane host at {} closed", socket.display());
            for frame in frames.push(&buf[..n]) {
                match frame {
                    Frame::Control(Control::PaneInfo { pid: p }) => pid = Some(p),
                    Frame::Bytes(b) => pending.extend_from_slice(&b),
                    Frame::Control(Control::Exit { code }) => greeting_exit = Some(code),
                    _ => {}
                }
            }
        }
        rd.set_read_timeout(None)?;
        let (pipe_r, mut pipe_w) = std::io::pipe().context("pane host pipe")?;
        let exited = greeting_exit.is_some();
        let exit = Arc::new(std::sync::Mutex::new(greeting_exit));
        let exit_for_demux = exit.clone();
        std::thread::spawn(move || {
            if pipe_w.write_all(&pending).is_err() || exited {
                return;
            }
            let mut buf = [0u8; 65536];
            loop {
                let n = match rd.read(&mut buf) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => n,
                };
                for frame in frames.push(&buf[..n]) {
                    match frame {
                        Frame::Bytes(b) => {
                            if pipe_w.write_all(&b).is_err() {
                                return;
                            }
                        }
                        Frame::Control(Control::Exit { code }) => {
                            *exit_for_demux.lock().unwrap() = Some(code);
                            return;
                        }
                        _ => {}
                    }
                }
            }
        });
        let conn = Arc::new(std::sync::Mutex::new(stream));
        Ok(Self {
            writer: Box::new(HostInput(conn.clone())),
            poll_fd: pipe_r.as_raw_fd(),
            reader: Box::new(pipe_r),
            shell_pid: pid.map(|p| p as i32),
            backend: PaneBackend::Hosted {
                conn,
                socket: socket.to_path_buf(),
                pid,
                exit,
            },
        })
    }
}

/// Block until the pty has data (or EOF/error — the following `read` then
/// observes it), until the shutdown pipe signals (its write end closed in
/// `Drop`), or until `deadline`, when there is one, passes (#1541). A bare
/// blocking `read` is not enough to ever exit: on Linux a background job
/// keeps the pty slave open after the shell dies, so the read never returns
/// and joining the reader would freeze the UI for as long as the job runs.
pub(super) fn wait_pty_readable(
    pty_fd: std::os::fd::RawFd,
    shutdown: &std::io::PipeReader,
    deadline: Option<std::time::Instant>,
) -> PtyWait {
    loop {
        let timeout = match deadline {
            None => -1,
            Some(d) => {
                let left = d.saturating_duration_since(std::time::Instant::now());
                // Rounded up, so the wake is never early.
                left.as_micros().div_ceil(1000).min(i32::MAX as u128) as i32
            }
        };
        match poll_pty(pty_fd, shutdown, timeout) {
            // Woken before `deadline` all the same: wait out the rest.
            PtyWait::Idle if deadline.is_some_and(|d| std::time::Instant::now() < d) => {}
            wait => return wait,
        }
    }
}

/// How long the reader waits for more output before sending a held-back
/// dev-server URL as it stands.
pub(super) const PORT_URL_SETTLE_MS: i32 = 150;

/// How long a pane whose shell was reaped waits for its output to close
/// before reporting the exit anyway: on Linux a background job the shell
/// left running keeps the pty open (see [`wait_pty_readable`]).
pub(super) const EXIT_OUTPUT_GRACE: std::time::Duration = std::time::Duration::from_millis(500);

/// What [`poll_pty`] saw.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum PtyWait {
    /// The pty has output.
    Ready,
    /// Shutdown was signalled, or polling failed.
    Closed,
    /// `timeout_ms` passed with neither.
    Idle,
}

/// [`wait_pty_readable`] with a timeout in milliseconds (`-1` waits forever).
pub(super) fn poll_pty(
    pty_fd: std::os::fd::RawFd,
    shutdown: &std::io::PipeReader,
    timeout_ms: i32,
) -> PtyWait {
    use std::os::fd::AsRawFd;
    let mut fds = [
        libc::pollfd {
            fd: pty_fd,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: shutdown.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    loop {
        let n = unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout_ms) };
        if n < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return PtyWait::Closed;
        }
        if n == 0 {
            return PtyWait::Idle;
        }
        if fds[1].revents != 0 {
            return PtyWait::Closed;
        }
        if fds[0].revents != 0 {
            return PtyWait::Ready;
        }
    }
}

impl Drop for PtyTerminal {
    fn drop(&mut self) {
        // Kill the shell AND reap it: `kill()` reaps only when the shell dies
        // inside its SIGHUP grace loop; the SIGKILL escalation (a shell that
        // traps HUP) never waits, and `Child`'s own drop doesn't either, so
        // without the `wait` every such closed pane left a zombie for the
        // life of the process. The responder thread ends on its own once
        // `self.term` (holding its channel sender) drops with this struct.
        //
        // A hosted pane (#694) asks its host to end the shell the same way:
        // closing a pane or quitting ends it, and only a croft that dies
        // without dropping its panes leaves hosts to reattach to.
        self.backend.end();
        // Free the rewind buffer now and hand its share of the budget back
        // to the other panes (#694). Not left to the Arc: the reader thread
        // holds a clone until it is joined below.
        crate::rewind::budget().release(&self.rewind);
        // Wake the reader (POLLHUP on its shutdown fd) and join it. EOF alone
        // is not a reliable wake: see `wait_pty_readable`.
        drop(self.reader_shutdown.take());
        if let Some(handle) = self.reader_thread.take() {
            let _ = handle.join();
        }
    }
}
