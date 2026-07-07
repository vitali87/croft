//! iTerm2-style terminal triggers: user regexes watched against terminal
//! output, each firing an action. The pragmatic action set that covers real
//! trigger usage (per iTerm2 / kitty marks): **highlight** (recolour every
//! visible occurrence, live, scrollback included), **notify** (status-bar
//! notice with `\0`..`\9` capture interpolation), and **bell** (status-bar
//! bell notice). Auto-respond / run-command actions are deliberately absent:
//! they are the classic security footgun (hostile output typing into your
//! shell) and iTerm2 itself ships them behind warnings.
//!
//! Config lives at `~/.config/croft/triggers.json` (same tolerant JSONC
//! loading as `keybindings.json` / `snippets.json`; a bad row skips that row,
//! never blocks startup), reloaded on save.
//!
//! Evaluation model (iTerm2's, the safe one): highlight triggers are painted
//! at render time over the visible rows, so they cost nothing when the pane
//! is idle and persist into scrollback for free; notify / bell triggers are
//! matched once per completed output line by a byte-stream scanner in the
//! PTY reader thread, with escape sequences stripped, a `\r` treated as a
//! line rewrite (progress bars never spam), a hard per-line cap, and the
//! whole scan skipped while the pane is in the alternate screen (a
//! full-screen app owns the bytes; iTerm2 skips those too).

use regex::Regex;
use std::path::{Path, PathBuf};

/// Hard cap on the completed-line buffer the notify/bell scanner matches
/// against; bytes past it are dropped (iTerm2 caps at the last few wrapped
/// rows for the same reason: never regex a gigabyte `cat`).
pub const LINE_CAP: usize = 2048;

/// What a matched trigger does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TriggerAction {
    /// Recolour the matched span on every visible row (fg/bg from the row).
    Highlight,
    /// Status-bar notice; the message template interpolates `\0`..`\9`.
    Notify,
    /// Status-bar bell notice.
    Bell,
}

/// One user trigger, compiled and ready to match.
#[derive(Clone, Debug)]
pub struct Trigger {
    pub regex: Regex,
    pub action: TriggerAction,
    /// Highlight colours (either may be absent; the other side of the cell
    /// style is left alone).
    pub fg: Option<(u8, u8, u8)>,
    pub bg: Option<(u8, u8, u8)>,
    /// Notify message template; `\0` = whole match, `\1`..`\9` = groups.
    pub message: Option<String>,
}

/// The user's trigger list. Shared read-only between the app, the render
/// loop, and every pane's reader thread.
#[derive(Clone, Debug, Default)]
pub struct TriggerSet {
    pub triggers: Vec<Trigger>,
}

/// One highlight span on a row, in char indices (the render colmap
/// translates to grid columns).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HighlightSpan {
    pub start: usize,
    pub len: usize,
    pub fg: Option<(u8, u8, u8)>,
    pub bg: Option<(u8, u8, u8)>,
}

/// One notify/bell firing, drained by the app into the status bar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TriggerHit {
    pub action: TriggerAction,
    pub message: String,
}

pub fn triggers_path() -> PathBuf {
    crate::prefs::config_dir().join("triggers.json")
}

/// One raw `triggers.json` row before compilation. Unknown actions, bad
/// regexes and disabled rows drop out in [`TriggerSet::from_json`].
#[derive(serde::Deserialize)]
struct TriggerRow {
    regex: String,
    action: String,
    #[serde(default)]
    fg: Option<String>,
    #[serde(default)]
    bg: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default = "enabled_default")]
    enabled: bool,
}

fn enabled_default() -> bool {
    true
}

/// `#rrggbb` → bytes; anything else is treated as unset.
fn parse_hex(s: &str) -> Option<(u8, u8, u8)> {
    let hex = s.strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    let n = u32::from_str_radix(hex, 16).ok()?;
    Some(((n >> 16) as u8, (n >> 8) as u8, n as u8))
}

