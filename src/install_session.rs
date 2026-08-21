use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::thread::JoinHandle;

use crate::remote::AdoptedMaster;

#[derive(Debug)]
pub enum InstallEvent {
    Log(String),
    /// A croft is already installed on the remote, so the user can be
    /// dropped into it now while the (re)install continues in the
    /// background. Fired at most once, before `Done`.
    CanLaunch,
    /// The fast cross-build path is unavailable and the installer wants to
    /// compile on the remote box instead (slow). The UI must answer via
    /// `respond_fallback`: `true` runs the remote compile, `false` aborts
    /// the install so the user can run `croft setup-cross` and reconnect
    /// for the fast path. Fired only while the install still blocks the
    /// connect (never after `CanLaunch`).
    FallbackPrompt {
        reason: String,
    },
    Done(Result<(), String>),
}

pub struct InstallSession {
    pub host: String,
    pub path: Option<String>,
    pub adopted: Option<AdoptedMaster>,
    events_rx: Receiver<InstallEvent>,
    fallback_answer_tx: Sender<bool>,
    _handle: Option<JoinHandle<()>>,
}

impl InstallSession {
    pub fn start(adopted: AdoptedMaster, host: String, path: Option<String>) -> Self {
        let (tx, rx): (Sender<InstallEvent>, Receiver<InstallEvent>) = channel();
        let adopted_for_thread = adopted.clone();
        let log_tx_for_install: Sender<String> = {
            let tx = tx.clone();
            let (s, r) = channel::<String>();
            std::thread::spawn(move || {
                while let Ok(line) = r.recv() {
                    if tx.send(InstallEvent::Log(line)).is_err() {
                        break;
                    }
                }
            });
            s
        };
        let can_launch_tx: Sender<()> = {
            let tx = tx.clone();
            let (s, r) = channel::<()>();
            std::thread::spawn(move || {
                if r.recv().is_ok() {
                    let _ = tx.send(InstallEvent::CanLaunch);
                }
            });
            s
        };
        let (fallback_answer_tx, fallback_answer_rx) = channel::<bool>();
        let prompt_tx = tx.clone();
        let done_tx = tx.clone();
        let handle = std::thread::spawn(move || {
            let mut confirm_fallback = move |reason: &str| {
                if prompt_tx
                    .send(InstallEvent::FallbackPrompt {
                        reason: reason.to_string(),
                    })
                    .is_err()
                {
                    return false;
                }
                // Blocks until the dialog answers. A torn-down dialog drops
                // the answer sender, which reads as a decline.
                fallback_answer_rx.recv().unwrap_or(false)
            };
            let result = crate::remote::install_only_streaming(
                adopted_for_thread,
                log_tx_for_install,
                can_launch_tx,
                &mut confirm_fallback,
            );
            let payload = match result {
                Ok(_) => Ok(()),
                Err(e) => Err(format!("{e:#}")),
            };
            let _ = done_tx.send(InstallEvent::Done(payload));
        });
        Self {
            host,
            path,
            adopted: Some(adopted),
            events_rx: rx,
            fallback_answer_tx,
            _handle: Some(handle),
        }
    }

    pub fn drain_events(&mut self) -> Vec<InstallEvent> {
        let mut out = Vec::new();
        loop {
            match self.events_rx.try_recv() {
                Ok(ev) => out.push(ev),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }
        out
    }

    /// Answer a pending `FallbackPrompt`: `true` proceeds with the slow
    /// remote compile, `false` aborts the install.
    pub fn respond_fallback(&self, proceed: bool) {
        let _ = self.fallback_answer_tx.send(proceed);
    }

    pub fn take_adopted(&mut self) -> Option<AdoptedMaster> {
        self.adopted.take()
    }
}
