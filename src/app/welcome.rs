use std::sync::mpsc::Receiver;

use crate::git::{CommitInfo, RecentCommits, RecentCommitsError};

use super::WelcomeLink;

#[derive(Default)]
pub struct RecentDrain {
    pub installed: bool,
    pub status: Option<String>,
}

pub struct WelcomeState {
    remote: Option<String>,
    commits: Vec<CommitInfo>,
    rx: Option<Receiver<(RecentCommits, RecentCommitsError)>>,
    links: Vec<WelcomeLink>,
}

impl WelcomeState {
    pub fn spawn() -> Self {
        let (commits_tx, commits_rx) = std::sync::mpsc::channel();
        let remote = crate::git::croft_repository_remote();
        std::thread::spawn(move || {
            let timeout = std::time::Duration::from_secs(5);
            let result = crate::git::fetch_croft_recent_commits_full(timeout);
            let _ = commits_tx.send(result);
        });
        Self {
            remote,
            commits: Vec::new(),
            rx: Some(commits_rx),
            links: Vec::new(),
        }
    }

    pub fn remote(&self) -> Option<&str> {
        self.remote.as_deref()
    }

    pub fn commits(&self) -> &[CommitInfo] {
        &self.commits
    }

    pub fn has_recent_panel(&self) -> bool {
        self.remote.is_some() || !self.commits.is_empty()
    }

    pub fn clear_links(&mut self) {
        self.links.clear();
    }

    pub fn push_link(&mut self, link: WelcomeLink) {
        self.links.push(link);
    }

    pub fn links(&self) -> &[WelcomeLink] {
        &self.links
    }

    pub fn drain(&mut self) -> RecentDrain {
        let Some(rx) = self.rx.as_ref() else {
            return RecentDrain::default();
        };
        match rx.try_recv() {
            Ok((commits, err)) => {
                self.remote = commits.remote;
                self.commits = commits.commits;
                self.rx = None;
                let status = match err {
                    RecentCommitsError::None => None,
                    RecentCommitsError::Network => {
                        Some(String::from("Recent commits unavailable: git fetch failed"))
                    }
                    RecentCommitsError::NoEndpoint => Some(String::from(
                        "Recent commits unavailable: no remote configured",
                    )),
                };
                RecentDrain {
                    installed: true,
                    status,
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => RecentDrain::default(),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.rx = None;
                RecentDrain::default()
            }
        }
    }

    #[cfg(test)]
    pub fn set_test_remote(&mut self, remote: Option<String>) {
        self.remote = remote;
    }

    #[cfg(test)]
    pub fn set_test_commits(&mut self, commits: Vec<CommitInfo>) {
        self.commits = commits;
    }
}
