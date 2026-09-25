//! Live Run: a Python file that runs itself as you type.
//!
//! The debugger answers "what is `x` here?" once you have set a breakpoint,
//! launched a session, and stepped to the line. Live Run answers it for every
//! line at once, continuously: each pause in typing re-runs the buffer and
//! paints what happened beside the code, the values each assignment produced
//! (with a loop's history, `i = 0, 1, 2 … 9 ×10`), what a line printed, what a
//! function returned, which lines never ran, and where the run died.
//!
//! # What runs, and where
//!
//! The BUFFER runs, not the file on disk: the point is to see the edit you
//! have not saved yet. It is piped to the project's interpreter (its `.venv`
//! python when there is one, so the project's imports resolve) with
//! [`INSTRUMENTER`], which rewrites the AST to record values and writes a JSON
//! report. The run has the file's own directory as cwd and `sys.path[0]`, so
//! relative opens and sibling imports behave as they would from a terminal.
//!
//! Running code on every keystroke is only acceptable because it is opt-in
//! per FILE: toggling Live Run arms the active file and no other, so opening
//! a script that deletes things never runs it on its own.
//!
//! # Why a stale annotation is never shown
//!
//! A run answers for the text it was given, and the buffer keeps moving while
//! it is in flight. Every report therefore carries the lines it ran, and the
//! editor paints a line's note only while that line is still byte-identical to
//! what ran. An edited line goes bare until the next run lands, rather than
//! wearing a value its current text never produced.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

/// The Python side: an AST rewrite plus a JSON report writer.
pub const INSTRUMENTER: &str = include_str!("../assets/live_run/instrument.py");

/// How long a run may take before the instrumenter interrupts it and reports
/// the line it was stuck on. Long enough for real scripts, short enough that
/// an accidental `while True` costs a blink.
pub const RUN_BUDGET: Duration = Duration::from_secs(3);

/// Grace past [`RUN_BUDGET`] before the process is killed outright: the
/// interpreter's own alarm cannot interrupt a C call that ignores signals.
const KILL_GRACE: Duration = Duration::from_secs(2);

/// Quiet time after the last edit before a re-run, so a burst of typing costs
/// one run rather than one per key.
pub const DEBOUNCE: Duration = Duration::from_millis(350);

/// Longest single value shown before it is elided in the middle.
const MAX_VALUE_CHARS: usize = 40;

/// What a trailer segment reports, which picks its colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A value an assignment, loop, call, or return produced.
    Value,
    /// Text the line printed.
    Output,
    /// The exception the run died with.
    Error,
    /// The run was interrupted for taking too long.
    Timeout,
}

/// One line's trailer, as coloured segments painted left to right.
pub type Note = Vec<(String, Kind)>;

/// Whether a statement line ran, for the line-number colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Coverage {
    Ran,
    NeverRan,
}

/// A finished run, ready for the editor. Line keys are 0-based.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    pub notes: BTreeMap<usize, Note>,
    pub coverage: BTreeMap<usize, Coverage>,
    /// Everything the run printed, in order (capped by the instrumenter).
    pub stdout: String,
    /// The one-line summary of how the run ended, for the status bar.
    pub summary: String,
}

#[derive(serde::Deserialize)]
struct RawValue {
    k: String,
    n: u64,
    seen: Vec<String>,
    last: String,
}

#[derive(serde::Deserialize)]
struct RawLine {
    line: usize,
    #[serde(default)]
    vals: Vec<RawValue>,
    out: Option<String>,
    #[serde(default)]
    outn: u64,
}

#[derive(serde::Deserialize)]
struct RawError {
    line: usize,
    msg: String,
}

#[derive(serde::Deserialize)]
struct RawReport {
    lines: Vec<RawLine>,
    error: Option<RawError>,
    #[serde(default)]
    stmts: Vec<usize>,
    covered: Option<Vec<usize>>,
    #[serde(default)]
    stdout: String,
    #[serde(default)]
    ms: f64,
    #[serde(default)]
    timed_out: bool,
}

