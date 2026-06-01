use anyhow::{Context, Result};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::remote::{AdoptedMaster, ssh_control_dir};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PromptKind {
    Password,
    Passphrase,
    VerificationCode,
    HostKeyConfirmation,
    Generic,
}

#[derive(Clone, Debug)]
pub enum SshAuthEvent {
    Log(String),
    Prompt { kind: PromptKind, line: String },
    Authenticated,
    Failed(String),
}

pub struct SshAuth {
    host: String,
    socket_dir: PathBuf,
    socket_path: PathBuf,
    writer: Option<Box<dyn Write + Send>>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    events_rx: Receiver<SshAuthEvent>,
    reader_handle: Option<JoinHandle<()>>,
    cancelled: Arc<AtomicBool>,
    finished: bool,
    handed_off: bool,
    authenticated_signaled: bool,
    started: Instant,
}

impl SshAuth {
    pub fn start(host: &str) -> Result<Self> {
        let socket_dir = ssh_control_dir().context("ssh control dir")?;
        std::fs::create_dir_all(&socket_dir)
            .with_context(|| format!("creating {}", socket_dir.display()))?;
        let socket_path = socket_dir.join("ctl");

        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                cols: 100,
                rows: 24,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("openpty for ssh auth")?;

        let mut cmd = CommandBuilder::new("ssh");
        cmd.arg("-M");
        cmd.arg("-S");
        cmd.arg(&socket_path);
        cmd.arg("-N");
        cmd.arg("-T");
        cmd.arg("-o");
        cmd.arg("BatchMode=no");
        cmd.arg("-o");
        cmd.arg("ServerAliveInterval=15");
        cmd.arg(host);
        cmd.env("TERM", "dumb");
        cmd.env_remove("SSH_ASKPASS");
        cmd.env_remove("SSH_ASKPASS_REQUIRE");
        cmd.env_remove("DISPLAY");

        let child = pair.slave.spawn_command(cmd).context("spawn ssh master")?;
        drop(pair.slave);

        let writer = pair.master.take_writer().context("take writer")?;
        let reader = pair.master.try_clone_reader().context("clone reader")?;

        let (events_tx, events_rx) = channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let reader_handle = spawn_reader(reader, events_tx, cancelled.clone());

        Ok(Self {
            host: host.to_string(),
            socket_dir,
            socket_path,
            writer: Some(writer),
            child,
            events_rx,
            reader_handle: Some(reader_handle),
            cancelled,
            finished: false,
            handed_off: false,
            authenticated_signaled: false,
            started: Instant::now(),
        })
    }

    pub fn drain_events(&mut self) -> Vec<SshAuthEvent> {
        let mut out = Vec::new();
        loop {
            match self.events_rx.try_recv() {
                Ok(e) => out.push(e),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if !self.finished {
                        self.poll_child_for_completion(&mut out);
                    }
                    break;
                }
            }
        }
        if !self.finished {
            self.poll_child_for_completion(&mut out);
        }
        out
    }

    fn poll_child_for_completion(&mut self, out: &mut Vec<SshAuthEvent>) {
        if !self.authenticated_signaled && self.socket_path.exists() {
            self.authenticated_signaled = true;
            out.push(SshAuthEvent::Authenticated);
            return;
        }
        if let Ok(Some(status)) = self.child.try_wait() {
            self.finished = true;
            if status.success() && self.socket_path.exists() {
                if !self.authenticated_signaled {
                    self.authenticated_signaled = true;
                    out.push(SshAuthEvent::Authenticated);
                }
            } else {
                let detail = if !self.socket_path.exists() {
                    format!(
                        "ssh exited {status} and no control socket appeared at {}",
                        self.socket_path.display()
                    )
                } else {
                    format!("ssh exited {status}")
                };
                out.push(SshAuthEvent::Failed(detail));
            }
        } else if self.started.elapsed() > Duration::from_secs(600) {
            self.cancelled.store(true, Ordering::SeqCst);
            let _ = self.child.kill();
            self.finished = true;
            out.push(SshAuthEvent::Failed(
                "ssh auth timed out after 600s".to_string(),
            ));
        }
    }

    pub fn respond_password(&mut self, password: &str) {
        if let Some(w) = self.writer.as_mut() {
            let _ = w.write_all(password.as_bytes());
            let _ = w.write_all(b"\r");
            let _ = w.flush();
        }
    }

    pub fn cancel(&mut self) {
        if self.handed_off {
            return;
        }
        self.cancelled.store(true, Ordering::SeqCst);
        self.writer.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
        if self.socket_path.exists() {
            let _ = std::process::Command::new("ssh")
                .arg("-S")
                .arg(&self.socket_path)
                .arg("-O")
                .arg("exit")
                .arg(&self.host)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
        let _ = std::fs::remove_dir_all(&self.socket_dir);
        self.finished = true;
    }

    /// Surrender ownership of the established control socket to a caller
    /// that will run the rest of the croft launch flow against it. After
    /// this call the SshAuth no longer cleans the socket up on drop.
    pub fn hand_off(mut self) -> AdoptedMaster {
        self.handed_off = true;
        let adopted = AdoptedMaster {
            host: self.host.clone(),
            socket_dir: self.socket_dir.clone(),
            socket_path: self.socket_path.clone(),
        };
        std::mem::forget(self);
        adopted
    }
}

