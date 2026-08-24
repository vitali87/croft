//! User-defined problem matchers (#252): regexes that turn command output
//! into PROBLEMS entries, extending the built-in table in
//! `build_matchers.rs` to tools it does not know, plus VS Code
//! `problemMatcher` mapping for tasks.json tasks and background/watch
//! matchers (`begins`/`ends` patterns) that repopulate PROBLEMS on every
//! recompile cycle of a long-running watcher without waiting for the
//! process to exit.
//!
//! Three sources feed one internal representation ([`CompiledMatcher`]):
//! - `matchers.json` next to `triggers.json` (`~/.config/croft/`, plus a
//!   workspace `.croft/matchers.json`) — croft-native schema with NAMED
//!   capture groups: `(?P<file>…)`, `(?P<line>…)`, `(?P<col>…)`,
//!   `(?P<severity>…)`, `(?P<message>…)`, `(?P<code>…)`.
//! - tasks.json `problemMatcher` — VS Code's schema: `$well-known` names
//!   map onto the built-in table; inline pattern objects (numeric group
//!   indices) translate into the same representation.
//! - the well-known table itself (`$tsc`, `$tsc-watch`, `$rustc`,
//!   `$eslint-stylish`, `$gcc`).
//!
//! Batch scanning happens at the `FinishedCommand` boundary like the
//! built-ins. Watch matchers run on the live stream: the trigger engine's
//! per-line scanner (`triggers::TriggerScanner`) already completes
//! escape-stripped primary-screen lines, so [`WatchEngine`] consumes those
//! lines rather than adding a second byte scanner — `begins` opens a
//! collection window, `ends` publishes the window's diagnostics as the
//! pane's new batch (replacing the previous one, so fixed errors clear).
//!
//! A matcher whose regex fails to compile is dropped with a warning
//! surfaced once at load; the stream path only ever sees compiled regexes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use regex::Regex;

use crate::build_matchers::BuildDiag;
use crate::lsp::manager::DiagnosticSeverity;

/// Hard cap on a watch window's collected lines: a watcher that never
/// prints its `ends` pattern must not grow memory without bound, and no
/// real compile cycle's diagnostics need more.
pub const WATCH_WINDOW_CAP: usize = 5000;

/// Where a diagnostic field comes from in a pattern's captures.
#[derive(Clone, Debug, PartialEq, Eq)]
enum GroupRef {
    /// A named group (matchers.json style); absent groups resolve to None.
    Name(&'static str),
    /// A numeric group index (VS Code pattern style); 0 = whole match.
    Index(usize),
    /// The field is not captured by this pattern.
    None,
}

impl GroupRef {
    fn get<'t>(&self, caps: &regex::Captures<'t>) -> Option<&'t str> {
        match self {
            GroupRef::Name(n) => caps.name(n).map(|m| m.as_str()),
            GroupRef::Index(i) => caps.get(*i).map(|m| m.as_str()),
            GroupRef::None => None,
        }
    }
}

/// One compiled line pattern of a matcher's (possibly multi-line) sequence.
#[derive(Clone, Debug)]
struct CompiledPattern {
    regex: Regex,
    file: GroupRef,
    line: GroupRef,
    col: GroupRef,
    severity: GroupRef,
    message: GroupRef,
    code: GroupRef,
    /// VS Code `"loop": true` on the LAST pattern: it re-applies to each
    /// following line, emitting one diagnostic per match (eslint-style
    /// indented rows under a file header).
    loops: bool,
}

/// Watch-task window delimiters (VS Code `background.beginsPattern` /
/// `endsPattern`).
#[derive(Clone, Debug)]
pub struct Background {
    begins: Regex,
    ends: Regex,
}

/// One matcher, from any source, compiled and ready to scan.
#[derive(Clone, Debug)]
pub struct CompiledMatcher {
    pub name: String,
    /// Empty ⇒ delegate batch scanning to the built-in table
    /// (`build_matchers::scan`), optionally narrowed by `builtin_filter`.
    patterns: Vec<CompiledPattern>,
    /// For well-known `$names`: keep only built-in diags with this source
    /// tag (`$gcc` ⇒ "build"), so the task reports what VS Code would.
    builtin_filter: Option<&'static str>,
    /// Extra severity words beyond error/warning/info ("E" ⇒ error, …).
    severity_map: BTreeMap<String, DiagnosticSeverity>,
    /// Glob over the finished command line (batch scans only): the matcher
    /// fires only for commands it claims. No glob ⇒ every command.
    applies_to: Option<String>,
    pub background: Option<Background>,
}

