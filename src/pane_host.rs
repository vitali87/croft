//! A terminal pane's shell, held by a process of its own (#694).
//!
//! A pane's PTY normally belongs to the croft process, so when croft is
//! killed (the OOM killer's favourite, being the largest process) the master
//! closes and every shell and job in every pane dies with it. A pane host
//! holds one pane's PTY instead: croft becomes a client of it over a unix
//! socket, and a croft that dies leaves the host, the shell and its jobs
//! running for the next croft to reattach.
//!
//! It speaks [`crate::session_host`]'s framing (bytes and JSON controls) but
//! is its own, much smaller server: one pane, one client at a time (a new
//! client replaces the old, so a relaunched croft takes over from a hung
//! one), no participants or write control, and none of the croft-session
//! environment `session-host` sets up for a whole croft.
//!
//! # Reattaching shows the screen
//!
//! Output that arrives with no client attached would otherwise be lost, and
//! replaying raw bytes from some offset draws a screen that never existed
//! (see [`crate::rewind`]). So the host keeps a terminal model of its own,
//! fed every byte, and a client that connects is first sent that model's
//! state drawn afresh: scrollback, screen, the alternate screen when a
//! full-screen program has it, the input modes that change what keys send,
//! and the cursor. Live output follows in order.
//!
//! # Lifetime
//!
//! The host ends when its shell does. A client asks it to end the shell
//! with [`Control::Kill`] (closing the pane, or croft quitting normally);
//! disconnecting leaves it running. A host no client has attached to for
//! [`IDLE_LIMIT`] ends its shell, so hosts of a workspace never reopened do
//! not run forever.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use alacritty_terminal::Term;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, TermMode};
use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};
use anyhow::{Context, Result};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::session_host::{Control, Frame, FrameReader, encode_bytes_frame, encode_control_frame};
use crate::widgets::terminal::VoidListener;

/// How long a host with no client keeps its shell before ending it.
pub const IDLE_LIMIT: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Scrollback the host keeps for a reattaching client.
pub const SCROLLBACK_LINES: usize = 2000;

/// How long a write to the client may block before the client is dropped:
/// a hung croft must not stall the shell's output (it keeps flowing into
/// the model, and the next client gets it redrawn).
const CLIENT_WRITE_TIMEOUT: Duration = Duration::from_secs(2);

/// What starts a pane host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneSpec {
    pub socket: PathBuf,
    pub cwd: Option<PathBuf>,
    /// The shell's environment. [`spawn`] gives it to the host process,
    /// whose shell inherits it: never as arguments, which any user on the
    /// machine can read (`ps`), and the environment can hold tokens. When
    /// the host is run directly, `--env` adds to its own environment.
    pub env: Vec<(String, String)>,
    pub cols: u16,
    pub rows: u16,
    /// The shell (or program) and its arguments.
    pub argv: Vec<String>,
}

/// The `croft` arguments that run a pane host for `spec`.
pub fn host_argv(spec: &PaneSpec) -> Vec<String> {
    let mut argv = vec![
        String::from("pane-host"),
        String::from("--socket"),
        spec.socket.display().to_string(),
        String::from("--size"),
        format!("{}x{}", spec.cols, spec.rows),
    ];
    if let Some(cwd) = &spec.cwd {
        argv.push(String::from("--cwd"));
        argv.push(cwd.display().to_string());
    }
    argv.push(String::from("--"));
    argv.extend(spec.argv.iter().cloned());
    argv
}

/// Parse `--size`'s `COLSxROWS`.
pub fn parse_size(s: &str) -> Option<(u16, u16)> {
    let (c, r) = s.split_once('x')?;
    let (c, r) = (c.parse().ok()?, r.parse().ok()?);
    (c > 0 && r > 0).then_some((c, r))
}

/// Parse an `--env` `KEY=VALUE`.
pub fn parse_env(s: &str) -> Option<(String, String)> {
    let (k, v) = s.split_once('=')?;
    (!k.is_empty()).then(|| (k.to_string(), v.to_string()))
}

