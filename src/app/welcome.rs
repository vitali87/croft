use crate::git::{CommitInfo, RecentCommits};

use super::WelcomeLink;

pub struct WelcomeState {
    remote: Option<String>,
    commits: Vec<CommitInfo>,
    links: Vec<WelcomeLink>,
}

impl WelcomeState {
    /// Read the commits baked into this binary at build time. No network and
    /// no background thread: the list is a property of the build, so every
    /// launch is free and the panel reflects exactly what this version ships
    /// (see [`crate::git::release_commits`]).
    pub fn new() -> Self {
        let RecentCommits { remote, commits } = crate::git::release_commits();
        Self {
            remote,
            commits,
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

    #[cfg(test)]
    pub fn set_test_remote(&mut self, remote: Option<String>) {
        self.remote = remote;
    }

    #[cfg(test)]
    pub fn set_test_commits(&mut self, commits: Vec<CommitInfo>) {
        self.commits = commits;
    }
}