/// Severity words every matcher understands; `severity_map` extends them.
fn severity_word(word: &str, map: &BTreeMap<String, DiagnosticSeverity>) -> DiagnosticSeverity {
    if let Some(s) = map.get(word).or_else(|| map.get(&word.to_lowercase())) {
        return *s;
    }
    match word.to_lowercase().as_str() {
        "error" | "fatal error" | "fatal" | "err" | "e" => DiagnosticSeverity::Error,
        "warning" | "warn" | "w" => DiagnosticSeverity::Warning,
        "hint" => DiagnosticSeverity::Hint,
        _ => DiagnosticSeverity::Information,
    }
}

fn parse_severity_name(s: &str) -> Option<DiagnosticSeverity> {
    match s.to_lowercase().as_str() {
        "error" => Some(DiagnosticSeverity::Error),
        "warning" => Some(DiagnosticSeverity::Warning),
        "info" | "information" => Some(DiagnosticSeverity::Information),
        "hint" => Some(DiagnosticSeverity::Hint),
        _ => None,
    }
}

/// Minimal glob: `*` matches any run (including empty), `?` one char,
/// everything else literal. Matched against the trimmed command line.
fn glob_match(pattern: &str, text: &str) -> bool {
    fn inner(p: &[char], t: &[char]) -> bool {
        match p.first() {
            None => t.is_empty(),
            Some('*') => (0..=t.len()).any(|i| inner(&p[1..], &t[i..])),
            Some('?') => !t.is_empty() && inner(&p[1..], &t[1..]),
            Some(c) => t.first() == Some(c) && inner(&p[1..], &t[1..]),
        }
    }
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.trim().chars().collect();
    inner(&p, &t)
}

/// 1-based tool coordinate → the 0-based one PROBLEMS uses.
fn zero(n: Option<&str>) -> u32 {
    n.and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(1)
        .saturating_sub(1)
}

impl CompiledMatcher {
    /// Whether this matcher claims `command` (batch scans; watch windows
    /// are gated by `begins` instead).
    pub fn applies_to_command(&self, command: &str) -> bool {
        self.applies_to
            .as_deref()
            .is_none_or(|g| glob_match(g, command))
    }

    /// Scan a whole output block (a finished command, or a watch window).
    pub fn scan_batch(&self, output: &str) -> Vec<BuildDiag> {
        if self.patterns.is_empty() {
            let mut diags = crate::build_matchers::scan(output);
            if let Some(tag) = self.builtin_filter {
                diags.retain(|d| d.source == tag);
            }
            return diags;
        }
        let lines: Vec<&str> = output.lines().collect();
        let mut out = Vec::new();
        let mut i = 0;
        while i < lines.len() {
            match self.try_sequence_at(&lines, i, &mut out) {
                Some(consumed) => i += consumed.max(1),
                None => i += 1,
            }
        }
        out
    }

    /// Try the full pattern sequence anchored at `lines[start]`. On success
    /// pushes the diagnostics and returns how many lines were consumed.
    fn try_sequence_at(
        &self,
        lines: &[&str],
        start: usize,
        out: &mut Vec<BuildDiag>,
    ) -> Option<usize> {
        // Fields accumulate across the sequence (file from the header line,
        // line/col/message from later ones), VS Code's multi-line model.
        #[derive(Default, Clone)]
        struct Acc {
            file: Option<String>,
            line: Option<String>,
            col: Option<String>,
            severity: Option<String>,
            message: Option<String>,
            code: Option<String>,
        }
        fn absorb(acc: &mut Acc, p: &CompiledPattern, caps: &regex::Captures) {
            let take = |g: &GroupRef| g.get(caps).map(str::to_string);
            if let Some(v) = take(&p.file) {
                acc.file = Some(v);
            }
            if let Some(v) = take(&p.line) {
                acc.line = Some(v);
            }
            if let Some(v) = take(&p.col) {
                acc.col = Some(v);
            }
            if let Some(v) = take(&p.severity) {
                acc.severity = Some(v);
            }
            if let Some(v) = take(&p.message) {
                acc.message = Some(v);
            }
            if let Some(v) = take(&p.code) {
                acc.code = Some(v);
            }
        }
        let emit = |acc: &Acc, fallback_line: &str, out: &mut Vec<BuildDiag>| -> bool {
            let Some(file) = acc.file.clone() else {
                return false;
            };
            let severity = acc
                .severity
                .as_deref()
                .map(|w| severity_word(w, &self.severity_map))
                .unwrap_or(DiagnosticSeverity::Error);
            let mut message = acc
                .message
                .clone()
                .unwrap_or_else(|| fallback_line.trim().to_string());
            if let Some(code) = &acc.code {
                message = format!("{code}: {message}");
            }
            out.push(BuildDiag {
                file,
                line: zero(acc.line.as_deref()),
                col: zero(acc.col.as_deref()),
                severity,
                message,
                source: self.name.clone(),
            });
            true
        };

        let mut acc = Acc::default();
        let mut idx = start;
        for (pi, p) in self.patterns.iter().enumerate() {
            let last = pi == self.patterns.len() - 1;
            if last && p.loops && self.patterns.len() > 1 {
                // The looping tail: one diagnostic per consecutive match.
                let mut any = false;
                while idx < lines.len()
                    && let Some(caps) = p.regex.captures(lines[idx])
                {
                    let mut row = acc.clone();
                    absorb(&mut row, p, &caps);
                    any |= emit(&row, lines[idx], out);
                    idx += 1;
                }
                if !any {
                    return None;
                }
                return Some(idx - start);
            }
            let caps = p.regex.captures(lines.get(idx)?)?;
            absorb(&mut acc, p, &caps);
            idx += 1;
        }
        if emit(&acc, lines[idx - 1], out) {
            Some(idx - start)
        } else {
            None
        }
    }
}