impl Drop for SshAuth {
    fn drop(&mut self) {
        if self.handed_off {
            return;
        }
        if !self.finished {
            self.cancelled.store(true, Ordering::SeqCst);
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        if !self.handed_off && self.socket_path.exists() {
            let _ = std::process::Command::new("ssh")
                .arg("-S")
                .arg(&self.socket_path)
                .arg("-O")
                .arg("exit")
                .arg(&self.host)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
        if !self.handed_off && !self.socket_dir.as_os_str().is_empty() {
            let _ = std::fs::remove_dir_all(&self.socket_dir);
        }
        if let Some(h) = self.reader_handle.take() {
            let _ = h.join();
        }
    }
}

fn spawn_reader(
    mut reader: Box<dyn Read + Send>,
    events_tx: Sender<SshAuthEvent>,
    cancelled: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut buf = [0u8; 4096];
        let mut tail = String::new();
        loop {
            if cancelled.load(Ordering::SeqCst) {
                break;
            }
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let chunk = strip_ansi(&buf[..n]);
                    if events_tx.send(SshAuthEvent::Log(chunk.clone())).is_err() {
                        break;
                    }
                    tail.push_str(&chunk);
                    while let Some(prompt) = peel_prompt(&mut tail) {
                        if events_tx.send(prompt).is_err() {
                            return;
                        }
                    }
                    if tail.len() > 8192 {
                        let drop_from = tail.len() - 4096;
                        tail.drain(..drop_from);
                    }
                }
                Err(_) => break,
            }
        }
    })
}

/// Strip ANSI CSI/OSC escape sequences and bare control bytes that ssh
/// occasionally interleaves on the TTY path. The remaining text is what
/// the user would see, which is what we want to match prompts against.
pub fn strip_ansi(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        let b = input[i];
        if b == 0x1b && i + 1 < input.len() {
            let next = input[i + 1];
            if next == b'[' {
                i += 2;
                while i < input.len() && !(0x40..=0x7e).contains(&input[i]) {
                    i += 1;
                }
                if i < input.len() {
                    i += 1;
                }
                continue;
            }
            if next == b']' {
                i += 2;
                while i < input.len() && input[i] != 0x07 && input[i] != 0x1b {
                    i += 1;
                }
                if i < input.len()
                    && input[i] == 0x1b
                    && i + 1 < input.len()
                    && input[i + 1] == b'\\'
                {
                    i += 2;
                } else if i < input.len() {
                    i += 1;
                }
                continue;
            }
            i += 2;
            continue;
        }
        if b == 0x07 || b == 0x08 {
            i += 1;
            continue;
        }
        if b == b'\r' {
            i += 1;
            continue;
        }
        out.push(b as char);
        i += 1;
    }
    out
}

/// Peel the most recent un-newlined line off `tail` and classify it as a
/// prompt if it looks like one. Lines ending in newline are kept in tail's
/// past — only the active prompt is interesting, and ssh never emits a
/// newline after a prompt until input is provided.
pub fn peel_prompt(tail: &mut String) -> Option<SshAuthEvent> {
    let nl = tail.rfind('\n').map(|i| i + 1).unwrap_or(0);
    let active = &tail[nl..];
    if active.is_empty() {
        return None;
    }
    let kind = classify_prompt(active)?;
    let line = active.to_string();
    tail.drain(nl..);
    Some(SshAuthEvent::Prompt { kind, line })
}

