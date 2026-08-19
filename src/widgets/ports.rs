//! The bottom panel's PORTS tab: the persistent registry of loopback ports
//! croft has detected this session (from the terminal output scrape and the
//! socket poll in [`crate::port_detect`]). On a remote session a port can be
//! forwarded home over the live SSH master; on a local session it's already
//! reachable, so the only action is to open it.
//!
//! The panel owns the list and the selection; it never performs the actions
//! (forward, open, copy) itself — those need the relay, the clipboard, and the
//! session's remote/local knowledge, so the app reads [`PortsPanel::selected`]
//! and acts. Keeping the widget pure makes it trivially testable.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

use std::collections::HashSet;

use crate::theme::Theme;

const COLOR_DIM: Color = Color::Rgb(0x80, 0x88, 0x98);
const COLOR_HEAD: Color = Color::Rgb(0x8b, 0x93, 0xa1);
const COLOR_MSG: Color = Color::Rgb(0xCC, 0xCC, 0xCC);
const COLOR_LIVE: Color = Color::Rgb(0x4f, 0xb1, 0xa6);

/// Outcome of [`PortsPanel::reconcile_live`]: whether the view changed (for
/// redraw) and any forwarded tunnels to tear down because their server stopped.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ReconcileOutcome {
    pub changed: bool,
    /// `(local_port, remote_port)` for each dropped forward.
    pub torn_down: Vec<(u16, u16)>,
}

/// Where a detected port lives, and therefore what we can do with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortOrigin {
    /// A port on this machine — already reachable, just openable.
    Local,
    /// A port on the remote host (label carried for display); forwardable home.
    Remote(String),
}

impl PortOrigin {
    fn is_remote(&self) -> bool {
        matches!(self, PortOrigin::Remote(_))
    }
}

/// One detected port. `local_port` is set once a remote port is forwarded, and
/// is the port to open locally; for a local-origin port it equals `port`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortEntry {
    pub port: u16,
    pub url: Option<String>,
    pub process: Option<String>,
    pub origin: PortOrigin,
    pub forwarded: bool,
    pub local_port: Option<u16>,
    /// Whether the most recent socket poll still saw this port listening. Drives
    /// the live/idle dot; a port the poll stops seeing goes not-live.
    pub live: bool,
    /// Consecutive poll cycles this port has been absent. A non-forwarded port
    /// is dropped once this passes the grace threshold.
    misses: u8,
    /// Whether the detection toast has already fired for this entry. Tied to the
    /// entry's lifetime, so a port re-added after its server restarts toasts
    /// again, but the toast never double-fires for one running server.
    toasted: bool,
}

impl PortEntry {
    /// The URL to hand the local browser: the scraped URL when the port is
    /// already local, else the forwarded loopback address.
    pub fn open_url(&self) -> Option<String> {
        match (&self.origin, self.local_port) {
            (PortOrigin::Local, _) => Some(
                self.url
                    .clone()
                    .unwrap_or_else(|| format!("http://127.0.0.1:{}/", self.port)),
            ),
            (PortOrigin::Remote(_), Some(lp)) => Some(format!("http://127.0.0.1:{lp}/")),
            (PortOrigin::Remote(_), None) => None,
        }
    }
}

pub struct PortsPanel {
    entries: Vec<PortEntry>,
    selected: usize,
    /// Ports the user dismissed with `x`; the socket poll re-reports a still
    /// listening port every few seconds, so without this a dismissed port would
    /// pop straight back into the list. Cleared for a port only by restarting.
    dismissed: HashSet<u16>,

    pub focus_gradient: bool,
    pub theme: Theme,
    pub focused: bool,
    pub hover_pointer: Option<(u16, u16)>,

    pub last_area: Rect,
    /// Body-row hit rects (index into `entries`), recomputed each render.
    row_rects: Vec<(Rect, usize)>,
    first_row_y: u16,
    /// Index of the first entry drawn, so a selection past the panel height
    /// scrolls into view instead of disappearing.
    scroll: usize,
}

impl PortsPanel {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            selected: 0,
            dismissed: HashSet::new(),
            focus_gradient: false,
            theme: Theme::default(),
            focused: false,
            hover_pointer: None,
            last_area: Rect::default(),
            row_rects: Vec::new(),
            first_row_y: 0,
            scroll: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Count of ports currently forwarded home over the SSH master. Drives the
    /// Remote activity-icon badge, mirroring VS Code's Ports view count.
    pub fn forwarded_count(&self) -> usize {
        self.entries.iter().filter(|e| e.forwarded).count()
    }

