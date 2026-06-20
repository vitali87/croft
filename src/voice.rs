//! Push-to-talk voice input for the Termux on-screen keyboard.
//!
//! croft's OSK replaces Android's native soft keyboard (Termux mouse tracking
//! suppresses it), which also removes the keyboard's mic button. This module
//! restores voice dictation by delegating to `termux-speech-to-text` from the
//! `termux-api` package, which drives Android's system `SpeechRecognizer` (the
//! same engine Gboard's mic uses). croft never captures audio or ships a model
//! itself, mirroring how it delegates directory ranking to the host `zoxide`.
//!
//! The mic is a tap, not push-to-talk: Termux turns any finger hold into its
//! own text-selection gesture (hardcoded in TerminalView, unsuppressible from a
//! TUI), so a sustained press can never reach croft. A tap starts a session;
//! the transcript only exists once Android finalizes recognition at
//! end-of-speech (silence, fired by `onResults` in its `SpeechRecognizer`), so
//! croft must let the user pause rather than kill to "finish" - killing
//! preempts the result. A second tap therefore cancels (kills the process
//! group), it is never the path to the transcript. Final-only mode is used (no
//! `-p`): the wrapper's `tail -1` yields exactly the final line at EOF, sparing
//! croft the upstream progressive-output buffering bug (termux-api-package#137).

use std::io::{BufRead, Read};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;

use crate::lsp::log_file;

/// The Termux:API CLI that fronts Android's `SpeechRecognizer`.
const BINARY: &str = "termux-speech-to-text";

/// Installed on Termux from the bootstrap's `pkg`; provides `BINARY` in
/// `$PREFIX/bin` (always on PATH). The Termux:API *app* (APK) must also be
/// present for the command to actually reach Android; a missing app surfaces as
/// a runtime failure, not something `pkg` can fix.
const TERMUX_INSTALL_PKG: &str = "pkg install -y termux-api";

/// Whether the host can do voice input at all, and if not, why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    /// Termux with `termux-speech-to-text` present: good to go.
    Ready,
    /// Not running under Termux; voice input is Termux-only.
    NeedsTermux,
    /// Termux, but the `termux-api` package is not installed yet.
    NeedsApi,
}

/// Outcome of one push-to-talk session, delivered to the app's per-tick drain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceMsg {
    /// Recognized text to inject into the focused pane.
    Transcript(String),
    /// The session ended without recognizing anything.
    Empty,
    /// The session could not run (spawn error, recognizer missing, ...).
    Failed(String),
}

/// True while a recognition session is live; drives the mic key's armed glow
/// and the status-line "Listening…" hint. A process-global flag so the OSK
/// render path can read it without app plumbing (one relaxed atomic load).
static LISTENING: AtomicBool = AtomicBool::new(false);

pub fn is_listening() -> bool {
    LISTENING.load(Ordering::Relaxed)
}

fn set_listening(on: bool) {
    LISTENING.store(on, Ordering::Relaxed);
}

/// Lifecycle of the one background `termux-api` install attempt per process,
/// surfaced so the mic key can tell the user to wait / retry. Mirrors the
/// zoxide installer's state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallState {
    Idle = 0,
    Running = 1,
    Done = 2,
    Failed = 3,
}

static INSTALL_STATE: AtomicU8 = AtomicU8::new(0);

fn set_install_state(state: InstallState) {
    INSTALL_STATE.store(state as u8, Ordering::Relaxed);
}

pub fn install_state() -> InstallState {
    match INSTALL_STATE.load(Ordering::Relaxed) {
        1 => InstallState::Running,
        2 => InstallState::Done,
        3 => InstallState::Failed,
        _ => InstallState::Idle,
    }
}