/// Elide a long value in the middle, keeping both ends: the start says what
/// it is and the end is where a growing list's newest element lives.
fn elide(value: &str) -> String {
    let count = value.chars().count();
    if count <= MAX_VALUE_CHARS {
        return value.to_string();
    }
    let head: String = value.chars().take(MAX_VALUE_CHARS / 2).collect();
    let tail: String = value
        .chars()
        .skip(count - (MAX_VALUE_CHARS / 2 - 1))
        .collect();
    format!("{head}\u{2026}{tail}")
}

/// One recorded name as the trailer shows it. A single hit is just its value;
/// repeated hits show the first few and the last, so a loop reads as the
/// sequence it walked: `i = 0, 1, 2 … 9 ×10`.
fn format_value(v: &RawValue) -> String {
    let values = if v.n <= 1 || v.seen.is_empty() {
        elide(&v.last)
    } else {
        let firsts: Vec<String> = v.seen.iter().map(|s| elide(s)).collect();
        let mut s = firsts.join(", ");
        if v.n as usize > v.seen.len() {
            s.push_str(" \u{2026} ");
            s.push_str(&elide(&v.last));
        }
        s.push_str(&format!(" \u{00d7}{}", v.n));
        s
    };
    match v.k.as_str() {
        // Return value and bare expression result: the instrumenter's
        // sentinel labels, never valid identifiers.
        "\u{21a9}" | "\u{2192}" => format!("{} {values}", v.k),
        name => format!("{name} = {values}"),
    }
}

/// Parse the instrumenter's JSON into a [`Report`].
pub fn parse_report(json: &str) -> Result<Report, String> {
    let raw: RawReport =
        serde_json::from_str(json).map_err(|e| format!("unreadable Live Run report: {e}"))?;
    let mut report = Report {
        stdout: raw.stdout,
        ..Report::default()
    };
    for line in raw.lines {
        let Some(idx) = line.line.checked_sub(1) else {
            continue;
        };
        let mut note: Note = Vec::new();
        let values: Vec<String> = line.vals.iter().map(format_value).collect();
        if !values.is_empty() {
            note.push((values.join("  "), Kind::Value));
        }
        if let Some(out) = line.out {
            let mut text = format!("\u{25b8} {out}");
            if line.outn > 1 {
                text.push_str(&format!(" \u{00d7}{}", line.outn));
            }
            note.push((text, Kind::Output));
        }
        if !note.is_empty() {
            report.notes.insert(idx, note);
        }
    }
    if let Some(covered) = raw.covered {
        let ran: BTreeSet<usize> = covered.into_iter().collect();
        for line in raw.stmts {
            let Some(idx) = line.checked_sub(1) else {
                continue;
            };
            let state = if ran.contains(&line) {
                Coverage::Ran
            } else {
                Coverage::NeverRan
            };
            report.coverage.insert(idx, state);
        }
    }
    let ms = if raw.ms >= 100.0 {
        format!("{:.0} ms", raw.ms)
    } else {
        format!("{:.1} ms", raw.ms)
    };
    report.summary = match raw.error {
        Some(err) => {
            let idx = err.line.saturating_sub(1);
            let (glyph, kind) = if raw.timed_out {
                ("\u{23f1}", Kind::Timeout)
            } else {
                ("\u{2716}", Kind::Error)
            };
            // The failure goes LAST on its line: the values that line
            // produced before dying are still true.
            report
                .notes
                .entry(idx)
                .or_default()
                .push((format!("{glyph} {}", err.msg), kind));
            format!("Live Run: {} (line {}, {ms})", err.msg, err.line)
        }
        None => {
            let ran = report
                .coverage
                .values()
                .filter(|c| **c == Coverage::Ran)
                .count();
            format!(
                "Live Run: ok in {ms}, {ran}/{} statements ran",
                report.coverage.len()
            )
        }
    };
    Ok(report)
}