/// The starter file written on first "Open Terminal Triggers (JSON)".
pub const TEMPLATE: &str = r##"// croft terminal triggers: regexes watched on terminal output.
//   "action": "highlight"  recolour every visible occurrence ("fg"/"bg" as #rrggbb)
//             "notify"     status-bar notice ("message" template: \0 whole match, \1-\9 groups)
//             "bell"       status-bar bell notice
// Highlights repaint live on the visible screen and persist into scrollback.
// notify/bell fire once per completed output line (never inside full-screen apps).
// "enabled": false keeps a rule without running it.
[
  { "regex": "\\b(ERROR|FATAL|panicked)\\b", "action": "highlight", "fg": "#ffffff", "bg": "#c0392b" },
  { "regex": "\\bwarning\\b", "action": "highlight", "fg": "#000000", "bg": "#e5c07b", "enabled": false },
  { "regex": "(BUILD|Compiling|error): (.+)", "action": "notify", "message": "\\1: \\2", "enabled": false }
]
"##;

impl TriggerSet {
    /// Load `triggers.json`, ignoring unparsable rows (a bad regex skips that
    /// row, never blocks startup). A missing file yields an empty set.
    pub fn load(path: &Path) -> Self {
        let Ok(json) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        Self::from_json(&json)
    }

    pub fn from_json(json: &str) -> Self {
        let rows: Vec<TriggerRow> =
            serde_json::from_str(&crate::keymap::strip_line_comments(json)).unwrap_or_default();
        let triggers = rows
            .into_iter()
            .filter(|r| r.enabled)
            .filter_map(|r| {
                let action = match r.action.as_str() {
                    "highlight" => TriggerAction::Highlight,
                    "notify" => TriggerAction::Notify,
                    "bell" => TriggerAction::Bell,
                    _ => return None,
                };
                Some(Trigger {
                    regex: Regex::new(&r.regex).ok()?,
                    action,
                    fg: r.fg.as_deref().and_then(parse_hex),
                    bg: r.bg.as_deref().and_then(parse_hex),
                    message: r.message,
                })
            })
            .collect();
        Self { triggers }
    }

    pub fn is_empty(&self) -> bool {
        self.triggers.is_empty()
    }

    /// Whether any trigger paints (drives the render pass).
    pub fn has_highlights(&self) -> bool {
        self.triggers
            .iter()
            .any(|t| t.action == TriggerAction::Highlight)
    }

    /// Whether any trigger fires events (drives the reader-thread scan).
    pub fn has_events(&self) -> bool {
        self.triggers
            .iter()
            .any(|t| t.action != TriggerAction::Highlight)
    }
}

/// Every highlight-trigger match on `line`, as char-index spans.
pub fn highlight_spans(line: &str, set: &TriggerSet) -> Vec<HighlightSpan> {
    let mut out = Vec::new();
    for t in &set.triggers {
        if t.action != TriggerAction::Highlight {
            continue;
        }
        for m in t.regex.find_iter(line) {
            out.push(HighlightSpan {
                start: line[..m.start()].chars().count(),
                len: m.as_str().chars().count(),
                fg: t.fg,
                bg: t.bg,
            });
        }
    }
    out
}

/// Fill a notify/bell message from its template: `\0` = whole match,
/// `\1`..`\9` = capture groups. No template means the whole match.
fn interpolate(template: Option<&str>, caps: &regex::Captures) -> String {
    let whole = caps.get(0).map(|m| m.as_str()).unwrap_or_default();
    let Some(template) = template else {
        return whole.to_string();
    };
    let mut out = String::new();
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\'
            && let Some(d) = chars.peek().and_then(|p| p.to_digit(10))
        {
            chars.next();
            out.push_str(caps.get(d as usize).map(|m| m.as_str()).unwrap_or_default());
        } else {
            out.push(c);
        }
    }
    out
}

