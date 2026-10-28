//! `croft record` (#356): write a persistent session to an asciicast v2 file.
//!
//! The recorder is an ordinary session-host client that says `Hello` with a
//! 0x0 size. The host already treats that as a pure observer: it never shrinks
//! the shared PTY, never takes write control, and its input is dropped. What it
//! receives is exactly what every attached client paints, the full-colour byte
//! stream, so the cast plays back the session as the participants saw it
//! (unlike the in-app terminal recorder, which samples one pane's text).
//!
//! The size comes from the roster: the host sizes the PTY to the smallest
//! participant, and a `Presence` frame carries everyone's size, so the
//! recorder recomputes the same minimum and writes an `r` event when it moves.
//! Attaching makes the host repaint (its SIGWINCH jiggle), so the cast opens on
//! the current screen rather than blank.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};

use crate::asciicast::{Event, Recorder};
use crate::session_host::{Control, Frame, Participant};

/// Turns session-host frames into asciicast lines. Pure: the caller supplies
/// the timestamp and writes the lines, so the rules are testable without a
/// socket.
pub(crate) struct CastState {
    title: String,
    recorder: Option<Recorder>,
    size: Option<(u16, u16)>,
    /// Output that arrived before any size was known, flushed after the
    /// header (a cast cannot start with events before its header).
    early: Vec<u8>,
    /// The tail of a UTF-8 sequence split across two frames.
    utf8_tail: Vec<u8>,
}

impl CastState {
    pub(crate) fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            recorder: None,
            size: None,
            early: Vec::new(),
            utf8_tail: Vec::new(),
        }
    }

    /// The asciicast lines `frame` produces at `at` seconds, and whether the
    /// recording is over (the session exited).
    pub(crate) fn on_frame(&mut self, frame: Frame, at: f64) -> (Vec<String>, bool) {
        let mut lines = Vec::new();
        match frame {
            Frame::Control(Control::Presence { participants }) => {
                self.on_roster(&participants, at, &mut lines);
            }
            Frame::Bytes(data) => match self.recorder.as_mut() {
                Some(rec) => {
                    self.utf8_tail.extend_from_slice(&data);
                    let text = take_utf8(&mut self.utf8_tail);
                    if !text.is_empty() {
                        lines.push(rec.line(&Event::Output { at, data: text }));
                    }
                }
                None => self.early.extend_from_slice(&data),
            },
            Frame::Control(Control::Exit { .. }) => return (lines, true),
            Frame::Control(_) => {}
        }
        (lines, false)
    }

    fn on_roster(&mut self, participants: &[Participant], at: f64, lines: &mut Vec<String>) {
        let Some(size) =
            crate::session_host::min_winsize(participants.iter().map(|p| (p.cols, p.rows)))
        else {
            return;
        };
        if self.size == Some(size) {
            return;
        }
        self.size = Some(size);
        match self.recorder.as_mut() {
            Some(rec) => lines.push(rec.line(&Event::Resize {
                at,
                cols: size.0,
                rows: size.1,
            })),
            None => {
                let mut rec = Recorder::new(size.0, size.1);
                lines.push(rec.header(Some(&self.title)));
                let mut early = std::mem::take(&mut self.early);
                let text = take_utf8(&mut early);
                self.utf8_tail = early;
                if !text.is_empty() {
                    lines.push(rec.line(&Event::Output { at, data: text }));
                }
                self.recorder = Some(rec);
            }
        }
    }
}

/// Take the longest valid UTF-8 prefix out of `buf`, leaving an incomplete
/// trailing sequence for the next frame. Bytes that can never become valid
/// are replaced (U+FFFD) rather than stalling the stream.
fn take_utf8(buf: &mut Vec<u8>) -> String {
    let mut out = String::new();
    loop {
        match std::str::from_utf8(buf) {
            Ok(s) => {
                out.push_str(s);
                buf.clear();
                return out;
            }
            Err(e) => {
                let valid = e.valid_up_to();
                out.push_str(std::str::from_utf8(&buf[..valid]).expect("valid prefix"));
                match e.error_len() {
                    // An incomplete sequence at the end: keep it for later.
                    None => {
                        buf.drain(..valid);
                        return out;
                    }
                    Some(bad) => {
                        out.push('\u{fffd}');
                        buf.drain(..valid + bad);
                    }
                }
            }
        }
    }
}

