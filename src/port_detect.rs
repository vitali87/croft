//! Port detection for the PORTS panel and the auto-forward toast.
//!
//! Two independent sources feed the same [`PortHit`] stream:
//!
//! * **Output scrape** — [`PortSniffer::sniff`] runs on every PTY chunk in the
//!   terminal reader thread (right beside `sniff_bracketed_paste_mode`),
//!   matching the `http://localhost:PORT` banners and `listening on :PORT`
//!   lines that dev servers print. Cheap: two precompiled regexes over a small
//!   carry buffer. Emission is position-based — only a match in the freshly
//!   read bytes fires, so the carried tail is never re-emitted, yet a banner
//!   reprinted by a restarted server is. Deduping live ports is the app's job.
//! * **Socket poll** — [`poll_listeners`] shells out to `lsof`/`ss` on a low
//!   cadence to catch servers that bind a port without announcing it. Scoped to
//!   the shell's own process subtree so system daemons don't leak in. On a
//!   remote session this runs on the remote (remote croft polls its own
//!   sockets), which is exactly the host whose ports we want to forward.
//!
//! The detector never decides what to do with a port: it reports, the app
//! deduplicates against its live registry and offers the forward.

use std::collections::HashSet;
use std::process::Command;
use std::sync::LazyLock;

use regex::Regex;

/// A loopback listener we noticed, from either source. The app turns this into
/// a registry entry and (on a remote session) an offer to forward.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortHit {
    pub port: u16,
    /// The full URL when scraped from one (`http://localhost:3000/`), so the
    /// app can preserve scheme and path when opening. `None` for socket-poll
    /// hits and bare `listening on :PORT` lines, where the app assumes http.
    pub url: Option<String>,
    /// Owning process name when known (socket poll), else `None`.
    pub process: Option<String>,
}

impl PortHit {
    fn bare(port: u16) -> Self {
        Self {
            port,
            url: None,
            process: None,
        }
    }
}

/// `http://localhost:3000/path` — captures the port and keeps the whole match
/// so the app can open the exact URL. Hosts are restricted to the loopback
/// spellings a local dev server uses; a public host isn't ours to forward.
static URL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bhttps?://(?:localhost|127\.0\.0\.1|0\.0\.0\.0|\[::1\])(?::(\d{1,5}))?(?:/[^\s'\x22)\]]*)?")
        .expect("port URL regex")
});

/// `Listening on port 8080`, `serving at :8080`, `bound to 0.0.0.0:8080` — the
/// announce lines that carry no scheme. Kept deliberately tight (an explicit
/// verb) so a stray number in ordinary output isn't mistaken for a port.
static BARE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:listening|serving|started|running|bound)\b[^\n]{0,40}?[:\s](\d{2,5})\b")
        .expect("bare port regex")
});

/// A general http(s) URL token, for Cmd/Ctrl+click in the terminal (any host,
/// not just loopback). Stops at whitespace and the brackets/quotes that
/// commonly surround a URL in log output.
static CLICK_URL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)\bhttps?://[^\s'"<>()\[\]]+"#).expect("click url regex"));

/// The http(s) URL covering character index `col` in `text`, if any — used to
/// resolve which printed link a Cmd/Ctrl+click landed on. Trailing sentence
/// punctuation is trimmed so a link at the end of a line doesn't grab the dot.
pub fn url_at(text: &str, col: usize) -> Option<String> {
    for m in CLICK_URL_RE.find_iter(text) {
        let start = text[..m.start()].chars().count();
        let end = start + m.as_str().chars().count();
        if col >= start && col < end {
            let url = m.as_str().trim_end_matches(['.', ',', ';', ':', '!', '?']);
            return Some(url.to_string());
        }
    }
    None
}

/// The loopback port of `url` when the whole string is a loopback dev-server
/// URL (`http://localhost:3000/...`), else `None`. Lets a click decide whether
/// a URL is a remote port to forward or an ordinary link to open. Defaults to
/// the scheme's port when none is spelled out.
pub fn loopback_url_port(url: &str) -> Option<u16> {
    let cap = URL_RE.captures(url)?;
    if cap.get(0)?.start() != 0 {
        return None;
    }
    cap.get(1).and_then(|m| parse_port(m.as_str())).or(Some(
        if url.to_ascii_lowercase().starts_with("https") {
            443
        } else {
            80
        },
    ))
}

fn parse_port(s: &str) -> Option<u16> {
    s.parse::<u32>()
        .ok()
        .filter(|p| *p >= 1 && *p <= 65535)
        .map(|p| p as u16)
}

