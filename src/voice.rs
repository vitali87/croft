//! Voice input for the Termux on-screen keyboard.
//!
//! croft's OSK replaces Android's native soft keyboard (Termux mouse tracking
//! suppresses it), which also removes the keyboard's mic button. This module
//! restores voice dictation by delegating to the `termux-api` package, which
//! drives Android's system `SpeechRecognizer` (the same engine Gboard's mic
//! uses). croft never captures audio or ships a model itself, mirroring how it
//! delegates directory ranking to the host `zoxide`.
//!
//! It calls `termux-dialog speech`, NOT `termux-speech-to-text`, and the
//! difference is the entire fix. Both wrap the same recognizer, but their
//! `RecognitionListener`s diverge on one line: the `termux-speech-to-text`
//! service closes its output stream on `onEndOfSpeech` (which fires the instant
//! you pause), and that races *ahead* of `onResults` - so the final, best
//! transcript is discarded and the caller sees at most buffered partials, or
//! nothing when the recognizer emits no partials (the "No speech detected" /
//! hang we hit). `termux-dialog speech`'s `onEndOfSpeech` is a no-op; it returns
//! `onResults` - the final transcript - as JSON `{"code":..,"text":".."}` (with
//! an `"error"` field on failure) and exits. It also requests `RECORD_AUDIO`
//! itself, so a denied mic surfaces as a real error rather than silence.
//!
//! The mic is a tap, not push-to-talk: Termux turns any finger hold into its
//! own text-selection gesture (hardcoded in TerminalView, unsuppressible from a
//! TUI), so a sustained press can never reach croft. A tap opens the system
//! speech dialog; speaking and then pausing commits the transcript, and the
//! dialog owns cancellation (its own button, or a second mic tap kills it).

use std::io::Read;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;

use crate::lsp::log_file;

/// The Termux:API CLI that fronts Android's `SpeechRecognizer`. The `speech`
/// dialog widget (`termux-dialog speech`) returns the final `onResults`
/// transcript as JSON; the `termux-speech-to-text` service discards it (see the
/// module doc), so `termux-dialog` is the binary croft drives.
const BINARY: &str = "termux-dialog";

/// `termux-dialog` widget that opens the system speech recognizer.
const SPEECH_WIDGET: &str = "speech";

/// Shown as the dialog's title while it listens.
const DIALOG_TITLE: &str = "croft voice input";

/// JSON keys in `termux-dialog`'s result object.
const JSON_TEXT: &str = "text";
const JSON_ERROR: &str = "error";

/// The recognizer error code Android raises when Termux:API lacks the mic
/// permission; mapped to an actionable hint instead of a raw code.
const ERR_NO_MIC_PERMISSION: &str = "ERROR_INSUFFICIENT_PERMISSIONS";

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
/// and the status-line "Listening" hint. A process-global flag so the OSK
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
    /// Cancel the dialog now (a second mic tap). `termux-dialog` is a wrapper
    /// script whose `termux-api` child inherits the stdout pipe, so killing the
    /// script alone leaves that child holding the pipe open and the reader never
    /// sees EOF. `start` put the tree in its own process group (setsid), so
    /// signal the whole group: that takes down `termux-api` too, the pipe
    /// closes, and the reader finalizes. The happy path does not need this - the
    /// dialog returns on its own when you stop speaking - it is only the escape
    /// hatch for an abandoned session.
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

/// Start a recognition session. Spawns `termux-dialog speech`, reads its single
/// JSON result object on a background thread, and sends the recognized text as
/// the transcript over `tx` (or `Failed`/`Empty` on a recognizer error or
/// nothing heard). Returns a handle the app keeps to cancel on a second mic tap.
/// On a spawn failure the session never opens and `VoiceMsg::Failed` is sent.
pub fn start(tx: Sender<VoiceMsg>) -> VoiceHandle {
    set_listening(true);
    // `termux-dialog speech` opens the system speech dialog and prints one JSON
    // object - `{"code":..,"text":".."[, "error":".."]}` - when the recognizer
    // finalizes (`onResults`) or fails (`onError`), then exits. Unlike the
    // `termux-speech-to-text` service it does NOT close on `onEndOfSpeech`, so
    // the final transcript is actually delivered (see the module doc). stderr is
    // captured for diagnosis.
    let mut cmd = Command::new(BINARY);
    cmd.arg(SPEECH_WIDGET)
        .arg("-t")
        .arg(DIALOG_TITLE)
        .stdin(Stdio::null())
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
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let shared: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(Some(child)));
    let reader_shared = Arc::clone(&shared);
    thread::spawn(move || {
        // The dialog prints a single small JSON object at end-of-speech; read it
        // whole rather than line-by-line.
        let mut json = String::new();
        if let Some(stdout) = stdout.as_mut() {
            let _ = stdout.read_to_string(&mut json);
        }
        // Drain stderr and reap, so a recognizer-side failure (no Termux:API
        // app, no speech-recognition service) leaves a real signal in the log.
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
        let parsed = serde_json::from_str::<serde_json::Value>(json.trim()).ok();
        let field = |key: &str| -> String {
            parsed
                .as_ref()
                .and_then(|v| v.get(key))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string()
        };
        let text = field(JSON_TEXT);
        let rec_err = field(JSON_ERROR);
        log_file::log(&format!(
            "voice: dialog ended status={status:?} text_chars={} error={rec_err:?} stderr={:?}",
            text.chars().count(),
            err.trim()
        ));
        let msg = if !text.is_empty() {
            VoiceMsg::Transcript(text)
        } else if rec_err == ERR_NO_MIC_PERMISSION {
            VoiceMsg::Failed(String::from(
                "Grant Termux:API microphone permission, then tap the mic again",
            ))
        } else if !rec_err.is_empty() {
            VoiceMsg::Failed(format!("Voice input error: {rec_err}"))
        } else {
            VoiceMsg::Empty
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