/// Start a detached host for `spec` (a new session, so it outlives this
/// process and its terminal) and wait for its socket to answer.
pub fn spawn(spec: &PaneSpec) -> Result<()> {
    let exe = std::env::current_exe().context("resolving croft binary path")?;
    let mut cmd = std::process::Command::new(exe);
    cmd.args(host_argv(spec));
    if !spec.env.is_empty() {
        cmd.env_clear().envs(spec.env.iter().map(|(k, v)| (k, v)));
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn().context("spawning pane host")?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if crate::session::is_alive(&spec.socket) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    anyhow::bail!("the pane host did not start ({})", spec.socket.display())
}

/// What the model has drawn, as bytes that draw it again on a blank
/// terminal of the same size: scrollback and screen rows with their colours,
/// or the alternate screen when a full-screen program holds it, then the
/// input modes that change what keys and the mouse send, then the cursor.
pub fn redraw(term: &Term<VoidListener>) -> Vec<u8> {
    let mut out = String::from("\x1b[0m\x1b[H\x1b[2J\x1b[3J");
    let alt = term.mode().contains(TermMode::ALT_SCREEN);
    let rows = crate::widgets::terminal::ansi_rows_from(
        term,
        if alt { 0 } else { term.grid().topmost_line().0 },
    );
    // Printing scrollback pushes it off the top into the client's own
    // scrollback, leaving the screen rows on screen, as they were.
    if alt {
        out.push_str("\x1b[?1049h\x1b[H\x1b[2J");
    }
    let last = rows.len().saturating_sub(1);
    for (i, (_, ansi, wraps)) in rows.iter().enumerate() {
        out.push_str(ansi);
        out.push_str("\x1b[0m");
        // A soft-wrapped row runs on into the next without a line break.
        if i < last && !wraps {
            out.push_str("\r\n");
        }
    }
    let mode = term.mode();
    for (flag, code) in [
        (TermMode::APP_CURSOR, "?1"),
        (TermMode::BRACKETED_PASTE, "?2004"),
        (TermMode::MOUSE_REPORT_CLICK, "?1000"),
        (TermMode::MOUSE_DRAG, "?1002"),
        (TermMode::MOUSE_MOTION, "?1003"),
        (TermMode::FOCUS_IN_OUT, "?1004"),
        (TermMode::SGR_MOUSE, "?1006"),
    ] {
        if mode.contains(flag) {
            out.push_str(&format!("\x1b[{code}h"));
        }
    }
    let p = term.grid().cursor.point;
    out.push_str(&format!("\x1b[{};{}H", p.line.0.max(0) + 1, p.column.0 + 1));
    if !mode.contains(TermMode::SHOW_CURSOR) {
        out.push_str("\x1b[?25l");
    }
    out.into_bytes()
}

struct Model {
    term: Term<VoidListener>,
    parser: Processor<StdSyncHandler>,
}

impl Model {
    fn new(cols: u16, rows: u16) -> Self {
        let cfg = Config {
            scrolling_history: SCROLLBACK_LINES,
            ..Config::default()
        };
        let size = TermSize::new(cols as usize, rows as usize);
        Self {
            term: Term::new(cfg, &size, VoidListener::default()),
            parser: Processor::new(),
        }
    }
}

struct Pane {
    model: Mutex<Model>,
    /// The attached client's connection, and the one it replaced.
    client: Mutex<Option<UnixStream>>,
    /// When the last client left (or the host started with none).
    alone_since: Mutex<Option<Instant>>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    input: Mutex<Box<dyn Write + Send>>,
    pid: u32,
}

impl Pane {
    /// Send `frame` to the client, dropping it when the write fails or
    /// blocks past [`CLIENT_WRITE_TIMEOUT`].
    fn send(&self, client: &mut Option<UnixStream>, frame: &[u8]) {
        if let Some(s) = client.as_mut()
            && s.write_all(frame).is_err()
        {
            let _ = s.shutdown(std::net::Shutdown::Both);
            *client = None;
            *self.alone_since.lock().unwrap() = Some(Instant::now());
        }
    }

    fn resize(&self, cols: u16, rows: u16) {
        if cols == 0 || rows == 0 {
            return;
        }
        // The model first, so output drawn for the new size lands on a
        // model of that size.
        self.model
            .lock()
            .unwrap()
            .term
            .resize(TermSize::new(cols as usize, rows as usize));
        let _ = self.master.lock().unwrap().resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
    }

    /// End the shell: hang up, as closing a terminal does (the kernel
    /// passes the hangup on to its jobs), then kill the shell's process
    /// group if it has not gone. The shell leads its own session and group
    /// on the pane's terminal, so the group is its pid.
    fn kill(&self) {
        let pid = self.pid as libc::pid_t;
        if pid <= 0 {
            return;
        }
        unsafe {
            libc::kill(pid, libc::SIGHUP);
        }
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(2));
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        });
    }
}