/// A report as one editor tab holds it, with the text it answers for.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct View {
    /// The buffer lines the run executed.
    pub lines: Vec<String>,
    pub report: Report,
}

impl View {
    /// Whether 0-based `idx` still reads exactly as it did when it ran.
    fn current(&self, idx: usize, line: &str) -> bool {
        self.lines.get(idx).is_some_and(|ran| ran == line)
    }

    /// The trailer for `idx`, unless the line has changed since it ran.
    pub fn note(&self, idx: usize, line: &str) -> Option<&Note> {
        self.report
            .notes
            .get(&idx)
            .filter(|_| self.current(idx, line))
    }

    /// Whether `idx` ran, unless the line has changed since.
    pub fn coverage(&self, idx: usize, line: &str) -> Option<Coverage> {
        self.report
            .coverage
            .get(&idx)
            .copied()
            .filter(|_| self.current(idx, line))
    }
}

/// One request: run `lines` as the file at `path`.
#[derive(Clone, Debug)]
pub struct Job {
    pub path: PathBuf,
    pub lines: Vec<String>,
    pub python: PathBuf,
}

/// A run's answer, tagged with exactly what ran so the caller can tell a
/// report for the current text from one for text that has since changed.
#[derive(Debug)]
pub struct Done {
    pub path: PathBuf,
    pub lines: Vec<String>,
    pub outcome: Result<Report, String>,
}

/// The background runner: one thread, newest job per file wins.
pub struct Runner {
    tx: Sender<Job>,
    rx: Receiver<Done>,
}

impl Runner {
    pub fn spawn() -> Self {
        let (job_tx, job_rx) = mpsc::channel::<Job>();
        let (done_tx, done_rx) = mpsc::channel::<Done>();
        std::thread::Builder::new()
            .name("croft-live-run".into())
            .spawn(move || {
                // Typing outpaces runs: only the newest buffer of each file
                // matters, but every armed file with a pending job still runs
                // (the app won't resubmit a file whose text it already sent).
                let mut pending: Vec<Job> = Vec::new();
                loop {
                    if pending.is_empty() {
                        match job_rx.recv() {
                            Ok(job) => pending.push(job),
                            Err(_) => return,
                        }
                    }
                    while let Ok(newer) = job_rx.try_recv() {
                        pending.retain(|j| j.path != newer.path);
                        pending.push(newer);
                    }
                    let job = pending.remove(0);
                    let outcome = run(&job);
                    let done = Done {
                        path: job.path,
                        lines: job.lines,
                        outcome,
                    };
                    if done_tx.send(done).is_err() {
                        return;
                    }
                }
            })
            .expect("spawn live-run thread");
        Runner {
            tx: job_tx,
            rx: done_rx,
        }
    }

    pub fn submit(&self, job: Job) {
        let _ = self.tx.send(job);
    }

    pub fn try_recv(&self) -> Option<Done> {
        self.rx.try_recv().ok()
    }
}

/// The file the instrumenter writes its report to: created exclusively (so a
/// pre-planted symlink at the name cannot redirect the write), owner-only,
/// and removed when the run is done with it.
struct ReportFile(PathBuf);

