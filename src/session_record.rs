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
/// goes away, or `until` passes (a test's bound; `None` in the CLI).
pub(crate) fn record_to(
    socket: &Path,
    out: &mut impl Write,
    title: &str,
    until: Option<std::time::Instant>,
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
    if until.is_some() {
        stream.set_read_timeout(Some(std::time::Duration::from_millis(50)))?;
    }
    let mut state = CastState::new(title);
    let mut reader = crate::session_host::FrameReader::default();
    let started = std::time::Instant::now();
    let mut buf = [0u8; 64 * 1024];
    loop {
        if until.is_some_and(|u| std::time::Instant::now() >= u) {
            return Ok(());
        }
        let n = match stream.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
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

/// Where a recorder for `socket` leaves its pid, for `croft record --stop`.
fn pid_path(socket: &Path) -> PathBuf {
    socket.with_extension("rec.pid")
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
        let pid = std::fs::read_to_string(&pidfile)
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .with_context(|| format!("no recording is running for {}", workspace.display()))?;
        let status = std::process::Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status()
            .context("signalling the recorder")?;
        let _ = std::fs::remove_file(&pidfile);
        anyhow::ensure!(status.success(), "the recorder (pid {pid}) is not running");
        println!("Stopped the recording of {}", workspace.display());
        return Ok(());
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
    let mut file =
        std::fs::File::create(&out).with_context(|| format!("creating {}", out.display()))?;
    std::fs::write(&pidfile, std::process::id().to_string())?;
    eprintln!(
        "Recording {} to {} (stop with `croft record --stop` or Ctrl-C)",
        workspace.display(),
        out.display()
    );
    let title = workspace
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let result = record_to(&socket, &mut file, &title, None);
    let _ = std::fs::remove_file(&pidfile);
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
}