/// Connect to the session host at `socket` as a pure observer and write the
/// session to `out` as asciicast lines until the session exits, the host
/// goes away, `stop` is set, or `until` passes (a test's bound; `None` in
/// the CLI).
pub(crate) fn record_to(
    socket: &Path,
    out: &mut impl Write,
    title: &str,
    until: Option<std::time::Instant>,
    stop: &AtomicBool,
) -> Result<()> {
    let mut stream = std::os::unix::net::UnixStream::connect(socket)
        .with_context(|| format!("connecting to {}", socket.display()))?;
    let hello = Control::Hello {
        name: String::from("recorder"),
        cols: 0,
        rows: 0,
        version: env!("CARGO_PKG_VERSION").to_string(),
        client_id: format!("recorder-{}", std::process::id()),
    };
    stream.write_all(&crate::session_host::encode_control_frame(&hello))?;
    // Ctrl-C and `--stop` only set `stop`, from a signal handler, so a read
    // must not block for longer than it takes to look again.
    stream.set_read_timeout(Some(std::time::Duration::from_millis(100)))?;
    let mut state = CastState::new(title);
    let mut reader = crate::session_host::FrameReader::default();
    let started = std::time::Instant::now();
    let mut buf = [0u8; 64 * 1024];
    loop {
        if stop.load(Ordering::SeqCst) || until.is_some_and(|u| std::time::Instant::now() >= u) {
            return Ok(());
        }
        let n = match stream.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) =>
            {
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        for frame in reader.push(&buf[..n]) {
            let (lines, done) = state.on_frame(frame, started.elapsed().as_secs_f64());
            for line in lines {
                // Line by line, so a stopped recorder leaves a valid cast.
                writeln!(out, "{line}")?;
            }
            out.flush()?;
            if done {
                return Ok(());
            }
        }
    }
}

/// Where a recorder for `socket` leaves its pidfile, for `croft record
/// --stop` and for a second `croft record` to find.
fn pid_path(socket: &Path) -> PathBuf {
    socket.with_extension("rec.pid")
}

/// The recorder a pidfile names (#1058). A pid alone is not enough to
/// signal: a recorder killed before it could remove its pidfile leaves the
/// pid to be recycled, and `--stop` would then terminate whatever unrelated
/// process holds it now.
#[derive(Debug, PartialEq)]
struct Owner {
    pid: u32,
    /// When the recorder started, in seconds since the epoch: with the pid,
    /// one process.
    started: u64,
    /// The cast it writes, named when a second recorder is refused.
    out: PathBuf,
}

impl Owner {
    /// This process, recording to `out`.
    fn me(out: &Path) -> Self {
        let pid = std::process::id();
        Self {
            pid,
            started: crate::hot_exit::start_time_of(pid).unwrap_or(0),
            out: std::path::absolute(out).unwrap_or_else(|_| out.to_path_buf()),
        }
    }

    /// The pidfile's text: the pid, the start time and the cast, a line each.
    fn to_text(&self) -> String {
        format!("{}\n{}\n{}\n", self.pid, self.started, self.out.display())
    }

    /// `None` for anything [`Owner::to_text`] did not write, such as the
    /// bare pid an older croft left.
    fn parse(text: &str) -> Option<Self> {
        let mut lines = text.splitn(3, '\n');
        Some(Self {
            pid: lines.next()?.parse().ok()?,
            started: lines.next()?.parse().ok()?,
            out: PathBuf::from(lines.next()?.strip_suffix('\n')?),
        })
    }

    /// Whether this recorder still runs: its pid holds a process that
    /// started when it did and is croft running `record`. A recycled pid
    /// fails the start time; the command line catches one recycled within
    /// the same second.
    fn runs(&self) -> bool {
        crate::hot_exit::start_time_of(self.pid) == Some(self.started)
            && i32::try_from(self.pid)
                .ok()
                .and_then(crate::widgets::terminal::process_cmdline)
                .is_some_and(|cmd| is_recorder(&cmd))
    }
}

/// Whether `cmd`, a process's argv, is croft running `record`.
fn is_recorder(cmd: &[String]) -> bool {
    let Some((program, args)) = cmd.split_first() else {
        return false;
    };
    Path::new(program)
        .file_name()
        .is_some_and(|name| name.to_string_lossy().starts_with("croft"))
        && args.iter().any(|a| a == "record")
}

/// The recorder `pidfile` names, while it still runs.
fn running_owner(pidfile: &Path) -> Option<Owner> {
    let text = std::fs::read_to_string(pidfile).ok()?;
    Owner::parse(&text).filter(Owner::runs)
}