impl ReportFile {
    fn create() -> std::io::Result<Self> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let name = format!(
            "croft-live-run-{}-{}-{nanos}.json",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let path = std::env::temp_dir().join(name);
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        opts.open(&path)?;
        Ok(ReportFile(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ReportFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Run one job to completion (or to the kill deadline) and parse its report.
fn run(job: &Job) -> Result<Report, String> {
    let report_file = ReportFile::create().map_err(|e| format!("Live Run: no temp file: {e}"))?;
    let cwd = job
        .path
        .parent()
        .filter(|p| p.is_dir())
        .map(Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir);
    let mut child = Command::new(&job.python)
        .arg("-c")
        .arg(INSTRUMENTER)
        .arg(&job.path)
        .arg(report_file.path())
        .arg(RUN_BUDGET.as_secs_f64().to_string())
        .current_dir(cwd)
        // A live run must not litter the project with bytecode caches.
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Live Run: cannot start {}: {e}", job.python.display()))?;
    let mut source = job.lines.join("\n");
    source.push('\n');
    if let Some(mut stdin) = child.stdin.take() {
        // A write error means the interpreter already exited; its stderr
        // below says why.
        let _ = stdin.write_all(source.as_bytes());
    }
    let deadline = Instant::now() + RUN_BUDGET + KILL_GRACE;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "Live Run: killed after {}s (a call ignored the interrupt)",
                    (RUN_BUDGET + KILL_GRACE).as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(e) => return Err(format!("Live Run: {e}")),
        }
    }
    let json = std::fs::read_to_string(report_file.path()).unwrap_or_default();
    if json.trim().is_empty() {
        let mut err = String::new();
        if let Some(mut stderr) = child.stderr.take() {
            use std::io::Read;
            let _ = stderr.read_to_string(&mut err);
        }
        let last = err.lines().rev().find(|l| !l.trim().is_empty());
        return Err(match last {
            Some(line) => format!("Live Run: {}", line.trim()),
            None => String::from("Live Run: the script exited before reporting (os._exit?)"),
        });
    }
    parse_report(&json)
}

/// Whether Live Run can run this file at all.
pub fn supports(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("py") | Some("pyw")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(json: &str) -> Report {
        parse_report(json).expect("parses")
    }

    #[test]
    fn a_single_assignment_reads_name_equals_value() {
        let r = report(
            r#"{"lines":[{"line":2,"vals":[{"k":"x","n":1,"seen":["5"],"last":"5"}]}],
                "error":null,"stmts":[2],"covered":[2],"stdout":"","ms":1.0}"#,
        );
        assert_eq!(r.notes[&1], vec![(String::from("x = 5"), Kind::Value)]);
        assert_eq!(r.coverage[&1], Coverage::Ran);
    }

    #[test]
    fn a_loop_shows_its_first_values_and_last_with_a_count() {
        let r = report(
            r#"{"lines":[{"line":1,"vals":[{"k":"i","n":10,"seen":["0","1","2"],"last":"9"}]}],
                "error":null,"stmts":[1],"covered":[1]}"#,
        );
        assert_eq!(r.notes[&0][0].0, "i = 0, 1, 2 \u{2026} 9 \u{00d7}10");
    }