    pub fn selected(&self) -> Option<&PortEntry> {
        self.entries.get(self.selected)
    }

    /// Insert a freshly detected port, or refresh an existing one's metadata.
    /// Returns true only when the port was newly added, so the app fires the
    /// detection toast exactly once per port.
    pub fn upsert(
        &mut self,
        port: u16,
        url: Option<String>,
        process: Option<String>,
        origin: PortOrigin,
    ) -> bool {
        if self.dismissed.contains(&port) {
            return false;
        }
        if let Some(e) = self.entries.iter_mut().find(|e| e.port == port) {
            if e.url.is_none() {
                e.url = url;
            }
            if e.process.is_none() {
                e.process = process;
            }
            // A scrape may learn the origin before the poll, or vice versa; a
            // remote origin (carrying the host label) is the more specific one.
            if origin.is_remote() {
                e.origin = origin;
            }
            return false;
        }
        let local_port = matches!(origin, PortOrigin::Local).then_some(port);
        self.entries.push(PortEntry {
            port,
            url,
            process,
            origin,
            forwarded: false,
            local_port,
            live: true,
            misses: 0,
            toasted: false,
        });
        true
    }

    /// Mark that the output scrape has confirmed `port` is browser-facing, and
    /// report whether the detection toast should fire now (true the first time
    /// per entry). Lets the toast fire even when the silent socket poll added
    /// the port first, and re-fire after a restart re-creates the entry.
    pub fn note_scrape_toast(&mut self, port: u16) -> bool {
        match self.entries.iter_mut().find(|e| e.port == port) {
            Some(e) if !e.toasted => {
                e.toasted = true;
                true
            }
            _ => false,
        }
    }

    /// Reconcile the registry against the latest socket-poll snapshot (`live` =
    /// the ports currently listening). A port still present is live; one that's
    /// gone is marked not-live, and stays gone past a short grace is dropped —
    /// the row follows the server, forwarded or not. A dropped forward's
    /// `(local, remote)` pair is reported so the app can tear the tunnel down.
    ///
    /// `live` must therefore be EVERY loopback listener on the box
    /// ([`crate::port_detect::poll_all_listening`]), not the pane-subtree scan
    /// that decides what to surface: a container's published port is nobody's
    /// descendant, and retiring it for missing from a snapshot that could never
    /// contain it dropped a live row and cancelled its tunnel.
    pub fn reconcile_live(&mut self, live: &HashSet<u16>) -> ReconcileOutcome {
        const GRACE_CYCLES: u8 = 2;
        let before = self.entries.len();
        let mut changed = false;
        for e in &mut self.entries {
            if live.contains(&e.port) {
                if !e.live || e.misses != 0 {
                    changed = true;
                }
                e.live = true;
                e.misses = 0;
            } else {
                if e.live {
                    changed = true;
                }
                e.live = false;
                e.misses = e.misses.saturating_add(1);
            }
        }
        // Forwards about to be dropped: hand back their tunnels for teardown.
        let torn_down: Vec<(u16, u16)> = self
            .entries
            .iter()
            .filter(|e| e.misses >= GRACE_CYCLES && e.forwarded)
            .filter_map(|e| e.local_port.map(|lp| (lp, e.port)))
            .collect();
        self.entries.retain(|e| e.misses < GRACE_CYCLES);
        if self.selected >= self.entries.len() {
            self.selected = self.entries.len().saturating_sub(1);
        }
        ReconcileOutcome {
            changed: changed || self.entries.len() != before,
            torn_down,
        }
    }

    /// Drop a port from the registry and suppress re-detection, so a still
    /// listening port the poll keeps seeing doesn't pop back into the list.
    pub fn remove(&mut self, port: u16) {
        self.entries.retain(|e| e.port != port);
        self.dismissed.insert(port);
        if self.selected >= self.entries.len() {
            self.selected = self.entries.len().saturating_sub(1);
        }
    }