/// Run the host for `spec` until its shell exits; the exit code is the
/// shell's.
pub fn serve(spec: &PaneSpec) -> Result<i32> {
    anyhow::ensure!(!spec.argv.is_empty(), "pane-host needs a command after --");
    if let Some(dir) = spec.socket.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let listener = crate::session::bind_socket_0600(&spec.socket)
        .with_context(|| format!("binding {}", spec.socket.display()))?;
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: spec.rows,
            cols: spec.cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("openpty")?;
    let mut cmd = CommandBuilder::new(&spec.argv[0]);
    cmd.args(&spec.argv[1..]);
    if let Some(cwd) = &spec.cwd {
        cmd.cwd(cwd);
    }
    for (k, v) in &spec.env {
        cmd.env(k, v);
    }
    let mut child = pair
        .slave
        .spawn_command(cmd)
        .context("spawning the pane's shell")?;
    drop(pair.slave);
    let pid = child.process_id().unwrap_or(0);
    let mut reader = pair
        .master
        .try_clone_reader()
        .context("cloning pty reader")?;
    let input = pair.master.take_writer().context("taking pty writer")?;
    let pane = Arc::new(Pane {
        model: Mutex::new(Model::new(spec.cols, spec.rows)),
        client: Mutex::new(None),
        alone_since: Mutex::new(Some(Instant::now())),
        master: Mutex::new(pair.master),
        input: Mutex::new(input),
        pid,
    });

    // Shell output: into the model, then to the client, under the client
    // lock so a client that attaches in between gets its redraw first.
    let (output_done, output_finished) = std::sync::mpsc::channel::<()>();
    {
        let pane = Arc::clone(&pane);
        std::thread::spawn(move || {
            let _done = output_done;
            let mut buf = [0u8; 65536];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let mut client = pane.client.lock().unwrap();
                        {
                            let mut m = pane.model.lock().unwrap();
                            let Model { term, parser } = &mut *m;
                            parser.advance(term, &buf[..n]);
                        }
                        pane.send(&mut client, &encode_bytes_frame(&buf[..n]));
                    }
                }
            }
        });
    }

    {
        let pane = Arc::clone(&pane);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let pane = Arc::clone(&pane);
                std::thread::spawn(move || client_thread(&pane, stream));
            }
        });
    }

    {
        let pane = Arc::clone(&pane);
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(60));
                let alone = *pane.alone_since.lock().unwrap();
                if alone.is_some_and(|t| t.elapsed() >= IDLE_LIMIT) {
                    pane.kill();
                    return;
                }
            }
        });
    }

    let status = child.wait().context("waiting on the pane's shell")?;
    let code = status.exit_code() as i32;
    // Let the last output reach the client before the exit notice. A job
    // the shell left running can hold the terminal open, so the wait is
    // bounded: the output thread ends when it drops its sender.
    let _ = output_finished.recv_timeout(Duration::from_secs(1));
    {
        let mut client = pane.client.lock().unwrap();
        pane.send(&mut client, &encode_control_frame(&Control::Exit { code }));
    }
    let _ = std::fs::remove_file(&spec.socket);
    Ok(code)
}

/// One client: its Hello attaches it (replacing any other), then its bytes
/// are the shell's input.
fn client_thread(pane: &Pane, stream: UnixStream) {
    let Ok(mut rd) = stream.try_clone() else {
        return;
    };
    let _ = stream.set_write_timeout(Some(CLIENT_WRITE_TIMEOUT));
    let mut frames = FrameReader::new();
    let mut buf = [0u8; 16384];
    let mut attached = false;
    loop {
        let n = match rd.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        for frame in frames.push(&buf[..n]) {
            match frame {
                Frame::Control(Control::Hello { cols, rows, .. }) if !attached => {
                    attached = true;
                    pane.resize(cols, rows);
                    let mut client = pane.client.lock().unwrap();
                    if let Some(old) = client.take() {
                        let _ = old.shutdown(std::net::Shutdown::Both);
                    }
                    let Ok(conn) = stream.try_clone() else {
                        return;
                    };
                    *client = Some(conn);
                    *pane.alone_since.lock().unwrap() = None;
                    let redraw = redraw(&pane.model.lock().unwrap().term);
                    let version = env!("CARGO_PKG_VERSION").to_string();
                    pane.send(
                        &mut client,
                        &encode_control_frame(&Control::ServerHello { version }),
                    );
                    pane.send(
                        &mut client,
                        &encode_control_frame(&Control::PaneInfo { pid: pane.pid }),
                    );
                    pane.send(&mut client, &encode_bytes_frame(&redraw));
                }
                Frame::Bytes(data) if attached => {
                    let _ = pane.input.lock().unwrap().write_all(&data);
                }
                Frame::Control(Control::Resize { cols, rows }) if attached => {
                    pane.resize(cols, rows);
                }
                Frame::Control(Control::Kill) => pane.kill(),
                Frame::Control(Control::Detach) => {
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                }
                _ => {}
            }
        }
    }
    // This connection is gone; if it was the attached client, the pane is
    // alone from now (a replacement already cleared that).
    let mut client = pane.client.lock().unwrap();
    let ours = client.as_ref().is_some_and(|c| same_socket(c, &stream));
    if ours {
        *client = None;
        *pane.alone_since.lock().unwrap() = Some(Instant::now());
    }
}