    #[test]
    fn a_short_loop_lists_every_value_without_an_ellipsis() {
        let r = report(
            r#"{"lines":[{"line":1,"vals":[{"k":"i","n":2,"seen":["0","1"],"last":"1"}]}],
                "error":null}"#,
        );
        assert_eq!(r.notes[&0][0].0, "i = 0, 1 \u{00d7}2");
    }

    #[test]
    fn returns_and_expressions_use_their_arrows_not_a_name() {
        let r = report(
            r#"{"lines":[{"line":3,"vals":[{"k":"↩","n":1,"seen":["55"],"last":"55"}]},
                         {"line":4,"vals":[{"k":"→","n":1,"seen":["5.0"],"last":"5.0"}]}],
                "error":null}"#,
        );
        assert_eq!(r.notes[&2][0].0, "\u{21a9} 55");
        assert_eq!(r.notes[&3][0].0, "\u{2192} 5.0");
    }

    #[test]
    fn printed_output_follows_the_values_on_its_line() {
        let r = report(r#"{"lines":[{"line":1,"vals":[],"out":"hello","outn":3}],"error":null}"#);
        assert_eq!(
            r.notes[&0],
            vec![(String::from("\u{25b8} hello \u{00d7}3"), Kind::Output)]
        );
    }

    #[test]
    fn the_error_lands_on_its_line_after_that_lines_values() {
        let r = report(
            r#"{"lines":[{"line":5,"vals":[{"k":"y","n":1,"seen":["1"],"last":"1"}]}],
                "error":{"line":5,"msg":"ZeroDivisionError: division by zero"},
                "stmts":[5],"covered":[5]}"#,
        );
        let note = &r.notes[&4];
        assert_eq!(note[0].1, Kind::Value);
        assert_eq!(
            note[1],
            (
                String::from("\u{2716} ZeroDivisionError: division by zero"),
                Kind::Error
            )
        );
        assert!(r.summary.contains("line 5"), "{}", r.summary);
    }

    #[test]
    fn a_timeout_is_its_own_kind() {
        let r = report(
            r#"{"lines":[],"error":{"line":3,"msg":"still running after 3s"},"timed_out":true}"#,
        );
        assert_eq!(r.notes[&2][0].1, Kind::Timeout);
    }

    #[test]
    fn statements_that_never_ran_are_marked() {
        let r = report(r#"{"lines":[],"error":null,"stmts":[1,2,3],"covered":[1,3]}"#);
        assert_eq!(r.coverage[&0], Coverage::Ran);
        assert_eq!(r.coverage[&1], Coverage::NeverRan);
        assert!(r.summary.contains("2/3 statements"), "{}", r.summary);
    }

    #[test]
    fn missing_coverage_marks_nothing_rather_than_everything_unrun() {
        let r = report(r#"{"lines":[],"error":null,"stmts":[1,2],"covered":null}"#);
        assert!(r.coverage.is_empty());
    }

    #[test]
    fn an_edited_line_drops_its_note_until_the_next_run() {
        let view = View {
            lines: vec![String::from("x = 1"), String::from("y = 2")],
            report: report(
                r#"{"lines":[{"line":1,"vals":[{"k":"x","n":1,"seen":["1"],"last":"1"}]}],
                    "error":null,"stmts":[1,2],"covered":[1,2]}"#,
            ),
        };
        assert!(view.note(0, "x = 1").is_some());
        assert!(
            view.note(0, "x = 10").is_none(),
            "the value is for the old text"
        );
        assert_eq!(view.coverage(1, "y = 2"), Some(Coverage::Ran));
        assert_eq!(view.coverage(1, "y = 3"), None);
        assert_eq!(view.coverage(5, ""), None, "past the end of what ran");
    }

    #[test]
    fn long_values_elide_in_the_middle() {
        let long = "a".repeat(30) + &"b".repeat(30);
        let shown = elide(&long);
        assert_eq!(shown.chars().count(), MAX_VALUE_CHARS);
        assert!(shown.starts_with('a') && shown.ends_with('b'));
    }

    #[test]
    fn only_python_files_are_supported() {
        assert!(supports(Path::new("/p/a.py")));
        assert!(!supports(Path::new("/p/a.rs")));
        assert!(!supports(Path::new("/p/Makefile")));
    }

    /// End to end against a real interpreter, when the machine has one: the
    /// instrumenter and the parser must agree on the report's shape.
    #[test]
    fn runs_a_real_script_when_python3_is_available() {
        let Ok(out) = Command::new("python3").arg("--version").output() else {
            return;
        };
        if !out.status.success() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("demo.py");
        let lines: Vec<String> = [
            "total = 0",
            "for k in range(4):",
            "    total += k",
            "print('sum', total)",
            "if total > 100:",
            "    print('never')",
            "1 / 0",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let job = Job {
            path: path.clone(),
            lines,
            python: PathBuf::from("python3"),
        };
        let r = run(&job).expect("runs");
        assert!(
            !r.notes.contains_key(&0),
            "a literal assignment is not echoed back"
        );
        assert_eq!(r.notes[&1][0].0, "k = 0, 1, 2 \u{2026} 3 \u{00d7}4");
        assert_eq!(r.notes[&2][0].0, "total = 0, 1, 3 \u{2026} 6 \u{00d7}4");
        assert_eq!(
            r.notes[&3][0],
            (String::from("\u{25b8} sum 6"), Kind::Output)
        );
        assert_eq!(r.coverage[&5], Coverage::NeverRan);
        assert_eq!(r.notes[&6].last().unwrap().1, Kind::Error);
        assert_eq!(r.stdout, "sum 6\n");
    }
}