    /// Stop forwarding `port`, reporting its `(local, remote)` tunnel for
    /// teardown. The server is still listening, so unlike [`Self::remove`] the
    /// row stays and the port is not suppressed — `x` is labelled "stop
    /// forwarding" there, and the user must be able to forward it again.
    pub fn stop_forwarding(&mut self, port: u16) -> Option<(u16, u16)> {
        let e = self.entries.iter_mut().find(|e| e.port == port)?;
        let local = e.local_port.take()?;
        e.forwarded = false;
        Some((local, port))
    }

    /// Record that a remote port is now forwarded to `local_port`.
    pub fn mark_forwarded(&mut self, port: u16, local_port: u16) {
        if let Some(e) = self.entries.iter_mut().find(|e| e.port == port) {
            e.forwarded = true;
            e.local_port = Some(local_port);
        }
    }

    pub fn move_selection(&mut self, delta: i32) {
        if self.entries.is_empty() {
            return;
        }
        let n = self.entries.len() as i32;
        self.selected = (self.selected as i32 + delta).rem_euclid(n) as usize;
    }

    pub fn select_index(&mut self, idx: usize) {
        if idx < self.entries.len() {
            self.selected = idx;
        }
    }

    /// Index of the body row at `y`, if any (for click selection).
    pub fn row_at(&self, x: u16, y: u16) -> Option<usize> {
        self.row_rects
            .iter()
            .find(|(r, _)| rect_has(*r, x, y))
            .map(|(_, idx)| *idx)
    }
}

impl Default for PortsPanel {
    fn default() -> Self {
        Self::new()
    }
}

fn rect_has(r: Rect, x: u16, y: u16) -> bool {
    r.width > 0 && r.height > 0 && x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
}

impl Widget for &mut PortsPanel {
    fn render(self, area: Rect, buf: &mut Buffer) {
        self.last_area = area;
        self.row_rects.clear();
        if area.width == 0 || area.height == 0 {
            return;
        }
        let accent = if self.focus_gradient {
            crate::gradient::rgb_color(crate::gradient::PANEL_TITLE_FG)
        } else {
            self.theme.ui(Color::White)
        };
        let orange = crate::gradient::rgb_color(crate::gradient::GRAD_TR);
        buf.set_style(area, Style::default().bg(self.theme.editor_bg()));

        if self.entries.is_empty() {
            Paragraph::new(Line::from(Span::styled(
                "No ports detected yet. Croft watches this session and surfaces a port the moment one starts listening.",
                Style::default().fg(self.theme.ui(COLOR_DIM)),
            )))
            .render(area, buf);
            return;
        }

        // Header row.
        let header = Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: 1,
        };
        buf.set_string(
            header.x + 1,
            header.y,
            format!(
                "{:<8}{:<24}{:<18}{}",
                "PORT", "ADDRESS", "PROCESS", "ORIGIN"
            ),
            Style::default()
                .fg(self.theme.ui(COLOR_HEAD))
                .add_modifier(Modifier::BOLD),
        );