/// Stateful scraper owned by a terminal's reader thread. Carries a short tail of
/// already-scanned text so a banner split across two `read()`s still matches.
/// Emission is position-based: only a match reaching into the *new* chunk fires,
/// so the carried tail is never re-emitted, yet a banner printed afresh (a
/// restarted server) is. Dedup against the live set is the app's job, not the
/// sniffer's — which is what lets a restart re-toast.
pub struct PortSniffer {
    carry: String,
}

/// How many trailing chars of the combined text to keep for the next scan,
/// covering a URL straddling a read boundary without rescanning whole chunks.
const CARRY_BYTES: usize = 256;

impl PortSniffer {
    pub fn new() -> Self {
        Self {
            carry: String::new(),
        }
    }

    /// Scan one PTY chunk, pushing a [`PortHit`] for each port whose match
    /// extends past the carried prefix (i.e. lives in the new bytes). The send
    /// is best-effort: a dropped hit just means the app learns of the port from
    /// the next line or the socket poll.
    pub fn sniff(&mut self, chunk: &[u8], tx: &std::sync::mpsc::Sender<PortHit>) {
        // ANSI colour codes around a URL are outside the match, and a colour
        // reset mid-URL is vanishingly rare, so a string scan is enough without
        // a full terminal parse.
        let carry_len = self.carry.len();
        let text = format!("{}{}", self.carry, String::from_utf8_lossy(chunk));
        for (end, hit) in scan_with_pos(&text) {
            // A match ending within the carried prefix was already scanned last
            // call; only emit one that reaches into the freshly read bytes.
            if end > carry_len {
                let _ = tx.send(hit);
            }
        }
        // Keep the tail (on a char boundary) for the next call's split coverage.
        let mut keep_from = text.len().saturating_sub(CARRY_BYTES);
        while keep_from < text.len() && !text.is_char_boundary(keep_from) {
            keep_from += 1;
        }
        self.carry = text[keep_from..].to_string();
    }
}

impl Default for PortSniffer {
    fn default() -> Self {
        Self::new()
    }
}

/// Position-free view of [`scan_with_pos`] for the tests. URL hits win over bare
/// hits for the same port (the URL carries scheme and path).
#[cfg(test)]
fn scan(text: &str) -> Vec<PortHit> {
    scan_with_pos(text).into_iter().map(|(_, h)| h).collect()
}

/// Like [`scan`] but pairs each hit with the byte offset where its match ends,
/// so [`PortSniffer::sniff`] can tell a hit in the freshly read bytes from one
/// that only lives in the carried-over prefix.
fn scan_with_pos(text: &str) -> Vec<(usize, PortHit)> {
    let mut out: Vec<(usize, PortHit)> = Vec::new();
    for cap in URL_RE.captures_iter(text) {
        let m = cap.get(0).expect("group 0 always present");
        let whole = m.as_str().to_string();
        // Explicit port, or the scheme's default (http=80, https=443).
        let port = cap.get(1).and_then(|g| parse_port(g.as_str())).unwrap_or(
            if whole.to_ascii_lowercase().starts_with("https") {
                443
            } else {
                80
            },
        );
        out.push((
            m.end(),
            PortHit {
                port,
                url: Some(whole),
                process: None,
            },
        ));
    }
    for cap in BARE_RE.captures_iter(text) {
        if let Some(g) = cap.get(1)
            && let Some(port) = parse_port(g.as_str())
            && !out.iter().any(|(_, h)| h.port == port)
        {
            out.push((g.end(), PortHit::bare(port)));
        }
    }
    out
}

/// PIDs in `root`'s process subtree (inclusive), from one `ps` snapshot. Used
/// to scope the socket poll to the shell's own children so we don't surface
/// every system daemon listening on loopback. Empty on `ps` failure (the poll
/// then reports nothing, falling back to the output scrape).
fn descendant_pids(root: i32) -> HashSet<i32> {
    let mut subtree: HashSet<i32> = HashSet::new();
    subtree.insert(root);
    let Ok(out) = Command::new("ps").args(["-axo", "pid=,ppid="]).output() else {
        return subtree;
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let pairs: Vec<(i32, i32)> = text
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let pid = it.next()?.parse().ok()?;
            let ppid = it.next()?.parse().ok()?;
            Some((pid, ppid))
        })
        .collect();
    // Iterate to a fixed point: a child whose parent joined the set joins too.
    loop {
        let before = subtree.len();
        for (pid, ppid) in &pairs {
            if subtree.contains(ppid) {
                subtree.insert(*pid);
            }
        }
        if subtree.len() == before {
            break;
        }
    }
    subtree
}