/// Write `me` to `pidfile`, refusing while another recorder of the same
/// session runs: the second would take over the pidfile, and `--stop` could
/// no longer reach the first. A pidfile whose recorder is gone is replaced.
/// An exclusive lock beside it makes that check and replacement one step:
/// two recorders starting together each saw the stale file as theirs, and
/// the second removed the first's new claim.
fn claim(pidfile: &Path, me: &Owner, workspace: &Path) -> Result<()> {
    use std::os::fd::AsRawFd;
    let lock_path = pidfile.with_extension("lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("creating {}", lock_path.display()))?;
    // Released when `lock` closes, at the end of this function.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("locking {}", lock_path.display()));
    }
    let create = || {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(pidfile)
    };
    let mut file = match create() {
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            if let Some(other) = running_owner(pidfile) {
                anyhow::bail!(
                    "already recording {} to {}; stop it with `croft record --stop`",
                    workspace.display(),
                    other.out.display()
                );
            }
            let _ = std::fs::remove_file(pidfile);
            create()
        }
        other => other,
    }
    .with_context(|| format!("creating {}", pidfile.display()))?;
    file.write_all(me.to_text().as_bytes())?;
    Ok(())
}

/// Remove `pidfile` if it still names this process.
fn release(pidfile: &Path) {
    let text = std::fs::read_to_string(pidfile).unwrap_or_default();
    if Owner::parse(&text).is_some_and(|o| o.pid == std::process::id()) {
        let _ = std::fs::remove_file(pidfile);
    }
}

