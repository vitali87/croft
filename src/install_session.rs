use std::sync::mpsc::{Sender, channel};

use crate::jobs::{Start, Stream, StreamPoll};
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

/// What an install whose thread crashed before it finished says, in the
/// connect dialog and on the status line.
pub const CRASHED_DETAIL: &str = "the install's thread crashed";

pub struct InstallSession {
    pub host: String,
    pub path: Option<String>,
    pub adopted: Option<AdoptedMaster>,
    /// The install's progress, until it ends or its thread dies.
    events: Option<Stream<InstallEvent>>,
    fallback_answer_tx: Sender<bool>,
}

impl InstallSession {
    /// Run the install as the `remote-install` stream. Fails only when no
    /// thread can be started.
    pub fn start(
        adopted: AdoptedMaster,
        host: String,
        path: Option<String>,
    ) -> std::io::Result<Self> {
        let adopted_for_thread = adopted.clone();
        let (fallback_answer_tx, fallback_answer_rx) = channel::<bool>();
        let events = Stream::spawn("remote-install", Start::Now, move |events| {
            let log_tx_for_install: Sender<String> = {
                let events = events.clone();
                let (s, r) = channel::<String>();
                std::thread::spawn(move || {
                    while let Ok(line) = r.recv() {
                        if !events.send(InstallEvent::Log(line)) {
                            break;
                        }
                    }
                });
                s
            };
            let can_launch_tx: Sender<()> = {
                let events = events.clone();
                let (s, r) = channel::<()>();
                std::thread::spawn(move || {
                    if r.recv().is_ok() {
                        events.send(InstallEvent::CanLaunch);
                    }
                });
                s
            };
            let mut confirm_fallback = move |reason: &str| {
                if !events.send(InstallEvent::FallbackPrompt {
                    reason: reason.to_string(),
                }) {
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
            events.send(InstallEvent::Done(payload));
        })?;
        Ok(Self {
            host,
            path,
            adopted: Some(adopted),
            events: Some(events),
            fallback_answer_tx,
        })
    }

    /// The events sent since the last call. An install whose thread crashed
    /// before it finished ends with a failed `Done`, for [`CRASHED_DETAIL`],
    /// so the connect dialog never waits on it for good.
    pub fn drain_events(&mut self) -> Vec<InstallEvent> {
        let mut out = Vec::new();
        while let Some(events) = self.events.as_ref() {
            match events.poll() {
                StreamPoll::Update(event) => out.push(event),
                StreamPoll::Pending => break,
                StreamPoll::Ended => self.events = None,
                StreamPoll::Died => {
                    self.events = None;
                    out.push(InstallEvent::Done(Err(String::from(CRASHED_DETAIL))));
                }
            }
        }
        out
    }

    /// Answer a pending `FallbackPrompt`: `true` proceeds with the slow
    /// remote compile, `false` aborts the install.
    pub fn respond_fallback(&self, proceed: bool) {
        let _ = self.fallback_answer_tx.send(proceed);
    }

    /// Hand the adopted control master to the caller, leaving `None`
    /// behind. Taken once the TUI has surrendered the alt-screen and is
    /// ready to launch through the existing connection.
    pub fn take_adopted(&mut self) -> Option<AdoptedMaster> {
        self.adopted.take()
    }
}

#[cfg(test)]
impl InstallSession {
    /// A session whose installer has reported `events` and is still running,
    /// so a test can drive the App's handling of them without ssh.
    pub fn with_events(
        host: &str,
        path: Option<String>,
        adopted: Option<AdoptedMaster>,
        events: Vec<InstallEvent>,
    ) -> Self {
        let (fallback_answer_tx, _) = channel();
        Self {
            host: host.to_string(),
            path,
            adopted,
            events: Some(Stream::running(events)),
            fallback_answer_tx,
        }
    }

    /// A session whose installer logs a line and then crashes.
    pub fn crashing(host: &str) -> Self {
        let (fallback_answer_tx, _) = channel();
        let events = Stream::spawn("remote-install", Start::Now, |events| {
            events.send(InstallEvent::Log(String::from("Probing the remote")));
            panic!("the install crashed (on purpose, in a test)")
        })
        .unwrap();
        Self {
            host: host.to_string(),
            path: None,
            adopted: None,
            events: Some(events),
            fallback_answer_tx,
        }
    }
}
