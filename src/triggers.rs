//! iTerm2-style terminal triggers: user regexes watched against terminal
//! output, each firing an action. The pragmatic action set that covers real
//! trigger usage (per iTerm2 / kitty marks): **highlight** (recolour every
//! visible occurrence, live, scrollback included), **notify** (status-bar
//! notice with `\0`..`\9` capture interpolation), **bell** (status-bar
//! bell notice), **capture** (iTerm2's Capture Output: collect every
//! matched line into the CAPTURES panel, where clicking an entry jumps the
//! pane to that line), and **redact** (#360: paint the match as a run of
//! `•` of the same width, the grid untouched; a click on the mask pops the
//! real text; copies yield the real value unless the rule says
//! `"copy": "masked"`; the scrollback-to-editor dump is always masked). A
//! built-in redact set for the usual key shapes (AWS, OpenAI `sk-`, GitHub
//! `ghp_`, Slack `xox`, JWTs, bearer tokens) is on by default and switched
//! off in Settings. Auto-respond / run-command actions are deliberately absent:
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
//! line rewrite (progress bars never spam), a hard per-line cap, and
//! alternate-screen content excluded POSITIONALLY (the scanner tracks
//! DECSET/DECRST 47/1047/1049 and RIS through the stream itself, so a
//! full-screen app's bytes never fire while primary text sharing a chunk
//! with a transition still does; iTerm2 skips alt output too).

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
    /// Collect the matched line into the CAPTURES panel (iTerm2's Capture
    /// Output); the message template labels the entry.
    Capture,
    /// Mask the matched span (capture group 1 when the regex has one, else
    /// the whole match) at paint time; see [`redact_spans`].
    Redact,
}

/// The glyph a redacted cell shows. One per masked char, so the row keeps
/// its width and columns line up.
pub const MASK: char = '\u{2022}';

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
    /// Redact only: a copy of a masked span yields the mask, not the value
    /// (`"copy": "masked"`). Default: copies carry the real text.
    pub copy_masked: bool,
    /// Redact only: the span's BODY (after its last hyphen) must carry a
    /// digit or an uppercase letter to count. A key body is random
    /// alphanumerics (all-lowercase-letters is a one-in-a-million shape at
    /// 16 chars); a hyphenated prose word's last segment is a word. Built-in
    /// `sk-` uses it; user rules do not.
    pub needs_entropy: bool,
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

/// One notify/bell/capture firing, drained by the app (status bar for
/// notify/bell, the CAPTURES panel for capture).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TriggerHit {
    pub action: TriggerAction,
    pub message: String,
    /// The whole escape-stripped output line that matched, so a captured
    /// entry can be jumped back to (and shown) in full.
    pub line: String,
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
    /// Redact rows: `"masked"` keeps the mask in copies; anything else
    /// (or absent) copies the real value.
    #[serde(default)]
    copy: Option<String>,
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
//             "capture"    collect matched lines into the CAPTURES panel (click jumps to the line)
//             "redact"     mask the match (group 1 if the regex has one) as •••• on screen;
//                          click the mask to see the value; "copy": "masked" masks copies too.
//                          Built-in key/token rules run first (Settings: Terminal: Redact Secrets).
// Highlights repaint live on the visible screen and persist into scrollback.
// notify/bell/capture fire once per completed output line (never inside full-screen apps).
// "enabled": false keeps a rule without running it.
[
  { "regex": "\\b(ERROR|FATAL|panicked)\\b", "action": "highlight", "fg": "#ffffff", "bg": "#c0392b" },
  { "regex": "\\bwarning\\b", "action": "highlight", "fg": "#000000", "bg": "#e5c07b", "enabled": false },
  { "regex": "(BUILD|Compiling|error): (.+)", "action": "notify", "message": "\\1: \\2", "enabled": false },
  { "regex": "(?i)x-api-key: *(\\S+)", "action": "redact", "copy": "masked", "enabled": false }
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
                    "capture" => TriggerAction::Capture,
                    "redact" => TriggerAction::Redact,
                    _ => return None,
                };
                Some(Trigger {
                    regex: Regex::new(&r.regex).ok()?,
                    action,
                    fg: r.fg.as_deref().and_then(parse_hex),
                    bg: r.bg.as_deref().and_then(parse_hex),
                    message: r.message,
                    copy_masked: r.copy.as_deref() == Some("masked"),
                    needs_entropy: false,
                })
            })
            .collect();
        Self { triggers }
    }

    pub fn is_empty(&self) -> bool {
        self.triggers.is_empty()
    }

    /// The built-in secret rules in front of the user's own, so a key
    /// shape the user never thought to list is still masked. Off in
    /// Settings ("Terminal: Redact Secrets") means this is never called.
    pub fn with_builtin_redactions(mut self) -> Self {
        let mut all = builtin_redactions();
        all.append(&mut self.triggers);
        self.triggers = all;
        self
    }

    /// Whether any trigger paints (drives the render pass): highlight and
    /// redact both act on the visible rows.
    pub fn has_highlights(&self) -> bool {
        self.triggers
            .iter()
            .any(|t| matches!(t.action, TriggerAction::Highlight | TriggerAction::Redact))
    }

    /// Whether any trigger redacts (drives the copy / dump masking).
    pub fn has_redactions(&self) -> bool {
        self.triggers
            .iter()
            .any(|t| t.action == TriggerAction::Redact)
    }

    /// Whether any trigger fires events (drives the reader-thread scan).
    pub fn has_events(&self) -> bool {
        self.triggers
            .iter()
            .any(|t| !matches!(t.action, TriggerAction::Highlight | TriggerAction::Redact))
    }
}