/// Listening loopback TCP ports owned by `shell_pid`'s subtree, via `lsof`
/// (macOS + Linux) with an `ss` fallback (Linux without lsof). Best-effort:
/// any failure yields an empty list and the output scrape carries the load.
pub fn poll_listeners(shell_pid: i32) -> Vec<PortHit> {
    let pids = descendant_pids(shell_pid);
    if let Some(hits) = poll_via_lsof(&pids) {
        return hits;
    }
    poll_via_ss(&pids).unwrap_or_default()
}

fn poll_via_lsof(pids: &HashSet<i32>) -> Option<Vec<PortHit>> {
    if pids.is_empty() {
        return Some(Vec::new());
    }
    let pid_arg = pids
        .iter()
        .map(|p| p.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let out = Command::new("lsof")
        .args(["-nP", "-iTCP", "-sTCP:LISTEN", "-a", "-p", &pid_arg])
        .output()
        .ok()?;
    if !out.status.success() && out.stdout.is_empty() {
        return None;
    }
    Some(parse_lsof(&String::from_utf8_lossy(&out.stdout)))
}

/// Parse `lsof -nP -iTCP -sTCP:LISTEN` rows. Field 0 is the command; the NAME
/// column (`*:3000`, `127.0.0.1:3000`) is followed by a `(LISTEN)` token, so we
/// scan the row for the first field that parses as a loopback/wildcard address.
fn parse_lsof(text: &str) -> Vec<PortHit> {
    let mut out: Vec<PortHit> = Vec::new();
    for line in text.lines().skip(1) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let Some(cmd) = fields.first() else { continue };
        if let Some(port) = fields.iter().find_map(|f| loopback_port(f))
            && !out.iter().any(|h| h.port == port)
        {
            out.push(PortHit {
                port,
                url: None,
                process: Some((*cmd).to_string()),
            });
        }
    }
    out
}

