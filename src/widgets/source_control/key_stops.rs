//! Which of Source Control's Tab stops has the keys (#871): the stops on
//! screen in Tab order, where Tab and Shift+Tab go next, and where the keys
//! go when their stop leaves the screen.

use super::*;

impl SourceControlPanel {
    /// The Tab stops on screen, in Tab order. The card's buttons count as
    /// one stop here: Tab walks them before it leaves the card.
    fn stops(&self) -> Vec<KeyStop> {
        let mut stops = Vec::with_capacity(4);
        // REPOSITORIES while it shows (two or more) and the last frame drew
        // a row of it: a panel too short for any has no such stop.
        if self.repositories.len() >= 2 && self.drawn_repo_rows() > 0 {
            stops.push(KeyStop::Repositories);
        }
        if self.shows_repository() {
            stops.push(KeyStop::Message);
            if !self.entries.is_empty() {
                stops.push(KeyStop::List);
            }
            stops.push(KeyStop::Commits);
        } else {
            stops.push(KeyStop::Card);
        }
        stops
    }

    /// Whether the keys work the repository stops rather than the card: in
    /// a repository, and until git has said the folder is in none.
    fn shows_repository(&self) -> bool {
        self.status.in_repo || !self.status_known
    }

    /// The keys' stop after their stop has left the screen: the last change
    /// committed, a folder closed, the folder made a repository. They go to
    /// the message box, or to the card in a folder with no repository.
    pub fn settle_keys(&mut self) {
        if !self.stops().contains(&self.keys) {
            self.keys = if self.shows_repository() {
                KeyStop::Message
            } else {
                KeyStop::Card
            };
        }
        self.repo_cursor = self
            .repo_cursor
            .min(self.repositories.len().saturating_sub(1));
        self.card_button = self.card_button.min(self.card_buttons().saturating_sub(1));
    }

    /// The stop Tab (`forward`) or Shift+Tab goes to from the keys' one,
    /// round again past either end.
    pub fn next_stop(&self, forward: bool) -> KeyStop {
        let stops = self.stops();
        let n = stops.len();
        let at = stops.iter().position(|s| *s == self.keys).unwrap_or(0);
        stops[if forward {
            (at + 1) % n
        } else {
            (at + n - 1) % n
        }]
    }
}