/// The key and token shapes masked out of the box (#360). Each pattern is
/// anchored on a distinctive prefix (or, for bearer tokens, the
/// `Authorization` header) so ordinary words never match; a bearer token
/// masks only the token (group 1), leaving the header readable. PEM bodies
/// are not covered: their base64 lines carry no prefix to anchor on, and
/// masking every long base64 line would eat checksums and blobs. Compiled
/// once; the set is cloned per trigger reload.
pub fn builtin_redactions() -> Vec<Trigger> {
    static RULES: std::sync::LazyLock<Vec<Trigger>> = std::sync::LazyLock::new(|| {
        // (pattern, the span must carry a digit or an uppercase letter)
        const PATTERNS: &[(&str, bool)] = &[
            // AWS access key ids.
            (r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b", false),
            // OpenAI / Anthropic / OpenRouter-style secret keys: `sk-`, up
            // to three short labels (`sk-proj-`, `sk-ant-api03-`, `sk-or-v1-`)
            // and a long body. Entropy-gated: `sk-` also opens hyphenated
            // prose ("sk-learn-preprocessingpipeline"), whose body is all
            // lowercase letters.
            (r"\bsk-(?:[A-Za-z0-9]{1,10}-){0,3}[A-Za-z0-9_]{16,}\b", true),
            // GitHub tokens: classic (ghp_), OAuth (gho_), app (ghu_/ghs_/ghr_), fine-grained.
            (
                r"\b(?:gh[pousr]_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{22,})\b",
                false,
            ),
            // Slack tokens.
            (r"\bxox[abpors]-[A-Za-z0-9-]{10,}\b", false),
            // JWTs: three base64url segments, the first a JSON header.
            (
                r"\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\b",
                false,
            ),
            // `Authorization: Bearer <token>` in header or JSON form
            // (`"Authorization": "Bearer …"`) - only the token is masked.
            // Anchored on the header so "Bearer" as an English word never
            // matches; a closing quote, comma or semicolon stays outside.
            (
                r#"(?i)\bauthorization"? *: *"?bearer +([^\s'",;]{16,})"#,
                false,
            ),
        ];
        PATTERNS
            .iter()
            .map(|(r, needs_entropy)| Trigger {
                regex: Regex::new(r).expect("built-in redaction regex compiles"),
                action: TriggerAction::Redact,
                fg: None,
                bg: None,
                message: None,
                copy_masked: false,
                needs_entropy: *needs_entropy,
            })
            .collect()
    });
    RULES.clone()
}

/// One masked span on a row, in char indices.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RedactSpan {
    pub start: usize,
    pub len: usize,
    /// Whether a copy of this span keeps the mask.
    pub copy_masked: bool,
}