fn poll_via_ss(pids: &HashSet<i32>) -> Option<Vec<PortHit>> {
    let out = Command::new("ss").args(["-tlnpH"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(parse_ss(&String::from_utf8_lossy(&out.stdout), pids))
}

/// Parse `ss -tlnpH` rows. The local-address column is field 3 and the process
/// column embeds `pid=NNN`; we keep only rows whose pid is in the subtree.
fn parse_ss(text: &str, pids: &HashSet<i32>) -> Vec<PortHit> {
    let mut out: Vec<PortHit> = Vec::new();
    for line in text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let Some(local) = fields.get(3) else { continue };
        let Some(port) = loopback_port(local) else {
            continue;
        };
        let proc = fields
            .last()
            .and_then(|f| ss_pid(f).map(|pid| (pid, ss_proc_name(f))));
        match proc {
            Some((pid, name))
                if (pids.is_empty() || pids.contains(&pid))
                    && !out.iter().any(|h| h.port == port) =>
            {
                out.push(PortHit {
                    port,
                    url: None,
                    process: name,
                });
            }
            _ => {}
        }
    }
    out
}

fn ss_pid(field: &str) -> Option<i32> {
    let start = field.find("pid=")? + 4;
    let rest = &field[start..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

fn ss_proc_name(field: &str) -> Option<String> {
    let start = field.find("((\"")? + 3;
    let rest = &field[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// The port from a `host:port` address, but only for loopback / wildcard hosts
/// (`*`, `0.0.0.0`, `127.0.0.1`, `[::]`, `[::1]`, `localhost`). A bound public
/// interface isn't a dev server we should be offering to tunnel home.
fn loopback_port(addr: &str) -> Option<u16> {
    let (host, port) = addr.rsplit_once(':')?;
    let host = host.trim_matches(|c| c == '[' || c == ']');
    let loopback = matches!(
        host,
        "" | "*" | "0.0.0.0" | "127.0.0.1" | "::" | "::1" | "localhost"
    );
    if !loopback {
        return None;
    }
    parse_port(port)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_a_vite_banner_url_with_port_and_path() {
        let hits = scan("  VITE v5.2  ready  ->  Local: http://localhost:3000/app");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].port, 3000);
        assert_eq!(hits[0].url.as_deref(), Some("http://localhost:3000/app"));
    }

    #[test]
    fn scans_a_bare_listening_line() {
        let hits = scan("INFO server listening on port 8080");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].port, 8080);
        assert!(hits[0].url.is_none());
    }

    #[test]
    fn url_wins_over_bare_for_the_same_port() {
        let hits = scan("listening on http://127.0.0.1:5000 — now serving 5000");
        assert_eq!(hits.iter().filter(|h| h.port == 5000).count(), 1);
        assert!(hits.iter().any(|h| h.port == 5000 && h.url.is_some()));
    }

    #[test]
    fn ignores_public_hosts() {
        assert!(scan("see https://example.com:8443/docs").is_empty());
    }

    #[test]
    fn https_url_without_explicit_port_defaults_to_443() {
        let hits = scan("ready at https://localhost/");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].port, 443);
    }

    #[test]
    fn sniffer_does_not_double_emit_a_banner_via_the_carry() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut s = PortSniffer::new();
        s.sniff(b"up at http://localhost:3000/\n", &tx);
        // The banner now lives only in the carried tail; a follow-up chunk that
        // doesn't reprint it must not re-emit the port.
        s.sniff(b"$ \n", &tx);
        let got: Vec<_> = rx.try_iter().collect();
        assert_eq!(got.len(), 1, "carried banner is not re-emitted: {got:?}");
        assert_eq!(got[0].port, 3000);
    }

    #[test]
    fn sniffer_re_emits_a_port_after_the_server_stops_and_restarts() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut s = PortSniffer::new();
        s.sniff(b"Serving on http://localhost:8000/\n", &tx);
        // Server killed: a chunk with no port clears the suppression.
        s.sniff(b"^C\nKeyboard interrupt received, exiting.\n", &tx);
        // Restarted: the same port must be reported again so the toast fires.
        s.sniff(b"Serving on http://localhost:8000/\n", &tx);
        let got: Vec<_> = rx.try_iter().collect();
        assert_eq!(got.len(), 2, "restart re-emits the port: {got:?}");
        assert!(got.iter().all(|h| h.port == 8000));
    }

    #[test]
    fn sniffer_catches_a_port_split_across_chunks() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut s = PortSniffer::new();
        s.sniff(b"Local:  http://localho", &tx);
        s.sniff(b"st:4173/ ready", &tx);
        let got: Vec<_> = rx.try_iter().collect();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].port, 4173);
    }

    #[test]
    fn parses_lsof_loopback_listeners_only() {
        let sample = "COMMAND PID USER FD TYPE DEVICE SIZE/OFF NODE NAME\n\
                      node 123 me 21u IPv4 0xabc 0t0 TCP 127.0.0.1:3000 (LISTEN)\n\
                      ssh 99 me 5u IPv4 0xdef 0t0 TCP 10.0.0.2:22 (LISTEN)\n";
        let hits = parse_lsof(sample);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].port, 3000);
        assert_eq!(hits[0].process.as_deref(), Some("node"));
    }

    #[test]
    fn parses_ss_rows_scoped_to_pids() {
        let sample = "LISTEN 0 511 127.0.0.1:5432 0.0.0.0:* users:((\"postgres\",pid=4242,fd=7))\n\
                      LISTEN 0 128 0.0.0.0:22 0.0.0.0:* users:((\"sshd\",pid=1,fd=3))\n";
        let mut pids = HashSet::new();
        pids.insert(4242);
        let hits = parse_ss(sample, &pids);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].port, 5432);
        assert_eq!(hits[0].process.as_deref(), Some("postgres"));
    }

    #[test]
    fn url_at_finds_the_link_under_the_cursor() {
        let line = "  visit http://localhost:3000/app for the UI";
        // Column inside the URL.
        assert_eq!(
            url_at(line, 20).as_deref(),
            Some("http://localhost:3000/app")
        );
        // Column in the surrounding prose: no URL.
        assert!(url_at(line, 2).is_none());
    }

    #[test]
    fn url_at_trims_trailing_punctuation() {
        let line = "see https://example.com/docs.";
        assert_eq!(
            url_at(line, 10).as_deref(),
            Some("https://example.com/docs")
        );
    }

    #[test]
    fn loopback_url_port_classifies_dev_servers() {
        assert_eq!(loopback_url_port("http://localhost:3000/app"), Some(3000));
        assert_eq!(loopback_url_port("https://127.0.0.1/"), Some(443));
        assert_eq!(loopback_url_port("http://localhost/"), Some(80));
        assert!(loopback_url_port("https://example.com:8443/").is_none());
    }

    #[test]
    fn loopback_port_rejects_public_bind() {
        assert_eq!(loopback_port("127.0.0.1:3000"), Some(3000));
        assert_eq!(loopback_port("*:8080"), Some(8080));
        assert_eq!(loopback_port("[::1]:9000"), Some(9000));
        assert_eq!(loopback_port("192.168.1.5:3000"), None);
    }
}