/// Byte-stream scanner for notify/bell triggers: strips escape sequences,
/// buffers the current output line (capped), treats `\r` as a line rewrite
/// (reset without matching) and `\n` as line completion (match the event
/// triggers against the finished line).
#[derive(Default)]
pub struct TriggerScanner {
    line: Vec<u8>,
    state: ScanState,
    /// A `\r` was seen and its meaning is still open: followed by `\n` it is
    /// the PTY's plain CRLF line ending (complete the line); followed by
    /// anything else it is an in-place rewrite (reset without matching).
    /// Kept across chunks — a CR can be the last byte of a read.
    cr_pending: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ScanState {
    #[default]
    Ground,
    Esc,
    Csi,
    /// OSC / DCS / APC string bodies: swallowed until BEL or ST.
    InString,
    /// Saw ESC inside a string body (the first half of ST).
    StringEsc,
}

impl TriggerScanner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a raw PTY chunk; completed lines are matched against `set`'s
    /// event triggers and any firings appended to `out`.
    pub fn scan(&mut self, bytes: &[u8], set: &TriggerSet, out: &mut Vec<TriggerHit>) {
        for &b in bytes {
            // Resolve a pending `\r` now that its successor is known: CRLF is
            // the PTY line ending (complete), a lone CR rewrites the line in
            // place (progress bars, spinners — reset without matching, so
            // only text that survives to a newline fires).
            if self.cr_pending && self.state == ScanState::Ground {
                self.cr_pending = false;
                if b == b'\n' {
                    self.complete_line(set, out);
                    continue;
                }
                self.reset_line();
            }
            match self.state {
                ScanState::Ground => match b {
                    0x1b => self.state = ScanState::Esc,
                    b'\n' => self.complete_line(set, out),
                    b'\r' => self.cr_pending = true,
                    0x00..=0x1f | 0x7f => {}
                    _ => {
                        // Past the cap the byte is dropped; the capped prefix
                        // still matches on completion.
                        if self.line.len() < LINE_CAP {
                            self.line.push(b);
                        }
                    }
                },
                ScanState::Esc => {
                    self.state = match b {
                        b'[' => ScanState::Csi,
                        b']' | b'P' | b'X' | b'^' | b'_' => ScanState::InString,
                        _ => ScanState::Ground,
                    };
                }
                ScanState::Csi => {
                    if (0x40..=0x7e).contains(&b) {
                        self.state = ScanState::Ground;
                    }
                }
                ScanState::InString => match b {
                    0x07 => self.state = ScanState::Ground,
                    0x1b => self.state = ScanState::StringEsc,
                    _ => {}
                },
                ScanState::StringEsc => {
                    // `ESC \` (ST) ends the string; anything else is still
                    // string body.
                    self.state = if b == b'\\' {
                        ScanState::Ground
                    } else {
                        ScanState::InString
                    };
                }
            }
        }
    }

    fn reset_line(&mut self) {
        self.line.clear();
    }