/// Every redact-trigger match on `line`: capture group 1 when the regex
/// has one (so a rule can name the secret inside a header), else the
/// whole match. Empty matches are skipped.
pub fn redact_spans(line: &str, set: &TriggerSet) -> Vec<RedactSpan> {
    let mut out = Vec::new();
    for t in &set.triggers {
        if t.action != TriggerAction::Redact {
            continue;
        }
        for caps in t.regex.captures_iter(line) {
            let m = match caps.get(1) {
                Some(g) => g,
                None => match caps.get(0) {
                    Some(m) => m,
                    None => continue,
                },
            };
            // The entropy test looks at the key BODY - the segment after the
            // last label hyphen - so a digit in a label (`api03`) cannot
            // vouch for a lowercase-only body, and an all-letter label
            // cannot sink a real one.
            let body = m.as_str().rsplit('-').next().unwrap_or(m.as_str());
            if m.as_str().is_empty()
                || (t.needs_entropy
                    && !body
                        .chars()
                        .any(|c| c.is_ascii_digit() || c.is_ascii_uppercase()))
            {
                continue;
            }
            out.push(RedactSpan {
                start: line[..m.start()].chars().count(),
                len: m.as_str().chars().count(),
                copy_masked: t.copy_masked,
            });
        }
    }
    out
}

/// `text` with every redact-trigger match replaced by [`MASK`] runs of the
/// same char length, line by line. `copy_only` restricts the masking to
/// rules marked `"copy": "masked"` (the clipboard path); `false` masks
/// everything (the scrollback dump, and anything else that leaves the
/// pane as bytes). Returns the input untouched when nothing matches.
pub fn mask_text(text: &str, set: &TriggerSet, copy_only: bool) -> String {
    if !set.has_redactions() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let spans: Vec<RedactSpan> = redact_spans(line, set)
            .into_iter()
            .filter(|s| !copy_only || s.copy_masked)
            .collect();
        if spans.is_empty() {
            out.push_str(line);
            continue;
        }
        for (k, c) in line.chars().enumerate() {
            if spans.iter().any(|s| k >= s.start && k < s.start + s.len) {
                out.push(MASK);
            } else {
                out.push(c);
            }
        }
    }
    out
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
    /// Whether the byte position currently being scanned sits inside the
    /// alternate screen. Tracked POSITIONALLY from DECSET/DECRST 47/1047/
    /// 1049 (and RIS) in the stream itself, so alt-screen content never
    /// fires and primary text sharing a chunk with a transition still does
    /// — the chunk's final terminal mode says nothing about where a line
    /// fell. Requires the scanner to see EVERY chunk, never a gated subset,
    /// or the tracking (and the string state machine) desyncs.
    in_alt: bool,
    /// Parameter bytes of the CSI currently being consumed (bounded; only
    /// needed to recognize the alt-screen DECSET/DECRST lists).
    csi: Vec<u8>,
}