/// `croft record --stop`: SIGTERM the recorder `pidfile` names, only while
/// it still runs, then wait for it to finish its cast and exit. A pidfile
/// whose recorder is gone is removed, and nothing is signalled.
fn stop_recorder(pidfile: &Path, workspace: &Path) -> Result<()> {
    let Some(owner) = running_owner(pidfile) else {
        let _ = std::fs::remove_file(pidfile);
        anyhow::bail!("no recording is running for {}", workspace.display());
    };
    // The recorder catches it and finishes; the default action would make
    // `croft record` exit 143 (#1058).
    unsafe { libc::kill(owner.pid as libc::pid_t, libc::SIGTERM) };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while owner.runs() {
        anyhow::ensure!(
            std::time::Instant::now() < deadline,
            "the recorder (pid {}) did not stop",
            owner.pid
        );
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    println!("Stopped the recording of {}", workspace.display());
    Ok(())
}

/// Set by SIGINT or SIGTERM in `croft record`: the recording ends.
static STOP: AtomicBool = AtomicBool::new(false);

/// End the recording on Ctrl-C (SIGINT) and `croft record --stop` (SIGTERM)
/// by setting [`STOP`], rather than by the default action killing the
/// process: the pidfile is then removed and `croft record` exits 0. A
/// SIGINT the launching shell ignores (a script's background job) stays
/// ignored.
fn stop_on_signals() {
    extern "C" fn on_signal(_: libc::c_int) {
        STOP.store(true, Ordering::SeqCst);
    }
    for sig in [libc::SIGINT, libc::SIGTERM] {
        // SAFETY: sigaction(2) on zeroed structs; the handler only stores
        // to an atomic, which is async-signal-safe.
        unsafe {
            let mut old: libc::sigaction = std::mem::zeroed();
            if sig == libc::SIGINT
                && libc::sigaction(sig, std::ptr::null(), &mut old) == 0
                && old.sa_sigaction == libc::SIG_IGN
            {
                continue;
            }
            // No SA_RESTART: a read blocked when the signal lands returns
            // at once, and the loop sees `STOP`.
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = on_signal as *const () as libc::sighandler_t;
            libc::sigaction(sig, &action, std::ptr::null_mut());
        }
    }
}

/// `croft record`: record the session running for `workspace` to `out`, or
/// stop the recorder that is.
pub fn record(workspace: Option<PathBuf>, out: Option<PathBuf>, stop: bool) -> Result<()> {
    let workspace = match workspace {
        Some(p) => p,
        None => std::env::current_dir().context("resolving workspace path")?,
    }
    .canonicalize()
    .context("resolving workspace path")?;
    let socket = crate::session::mux_socket_path(&workspace);
    let pidfile = pid_path(&socket);
    if stop {
        return stop_recorder(&pidfile, &workspace);
    }
    if !socket.exists() {
        anyhow::bail!(
            "no croft session is running for {}; start one with `croft attach`",
            workspace.display()
        );
    }
    let out = out.unwrap_or_else(|| {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        workspace.join(format!("croft-session-{secs}.cast"))
    });
    // Before the pidfile exists, so no Ctrl-C can leave it behind.
    stop_on_signals();
    // Before the cast is created: a refused second recorder must not
    // truncate the first one's file.
    claim(&pidfile, &Owner::me(&out), &workspace)?;
    let title = workspace
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let result = std::fs::File::create(&out)
        .with_context(|| format!("creating {}", out.display()))
        .and_then(|mut file| {
            eprintln!(
                "Recording {} to {} (stop with `croft record --stop` or Ctrl-C)",
                workspace.display(),
                out.display()
            );
            record_to(&socket, &mut file, &title, None, &STOP)
        });
    release(&pidfile);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roster(sizes: &[(u16, u16)]) -> Frame {
        Frame::Control(Control::Presence {
            participants: sizes
                .iter()
                .enumerate()
                .map(|(i, &(cols, rows))| Participant {
                    id: i as u64,
                    name: format!("p{i}"),
                    cols,
                    rows,
                    control: i == 0,
                    version: String::new(),
                })
                .collect(),
        })
    }

    /// The header waits for a size, output before it is kept rather than
    /// lost, and the recorder's own 0x0 does not count toward the size.
    #[test]
    fn the_header_waits_for_a_size_and_early_output_is_kept() {
        let mut s = CastState::new("ws");
        let (lines, _) = s.on_frame(Frame::Bytes(b"hi ".to_vec()), 0.0);
        assert!(lines.is_empty(), "no header yet, so nothing to write");
        let (lines, _) = s.on_frame(roster(&[(100, 30), (80, 24), (0, 0)]), 0.1);
        assert_eq!(lines.len(), 2);
        assert!(
            lines[0].contains("\"width\":80") && lines[0].contains("\"height\":24"),
            "{}",
            lines[0]
        );
        assert!(
            lines[1].contains("\"o\"") && lines[1].contains("hi "),
            "{}",
            lines[1]
        );
    }

    #[test]
    fn a_roster_size_change_writes_a_resize_event_once() {
        let mut s = CastState::new("ws");
        s.on_frame(roster(&[(80, 24)]), 0.0);
        let (lines, _) = s.on_frame(roster(&[(80, 24)]), 0.5);
        assert!(lines.is_empty(), "same size, no event");
        let (lines, _) = s.on_frame(roster(&[(120, 40)]), 1.0);
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0].contains("\"r\"") && lines[0].contains("120x40"),
            "{}",
            lines[0]
        );
    }

    /// A UTF-8 character split across two frames arrives whole, not as two
    /// replacement characters.
    #[test]
    fn a_character_split_across_frames_is_written_whole() {
        let mut s = CastState::new("ws");
        s.on_frame(roster(&[(80, 24)]), 0.0);
        let bytes = "é".as_bytes();
        let (first, _) = s.on_frame(Frame::Bytes(vec![b'a', bytes[0]]), 0.1);
        assert_eq!(first.len(), 1);
        assert!(
            first[0].contains("\"a\""),
            "only the complete part: {}",
            first[0]
        );
        let (second, _) = s.on_frame(Frame::Bytes(vec![bytes[1], b'b']), 0.2);
        assert!(second[0].contains("éb"), "{}", second[0]);
    }

    #[test]
    fn an_exit_ends_the_recording() {
        let mut s = CastState::new("ws");
        s.on_frame(roster(&[(80, 24)]), 0.0);
        let (_, done) = s.on_frame(Frame::Control(Control::Exit { code: 0 }), 1.0);
        assert!(done);
    }

    #[test]
    fn take_utf8_replaces_bytes_that_can_never_be_valid() {
        let mut buf = vec![b'x', 0xff, b'y'];
        assert_eq!(take_utf8(&mut buf), "x\u{fffd}y");
        assert!(buf.is_empty());
    }

    /// #1058: Ctrl-C and `--stop` only set the flag. The loop sees it while
    /// the session is quiet and returns, the cast written so far intact.
    #[test]
    fn record_to_returns_once_stop_is_set() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("s.mux.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        // A host that paints once, then goes quiet with the connection open.
        let host = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let Frame::Control(presence) = roster(&[(80, 24)]) else {
                unreachable!()
            };
            conn.write_all(&crate::session_host::encode_control_frame(&presence))
                .unwrap();
            conn.write_all(&crate::session_host::encode_bytes_frame(b"hi"))
                .unwrap();
            conn
        });
        let stop = AtomicBool::new(false);
        let mut cast = Vec::new();
        let began = std::time::Instant::now();
        std::thread::scope(|s| {
            s.spawn(|| {
                std::thread::sleep(std::time::Duration::from_millis(300));
                stop.store(true, Ordering::SeqCst);
            });
            let bound = began + std::time::Duration::from_secs(30);
            record_to(&socket, &mut cast, "ws", Some(bound), &stop).expect("record_to");
        });
        assert!(
            began.elapsed() < std::time::Duration::from_secs(5),
            "returned on the flag, not at the test's bound"
        );
        let text = String::from_utf8(cast).unwrap();
        assert_eq!(text.lines().count(), 2, "{text}");
        assert!(
            text.contains("\"width\":80") && text.contains("hi"),
            "{text}"
        );
        drop(host.join().unwrap());
    }

    /// A stand-in for a running `croft record`: a shell whose argv reads
    /// `croft -c "read x" sh record`, waiting on the stdin the caller holds.
    fn fake_recorder() -> (std::process::Child, Owner) {
        use std::os::unix::process::CommandExt;
        let child = std::process::Command::new("sh")
            .arg0("croft")
            .args(["-c", "read x", "sh", "record"])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let pid = child.id();
        let owner = Owner {
            pid,
            started: crate::hot_exit::start_time_of(pid).expect("a child's start time"),
            out: PathBuf::from("/tmp/first.cast"),
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !owner.runs() {
            assert!(
                std::time::Instant::now() < deadline,
                "the stand-in never ran"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        (child, owner)
    }

    /// #1058: a pidfile left behind names a pid that may since belong to
    /// any process. `--stop` signals none of these, and removes the file.
    #[test]
    fn stop_never_signals_a_pid_that_is_not_the_recorder() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("s.mux.rec.pid");
        let mut sleep = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = sleep.id();
        let started = crate::hot_exit::start_time_of(pid).expect("a child's start time");
        for text in [
            // An older croft's bare pid.
            pid.to_string(),
            // The pid recycled after the recorder died.
            format!("{pid}\n{}\n/tmp/x.cast\n", started - 1),
            // The right start time, but `sleep` is not croft recording.
            format!("{pid}\n{started}\n/tmp/x.cast\n"),
        ] {
            std::fs::write(&pidfile, &text).unwrap();
            let err = stop_recorder(&pidfile, dir.path()).unwrap_err();
            assert!(err.to_string().contains("no recording is running"), "{err}");
            assert!(!pidfile.exists(), "the stale pidfile stays: {text:?}");
            assert!(sleep.try_wait().unwrap().is_none(), "{text:?} killed it");
        }
        sleep.kill().unwrap();
        sleep.wait().unwrap();
    }

    #[test]
    fn stop_removes_the_pidfile_of_a_recorder_that_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("s.mux.rec.pid");
        // 999999999 never exists.
        std::fs::write(&pidfile, "999999999\n1700000000\n/tmp/x.cast\n").unwrap();
        let err = stop_recorder(&pidfile, dir.path()).unwrap_err();
        assert!(err.to_string().contains("no recording is running"), "{err}");
        assert!(!pidfile.exists());
    }

    /// #1058: a second recorder for the session is refused while the first
    /// runs, rather than taking the pidfile over and leaving the first
    /// unstoppable. `--stop` ends the first, and then a new one may start.
    #[test]
    fn a_second_recorder_is_refused_until_stop_ends_the_first() {
        use std::os::unix::process::ExitStatusExt;
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("s.mux.rec.pid");
        let (mut first, owner) = fake_recorder();
        // Held here: `wait` would close it, and `read` would end the shell.
        let _stdin = first.stdin.take();
        std::fs::write(&pidfile, owner.to_text()).unwrap();
        let me = Owner::me(&dir.path().join("second.cast"));
        let err = claim(&pidfile, &me, dir.path()).unwrap_err().to_string();
        assert!(
            err.contains("already recording") && err.contains("/tmp/first.cast"),
            "{err}"
        );
        assert_eq!(std::fs::read_to_string(&pidfile).unwrap(), owner.to_text());

        let reaper = std::thread::spawn(move || first.wait().unwrap());
        stop_recorder(&pidfile, dir.path()).expect("the running recorder stops");
        assert_eq!(reaper.join().unwrap().signal(), Some(libc::SIGTERM));

        claim(&pidfile, &me, dir.path()).expect("the stale pidfile is replaced");
        let text = std::fs::read_to_string(&pidfile).unwrap();
        assert_eq!(Owner::parse(&text), Some(me));
        release(&pidfile);
        assert!(!pidfile.exists());
    }
}