        // Body rows; reserve the last line for the keyboard hints.
        let body_h = area.height.saturating_sub(2);
        self.first_row_y = area.y + 1;
        if self.selected >= self.entries.len() {
            self.selected = self.entries.len() - 1;
        }
        // Scroll so the selection is always on screen: `f` and `x` act on it,
        // and acting on a row the user cannot see is how a tunnel gets torn
        // down by surprise.
        let body_h_usize = body_h as usize;
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if body_h_usize > 0 && self.selected >= self.scroll + body_h_usize {
            self.scroll = self.selected + 1 - body_h_usize;
        }
        // And never scrolled past the last full page, or a panel that grew (the
        // splitter dragged up, the terminal maximized) or a list that shrank
        // would draw a handful of rows into a tall body with the rest above the
        // top, invisible and unclickable.
        self.scroll = self
            .scroll
            .min(self.entries.len().saturating_sub(body_h_usize));
        for (row, (idx, entry)) in self
            .entries
            .iter()
            .enumerate()
            .skip(self.scroll)
            .take(body_h_usize)
            .enumerate()
        {
            let y = self.first_row_y + row as u16;
            let r = Rect {
                x: area.x,
                y,
                width: area.width,
                height: 1,
            };
            self.row_rects.push((r, idx));
            let selected = idx == self.selected;
            let hovered =
                crate::widgets::hover::row_hover_bg(r, self.hover_pointer, self.theme).is_some();
            if selected || hovered {
                buf.set_style(
                    r,
                    Style::default().bg(crate::gradient::rgb_color(crate::gradient::POPUP_SEL_BG)),
                );
            }

            // Green only while the port is actually listening; a stopped server
            // greys out (and a non-forwarded one is then dropped by the poll).
            let dot_color = if entry.live {
                self.theme.ui(COLOR_LIVE)
            } else {
                self.theme.ui(COLOR_DIM)
            };
            let addr = match (&entry.origin, entry.local_port) {
                (PortOrigin::Remote(_), Some(lp)) if lp != entry.port => {
                    format!("localhost:{lp} \u{2190} :{}", entry.port)
                }
                _ => format!("localhost:{}", entry.port),
            };
            let origin = match &entry.origin {
                PortOrigin::Local => {
                    Span::styled("local", Style::default().fg(self.theme.ui(COLOR_DIM)))
                }
                PortOrigin::Remote(host) => {
                    let fg = if entry.forwarded {
                        orange
                    } else {
                        self.theme.ui(COLOR_DIM)
                    };
                    Span::styled(format!("\u{21c4} {host}"), Style::default().fg(fg))
                }
            };
            let spans = vec![
                Span::styled("\u{25cf} ", Style::default().fg(dot_color)),
                Span::styled(
                    format!("{:<6}", entry.port),
                    Style::default().fg(accent).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("{addr:<24}"),
                    Style::default().fg(self.theme.ui(COLOR_MSG)),
                ),
                Span::styled(
                    format!("{:<18}", entry.process.as_deref().unwrap_or("")),
                    Style::default().fg(self.theme.ui(COLOR_DIM)),
                ),
                origin,
            ];
            Paragraph::new(Line::from(spans)).render(
                Rect {
                    x: r.x + 1,
                    y: r.y,
                    width: r.width.saturating_sub(1),
                    height: 1,
                },
                buf,
            );
        }