/// Every loaded matcher plus the load warnings to surface once.
#[derive(Clone, Debug, Default)]
pub struct MatcherSet {
    pub matchers: Vec<Arc<CompiledMatcher>>,
    pub warnings: Vec<String>,
}

/// The background-capable subset, shared with every pane's reader thread
/// (swapped whole on reload, like `TriggerSet`).
#[derive(Clone, Debug, Default)]
pub struct WatchSet {
    pub matchers: Vec<Arc<CompiledMatcher>>,
}

pub fn matchers_path() -> PathBuf {
    crate::prefs::config_dir().join("matchers.json")
}

pub fn workspace_matchers_path(root: &Path) -> PathBuf {
    root.join(".croft").join("matchers.json")
}

/// The starter file written on first "Open Problem Matchers (JSON)".
pub const TEMPLATE: &str = r##"// croft problem matchers: regexes that turn command output into PROBLEMS
// entries, extending the built-in set (rustc, tsc, gcc, python, eslint).
// Each entry:
//   "name"      tag shown in the PROBLEMS panel
//   "pattern"   regex with NAMED groups: (?P<file>…) (?P<line>…) (?P<col>…)
//               (?P<severity>…) (?P<message>…) (?P<code>…)  — file is
//               required, the rest optional
//   "patterns"  instead of "pattern": an array matched against consecutive
//               lines (file header line, then rows); add "loop": true on
//               the last one to emit a diagnostic per repeated row
//   "applies_to"   command-line glob, e.g. "mylint*" — only matching
//                  commands are scanned (omit to scan every command)
//   "severity_map" extra severity words: { "E": "error", "W": "warning" }
//   "background"   watch tasks: { "begins": "…", "ends": "…" } — output
//                  between the two is collected and (re)published to
//                  PROBLEMS on every cycle, replacing the previous batch
//   "enabled": false keeps an entry without running it
[
  // {
  //   "name": "mylint",
  //   "pattern": "^(?P<file>\\S+):(?P<line>\\d+):(?P<col>\\d+) (?P<severity>\\w+) (?P<message>.+)$",
  //   "applies_to": "mylint*"
  // }
]
"##;

/// One raw matchers.json row before compilation.
#[derive(serde::Deserialize)]
struct MatcherRow {
    name: String,
    #[serde(default)]
    pattern: Option<String>,
    #[serde(default)]
    patterns: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    severity_map: BTreeMap<String, String>,
    #[serde(default)]
    applies_to: Option<String>,
    #[serde(default)]
    background: Option<BackgroundRow>,
    #[serde(default = "enabled_default")]
    enabled: bool,
}

#[derive(serde::Deserialize)]
struct BackgroundRow {
    begins: String,
    ends: String,
}

fn enabled_default() -> bool {
    true
}

/// Compile a matchers.json regex whose named groups name the fields.
fn compile_named_pattern(src: &str, loops: bool) -> Result<CompiledPattern, String> {
    let regex = Regex::new(src).map_err(|e| format!("bad regex `{src}`: {e}"))?;
    let has = |n: &str| regex.capture_names().flatten().any(|g| g == n);
    let named = |n: &'static str| {
        if has(n) {
            GroupRef::Name(n)
        } else {
            GroupRef::None
        }
    };
    Ok(CompiledPattern {
        file: named("file"),
        line: named("line"),
        col: named("col"),
        severity: named("severity"),
        message: named("message"),
        code: named("code"),
        loops,
        regex,
    })
}