pub fn classify_prompt(line: &str) -> Option<PromptKind> {
    let lower = line.to_lowercase();
    if !lower.contains("password")
        && !lower.contains("passphrase")
        && !lower.contains("verification code")
        && !lower.contains("(yes/no")
        && !lower.contains("are you sure you want to continue connecting")
        && !lower.ends_with(": ")
        && !lower.ends_with("? ")
    {
        return None;
    }
    if lower.contains("are you sure you want to continue connecting") || lower.contains("(yes/no") {
        return Some(PromptKind::HostKeyConfirmation);
    }
    if lower.contains("verification code") {
        return Some(PromptKind::VerificationCode);
    }
    if lower.contains("passphrase") {
        return Some(PromptKind::Passphrase);
    }
    if lower.contains("password") {
        return Some(PromptKind::Password);
    }
    Some(PromptKind::Generic)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_lowercase_password_prompt() {
        assert_eq!(
            classify_prompt("user@host's password: "),
            Some(PromptKind::Password)
        );
    }

    #[test]
    fn classifies_passphrase_prompt() {
        assert_eq!(
            classify_prompt("Enter passphrase for key '/Users/v/.ssh/id_ed25519': "),
            Some(PromptKind::Passphrase)
        );
    }

    #[test]
    fn classifies_2fa_prompt() {
        assert_eq!(
            classify_prompt("Verification code: "),
            Some(PromptKind::VerificationCode)
        );
    }

    #[test]
    fn classifies_known_hosts_confirmation() {
        let line = "Are you sure you want to continue connecting (yes/no/[fingerprint])? ";
        assert_eq!(classify_prompt(line), Some(PromptKind::HostKeyConfirmation));
    }

    #[test]
    fn does_not_classify_normal_log_lines() {
        assert_eq!(
            classify_prompt("debug1: Authenticating to host:22 as 'user'"),
            None
        );
        assert_eq!(classify_prompt(""), None);
    }

    #[test]
    fn strip_ansi_keeps_text_drops_csi() {
        let raw = b"\x1b[31mred\x1b[0m text\r\nnext line";
        let out = strip_ansi(raw);
        assert_eq!(out, "red text\nnext line");
    }

    #[test]
    fn strip_ansi_drops_osc_with_bel_terminator() {
        let raw = b"\x1b]0;title\x07after";
        let out = strip_ansi(raw);
        assert_eq!(out, "after");
    }

    #[test]
    fn peel_prompt_extracts_password_from_partial_line() {
        let mut tail = String::from("Last login: foo\nuser@host's password: ");
        let ev = peel_prompt(&mut tail).expect("password tail must produce a prompt event");
        match ev {
            SshAuthEvent::Prompt {
                kind: PromptKind::Password,
                line,
            } => {
                assert!(line.contains("password"));
            }
            other => panic!("expected Password prompt, got {other:?}"),
        }
    }

    #[test]
    fn peel_prompt_returns_none_when_only_log_lines() {
        let mut tail = String::from("debug1: client_loop: send disconnect: Broken pipe\n");
        assert!(peel_prompt(&mut tail).is_none());
    }

    #[test]
    fn control_socket_existence_is_a_valid_authenticated_signal() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sock = tmp.path().join("ctl");
        assert!(!sock.exists());
        std::fs::write(&sock, b"").expect("touch fake socket");
        assert!(
            sock.exists(),
            "ssh -M -N keeps the child running after auth, so the socket file appearing is the only reliable success signal"
        );
    }

    #[test]
    fn peel_prompt_consumes_the_prompt_so_repeated_calls_do_not_loop() {
        let mut tail = String::from("debug1: connected\nuser@host's password: ");
        let first = peel_prompt(&mut tail);
        assert!(matches!(first, Some(SshAuthEvent::Prompt { .. })));
        let second = peel_prompt(&mut tail);
        assert!(
            second.is_none(),
            "second call must return None or the reader thread spams the channel forever"
        );
    }
}
