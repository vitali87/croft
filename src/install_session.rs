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

/// The install itself: [`crate::remote::install_only_streaming`], or a
/// test's stand-in. It sends its log lines and its "can launch" signal on the
/// two senders, and asks the fallback question through the callback.
type Install = fn(
    AdoptedMaster,
    Sender<String>,
    Sender<()>,
    &mut dyn FnMut(&str) -> bool,
) -> anyhow::Result<AdoptedMaster>;

impl InstallSession {
    /// Run the install as the `remote-install` stream. Fails only when no
    /// thread can be started, and then closes the control master it was
    /// handed, which nothing else will use.
    pub fn start(
        adopted: AdoptedMaster,
        host: String,
        path: Option<String>,
    ) -> std::io::Result<Self> {
        Self::start_with(adopted, host, path, crate::remote::install_only_streaming)
    }

    fn start_with(
        adopted: AdoptedMaster,
        host: String,
        path: Option<String>,
        install: Install,
    ) -> std::io::Result<Self> {
        let adopted_for_thread = adopted.clone();
        let (fallback_answer_tx, fallback_answer_rx) = channel::<bool>();
        let spawned = Stream::spawn("remote-install", Start::Now, move |events| {
            let (log_tx_for_install, log_rx) = channel::<String>();
            let log_forwarder = {
                let events = events.clone();
                std::thread::spawn(move || {
                    while let Ok(line) = log_rx.recv() {
                        if !events.send(InstallEvent::Log(line)) {
                            break;
                        }
                    }
                })
            };
            let (can_launch_tx, can_launch_rx) = channel::<()>();
            let can_launch_forwarder = {
                let events = events.clone();
                std::thread::spawn(move || {
                    if can_launch_rx.recv().is_ok() {
                        events.send(InstallEvent::CanLaunch);
                    }
                })
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
            let result = install(
                adopted_for_thread,
                log_tx_for_install,
                can_launch_tx,
                &mut confirm_fallback,
            );
            // The install has let go of both senders, so the forwarders end
            // once they have passed on what it sent: its last log lines and
            // its "can launch" always arrive before its outcome (#2605).
            let _ = log_forwarder.join();
            let _ = can_launch_forwarder.join();
            let payload = match result {
                Ok(_) => Ok(()),
                Err(e) => Err(format!("{e:#}")),
            };
            events.send(InstallEvent::Done(payload));
        });
        let events = match spawned {
            Ok(events) => events,
            Err(e) => {
                crate::remote::close_master(adopted);
                return Err(e);
            }
        };
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
        while let Some(events) = self.events.as_mut() {
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

    /// A session whose installer logs a line and then crashes while a
    /// helper of its own, like a build's output reader, still holds its log
    /// sender (#2605).
    pub fn crashing(host: &str) -> Self {
        fn install(
            _: AdoptedMaster,
            log_tx: Sender<String>,
            _: Sender<()>,
            _: &mut dyn FnMut(&str) -> bool,
        ) -> anyhow::Result<AdoptedMaster> {
            let _ = log_tx.send(String::from("Probing the remote"));
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(60));
                drop(log_tx);
            });
            panic!("the install crashed (on purpose, in a test)")
        }
        Self::start_with(test_master(host), host.to_string(), None, install).unwrap()
    }
}

/// A control master for a test's install session, at paths that do not
/// exist: nothing connects through it.
#[cfg(test)]
fn test_master(host: &str) -> AdoptedMaster {
    AdoptedMaster {
        host: host.to_string(),
        socket_dir: std::path::PathBuf::from("/nonexistent/croft-test-ctl"),
        socket_path: std::path::PathBuf::from("/nonexistent/croft-test-ctl/ctl"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #2605 review: an install that says croft can launch and then fails
    /// reports the launch before its outcome, so the user is dropped into the
    /// croft already there instead of being told the connect failed.
    #[test]
    fn an_install_that_could_launch_and_then_failed_reports_the_launch_first() {
        fn install(
            _: AdoptedMaster,
            log_tx: Sender<String>,
            can_launch_tx: Sender<()>,
            _: &mut dyn FnMut(&str) -> bool,
        ) -> anyhow::Result<AdoptedMaster> {
            let _ = log_tx.send(String::from("croft is already installed"));
            let _ = can_launch_tx.send(());
            anyhow::bail!("the reinstall failed")
        }
        for _ in 0..20 {
            let mut session =
                InstallSession::start_with(test_master("devbox"), "devbox".into(), None, install)
                    .unwrap();
            let mut events = Vec::new();
            crate::test_budget::await_spawned(
                std::time::Duration::from_secs(1),
                "the install to end",
                || {
                    events.extend(session.drain_events());
                    events.iter().any(|e| matches!(e, InstallEvent::Done(_)))
                },
            );
            let done = events
                .iter()
                .position(|e| matches!(e, InstallEvent::Done(_)))
                .unwrap();
            assert!(
                events[..done]
                    .iter()
                    .any(|e| matches!(e, InstallEvent::CanLaunch)),
                "{events:?}"
            );
            assert!(
                events[..done]
                    .iter()
                    .any(|e| matches!(e, InstallEvent::Log(_))),
                "{events:?}"
            );
            assert!(
                matches!(&events[done], InstallEvent::Done(Err(e)) if e == "the reinstall failed"),
                "{events:?}"
            );
        }
    }
}