fn same_socket(a: &UnixStream, b: &UnixStream) -> bool {
    use std::os::fd::AsRawFd;
    let stat = |s: &UnixStream| {
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        (unsafe { libc::fstat(s.as_raw_fd(), &mut st) } == 0).then_some((st.st_dev, st.st_ino))
    };
    stat(a).is_some() && stat(a) == stat(b)
}

/// Where a pane's host listens: under croft's sessions folder, named for
/// the pane.
pub fn socket_for(pane_id: &str) -> PathBuf {
    crate::session::sessions_dir().join(format!("pane-{pane_id}.sock"))
}

/// A fresh id for a pane's host socket: this process, the time and a
/// counter, so two croft instances and two panes never collide.
pub fn new_pane_id() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{}-{nanos:x}-{n}", std::process::id())
}

/// A client's connection to a pane host.
pub struct PaneClient {
    stream: UnixStream,
}

impl PaneClient {
    /// Attach to the host at `socket` with a `cols`x`rows` view.
    pub fn connect(socket: &Path, cols: u16, rows: u16) -> Result<Self> {
        let mut stream = UnixStream::connect(socket)
            .with_context(|| format!("connecting to {}", socket.display()))?;
        stream.write_all(&encode_control_frame(&Control::Hello {
            name: String::from("pane"),
            cols,
            rows,
            version: env!("CARGO_PKG_VERSION").to_string(),
            client_id: String::new(),
        }))?;
        Ok(Self { stream })
    }

    /// The connection itself, for a pane that drives it directly.
    pub fn into_stream(self) -> UnixStream {
        self.stream
    }

    /// A second handle on the connection, for a reader thread.
    #[cfg(test)]
    pub fn try_clone_stream(&self) -> Result<UnixStream> {
        Ok(self.stream.try_clone()?)
    }

    #[cfg(test)]
    pub fn write_input(&mut self, data: &[u8]) -> std::io::Result<()> {
        self.stream.write_all(&encode_bytes_frame(data))
    }

    /// Ask the host to end the shell (closing the pane).
    #[cfg(test)]
    pub fn kill(&mut self) -> std::io::Result<()> {
        self.stream.write_all(&encode_control_frame(&Control::Kill))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read frames from `s` until `done` says the collected ones suffice.
    fn read_until(s: &mut UnixStream, done: impl Fn(&[Frame]) -> bool) -> Vec<Frame> {
        s.set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut frames = FrameReader::new();
        let mut got = Vec::new();
        let mut buf = [0u8; 65536];
        while !done(&got) {
            assert!(Instant::now() < deadline, "timed out; got {got:?}");
            match s.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => got.extend(frames.push(&buf[..n])),
                Err(_) => {}
            }
        }
        got
    }