impl MatcherSet {
    /// Load and merge the user and workspace matcher files. Missing files
    /// contribute nothing; a bad row drops with a warning, never blocks
    /// startup. Workspace matchers are regexes applied to output — they
    /// never execute anything — so repo-provided ones are safe to honour.
    pub fn load(user: &Path, workspace: Option<&Path>) -> Self {
        let mut set = Self::default();
        for path in std::iter::once(user).chain(workspace) {
            let Ok(json) = std::fs::read_to_string(path) else {
                continue;
            };
            set.extend_from_json(&json, &path.display().to_string());
        }
        set
    }

    /// Test seam: one in-memory document, no filesystem.
    #[cfg(test)]
    pub fn from_json(json: &str) -> Self {
        let mut set = Self::default();
        set.extend_from_json(json, "matchers.json");
        set
    }

    fn extend_from_json(&mut self, json: &str, origin: &str) {
        let stripped = crate::tasks::strip_jsonc(json);
        let rows: Vec<serde_json::Value> = match serde_json::from_str(&stripped) {
            Ok(rows) => rows,
            Err(e) => {
                self.warnings
                    .push(format!("{origin}: not a JSON array: {e}"));
                return;
            }
        };
        for row in rows {
            let row: MatcherRow = match serde_json::from_value(row) {
                Ok(r) => r,
                Err(e) => {
                    self.warnings
                        .push(format!("{origin}: skipped an entry: {e}"));
                    continue;
                }
            };
            if !row.enabled {
                continue;
            }
            match compile_row(row) {
                Ok(m) => self.matchers.push(Arc::new(m)),
                Err(e) => self.warnings.push(format!("{origin}: {e}")),
            }
        }
    }

    /// The background-capable subset for the pane reader threads.
    pub fn watch_set(&self) -> WatchSet {
        WatchSet {
            matchers: self
                .matchers
                .iter()
                .filter(|m| m.background.is_some())
                .cloned()
                .collect(),
        }
    }

    /// Batch-scan a finished command's output: user matchers that claim the
    /// command first, then the built-in table, dropping built-in rows that
    /// duplicate a custom row's location (a custom matcher for a gcc-shaped
    /// format must not double-report).
    pub fn scan_batch(&self, output: &str, command: &str) -> Vec<BuildDiag> {
        let mut out = Vec::new();
        for m in &self.matchers {
            if m.patterns.is_empty() || !m.applies_to_command(command) {
                continue;
            }
            out.extend(m.scan_batch(output));
        }
        let custom: std::collections::BTreeSet<(String, u32, u32)> = out
            .iter()
            .map(|d| (d.file.clone(), d.line, d.col))
            .collect();
        out.extend(
            crate::build_matchers::scan(output)
                .into_iter()
                .filter(|d| !custom.contains(&(d.file.clone(), d.line, d.col))),
        );
        out
    }
}

fn compile_row(row: MatcherRow) -> Result<CompiledMatcher, String> {
    let name = row.name;
    let mut patterns = Vec::new();
    if let Some(p) = &row.pattern {
        patterns.push(compile_named_pattern(p, false).map_err(|e| format!("{name}: {e}"))?);
    } else if let Some(seq) = &row.patterns {
        let n = seq.len();
        for (i, v) in seq.iter().enumerate() {
            let (src, loops) = match v {
                serde_json::Value::String(s) => (s.as_str(), false),
                serde_json::Value::Object(o) => (
                    o.get("regex")
                        .and_then(|r| r.as_str())
                        .ok_or_else(|| format!("{name}: patterns[{i}] has no \"regex\""))?,
                    o.get("loop").and_then(|l| l.as_bool()).unwrap_or(false),
                ),
                _ => return Err(format!("{name}: patterns[{i}] must be a string or object")),
            };
            if loops && i + 1 != n {
                return Err(format!("{name}: only the last pattern may set \"loop\""));
            }
            patterns.push(compile_named_pattern(src, loops).map_err(|e| format!("{name}: {e}"))?);
        }
    }
    let background = row
        .background
        .map(|b| -> Result<Background, String> {
            Ok(Background {
                begins: Regex::new(&b.begins)
                    .map_err(|e| format!("{name}: bad begins regex: {e}"))?,
                ends: Regex::new(&b.ends).map_err(|e| format!("{name}: bad ends regex: {e}"))?,
            })
        })
        .transpose()?;
    if patterns.is_empty() && background.is_none() {
        return Err(format!(
            "{name}: needs a \"pattern\", \"patterns\" or \"background\""
        ));
    }
    let severity_map = row
        .severity_map
        .into_iter()
        .filter_map(|(k, v)| parse_severity_name(&v).map(|s| (k, s)))
        .collect();
    Ok(CompiledMatcher {
        name,
        patterns,
        builtin_filter: None,
        severity_map,
        applies_to: row.applies_to,
        background,
    })
}