/// `$PREFIX/bin/<BINARY>` existence (cheap, no subprocess), with a `command -v`
/// fallback for non-standard layouts.
fn binary_present() -> bool {
    if let Some(prefix) = std::env::var_os("PREFIX")
        && PathBuf::from(prefix).join("bin").join(BINARY).exists()
    {
        return true;
    }
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {BINARY}"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Whether voice input is usable here, and why not when it isn't.
pub fn availability() -> Availability {
    if !crate::iterm2_inline::detect_termux() {
        Availability::NeedsTermux
    } else if binary_present() {
        Availability::Ready
    } else {
        Availability::NeedsApi
    }
}

/// Install `termux-api` on a detached thread so the mic press never blocks the
/// UI; the user retries the mic once it lands. Best-effort and idempotent: a
/// second press while `Running` is a no-op at the call site.
pub fn ensure_api_installed_in_background() {
    if matches!(install_state(), InstallState::Running) {
        return;
    }
    set_install_state(InstallState::Running);
    thread::spawn(|| {
        match Command::new("sh")
            .arg("-c")
            .arg(TERMUX_INSTALL_PKG)
            .output()
        {
            Ok(out) if out.status.success() => {
                log_file::log(&format!("voice: `{TERMUX_INSTALL_PKG}` succeeded"))
            }
            Ok(out) => log_file::log(&format!(
                "voice: `{TERMUX_INSTALL_PKG}` exited {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            )),
            Err(e) => log_file::log(&format!(
                "voice: `{TERMUX_INSTALL_PKG}` could not start: {e}"
            )),
        }
        if binary_present() {
            set_install_state(InstallState::Done);
        } else {
            set_install_state(InstallState::Failed);
        }
    });
}

/// Handle to a live recognition session: lets the app kill the client process
/// on a second mic tap. Dropping it without `stop()` leaves the session to
/// finish on Android's own end-of-speech detection.
pub struct VoiceHandle {
    child: Arc<Mutex<Option<Child>>>,
}

impl VoiceHandle {
    /// Stop recognition now (a second mic tap). `termux-speech-to-text` is a
    /// wrapper script whose `termux-api` child inherits the stdout pipe, so
    /// killing the script alone leaves that child holding the pipe open and the
    /// reader never sees EOF. `start` put the tree in its own process group
    /// (setsid), so signal the whole group: that takes down `termux-api` too,
    /// the pipe closes, and the reader finalizes and reports the transcript.
    pub fn stop(&self) {
        if let Ok(mut guard) = self.child.lock()
            && let Some(child) = guard.as_mut()
        {
            #[cfg(unix)]
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.kill();
        }
    }
}

/// Start a recognition session. Spawns `termux-speech-to-text -p`, reads its
/// streamed hypotheses on a background thread, and on stream end sends the last
/// non-empty line as the transcript over `tx`. Returns a handle the app keeps
/// to stop the session on a second mic tap. On a spawn failure the session
/// never opens and `VoiceMsg::Failed` is sent.
pub fn start(tx: Sender<VoiceMsg>) -> VoiceHandle {
    set_listening(true);
    // Final-only mode (no `-p`): the wrapper pipes the recognizer through
    // `tail -1`, emitting exactly one line - the final transcript - when the
    // recognizer finalizes on end-of-speech (silence). The transcript only
    // exists after that finalize, so croft must NOT kill the session to "stop"
    // it (that preempts the result); the user pauses and it commits. `-p` is
    // avoided because its progressive output is buffered upstream (#137) and
    // would yield partial or empty text. stderr is captured for diagnosis.
    let mut cmd = Command::new(BINARY);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Run the wrapper script in its own session/process group so `stop` can
    // signal the whole tree (script + `termux-api`) as a unit; otherwise the
    // `termux-api` child survives, holds the stdout pipe, and the reader hangs.
    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            set_listening(false);
            log_file::log(&format!("voice: could not start `{BINARY}`: {e}"));
            let _ = tx.send(VoiceMsg::Failed(format!("voice input unavailable: {e}")));
            return VoiceHandle {
                child: Arc::new(Mutex::new(None)),
            };
        }
    };
    let stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let shared: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(Some(child)));
    let reader_shared = Arc::clone(&shared);
    thread::spawn(move || {
        let mut latest = String::new();
        if let Some(stdout) = stdout {
            // Final-only mode emits a single line (the transcript) at EOF; keep
            // the last non-empty line either way.
            for line in std::io::BufReader::new(stdout)
                .lines()
                .map_while(Result::ok)
            {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    latest = trimmed.to_string();
                }
            }
        }
        // Drain stderr and reap, so a recognizer-side failure (no Termux:API
        // app, denied mic permission, no speech-recognition service) leaves a
        // real signal in the log instead of a silent "No speech detected".
        let mut err = String::new();
        if let Some(stderr) = stderr.as_mut() {
            let _ = stderr.read_to_string(&mut err);
        }
        let status = if let Ok(mut guard) = reader_shared.lock()
            && let Some(child) = guard.as_mut()
        {
            child.wait().ok()
        } else {
            None
        };
        set_listening(false);
        log_file::log(&format!(
            "voice: session ended status={status:?} transcript_chars={} stderr={:?}",
            latest.chars().count(),
            err.trim()
        ));
        let msg = if latest.is_empty() {
            VoiceMsg::Empty
        } else {
            VoiceMsg::Transcript(latest)
        };
        let _ = tx.send(msg);
    });
    VoiceHandle { child: shared }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_state_round_trips_through_the_atomic() {
        set_install_state(InstallState::Running);
        assert_eq!(install_state(), InstallState::Running);
        set_install_state(InstallState::Done);
        assert_eq!(install_state(), InstallState::Done);
        set_install_state(InstallState::Idle);
        assert_eq!(install_state(), InstallState::Idle);
    }

    #[test]
    fn listening_flag_round_trips() {
        set_listening(true);
        assert!(is_listening());
        set_listening(false);
        assert!(!is_listening());
    }
}