/// Longest CSI parameter list retained; a real DECSET list is far shorter.
const CSI_CAP: usize = 32;

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
        self.scan_collect(bytes, set, out, None);
    }

    /// Like [`scan`](Self::scan), additionally pushing every completed
    /// primary-screen line (escape-stripped, blank ones included — batch
    /// matchers care about section-ending blanks) into `lines`. The watch
    /// problem matchers (#252) consume these instead of running a second
    /// byte scanner over the same stream.
    pub fn scan_collect(
        &mut self,
        bytes: &[u8],
        set: &TriggerSet,
        out: &mut Vec<TriggerHit>,
        mut lines: Option<&mut Vec<String>>,
    ) {
        for &b in bytes {
            // Resolve a pending `\r` now that its successor is known: CRLF is
            // the PTY line ending (complete), a lone CR rewrites the line in
            // place (progress bars, spinners — reset without matching, so
            // only text that survives to a newline fires).
            if self.cr_pending && self.state == ScanState::Ground {
                self.cr_pending = false;
                if b == b'\n' {
                    self.complete_line(set, out, lines.as_deref_mut());
                    continue;
                }
                self.reset_line();
            }
            match self.state {
                ScanState::Ground => match b {
                    0x1b => self.state = ScanState::Esc,
                    b'\n' => self.complete_line(set, out, lines.as_deref_mut()),
                    b'\r' => self.cr_pending = true,
                    0x00..=0x1f | 0x7f => {}
                    _ => {
                        // Past the cap the byte is dropped; the capped prefix
                        // still matches on completion. Alt-screen bytes are
                        // consumed but never accumulated.
                        if !self.in_alt && self.line.len() < LINE_CAP {
                            self.line.push(b);
                        }
                    }
                },
                ScanState::Esc => {
                    self.state = match b {
                        b'[' => ScanState::Csi,
                        b']' | b'P' | b'X' | b'^' | b'_' => ScanState::InString,
                        // RIS: full reset, back on the primary screen.
                        b'c' => {
                            self.set_alt(false);
                            ScanState::Ground
                        }
                        _ => ScanState::Ground,
                    };
                }
                ScanState::Csi => {
                    if (0x40..=0x7e).contains(&b) {
                        if (b == b'h' || b == b'l') && self.csi.first() == Some(&b'?') {
                            let alt = self.csi[1..]
                                .split(|&c| c == b';')
                                .any(|p| p == b"47" || p == b"1047" || p == b"1049");
                            if alt {
                                self.set_alt(b == b'h');
                            }
                        }
                        self.csi.clear();
                        self.state = ScanState::Ground;
                    } else if b == 0x18 || b == 0x1a {
                        // CAN/SUB cancel the sequence (VT100).
                        self.csi.clear();
                        self.state = ScanState::Ground;
                    } else if b == 0x1b {
                        // ESC inside an incomplete CSI begins a NEW escape;
                        // retaining it as a parameter would let a malformed
                        // CSI swallow the following alt-screen entry.
                        self.csi.clear();
                        self.state = ScanState::Esc;
                    } else if self.csi.len() < CSI_CAP {
                        self.csi.push(b);
                    }
                }
                ScanState::InString => match b {
                    0x07 => self.state = ScanState::Ground,
                    0x1b => self.state = ScanState::StringEsc,
                    // CAN/SUB abort the control string (ECMA-48); staying
                    // in-string would swallow ordinary output and the alt
                    // boundary until an unrelated BEL/ST.
                    0x18 | 0x1a => self.state = ScanState::Ground,
                    _ => {}
                },
                ScanState::StringEsc => {
                    // `ESC \` (ST) ends the string; CAN/SUB abort it;
                    // anything else is still string body.
                    self.state = match b {
                        b'\\' => ScanState::Ground,
                        0x18 | 0x1a => ScanState::Ground,
                        _ => ScanState::InString,
                    };
                }
            }
        }
    }

    /// Cross an alt-screen boundary: whatever partial primary line was
    /// pending is stale on both edges (the shell repaints its prompt after
    /// a full-screen app exits), so it must never splice with later text.
    fn set_alt(&mut self, alt: bool) {
        if self.in_alt != alt {
            self.in_alt = alt;
            self.reset_line();
            self.cr_pending = false;
        }
    }

    fn reset_line(&mut self) {
        self.line.clear();
    }

    fn complete_line(
        &mut self,
        set: &TriggerSet,
        out: &mut Vec<TriggerHit>,
        lines: Option<&mut Vec<String>>,
    ) {
        if self.in_alt {
            self.reset_line();
            return;
        }
        if let Some(lines) = lines {
            lines.push(String::from_utf8_lossy(&self.line).into_owned());
        }
        if !self.line.is_empty() {
            let line = String::from_utf8_lossy(&self.line);
            for t in &set.triggers {
                // Paint-only actions never become hits: a redact hit would
                // carry the secret itself into the status bar.
                if matches!(t.action, TriggerAction::Highlight | TriggerAction::Redact) {
                    continue;
                }
                if let Some(caps) = t.regex.captures(&line) {
                    out.push(TriggerHit {
                        action: t.action,
                        message: interpolate(t.message.as_deref(), &caps),
                        line: line.to_string(),
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
    fn redact_rows_parse_with_their_copy_mode() {
        let s = set(r##"[
  { "regex": "secret=(\\S+)", "action": "redact", "copy": "masked" },
  { "regex": "token=\\S+", "action": "redact" }
]"##);
        assert_eq!(s.triggers.len(), 2);
        assert!(s.has_highlights(), "redact rides the paint pass");
        assert!(s.has_redactions());
        assert!(
            !s.has_events(),
            "redact never wakes the reader-thread scanner"
        );
        assert!(s.triggers[0].copy_masked);
        assert!(!s.triggers[1].copy_masked);
    }

    #[test]
    fn redact_spans_mask_group_one_when_present_else_the_whole_match() {
        let s = set(r##"[
  { "regex": "secret=(\\S+)", "action": "redact", "copy": "masked" },
  { "regex": "token=\\S+", "action": "redact" }
]"##);
        let spans = redact_spans("naïve secret=abc token=xyz", &s);
        // "naïve secret=" is 13 chars: the group starts after the '='.
        assert_eq!(
            spans,
            vec![
                RedactSpan {
                    start: 13,
                    len: 3,
                    copy_masked: true
                },
                RedactSpan {
                    start: 17,
                    len: 9,
                    copy_masked: false
                },
            ]
        );
        assert_eq!(
            mask_text("naïve secret=abc token=xyz", &s, false),
            "naïve secret=••• •••••••••",
            "the dump masks every rule, width preserved"
        );
        assert_eq!(
            mask_text("naïve secret=abc token=xyz", &s, true),
            "naïve secret=••• token=xyz",
            "a copy masks only rules marked copy=masked"
        );
        assert_eq!(
            mask_text("line one\nsecret=q\n", &s, false),
            "line one\nsecret=•\n",
            "masking is per line and keeps the newlines"
        );
    }

    /// A redact rule paints; it must never become a status-bar hit, or the
    /// bar would print the secret the pane just masked.
    #[test]
    fn redact_rules_never_fire_as_hits() {
        let s = TriggerSet::default().with_builtin_redactions();
        let mut sc = TriggerScanner::new();
        let mut out = Vec::new();
        sc.scan(b"key AKIAIOSFODNN7EXAMPLE end\n", &s, &mut out);
        assert!(out.is_empty(), "{out:?}");
        let user = set(r##"[{ "regex": "hunter2", "action": "redact" }]"##);
        sc.scan(b"pw hunter2\n", &user, &mut out);
        assert!(out.is_empty(), "{out:?}");
        assert!(!s.has_events(), "nothing here wakes the scanner");
    }

    #[test]
    fn builtin_rules_mask_the_usual_key_shapes_and_leave_prose_alone() {
        let s = TriggerSet::default().with_builtin_redactions();
        let masked = |line: &str| mask_text(line, &s, false);
        assert_eq!(
            masked("export AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE"),
            "export AWS_ACCESS_KEY_ID=••••••••••••••••••••"
        );
        let key = "sk-proj-abcdefghijklmnopqrstuvwxyz0123";
        assert_eq!(
            masked(&format!("OPENAI_API_KEY={key}")),
            format!("OPENAI_API_KEY={}", "•".repeat(key.len()))
        );
        assert_eq!(
            masked("token: ghp_abcdefghijklmnopqrstuvwxyz0123456789"),
            "token: ••••••••••••••••••••••••••••••••••••••••"
        );
        assert_eq!(
            masked("xoxb-123456789012-abcdefGHIJ"),
            "••••••••••••••••••••••••••••"
        );
        assert_eq!(
            masked("ghp_abcdefghijklmnopqrstuvwxyzABCDEFGHIJ"),
            "•".repeat(40),
            "a real token can be digit-free; the prefix is the anchor"
        );
        assert_eq!(masked("xoxb-abcdefghijklmnopqrstuvwx"), "•".repeat(29));
        assert_eq!(
            masked("sk-proj-abcdefghijklmnopqrstuvwxyzABCDEFGH"),
            "•".repeat(42),
            "one labelled segment is a key shape"
        );
        let ant = "sk-ant-api03-AbCdEf1234567890AbCdEf1234567890";
        assert_eq!(masked(ant), "•".repeat(ant.len()), "two labels: Anthropic");
        let or = "sk-or-v1-abcdef1234567890abcdef1234567890";
        assert_eq!(masked(or), "•".repeat(or.len()), "two labels: OpenRouter");
        let plain_body = "sk-abcdefghijklmnopqrstuvwxyzabcdefgH1";
        assert_eq!(
            masked(plain_body),
            "•".repeat(plain_body.len()),
            "no label, mixed body"
        );
        let label_digit_only = "sk-ant-api03-abcdefghijklmnopqrstuvwxyzabcdef";
        assert_eq!(
            masked(label_digit_only),
            label_digit_only,
            "a digit in the label does not vouch for an all-lowercase body"
        );
        assert_eq!(
            masked(r#"{"Authorization": "Bearer tok_abcdef1234567890"}"#),
            r#"{"Authorization": "Bearer ••••••••••••••••••••"}"#,
            "the JSON header form"
        );
        assert_eq!(
            masked("Authorization: Bearer abcdef0123456789ABCDEF, X-Other: 1"),
            "Authorization: Bearer ••••••••••••••••••••••, X-Other: 1",
            "a trailing comma stays outside"
        );
        assert_eq!(
            masked("curl -H 'Authorization: Bearer abcdef0123456789ABCDEF' https://x"),
            "curl -H 'Authorization: Bearer ••••••••••••••••••••••' https://x",
            "the closing quote stays outside the mask"
        );
        assert_eq!(
            masked("Authorization: Bearer abcdef0123456789ABCDEF"),
            "Authorization: Bearer ••••••••••••••••••••••",
            "a bearer header keeps its words, the token goes"
        );
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
        assert_eq!(masked(jwt), "•".repeat(jwt.chars().count()));
        for prose in [
            "cargo build --release",
            "Bearer of bad news",
            "Bearer responsibilities matter",
            "run task sk-something-like-this-long",
            "sk-preprocessingtransformerspipeline",
            "sk-learn-preprocessingtransformerspipeline",
            "the skeleton key",
            "sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
        ] {
            assert_eq!(masked(prose), prose, "prose must not be masked");
        }
        // The user's own rules come after the built-ins and still apply.
        let both =
            set(r##"[{ "regex": "hunter2", "action": "redact" }]"##).with_builtin_redactions();
        assert_eq!(mask_text("pw hunter2", &both, false), "pw •••••••");
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
                line: String::from("BUILD FAILED"),
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

    /// The scanner tracks alt-screen entry/exit POSITIONALLY through the
    /// byte stream: alt-screen content never fires, primary text sharing a
    /// chunk with an alt transition still does, and a partial primary line
    /// interrupted by an alt round trip never splices with post-exit text.
    #[test]
    fn scanner_tracks_the_alt_screen_positionally() {
        let s = set(r##"[
  { "regex": "SECRET", "action": "notify" },
  { "regex": "BUILD FAILED", "action": "notify" },
  { "regex": "ERROR: failed", "action": "notify" }
]"##);
        let mut sc = TriggerScanner::new();
        let mut out = Vec::new();
        // (a) Content that only ever existed on the alt screen must not
        // fire, even though the chunk ENDS back on the primary screen.
        sc.scan(b"\x1b[?1049hSECRET\n\x1b[?1049l", &s, &mut out);
        assert!(out.is_empty(), "alt-screen content fired: {out:?}");
        // (b) Primary text in the same chunk as the alt ENTRY still fires.
        sc.scan(b"BUILD FAILED\n\x1b[?1049h", &s, &mut out);
        assert_eq!(out.len(), 1, "primary text before alt entry: {out:?}");
        assert_eq!(out[0].line, "BUILD FAILED");
        out.clear();
        sc.scan(b"\x1b[?1049l", &s, &mut out);
        // (c) A partial primary line interrupted by an alt round trip must
        // not splice with post-exit output into a line never printed.
        sc.scan(b"ERR", &s, &mut out);
        sc.scan(b"\x1b[?1049halt stuff\x1b[?1049l", &s, &mut out);
        sc.scan(b"OR: failed\n", &s, &mut out);
        assert!(out.is_empty(), "spliced a line never printed: {out:?}");
        // Multi-parameter DECSET lists count too (\x1b[?1002;1049h).
        sc.scan(b"\x1b[?1002;1049hSECRET\n\x1b[?1049;1002l", &s, &mut out);
        assert!(out.is_empty(), "combined DECSET alt entry missed: {out:?}");
        // And RIS resets to the primary screen.
        sc.scan(b"\x1b[?1049h\x1bcBUILD FAILED\n", &s, &mut out);
        assert_eq!(out.len(), 1, "RIS must exit the alt screen: {out:?}");
    }

    /// CAN/SUB cancel an incomplete CSI and an ESC inside one starts a new
    /// escape (VT100 semantics): retaining them as CSI parameters made the
    /// malformed CSI swallow the following `ESC [?1049h`, so alt-screen
    /// entry went unseen and full-screen content fired triggers.
    #[test]
    fn a_cancelled_csi_does_not_swallow_the_alt_screen_entry() {
        let s = set(r##"[ { "regex": "SECRET", "action": "notify" } ]"##);
        let mut sc = TriggerScanner::new();
        let mut out = Vec::new();
        // CAN aborts the dangling CSI; the alt entry after it must count.
        sc.scan(b"\x1b[\x18\x1b[?1049hSECRET\n\x1b[?1049l", &s, &mut out);
        assert!(
            out.is_empty(),
            "CAN-cancelled CSI ate the alt entry: {out:?}"
        );
        // SUB likewise.
        sc.scan(b"\x1b[12\x1a\x1b[?1049hSECRET\n\x1b[?1049l", &s, &mut out);
        assert!(
            out.is_empty(),
            "SUB-cancelled CSI ate the alt entry: {out:?}"
        );
        // An ESC inside an incomplete CSI begins a NEW escape sequence.
        sc.scan(b"\x1b[12\x1b[?1049hSECRET\n\x1b[?1049l", &s, &mut out);
        assert!(out.is_empty(), "ESC-in-CSI ate the alt entry: {out:?}");
    }

    /// CAN/SUB abort control strings too (ECMA-48): an unterminated OSC /
    /// DCS / APC / PM / SOS cancelled by CAN or SUB used to keep the
    /// scanner in its string state, swallowing ordinary output and the
    /// alt-screen boundary until some unrelated BEL/ST arrived.
    #[test]
    fn can_and_sub_cancel_an_unterminated_control_string() {
        let s = set(
            r##"[ { "regex": "PRIMARY OK", "action": "notify" }, { "regex": "SECRET", "action": "notify" } ]"##,
        );
        for intro in [b"\x1b]".as_slice(), b"\x1bP", b"\x1b_", b"\x1b^", b"\x1bX"] {
            for cancel in [0x18u8, 0x1a] {
                let mut sc = TriggerScanner::new();
                let mut out = Vec::new();
                let mut bytes = Vec::new();
                bytes.extend_from_slice(intro);
                bytes.extend_from_slice(b"unterminated-body");
                bytes.push(cancel);
                bytes.extend_from_slice(b"PRIMARY OK\n\x1b[?1049hSECRET\n\x1b[?1049l");
                sc.scan(&bytes, &s, &mut out);
                assert_eq!(
                    out.len(),
                    1,
                    "intro {intro:?} cancel {cancel:#x}: the cancelled string must \
                     release the scanner: {out:?}"
                );
                assert_eq!(
                    out[0].line, "PRIMARY OK",
                    "intro {intro:?} cancel {cancel:#x}"
                );
            }
        }
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
    fn capture_action_parses_and_scanner_reports_the_full_line() {
        let s = set(
            r#"[ { "regex": "error\\[(\\w+)\\]", "action": "capture", "message": "rustc \\1" } ]"#,
        );
        assert!(s.has_events(), "capture is an event action");
        let mut sc = TriggerScanner::new();
        let mut out = Vec::new();
        sc.scan(b"error[E0308]: mismatched types\r\n", &s, &mut out);
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].action, TriggerAction::Capture);
        assert_eq!(out[0].message, "rustc E0308");
        assert_eq!(
            out[0].line, "error[E0308]: mismatched types",
            "a capture hit carries the whole matched line for the panel"
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