/// The VS Code well-known matcher names croft maps onto its built-in
/// table. `$tsc-watch` additionally carries tsc's watch-cycle delimiters.
pub fn well_known(name: &str) -> Option<CompiledMatcher> {
    let (tag, filter): (&str, &'static str) = match name {
        "$tsc" => ("tsc", "tsc"),
        "$rustc" => ("rustc", "rustc"),
        "$eslint-stylish" => ("eslint", "eslint"),
        "$gcc" => ("gcc", "build"),
        "$tsc-watch" => {
            return Some(CompiledMatcher {
                name: String::from("tsc-watch"),
                patterns: Vec::new(),
                builtin_filter: Some("tsc"),
                severity_map: BTreeMap::new(),
                applies_to: None,
                background: Some(Background {
                    begins: Regex::new(
                        r"Starting compilation in watch mode|File change detected\. Starting incremental compilation",
                    )
                    .unwrap(),
                    ends: Regex::new(r"Watching for file changes\.").unwrap(),
                }),
            });
        }
        _ => return None,
    };
    Some(CompiledMatcher {
        name: tag.to_string(),
        patterns: Vec::new(),
        builtin_filter: Some(filter),
        severity_map: BTreeMap::new(),
        applies_to: None,
        background: None,
    })
}

/// Translate a tasks.json `problemMatcher` value (string, object, or
/// array — first usable entry wins) into a matcher. `None` means "no
/// usable matcher": unknown `$names` and untranslatable shapes degrade to
/// the built-in first-match-wins scan rather than erroring.
pub fn from_tasks_json(value: &serde_json::Value) -> Option<CompiledMatcher> {
    match value {
        serde_json::Value::String(s) => well_known(s),
        serde_json::Value::Array(items) => items.iter().find_map(from_tasks_json),
        serde_json::Value::Object(o) => from_vscode_object(o),
        _ => None,
    }
}

/// An inline VS Code problemMatcher object: `pattern` uses NUMERIC group
/// indices (`"file": 1`), `background.beginsPattern`/`endsPattern` gate
/// watch mode. `base` (`"$tsc"`-style) supplies what the object omits.
fn from_vscode_object(o: &serde_json::Map<String, serde_json::Value>) -> Option<CompiledMatcher> {
    let base = o.get("base").and_then(|b| b.as_str()).and_then(well_known);
    let idx = |v: Option<&serde_json::Value>, default: usize| -> GroupRef {
        match v.and_then(|v| v.as_u64()) {
            Some(i) => GroupRef::Index(i as usize),
            None if default != usize::MAX => GroupRef::Index(default),
            None => GroupRef::None,
        }
    };
    let compile_one = |p: &serde_json::Map<String, serde_json::Value>| -> Option<CompiledPattern> {
        let src = p.get("regexp")?.as_str()?;
        Some(CompiledPattern {
            regex: Regex::new(src).ok()?,
            file: idx(p.get("file"), 1),
            line: idx(p.get("line"), 2),
            col: idx(p.get("column"), 3),
            severity: idx(p.get("severity"), usize::MAX),
            message: idx(p.get("message"), usize::MAX),
            code: idx(p.get("code"), usize::MAX),
            loops: p.get("loop").and_then(|l| l.as_bool()).unwrap_or(false),
        })
    };
    let patterns: Vec<CompiledPattern> = match o.get("pattern") {
        Some(serde_json::Value::Object(p)) => compile_one(p).into_iter().collect(),
        Some(serde_json::Value::Array(seq)) => {
            let compiled: Vec<_> = seq
                .iter()
                .filter_map(|v| v.as_object())
                .filter_map(compile_one)
                .collect();
            // A partially-compiled sequence would mis-consume lines.
            if compiled.len() != seq.len() {
                return base;
            }
            compiled
        }
        Some(serde_json::Value::String(s)) => return well_known(s).or(base),
        _ => Vec::new(),
    };
    let pattern_str = |v: Option<&serde_json::Value>| -> Option<Regex> {
        let v = v?;
        let src = v.as_str().or_else(|| v.get("regexp")?.as_str())?;
        Regex::new(src).ok()
    };
    let background = o.get("background").and_then(|b| {
        Some(Background {
            begins: pattern_str(b.get("beginsPattern"))?,
            ends: pattern_str(b.get("endsPattern"))?,
        })
    });
    if patterns.is_empty() && background.is_none() {
        return base;
    }
    let owner = o
        .get("owner")
        .and_then(|s| s.as_str())
        .unwrap_or("task")
        .to_string();
    let base = base.unwrap_or(CompiledMatcher {
        name: owner.clone(),
        patterns: Vec::new(),
        builtin_filter: None,
        severity_map: BTreeMap::new(),
        applies_to: None,
        background: None,
    });
    Some(CompiledMatcher {
        name: if o.contains_key("owner") {
            owner
        } else {
            base.name
        },
        patterns: if patterns.is_empty() {
            base.patterns
        } else {
            patterns
        },
        builtin_filter: base.builtin_filter,
        severity_map: base.severity_map,
        applies_to: None,
        background: background.or(base.background),
    })
}

