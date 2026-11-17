//! The pane's state the app reads each frame: its label and name, the agent in it, its
//! reported status and progress, bells, notifications and ports, and whether it is quiet
//! or untouched.

use super::*;

/// The label to show for a pane: a manual name wins, else the live foreground
/// process name.
pub fn pick_pane_label<'a>(manual: Option<&'a str>, auto: &'a str) -> &'a str {
    manual.unwrap_or(auto)
}

impl PtyTerminal {
    /// Whether the pane is in the alternate screen (a full-screen app owns
    /// the viewport; anchored overlays make no sense there).
    pub fn alt_screen(&self) -> bool {
        self.term.lock().mode().contains(TermMode::ALT_SCREEN)
    }

    /// OSC 9 notification payloads since the last drain.
    pub fn drain_notifications(&self) -> Vec<String> {
        std::mem::take(&mut *self.notifications.lock().unwrap())
    }

    /// True once if the child rang BEL since the last call (drains the latch).
    pub fn take_bell(&self) -> bool {
        self.bell.swap(false, Ordering::AcqRel)
    }

    /// The label to show for this pane: manual name if set, else the live
    /// foreground-process label.
    pub fn label(&self) -> &str {
        pick_pane_label(self.manual_name.as_deref(), &self.auto_label)
    }

    /// Set the foreground-process label (from the off-loop refresh).
    pub fn set_auto_label(&mut self, label: String) {
        self.auto_label = label;
    }

    /// How long since the PTY last produced output; a pane that never has is
    /// quiet since it was spawned.
    pub fn quiet_for(&self) -> std::time::Duration {
        let last = self.last_output_ms.load(Ordering::Relaxed);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        std::time::Duration::from_millis(now.saturating_sub(last))
    }

    /// The agent seated in this pane, if the last sample found one (#344).
    pub fn agent(&self) -> Option<&crate::agents::AgentLane> {
        self.agent.as_ref()
    }

    pub fn set_agent(&mut self, lane: Option<crate::agents::AgentLane>) {
        self.agent = lane;
    }

    /// The status the pane's agent reported itself over OSC 7501, `None`
    /// when it has reported nothing.
    pub fn reported_status(&self) -> Option<crate::program_status::State> {
        self.program_status
            .lock()
            .unwrap()
            .reported_since(self.status_mark)
    }

    /// Keep the pane's OSC 7501 reports to the agent a sample found, given
    /// the agent the last sample found. Reports made while no agent had the
    /// foreground are no agent's. When one agent replaces another between
    /// two samples, a probe or shell prompt in the stream since the first
    /// was found divides their reports; with neither, the second's so far
    /// cannot be told from the first's and go too.
    pub fn sample_reporter(&mut self, prev: Option<&str>, next: Option<&str>) {
        let records = self.program_status.lock().unwrap();
        match (prev, next) {
            (_, None) => self.status_mark = records.seq(),
            (None, Some(_)) => self.status_seated = records.seq(),
            (Some(p), Some(n)) if p != n => {
                if !records.fresh_since(self.status_seated) {
                    self.status_mark = records.seq();
                }
                self.status_seated = records.seq();
            }
            _ => {}
        }
    }

    /// The user's manual pane name, when one was set via rename (what the
    /// session snapshot persists; `label()` mixes in the auto label).
    pub fn manual_name(&self) -> Option<&str> {
        self.manual_name.as_deref()
    }

    /// Set or clear the user's manual pane name (a blank name clears it).
    pub fn set_manual_name(&mut self, name: Option<String>) {
        self.manual_name = name.filter(|n| !n.trim().is_empty());
        if self.manual_name.is_some() {
            self.manual_name_seen = true;
        }
    }

    pub fn take_dirty(&self) -> bool {
        self.pty_pending_bytes.store(0, Ordering::Relaxed);
        self.pty_dirty.swap(false, Ordering::AcqRel)
    }

    /// The pane's live OSC 9;4 progress `(state, percent)`, if a program is
    /// reporting one.
    pub fn progress(&self) -> Option<(u8, u8)> {
        *self.progress.lock().unwrap()
    }

    /// The hostname the shell last reported over OSC 7 (an in-pane SSH
    /// session with integration moves it to the remote host).
    pub fn shell_host(&self) -> Option<String> {
        self.osc7_host.lock().unwrap().clone()
    }

    /// Drain the loopback ports the reader thread scraped since the last call.
    /// Each port is reported at most once over the terminal's lifetime, so this
    /// is empty on most ticks.
    pub fn drain_ports(&self) -> Vec<crate::port_detect::PortHit> {
        self.port_rx.try_iter().collect()
    }

    /// Read the dirty flag without clearing it. Lets the main loop decide
    /// whether to redraw now or coalesce, without losing the signal if we
    /// choose to skip this iteration.
    pub fn peek_dirty(&self) -> bool {
        self.pty_dirty.load(Ordering::Acquire)
    }

    /// The pane's stable identity (see the `uid` field).
    pub fn uid(&self) -> u64 {
        self.uid
    }

    /// True while the pane is exactly as croft created it: an interactive
    /// shell (never a `new_running` program pane, whose child is doing
    /// launched work with no input byte ever written) that has received
    /// no input and carries no manual name — nothing the user could miss
    /// if the pane were replaced. The re-root restore (#137) may swap
    /// only such panes for the incoming workspace's saved layout; any
    /// input (a keystroke, a paste, a seeded `cd`) or a rename clears
    /// this permanently.
    pub fn is_pristine(&self) -> bool {
        !self.run_pane && !self.input_seen && !self.manual_name_seen
    }
}