    fn complete_line(&mut self, set: &TriggerSet, out: &mut Vec<TriggerHit>) {
        if !self.line.is_empty() {
            let line = String::from_utf8_lossy(&self.line);
            for t in &set.triggers {
                if t.action == TriggerAction::Highlight {
                    continue;
                }
                if let Some(caps) = t.regex.captures(&line) {
                    out.push(TriggerHit {
                        action: t.action,
                        message: interpolate(t.message.as_deref(), &caps),
                    });
                }
            }
        }
        self.reset_line();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(json: &str) -> TriggerSet {
        TriggerSet::from_json(json)
    }

    #[test]
    fn config_rows_parse_and_bad_rows_are_skipped() {
        let s = set(r##"// comment survives
[
  { "regex": "\\bERROR\\b", "action": "highlight", "fg": "#ffffff", "bg": "#c0392b" },
  { "regex": "((broken", "action": "highlight" },
  { "regex": "done", "action": "notify", "message": "finished: \\0" },
  { "regex": "beep", "action": "bell" },
  { "regex": "off", "action": "notify", "enabled": false },
  { "regex": "what", "action": "unknown-action" }
]"##);
        assert_eq!(
            s.triggers.len(),
            3,
            "bad regex, disabled and unknown-action rows must be skipped: {s:?}"
        );
        assert!(s.has_highlights());
        assert!(s.has_events());
        assert_eq!(s.triggers[0].fg, Some((0xff, 0xff, 0xff)));
        assert_eq!(s.triggers[0].bg, Some((0xc0, 0x39, 0x2b)));
        assert!(set("total garbage").is_empty());
        assert!(set("").is_empty());
    }

    #[test]
    fn highlight_spans_are_char_indexed_per_trigger_colours() {
        let s = set(r##"[
  { "regex": "ERROR", "action": "highlight", "bg": "#c0392b" },
  { "regex": "ok", "action": "highlight", "fg": "#00ff00" }
]"##);
        let spans = highlight_spans("naïve ERROR then ok", &s);
        assert_eq!(spans.len(), 2, "{spans:?}");
        let err = &spans[0];
        // "naïve " is 6 chars (7 bytes) — spans must be char-indexed.
        assert_eq!((err.start, err.len), (6, 5));
        assert_eq!(err.bg, Some((0xc0, 0x39, 0x2b)));
        assert_eq!(err.fg, None);
        assert_eq!((spans[1].start, spans[1].len), (17, 2));
    }

    #[test]
    fn scanner_fires_on_completed_lines_with_interpolation() {
        let s = set(r##"[
  { "regex": "BUILD (\\w+)", "action": "notify", "message": "build: \\1" },
  { "regex": "beep", "action": "bell" }
]"##);
        let mut sc = TriggerScanner::new();
        let mut out = Vec::new();
        // Escape sequences are stripped; the line only fires on \n.
        sc.scan(b"\x1b[32mBUILD \x1b[1mFAILED\x1b[0m", &s, &mut out);
        assert!(out.is_empty(), "no newline yet: {out:?}");
        sc.scan(b"\n", &s, &mut out);
        assert_eq!(
            out,
            vec![TriggerHit {
                action: TriggerAction::Notify,
                message: String::from("build: FAILED"),
            }]
        );
        out.clear();
        // \r rewrites the line: the overwritten progress text never fires.
        sc.scan(b"beep 10%\rdone 100%\n", &s, &mut out);
        assert!(out.is_empty(), "text before \\r must not fire: {out:?}");
        // OSC bodies are swallowed even when split across chunks.
        sc.scan(b"\x1b]0;beep title", &s, &mut out);
        sc.scan(b"\x07real beep\n", &s, &mut out);
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].action, TriggerAction::Bell);
        assert_eq!(out[0].message, "beep");
        out.clear();
        // A PTY's plain CRLF line ending completes the line — the CR just
        // before the LF must NOT count as a rewrite (regression: every real
        // shell line ends \r\n, so triggers never fired at all). Also holds
        // when the CR is the last byte of a chunk.
        sc.scan(b"beep one\r\n", &s, &mut out);
        assert_eq!(out.len(), 1, "CRLF must complete the line: {out:?}");
        out.clear();
        sc.scan(b"beep two\r", &s, &mut out);
        sc.scan(b"\n", &s, &mut out);
        assert_eq!(out.len(), 1, "chunk-split CRLF must complete: {out:?}");
    }

    #[test]
    fn scanner_caps_runaway_lines() {
        let s = set(r#"[ { "regex": "needle", "action": "notify" } ]"#);
        let mut sc = TriggerScanner::new();
        let mut out = Vec::new();
        let long = vec![b'x'; LINE_CAP + 512];
        sc.scan(&long, &s, &mut out);
        sc.scan(b"needle\n", &s, &mut out);
        assert!(
            out.is_empty(),
            "bytes past the cap are dropped, the needle never enters the buffer: {out:?}"
        );
    }

    #[test]
    fn notify_without_a_template_reports_the_whole_match() {
        let s = set(r#"[ { "regex": "deploy (ok|failed)", "action": "notify" } ]"#);
        let mut sc = TriggerScanner::new();
        let mut out = Vec::new();
        sc.scan(b"deploy failed\n", &s, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].message, "deploy failed");
    }
}