/// Per-pane streaming state machine for watch matchers. Fed the completed
/// escape-stripped primary-screen lines the trigger scanner already
/// produces; returns a published batch when a window closes.
#[derive(Default)]
pub struct WatchEngine {
    active: Option<(Arc<CompiledMatcher>, Vec<String>)>,
}

impl WatchEngine {
    /// Feed one completed line. `pane_matcher` (the task's own matcher, if
    /// any) outranks the global set when both could open a window.
    pub fn feed(
        &mut self,
        line: &str,
        set: &WatchSet,
        pane_matcher: Option<&Arc<CompiledMatcher>>,
    ) -> Option<Vec<BuildDiag>> {
        if let Some((matcher, window)) = &mut self.active {
            let bg = matcher
                .background
                .as_ref()
                .expect("active window implies background");
            if bg.ends.is_match(line) {
                let output = window.join("\n");
                let diags = matcher.scan_batch(&output);
                self.active = None;
                return Some(diags);
            }
            if window.len() < WATCH_WINDOW_CAP {
                window.push(line.to_string());
            }
            // A new begins inside the window restarts the cycle (a watcher
            // interrupted mid-compile starts over; stale lines must not
            // leak into the fresh window).
            if bg.begins.is_match(line) {
                window.clear();
            }
            return None;
        }
        // An explicit task matcher is EXCLUSIVE for its pane (VS Code's
        // model, same rule as the FinishedCommand scan): global watch
        // matchers never open windows on task-owned output, or an
        // unrelated global `begins` could claim the cycle and its publish
        // would suppress the task matcher's own results.
        let global = if pane_matcher.is_some() {
            &[][..]
        } else {
            &set.matchers[..]
        };
        for m in pane_matcher.into_iter().chain(global) {
            if let Some(bg) = &m.background
                && bg.begins.is_match(line)
            {
                self.active = Some((m.clone(), Vec::new()));
                return None;
            }
        }
        None
    }