    fn text(frames: &[Frame]) -> String {
        frames
            .iter()
            .filter_map(|f| match f {
                Frame::Bytes(b) => Some(String::from_utf8_lossy(b).into_owned()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn size_env_and_argv_round_trip() {
        assert_eq!(parse_size("120x40"), Some((120, 40)));
        assert_eq!(parse_size("0x40"), None);
        assert_eq!(parse_size("wide"), None);
        assert_eq!(
            parse_env("A=b=c"),
            Some((String::from("A"), String::from("b=c")))
        );
        assert_eq!(parse_env("=x"), None);
        let spec = PaneSpec {
            socket: PathBuf::from("/s.sock"),
            cwd: Some(PathBuf::from("/w")),
            env: vec![(String::from("TERM"), String::from("xterm-256color"))],
            cols: 100,
            rows: 30,
            argv: vec![String::from("zsh"), String::from("-l")],
        };
        assert_eq!(
            host_argv(&spec),
            [
                "pane-host",
                "--socket",
                "/s.sock",
                "--size",
                "100x30",
                "--cwd",
                "/w",
                "--",
                "zsh",
                "-l"
            ],
            "the environment never goes on the command line"
        );
    }

    #[test]
    fn the_redraw_puts_back_scrollback_screen_modes_and_cursor() {
        let mut m = Model::new(20, 3);
        let Model { term, parser } = &mut m;
        parser.advance(
            term,
            b"one\r\ntwo\r\nthree\r\n\x1b[31mfour\x1b[0m\x1b[?2004h",
        );
        let out = String::from_utf8(redraw(term)).unwrap();
        assert!(out.starts_with("\x1b[0m\x1b[H\x1b[2J\x1b[3J"), "{out:?}");
        let (one, four) = (out.find("one").unwrap(), out.find("four").unwrap());
        assert!(one < four, "scrollback first: {out:?}");
        assert!(out.contains("\x1b[?2004h"), "bracketed paste kept: {out:?}");
        assert!(out.ends_with("\x1b[3;5H"), "cursor after four: {out:?}");

        // A full-screen program's alternate screen, not the shell's rows.
        let mut m = Model::new(20, 3);
        let Model { term, parser } = &mut m;
        parser.advance(term, b"shell\r\n\x1b[?1049h\x1b[Hvim\x1b[?25l");
        let out = String::from_utf8(redraw(term)).unwrap();
        assert!(out.contains("\x1b[?1049h"), "{out:?}");
        assert!(out.contains("vim") && !out.contains("shell"), "{out:?}");
        assert!(out.ends_with("\x1b[?25l"), "hidden cursor: {out:?}");
    }

    #[test]
    fn a_shell_outlives_its_client_and_a_new_client_sees_what_it_missed() {
        let tmp = tempfile::tempdir().unwrap();
        let spec = PaneSpec {
            socket: tmp.path().join("p.sock"),
            cwd: Some(tmp.path().to_path_buf()),
            env: vec![(String::from("PANE_MARK"), String::from("marked"))],
            cols: 60,
            rows: 10,
            argv: vec![
                String::from("sh"),
                String::from("-c"),
                String::from(
                    "echo \"start $PANE_MARK $(pwd)\"; read line; echo \"got:$line\"; sleep 0.5; echo later-output; sleep 60",
                ),
            ],
        };
        let host = {
            let spec = spec.clone();
            std::thread::spawn(move || serve(&spec))
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while !crate::session::is_alive(&spec.socket) {
            assert!(Instant::now() < deadline, "the host never listened");
            std::thread::sleep(Duration::from_millis(10));
        }
        // Output printed before anyone attached is in the first redraw.
        std::thread::sleep(Duration::from_millis(300));
        let first = PaneClient::connect(&spec.socket, 60, 10).unwrap();
        let mut rd = first.try_clone_stream().unwrap();
        let frames = read_until(&mut rd, |f| text(f).contains("start marked"));
        assert!(
            frames
                .iter()
                .any(|f| matches!(f, Frame::Control(Control::PaneInfo { pid }) if *pid > 0)),
            "{frames:?}"
        );
        let dir = tmp.path().canonicalize().unwrap();
        let shown = text(&frames);
        assert!(
            shown.contains(&dir.display().to_string()),
            "ran in its cwd: {shown:?}"
        );

        // Input reaches the shell; its answer comes back live.
        let mut first = first;
        first.write_input(b"abc\r").unwrap();
        read_until(&mut rd, |f| text(f).contains("got:abc"));

        // The client goes away (croft killed); the shell carries on and
        // prints with nobody watching.
        drop(rd);
        drop(first);
        std::thread::sleep(Duration::from_millis(900));
        assert!(
            crate::session::is_alive(&spec.socket),
            "the host outlived its client"
        );

        let mut second = PaneClient::connect(&spec.socket, 60, 10).unwrap();
        let mut rd = second.try_clone_stream().unwrap();
        let frames = read_until(&mut rd, |f| text(f).contains("later-output"));
        let shown = text(&frames);
        assert!(
            shown.contains("got:abc"),
            "earlier output redrawn: {shown:?}"
        );

        // Closing the pane ends the shell, and the host with it.
        second.kill().unwrap();
        let frames = read_until(&mut rd, |f| {
            f.iter()
                .any(|f| matches!(f, Frame::Control(Control::Exit { .. })))
        });
        assert!(!frames.is_empty());
        host.join().unwrap().unwrap();
        assert!(!spec.socket.exists(), "the socket is removed");
    }
}