        // Keyboard hint line on the last row. `x` never touches the server — it
        // stops croft forwarding/watching the port — so the label says so.
        if area.height >= 2 {
            let sel = self.selected();
            let forwarded = sel.map(|e| e.forwarded).unwrap_or(false);
            let remote_pending = sel
                .map(|e| e.origin.is_remote() && !e.forwarded)
                .unwrap_or(false);
            let hint = if forwarded {
                "  ⏎ open in browser   ·   c copy address   ·   x stop forwarding"
            } else if remote_pending {
                "  f forward   ·   c copy address   ·   x dismiss"
            } else {
                "  ⏎ open in browser   ·   c copy address   ·   x dismiss"
            };
            buf.set_stringn(
                area.x,
                area.y + area.height - 1,
                hint,
                area.width as usize,
                Style::default().fg(self.theme.ui(COLOR_DIM)),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(p: &mut PortsPanel, w: u16, h: u16) -> String {
        let area = Rect {
            x: 0,
            y: 0,
            width: w,
            height: h,
        };
        let mut buf = Buffer::empty(area);
        p.render(area, &mut buf);
        let mut out = String::new();
        for y in 0..h {
            for x in 0..w {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn empty_shows_a_hint() {
        let mut p = PortsPanel::new();
        assert!(render(&mut p, 80, 5).contains("No ports detected yet"));
    }

    #[test]
    fn forwarded_count_counts_only_tunneled_ports() {
        // The Remote activity badge mirrors VS Code's Ports view: it counts
        // ports actually forwarded home, not every detected port.
        let mut p = PortsPanel::new();
        p.upsert(3000, None, None, PortOrigin::Remote("box".into()));
        p.upsert(8080, None, None, PortOrigin::Remote("box".into()));
        assert_eq!(p.forwarded_count(), 0, "detected but not yet forwarded");
        p.mark_forwarded(3000, 53000);
        assert_eq!(p.forwarded_count(), 1, "one tunnel up");
    }

    #[test]
    fn upsert_is_idempotent_per_port() {
        let mut p = PortsPanel::new();
        assert!(p.upsert(3000, None, None, PortOrigin::Local));
        assert!(
            !p.upsert(3000, None, None, PortOrigin::Local),
            "same port again"
        );
        assert_eq!(p.len(), 1);
    }

    #[test]
    fn upsert_fills_in_missing_metadata() {
        let mut p = PortsPanel::new();
        p.upsert(3000, None, None, PortOrigin::Local);
        p.upsert(3000, None, Some("node".into()), PortOrigin::Local);
        assert_eq!(p.entries[0].process.as_deref(), Some("node"));
    }

    #[test]
    fn local_port_opens_directly() {
        let mut p = PortsPanel::new();
        p.upsert(
            3000,
            Some("http://localhost:3000/app".into()),
            None,
            PortOrigin::Local,
        );
        assert_eq!(
            p.selected().unwrap().open_url().as_deref(),
            Some("http://localhost:3000/app")
        );
    }

    #[test]
    fn remote_port_has_no_open_url_until_forwarded() {
        let mut p = PortsPanel::new();
        p.upsert(3000, None, None, PortOrigin::Remote("build-box".into()));
        assert!(p.selected().unwrap().open_url().is_none());
        p.mark_forwarded(3000, 3001);
        assert_eq!(
            p.selected().unwrap().open_url().as_deref(),
            Some("http://127.0.0.1:3001/")
        );
    }

    #[test]
    fn renders_port_and_forwarded_origin() {
        let mut p = PortsPanel::new();
        p.upsert(
            3000,
            None,
            Some("node".into()),
            PortOrigin::Remote("build-box".into()),
        );
        p.mark_forwarded(3000, 3000);
        let text = render(&mut p, 80, 5);
        assert!(text.contains("3000"), "shows port: {text:?}");
        assert!(text.contains("build-box"), "shows host: {text:?}");
    }

    #[test]
    fn a_polled_port_still_toasts_when_the_scrape_confirms_it() {
        let mut p = PortsPanel::new();
        // The socket poll adds it first (silent, no toast).
        p.upsert(8000, None, Some("python3".into()), PortOrigin::Local);
        // The output scrape then confirms it's browser-facing: toast once.
        assert!(
            p.note_scrape_toast(8000),
            "scrape toasts even though the poll added the port first"
        );
        assert!(!p.note_scrape_toast(8000), "but only once per detection");
    }

    #[test]
    fn a_restarted_port_toasts_again_after_reconcile_drops_it() {
        let mut p = PortsPanel::new();
        p.upsert(8000, None, None, PortOrigin::Local);
        assert!(p.note_scrape_toast(8000));
        // Server dies → reconciled out of the list.
        p.reconcile_live(&HashSet::new());
        p.reconcile_live(&HashSet::new());
        assert_eq!(p.len(), 0);
        // Restart: re-detected as a fresh entry, so it toasts again.
        p.upsert(8000, None, None, PortOrigin::Local);
        assert!(p.note_scrape_toast(8000), "a restart toasts again");
    }

    #[test]
    fn a_dismissed_port_does_not_come_back_on_the_next_poll() {
        let mut p = PortsPanel::new();
        p.upsert(8000, None, None, PortOrigin::Local);
        p.remove(8000);
        assert_eq!(p.len(), 0);
        // The socket poll re-reports the still-listening port; it must stay gone.
        assert!(!p.upsert(8000, None, Some("python3".into()), PortOrigin::Local));
        assert_eq!(p.len(), 0, "dismissed port stays dismissed");
    }

    #[test]
    fn a_port_that_stops_listening_goes_not_live_then_drops() {
        let mut p = PortsPanel::new();
        p.upsert(8000, None, None, PortOrigin::Local);
        assert!(
            p.selected().unwrap().live,
            "a freshly detected port is live"
        );
        // A poll cycle that no longer sees it: marked not-live but kept one
        // grace cycle so a brief blip doesn't flicker the row away.
        p.reconcile_live(&HashSet::new());
        assert!(
            !p.selected().unwrap().live,
            "absent from the poll => not live"
        );
        assert_eq!(p.len(), 1, "kept for one grace cycle");
        // Still gone on the next poll: dropped from the list.
        p.reconcile_live(&HashSet::new());
        assert_eq!(p.len(), 0, "dropped after the grace cycle");
    }

    #[test]
    fn a_stopped_forwarded_port_is_dropped_and_its_tunnel_torn_down() {
        let mut p = PortsPanel::new();
        p.upsert(3000, None, None, PortOrigin::Remote("box".into()));
        p.mark_forwarded(3000, 3001);
        // First poll without it: greyed but kept for the grace cycle.
        let r1 = p.reconcile_live(&HashSet::new());
        assert_eq!(p.len(), 1);
        assert!(!p.selected().unwrap().live);
        assert!(r1.torn_down.is_empty(), "not torn down during grace");
        // Still gone: dropped, and its (local, remote) tunnel reported so the
        // app can `ssh -O cancel` it.
        let r2 = p.reconcile_live(&HashSet::new());
        assert_eq!(p.len(), 0, "the row follows the server, forward or not");
        assert_eq!(r2.torn_down, vec![(3001, 3000)]);
    }

    #[test]
    fn reconcile_revives_a_port_seen_again() {
        let mut p = PortsPanel::new();
        p.upsert(8000, None, None, PortOrigin::Local);
        p.reconcile_live(&HashSet::new());
        assert!(!p.selected().unwrap().live);
        let mut live = HashSet::new();
        live.insert(8000);
        p.reconcile_live(&live);
        assert!(
            p.selected().unwrap().live,
            "seen again => live, miss count reset"
        );
        assert_eq!(p.len(), 1);
    }

    /// `x` on a forwarded port is labelled "stop forwarding", and the status
    /// says the server keeps running. Blacklisting the port there means the
    /// still-listening server can never be forwarded again this session.
    #[test]
    fn stopping_a_forward_leaves_the_port_forwardable_again() {
        let mut p = PortsPanel::new();
        p.upsert(3000, None, None, PortOrigin::Remote("box".into()));
        p.mark_forwarded(3000, 3001);
        assert_eq!(p.stop_forwarding(3000), Some((3001, 3000)));
        let e = p.selected().expect("the row stays, the server is still up");
        assert!(!e.forwarded, "but no longer marked forwarded");
        assert_eq!(e.local_port, None, "and its stale local port is cleared");
        assert_eq!(p.forwarded_count(), 0);
    }

    /// The body renders `body_h` rows. Without a scroll offset, moving the
    /// selection past that leaves nothing highlighted while `f` and `x` still
    /// act on the invisible entry.
    #[test]
    fn the_selected_row_scrolls_into_view() {
        let mut p = PortsPanel::new();
        for port in [3000u16, 3001, 3002, 3003, 3004, 3005, 3006] {
            p.upsert(port, None, None, PortOrigin::Local);
        }
        // Height 5 = header + 3 body rows + hint line.
        p.select_index(6);
        let out = render(&mut p, 80, 5);
        assert!(out.contains("3006"), "the selected row must be on screen");
        assert!(
            p.row_at(1, p.first_row_y).is_some(),
            "and the visible rows must still hit-test"
        );
        // Clicking the top visible row must select the entry actually drawn
        // there, not entry 0.
        let idx = p.row_at(1, p.first_row_y).unwrap();
        assert_eq!(
            p.entries[idx].port, 3004,
            "row rects must carry the scrolled index"
        );
    }

    /// The panel can grow (splitter dragged up, terminal maximized) and the
    /// list can shrink. A scroll offset left where it was then draws a handful
    /// of rows into a tall body with the rest above the top, invisible and
    /// unclickable, since `row_rects` only carries what was drawn.
    #[test]
    fn the_scroll_offset_shrinks_when_the_panel_grows() {
        let mut p = PortsPanel::new();
        for port in [3000u16, 3001, 3002, 3003, 3004, 3005, 3006] {
            p.upsert(port, None, None, PortOrigin::Local);
        }
        p.select_index(6);
        render(&mut p, 80, 5); // 3 body rows: scrolled to the bottom.
        let tall = render(&mut p, 80, 20); // 18 body rows: everything fits.
        assert!(
            tall.contains("3000"),
            "the first row must come back into view"
        );
        assert!(tall.contains("3006"), "and the last one must stay");
        assert_eq!(
            p.row_at(1, p.first_row_y),
            Some(0),
            "the top row must hit-test as the first entry"
        );
    }

    #[test]
    fn selection_wraps() {
        let mut p = PortsPanel::new();
        p.upsert(3000, None, None, PortOrigin::Local);
        p.upsert(4000, None, None, PortOrigin::Local);
        p.move_selection(-1);
        assert_eq!(p.selected().unwrap().port, 4000);
        p.move_selection(1);
        assert_eq!(p.selected().unwrap().port, 3000);
    }
}