    /// Drop any half-open watch window. Called when matcher ownership
    /// changes under the engine (config reload, task (re)assignment) so
    /// the next `ends` can't scan stale lines with the old matcher.
    pub fn reset(&mut self) {
        self.active = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_single_line_named_group_matcher_extracts_the_fields() {
        let set = MatcherSet::from_json(
            r##"[
  { "name": "mylint",
    "pattern": "^(?P<file>\\S+):(?P<line>\\d+):(?P<col>\\d+) (?P<severity>\\w+) (?P<message>.+)$",
    "severity_map": { "E": "error", "W": "warning" },
    "applies_to": "mylint*" }
]"##,
        );
        assert!(set.warnings.is_empty(), "{:?}", set.warnings);
        let out = set.scan_batch(
            "src/a.py:12:5 E undefined name 'x'\nok line\n",
            "mylint src/",
        );
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].file, "src/a.py");
        assert_eq!((out[0].line, out[0].col), (11, 4), "0-based conversion");
        assert_eq!(out[0].severity, DiagnosticSeverity::Error);
        assert_eq!(out[0].message, "undefined name 'x'");
        assert_eq!(out[0].source, "mylint");
        // The applies_to glob gates the matcher off other commands.
        assert!(
            set.scan_batch("src/a.py:12:5 E undefined name 'x'\n", "cargo build")
                .is_empty()
        );
    }

    #[test]
    fn a_multi_line_matcher_with_a_looping_tail_handles_header_plus_rows() {
        let set = MatcherSet::from_json(
            r##"[
  { "name": "homelint",
    "patterns": [
      "^== (?P<file>\\S+) ==$",
      { "regex": "^\\s+(?P<line>\\d+):(?P<col>\\d+)\\s+(?P<severity>\\w+)\\s+(?P<message>.+)$", "loop": true }
    ] }
]"##,
        );
        assert!(set.warnings.is_empty(), "{:?}", set.warnings);
        let out = set.scan_batch(
            "== src/main.c ==\n  3:1  warning  short name\n  9:2  error  bad cast\nnoise\n== src/util.c ==\n  1:1  error  oops\n",
            "homelint",
        );
        assert_eq!(out.len(), 3, "{out:?}");
        assert_eq!(out[0].file, "src/main.c");
        assert_eq!(out[0].severity, DiagnosticSeverity::Warning);
        assert_eq!(out[1].message, "bad cast");
        assert_eq!(out[2].file, "src/util.c");
        assert_eq!((out[2].line, out[2].col), (0, 0));
    }

    #[test]
    fn bad_rows_warn_once_and_never_reach_the_matcher_list() {
        let set = MatcherSet::from_json(
            r##"[
  { "name": "broken", "pattern": "((unclosed" },
  { "name": "empty" },
  { "name": "off", "pattern": "(?P<file>.+)", "enabled": false },
  { "name": "good", "pattern": "^(?P<file>\\S+): (?P<message>.+)$" }
]"##,
        );
        assert_eq!(set.matchers.len(), 1, "{:?}", set.warnings);
        assert_eq!(set.matchers[0].name, "good");
        assert_eq!(set.warnings.len(), 2, "{:?}", set.warnings);
        assert!(set.warnings[0].contains("broken"), "{:?}", set.warnings);
        let garbage = MatcherSet::from_json("not json");
        assert!(garbage.matchers.is_empty());
        assert_eq!(garbage.warnings.len(), 1);
    }

    #[test]
    fn custom_diags_suppress_duplicate_builtin_rows_but_builtins_still_run() {
        // A custom matcher over a gcc-shaped line: the built-in GENERIC
        // matcher would report the same location; only the custom row
        // survives, while an untouched rustc block still comes through.
        let set = MatcherSet::from_json(
            r##"[ { "name": "cc", "pattern": "^(?P<file>\\S+):(?P<line>\\d+):(?P<col>\\d+): (?P<severity>\\w+): (?P<message>.+)$" } ]"##,
        );
        let out = set.scan_batch(
            "main.c:7:3: error: expected ';'\nerror[E0308]: mismatched types\n  --> src/lib.rs:2:1\n",
            "make",
        );
        assert_eq!(out.len(), 2, "{out:?}");
        assert_eq!(out[0].source, "cc");
        assert_eq!(out[1].source, "rustc");
    }

    #[test]
    fn well_known_names_map_onto_the_builtin_table_with_a_source_filter() {
        let gcc = well_known("$gcc").unwrap();
        let out = gcc
            .scan_batch("main.c:7:3: error: expected ';'\nsrc/app.ts(4,10): error TS2322: nope\n");
        assert_eq!(
            out.len(),
            1,
            "$gcc must only report gcc-shaped rows: {out:?}"
        );
        assert_eq!(out[0].file, "main.c");
        assert!(well_known("$tsc-watch").unwrap().background.is_some());
        assert!(well_known("$made-up-name").is_none());
    }

    #[test]
    fn tasks_json_problem_matcher_values_translate() {
        // A $name string.
        let v: serde_json::Value = serde_json::json!("$gcc");
        assert_eq!(from_tasks_json(&v).unwrap().name, "gcc");
        // Unknown $name degrades to None (built-in scan takes over).
        assert!(from_tasks_json(&serde_json::json!("$whatever")).is_none());
        // An inline VS Code pattern object with numeric indices.
        let v = serde_json::json!({
            "owner": "mytool",
            "pattern": {
                "regexp": "^(.+?)\\|(\\d+)\\|(\\d+)\\|(error|warning)\\|(.+)$",
                "file": 1, "line": 2, "column": 3, "severity": 4, "message": 5
            }
        });
        let m = from_tasks_json(&v).unwrap();
        let out = m.scan_batch("src/x.zig|4|2|warning|unused variable\n");
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].file, "src/x.zig");
        assert_eq!(out[0].severity, DiagnosticSeverity::Warning);
        assert_eq!(out[0].source, "mytool");
        // An array picks the first usable entry.
        let v = serde_json::json!(["$nope", "$rustc"]);
        assert_eq!(from_tasks_json(&v).unwrap().name, "rustc");
        // background with string patterns (VS Code allows both shapes).
        let v = serde_json::json!({
            "base": "$tsc",
            "background": { "beginsPattern": "Compiling…", "endsPattern": "Done\\." }
        });
        let m = from_tasks_json(&v).unwrap();
        assert!(m.background.is_some());
        assert_eq!(m.builtin_filter, Some("tsc"));
    }

    #[test]
    fn the_watch_engine_collects_between_begins_and_ends_and_republishes() {
        let matcher = Arc::new(well_known("$tsc-watch").unwrap());
        let set = WatchSet::default();
        let mut engine = WatchEngine::default();
        let feed = |e: &mut WatchEngine, line: &str| e.feed(line, &set, Some(&matcher));
        assert!(feed(&mut engine, "stray line").is_none());
        assert!(feed(&mut engine, "Starting compilation in watch mode...").is_none());
        assert!(feed(&mut engine, "src/app.ts(4,10): error TS2322: bad type.").is_none());
        let batch = feed(&mut engine, "Found 1 error. Watching for file changes.").unwrap();
        assert_eq!(batch.len(), 1, "{batch:?}");
        assert_eq!(batch[0].file, "src/app.ts");
        // Next cycle: clean compile publishes an EMPTY batch, which is what
        // clears the fixed errors from PROBLEMS.
        assert!(
            feed(
                &mut engine,
                "File change detected. Starting incremental compilation..."
            )
            .is_none()
        );
        let batch = feed(&mut engine, "Found 0 errors. Watching for file changes.").unwrap();
        assert!(batch.is_empty(), "{batch:?}");
    }

    #[test]
    fn a_begins_inside_an_open_window_restarts_the_cycle() {
        let set = MatcherSet::from_json(
            r##"[ { "name": "watchy",
                   "pattern": "^(?P<file>\\S+): (?P<message>.+)$",
                   "background": { "begins": "^BUILD START$", "ends": "^BUILD END$" } } ]"##,
        );
        let watch = set.watch_set();
        assert_eq!(watch.matchers.len(), 1);
        let mut engine = WatchEngine::default();
        assert!(engine.feed("BUILD START", &watch, None).is_none());
        assert!(engine.feed("stale.c: old error", &watch, None).is_none());
        assert!(engine.feed("BUILD START", &watch, None).is_none());
        assert!(engine.feed("fresh.c: new error", &watch, None).is_none());
        let batch = engine.feed("BUILD END", &watch, None).unwrap();
        assert_eq!(
            batch.len(),
            1,
            "the restarted window dropped stale lines: {batch:?}"
        );
        assert_eq!(batch[0].file, "fresh.c");
    }

    #[test]
    fn a_pane_matcher_excludes_global_watch_matchers_and_reset_drops_the_window() {
        let set = MatcherSet::from_json(
            r##"[ { "name": "globby",
                   "pattern": "^(?P<file>\\S+): (?P<message>.+)$",
                   "background": { "begins": "^GLOBAL START$", "ends": "^GLOBAL END$" } } ]"##,
        );
        let watch = set.watch_set();
        let task = Arc::new(well_known("$tsc-watch").unwrap());
        // Task-owned pane: a global begins must NOT open a window — the
        // task matcher is exclusive, and a global publish would suppress
        // the task's own FinishedCommand scan via the skip-once flag.
        let mut engine = WatchEngine::default();
        assert!(engine.feed("GLOBAL START", &watch, Some(&task)).is_none());
        assert!(
            engine.feed("stale.c: err", &watch, Some(&task)).is_none()
                && engine.feed("GLOBAL END", &watch, Some(&task)).is_none(),
            "no window ever opened on the task pane"
        );
        // Without a pane matcher the same lines publish normally.
        let mut free = WatchEngine::default();
        assert!(free.feed("GLOBAL START", &watch, None).is_none());
        assert!(free.feed("a.c: boom", &watch, None).is_none());
        assert_eq!(free.feed("GLOBAL END", &watch, None).unwrap().len(), 1);
        // reset() mid-window: the interrupted cycle never publishes.
        let mut resettable = WatchEngine::default();
        assert!(resettable.feed("GLOBAL START", &watch, None).is_none());
        assert!(resettable.feed("a.c: boom", &watch, None).is_none());
        resettable.reset();
        assert!(
            resettable.feed("GLOBAL END", &watch, None).is_none(),
            "the dropped window's ends must not scan stale lines"
        );
    }

    #[test]
    fn glob_matching_covers_star_and_question_mark() {
        assert!(glob_match("mylint*", "mylint src/"));
        assert!(glob_match("*test*", "cargo test --all"));
        assert!(glob_match("l?nt", "lint"));
        assert!(!glob_match("mylint*", "cargo build"));
        assert!(glob_match("*", "anything at all"));
    }
}
