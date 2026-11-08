//! Build problem matchers (#119): turn a finished command's output into
//! PROBLEMS entries — VS Code's problem-matcher role, run croft-natively at
//! the `FinishedCommand` boundary (the whole 133;C→133;D output block at
//! once, so multi-line formats need no streaming state machine).
//!
//! Built-in matchers only, first match per line wins:
//! - rustc/cargo: `error[E0308]: msg` + `  --> src/x.rs:12:5`
//! - tsc: `src/x.ts(12,5): error TS2322: msg` and `src/x.ts:12:5 - error TS2322: msg`
//! - gcc/clang/generic: `file:line:col: error|warning: msg`
//! - python: a traceback's deepest `File "x", line N` frame, message from
//!   the final `SomethingError: …` line
//! - eslint (stylish): the indented `line:col  severity  msg  rule` rows
//!   under their file-path header line
//! - mypy, javac: `file:line: error|warning|note: msg`, with no column (#1594)
//! - go build / go vet: `file.go:line:col: msg`, with no severity word
//! - flake8, pylint, ruff (concise): `file:line:col: CODE msg`
//! - ruff (default): `CODE msg` + ` --> file:line:col`
//! - eslint (compact): `file: line N, col N, Error - msg (rule)`
//! - MSBuild / msCompile: `file(line,col): error CODE: msg`
//!
//! The go and `CODE msg` rows are loose enough to match other text, so the
//! caller keeps them only for a file that exists ([`BuildDiag::needs_file`]).

use std::sync::LazyLock;

use regex::Regex;

use crate::lsp::manager::DiagnosticSeverity;

/// What a gcc/clang-shaped `file:line:col` counts its column in. Compilers
/// disagree, and a line with a tab or a non-ASCII character shows it:
///
/// - `Display`: screen cells, a tab running to the next multiple of the
///   tab stop (8). gcc 11 and newer, by default.
/// - `Byte`: UTF-8 bytes, a tab one byte. clang, and gcc before 11 or with
///   `-fdiagnostics-column-unit=byte`.
/// - `Char`: characters, as rustc and most other tools count.
///
/// A setting rather than a guess from the output: `make` can run either
/// compiler and print nothing that tells them apart.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColumnUnit {
    #[default]
    Display,
    Byte,
    Char,
}

impl ColumnUnit {
    /// Read a `build_column_unit` value. Anything unrecognised is `Display`,
    /// the default, so a typo behaves like no setting.
    pub fn from_pref(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "byte" | "bytes" => Self::Byte,
            "char" | "chars" | "character" | "characters" => Self::Char,
            _ => Self::Display,
        }
    }
}

/// gcc's `-ftabstop` default.
pub const DEFAULT_TABSTOP: usize = 8;

/// The tab stop a command line asks for with `-ftabstop=N`, else 8.
pub fn tabstop_from_command(cmd: &str) -> usize {
    cmd.split_whitespace()
        .find_map(|w| w.strip_prefix("-ftabstop="))
        .and_then(|n| n.parse::<usize>().ok())
        .filter(|n| (1..=100).contains(n))
        .unwrap_or(DEFAULT_TABSTOP)
}

/// The 0-based character index in `line` of the 0-based column `col` counted
/// in `unit`. A column past the end of the line lands on its character count,
/// and one inside a wide character or a tab on that character. The
/// one-column reference the conversion's single walk (`char_cols`) is
/// checked against.
#[cfg(test)]
fn char_col(line: &str, col: usize, unit: ColumnUnit, tabstop: usize) -> usize {
    use unicode_width::UnicodeWidthChar;
    let tabstop = tabstop.max(1);
    match unit {
        ColumnUnit::Char => col,
        ColumnUnit::Byte => {
            for (i, (at, c)) in line.char_indices().enumerate() {
                if col < at + c.len_utf8() {
                    return i;
                }
            }
            line.chars().count()
        }
        ColumnUnit::Display => {
            let mut cell = 0usize;
            for (i, c) in line.chars().enumerate() {
                let w = if c == '\t' {
                    tabstop - cell % tabstop
                } else {
                    c.width().unwrap_or(0)
                };
                if col < cell + w {
                    return i;
                }
                cell += w;
            }
            line.chars().count()
        }
    }
}

/// Rewrite the columns of the gcc/clang-shaped rows (source `build`) in
/// `diags` from `unit` to characters, reading each row's line from its file
/// under `base`: the file the compiler just read, so its line is the one the
/// column counts. A row whose file cannot be read, or whose line is plain
/// ASCII without a tab, keeps its column, which is the same in every unit.
///
/// This runs on the UI thread as a batch is installed, so it is bounded: one
/// file is read at a time and dropped before the next, a file over 8 MiB is
/// skipped, and once the batch has read 32 MiB the remaining files' rows keep
/// their columns rather than stall the editor on the reads.
pub fn convert_columns(
    diags: &mut [BuildDiag],
    base: &std::path::Path,
    unit: ColumnUnit,
    tabstop: usize,
) {
    convert_columns_within(diags, base, unit, tabstop, 32 * 1024 * 1024);
}

/// [`convert_columns`] with the batch's read budget, `budget` bytes, given.
fn convert_columns_within(
    diags: &mut [BuildDiag],
    base: &std::path::Path,
    unit: ColumnUnit,
    tabstop: usize,
    mut budget: u64,
) {
    /// A file bigger than this is not read just to place a caret.
    const MAX_BYTES: u64 = 8 * 1024 * 1024;
    if unit == ColumnUnit::Char {
        return;
    }
    // Each file's rows in first-seen order, so a file is read once and its
    // text dropped before the next file is read.
    let mut by_file: Vec<(std::path::PathBuf, Vec<usize>)> = Vec::new();
    let mut slot_of: std::collections::HashMap<std::path::PathBuf, usize> =
        std::collections::HashMap::new();
    for (i, d) in diags.iter().enumerate() {
        if d.source != "build" || d.col == 0 {
            continue;
        }
        let p = std::path::Path::new(&d.file);
        let path = if p.is_absolute() {
            p.to_path_buf()
        } else {
            base.join(p)
        };
        let slot = *slot_of.entry(path.clone()).or_insert_with(|| {
            by_file.push((path, Vec::new()));
            by_file.len() - 1
        });
        by_file[slot].1.push(i);
    }
    for (path, rows) in by_file {
        // Only a regular file is read: a printed path can name a device or
        // a FIFO (`/dev/zero:1:2: error`), whose reported length is 0 but
        // which reads forever or blocks the open, on the UI thread.
        let Some(len) = std::fs::metadata(&path)
            .ok()
            .filter(std::fs::Metadata::is_file)
            .map(|m| m.len())
            .filter(|&n| n <= MAX_BYTES.min(budget))
        else {
            continue;
        };
        budget -= len;
        let Some(text) = read_capped(&path, len) else {
            continue;
        };
        let lines: Vec<&str> = text.lines().collect();
        // A line's rows are converted together, in one walk along it
        // (`char_cols`): thousands of rows on one long line cost one pass,
        // not one pass each.
        let mut keyed: Vec<(u32, usize, usize)> = rows
            .into_iter()
            .map(|i| (diags[i].line, diags[i].col as usize, i))
            .collect();
        keyed.sort_unstable();
        for group in keyed.chunk_by(|a, b| a.0 == b.0) {
            let Some(line) = lines.get(group[0].0 as usize) else {
                continue;
            };
            if line.is_ascii() && !line.contains('\t') {
                continue;
            }
            let cols: Vec<usize> = group.iter().map(|g| g.1).collect();
            for (g, c) in group.iter().zip(char_cols(line, &cols, unit, tabstop)) {
                diags[g.2].col = c as u32;
            }
        }
    }
}

/// [`char_col`] for many columns of one line in a single walk along it.
/// `cols` must be in ascending order; the result holds each column's
/// character index, in the same order.
fn char_cols(line: &str, cols: &[usize], unit: ColumnUnit, tabstop: usize) -> Vec<usize> {
    use unicode_width::UnicodeWidthChar;
    if unit == ColumnUnit::Char {
        return cols.to_vec();
    }
    let tabstop = tabstop.max(1);
    let mut out = Vec::with_capacity(cols.len());
    // Bytes (Byte) or cells (Display) before the current character.
    let mut at = 0usize;
    let mut count = 0usize;
    for (i, c) in line.chars().enumerate() {
        let end = at
            + match unit {
                ColumnUnit::Byte => c.len_utf8(),
                _ if c == '\t' => tabstop - at % tabstop,
                _ => c.width().unwrap_or(0),
            };
        while out.len() < cols.len() && cols[out.len()] < end {
            out.push(i);
        }
        if out.len() == cols.len() {
            return out;
        }
        at = end;
        count = i + 1;
    }
    out.resize(cols.len(), count);
    out
}

/// `path` as text if it is a regular file, reading at most `limit` bytes.
/// The open never blocks (`O_NONBLOCK`: a FIFO swapped in for the path
/// after the caller's metadata check would otherwise wait for a writer, on
/// the UI thread), and the opened handle itself is checked to be a regular
/// file before anything is read. A file that grew past `limit` reads as
/// `None` rather than as a truncated line.
fn read_capped(path: &std::path::Path, limit: u64) -> Option<String> {
    use std::io::Read;
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NONBLOCK);
    }
    let file = opts.open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut text = String::new();
    file.take(limit + 1).read_to_string(&mut text).ok()?;
    (text.len() as u64 <= limit).then_some(text)
}

/// One matched problem, file still as the tool printed it (resolution
/// against the command's cwd is the caller's job).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildDiag {
    pub file: String,
    /// 0-based, like the LSP diagnostics PROBLEMS already carries.
    pub line: u32,
    pub col: u32,
    pub severity: DiagnosticSeverity,
    pub message: String,
    /// Which matcher produced it: a built-in tag (`rustc`, `tsc`, …) or a
    /// user matcher's name (#252).
    pub source: String,
    /// Kept only when its file exists (#1594): set by the loose built-in go
    /// and lint rows (`file.go:1:2: text`, `file:1:2: X1 text`), shapes
    /// other output can take where every other built-in row needs a
    /// severity word. A flag rather than a check on `source`, which a user
    /// matcher may also name `go` or `lint`.
    pub needs_file: bool,
}

fn severity(word: &str) -> DiagnosticSeverity {
    match word.to_ascii_lowercase().as_str() {
        "error" | "fatal error" => DiagnosticSeverity::Error,
        "warning" => DiagnosticSeverity::Warning,
        _ => DiagnosticSeverity::Information,
    }
}

static RUSTC_HEAD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(error|warning)(\[[A-Z0-9]+\])?: (.+)$").unwrap());
static RUSTC_ARROW: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*-->\s+(.+?):(\d+):(\d+)\s*$").unwrap());
static TSC_PAREN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(.+?)\((\d+),(\d+)\): (error|warning) (TS\d+): (.+)$").unwrap());
static TSC_PRETTY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(.+?):(\d+):(\d+) - (error|warning) (TS\d+): (.+)$").unwrap());
static GENERIC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(.+?):(\d+):(\d+):\s*(error|warning|fatal error):\s*(.+)$").unwrap()
});
static NO_COLUMN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(.+?\.[A-Za-z0-9]+):(\d+): (error|warning|note): (.+)$").unwrap()
});
static GO: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\S+\.go):(\d+):(\d+): (.+)$").unwrap());
static LINT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\S+?):(\d+):(\d+): ([A-Z]+\d+):? (?:\[\*\] )?(.+)$").unwrap());
static RUFF_HEAD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([A-Z]+\d+) (?:\[\*\] )?(.+)$").unwrap());
static ESLINT_COMPACT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(.+?): line (\d+), col (\d+), (Error|Warning) - (.+)$").unwrap()
});
static MS_COMPILE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(.+?)\((\d+)(?:,(\d+))?\)\s*: (error|warning) ([A-Za-z]+\d+)\s*: (.+)$")
        .unwrap()
});
static PY_FRAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^\s*File "(.+?)", line (\d+)"#).unwrap());
static PY_RAISE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z_][\w.]*(Error|Exception|Warning)(: .*)?$").unwrap());
static ESLINT_FILE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\S+\.(m?[jt]sx?|vue|svelte)$").unwrap());
static ESLINT_ROW: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s+(\d+):(\d+)\s+(error|warning)\s+(.+?)\s\s+[\w@/-]+$").unwrap()
});

/// Scan a finished command's output. `line`/`col` convert from the tools'
/// 1-based coordinates to the 0-based ones PROBLEMS uses (clamped at 0).
pub fn scan(output: &str) -> Vec<BuildDiag> {
    let mut out = Vec::new();
    // rustc: severity+message on one line, the location arrow later.
    let mut rustc_pending: Option<(DiagnosticSeverity, String)> = None;
    // python: the deepest frame of the traceback in progress.
    let mut py_frame: Option<(String, u32)> = None;
    // eslint: the file-path header the indented rows belong to.
    let mut eslint_file: Option<String> = None;
    // ruff: a `CODE msg` line, which counts only when the very next line
    // is its ` --> file:line:col` arrow.
    let mut ruff_pending: Option<(usize, String)> = None;

    let zero = |n: &str| n.parse::<u32>().unwrap_or(1).saturating_sub(1);
    for (i, line) in output.lines().enumerate() {
        if let Some(c) = RUSTC_HEAD.captures(line) {
            rustc_pending = Some((severity(&c[1]), c[3].to_string()));
            continue;
        }
        if let Some(c) = RUSTC_ARROW.captures(line) {
            if let Some((at, msg)) = ruff_pending.take()
                && at + 1 == i
            {
                out.push(BuildDiag {
                    file: c[1].to_string(),
                    line: zero(&c[2]),
                    col: zero(&c[3]),
                    severity: DiagnosticSeverity::Warning,
                    message: msg,
                    source: String::from("ruff"),
                    needs_file: false,
                });
                continue;
            }
            if let Some((sev, msg)) = rustc_pending.take() {
                out.push(BuildDiag {
                    file: c[1].to_string(),
                    line: zero(&c[2]),
                    col: zero(&c[3]),
                    severity: sev,
                    message: msg,
                    source: String::from("rustc"),
                    needs_file: false,
                });
            }
            continue;
        }
        if let Some(c) = TSC_PAREN
            .captures(line)
            .or_else(|| TSC_PRETTY.captures(line))
        {
            out.push(BuildDiag {
                file: c[1].to_string(),
                line: zero(&c[2]),
                col: zero(&c[3]),
                severity: severity(&c[4]),
                message: format!("{}: {}", &c[5], &c[6]),
                source: String::from("tsc"),
                needs_file: false,
            });
            continue;
        }
        if let Some(c) = PY_FRAME.captures(line) {
            py_frame = Some((c[1].to_string(), zero(&c[2])));
            continue;
        }
        if PY_RAISE.is_match(line) {
            if let Some((file, lno)) = py_frame.take() {
                out.push(BuildDiag {
                    file,
                    line: lno,
                    col: 0,
                    severity: DiagnosticSeverity::Error,
                    message: line.trim().to_string(),
                    source: String::from("python"),
                    needs_file: false,
                });
            }
            continue;
        }
        if ESLINT_FILE.is_match(line.trim_end()) {
            eslint_file = Some(line.trim().to_string());
            continue;
        }
        if let Some(c) = ESLINT_ROW.captures(line) {
            if let Some(file) = eslint_file.clone() {
                out.push(BuildDiag {
                    file,
                    line: zero(&c[1]),
                    col: zero(&c[2]),
                    severity: severity(&c[3]),
                    message: c[4].trim().to_string(),
                    source: String::from("eslint"),
                    needs_file: false,
                });
            }
            continue;
        }
        if let Some(c) = GENERIC.captures(line) {
            out.push(BuildDiag {
                file: c[1].to_string(),
                line: zero(&c[2]),
                col: zero(&c[3]),
                severity: severity(&c[4]),
                message: c[5].to_string(),
                source: String::from("build"),
                needs_file: false,
            });
            continue;
        }
        if let Some(c) = MS_COMPILE.captures(line) {
            out.push(BuildDiag {
                file: c[1].trim().to_string(),
                line: zero(&c[2]),
                col: c.get(3).map_or(0, |m| zero(m.as_str())),
                severity: severity(&c[4]),
                message: format!("{}: {}", &c[5], &c[6]),
                source: String::from("msCompile"),
                needs_file: false,
            });
            continue;
        }
        if let Some(c) = ESLINT_COMPACT.captures(line) {
            out.push(BuildDiag {
                file: c[1].to_string(),
                line: zero(&c[2]),
                col: zero(&c[3]),
                severity: severity(&c[4]),
                message: c[5].to_string(),
                source: String::from("eslint-compact"),
                needs_file: false,
            });
            continue;
        }
        if let Some(c) = NO_COLUMN.captures(line) {
            out.push(BuildDiag {
                file: c[1].to_string(),
                line: zero(&c[2]),
                col: 0,
                severity: severity(&c[3]),
                message: c[4].to_string(),
                source: String::from("no-column"),
                needs_file: false,
            });
            continue;
        }
        if let Some(c) = LINT.captures(line) {
            out.push(BuildDiag {
                file: c[1].to_string(),
                line: zero(&c[2]),
                col: zero(&c[3]),
                severity: DiagnosticSeverity::Warning,
                message: format!("{} {}", &c[4], &c[5]),
                source: String::from("lint"),
                needs_file: true,
            });
            continue;
        }
        if let Some(c) = GO.captures(line) {
            out.push(BuildDiag {
                file: c[1].to_string(),
                line: zero(&c[2]),
                col: zero(&c[3]),
                severity: DiagnosticSeverity::Error,
                message: c[4].to_string(),
                source: String::from("go"),
                needs_file: true,
            });
            continue;
        }
        if let Some(c) = RUFF_HEAD.captures(line) {
            ruff_pending = Some((i, format!("{} {}", &c[1], &c[2])));
            continue;
        }
        if line.trim().is_empty() {
            // A blank line ends an eslint file section; rustc/python state
            // survives blanks (both formats interleave them).
            eslint_file = None;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rustc_pairs_the_message_with_its_arrow_line() {
        let out = scan(
            "   Compiling demo v0.1.0\n\
             error[E0308]: mismatched types\n\
             \x20 --> src/main.rs:12:5\n\
             \x20  |\n\
             warning: unused variable: `x`\n\
             \x20 --> src/lib.rs:3:9\n\
             error: could not compile `demo`\n",
        );
        assert_eq!(out.len(), 2);
        assert_eq!(
            (out[0].file.as_str(), out[0].line, out[0].col),
            ("src/main.rs", 11, 4),
            "1-based tool coordinates become the 0-based PROBLEMS ones"
        );
        assert_eq!(out[0].severity, DiagnosticSeverity::Error);
        assert_eq!(out[0].message, "mismatched types");
        assert_eq!(out[1].severity, DiagnosticSeverity::Warning);
        assert_eq!(out[1].source, "rustc");
    }

    #[test]
    fn tsc_matches_both_output_shapes() {
        let out = scan(
            "src/app.ts(4,10): error TS2322: Type 'string' is not assignable.\n\
             src/util.ts:9:1 - error TS1005: ';' expected.\n",
        );
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].file, "src/app.ts");
        assert_eq!((out[0].line, out[0].col), (3, 9));
        assert!(out[1].message.starts_with("TS1005"));
    }

    #[test]
    fn generic_gcc_shape_and_python_traceback_deepest_frame() {
        let out = scan(
            "main.c:7:3: error: expected ';' before 'return'\n\
             Traceback (most recent call last):\n\
             \x20 File \"app.py\", line 30, in <module>\n\
             \x20   run()\n\
             \x20 File \"app.py\", line 12, in run\n\
             \x20   x = 1 / 0\n\
             ZeroDivisionError: division by zero\n",
        );
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].source, "build");
        assert_eq!(out[0].file, "main.c");
        assert_eq!(
            (out[1].file.as_str(), out[1].line),
            ("app.py", 11),
            "the DEEPEST frame is where it raised"
        );
        assert_eq!(out[1].message, "ZeroDivisionError: division by zero");
    }

    #[test]
    fn eslint_stylish_rows_attach_to_their_file_header() {
        let out = scan(
            "src/index.js\n\
             \x20 12:5  error  Unexpected console statement  no-console\n\
             \x20 20:1  warning  Missing semicolon  semi\n\
             \n\
             \x20 3:3  error  Orphan row without a file header  no-undef\n",
        );
        assert_eq!(out.len(), 2, "the blank line ends the file section");
        assert_eq!(out[0].file, "src/index.js");
        assert_eq!(out[0].message, "Unexpected console statement");
        assert_eq!(out[1].severity, DiagnosticSeverity::Warning);
    }

    #[test]
    fn fatal_error_counts_as_an_error() {
        let out = scan("bad.c:1:1: fatal error: 'nope.h' file not found\n");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].severity, DiagnosticSeverity::Error);
    }

    #[test]
    fn clean_output_yields_nothing() {
        assert!(scan("   Compiling demo\n    Finished dev profile\n").is_empty());
    }

    /// The issue's sample lines (#1594), one per tool.
    const TOOLS: &str = "app.py:2: error: Incompatible types in assignment (expression has type \"str\", variable has type \"int\")  [assignment]\n\
Main.java:1: error: incompatible types: String cannot be converted to int\n\
./main.go:4:6: declared and not used: unused\n\
app.py:1:8: F401 [*] `os` imported but unused\n\
app.py:1:1: F401 'os' imported but unused\n\
csrc/c.c:1:20: error: 'y' undeclared (first use in this function)\n";

    /// #1594: mypy, javac, go, ruff (concise), flake8 and gcc each yield
    /// their one row, at the tool's file, line and column.
    #[test]
    fn mypy_javac_go_ruff_flake8_and_gcc_rows_all_match() {
        let out = scan(TOOLS);
        let rows: Vec<(&str, u32, u32, &str)> = out
            .iter()
            .map(|d| (d.file.as_str(), d.line, d.col, d.source.as_str()))
            .collect();
        assert_eq!(
            rows,
            [
                ("app.py", 1, 0, "no-column"),
                ("Main.java", 0, 0, "no-column"),
                ("./main.go", 3, 5, "go"),
                ("app.py", 0, 7, "lint"),
                ("app.py", 0, 0, "lint"),
                ("csrc/c.c", 0, 19, "build"),
            ]
        );
        assert_eq!(out[0].severity, DiagnosticSeverity::Error);
        assert!(out[0].message.starts_with("Incompatible types"));
        assert_eq!(out[2].severity, DiagnosticSeverity::Error);
        assert_eq!(out[2].message, "declared and not used: unused");
        assert_eq!(out[3].message, "F401 `os` imported but unused");
        assert_eq!(out[3].severity, DiagnosticSeverity::Warning);
    }

    /// #1594: a mypy note is information, a warning a warning, and pylint's
    /// `CODE:` form reads like flake8's.
    #[test]
    fn notes_warnings_and_pylint_codes_keep_their_meaning() {
        let out = scan(
            "app.py:3: note: Revealed type is \"builtins.int\"\n\
             Main.java:5: warning: [removal] Integer(int) has been deprecated\n\
             app.py:1:0: C0114: Missing module docstring (missing-module-docstring)\n",
        );
        assert_eq!(out.len(), 3, "{out:?}");
        assert_eq!(out[0].severity, DiagnosticSeverity::Information);
        assert_eq!(out[1].severity, DiagnosticSeverity::Warning);
        assert_eq!(
            out[2].message,
            "C0114 Missing module docstring (missing-module-docstring)"
        );
    }

    /// #1594: a mypy or javac path under a directory with a space still
    /// matches, whole.
    #[test]
    fn a_no_column_path_may_contain_spaces() {
        let out = scan("/tmp/my project/app.py:2: error: Incompatible types\n");
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].file, "/tmp/my project/app.py");
        assert_eq!(out[0].line, 1);
    }

    /// #1594: ruff's default output puts the place on the arrow line under
    /// the code and message.
    #[test]
    fn ruff_default_pairs_the_code_with_its_arrow() {
        let out = scan(
            "F401 [*] `os` imported but unused\n\
             \x20--> app.py:1:8\n\
             \x20 |\n\
             1 | import os\n\
             \x20 |        ^^\n\
             \n\
             Found 1 error.\n",
        );
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(
            (out[0].file.as_str(), out[0].line, out[0].col),
            ("app.py", 0, 7)
        );
        assert_eq!(out[0].message, "F401 `os` imported but unused");
        assert_eq!(out[0].source, "ruff");
    }

    /// #1594: eslint's compact format and MSBuild's.
    #[test]
    fn eslint_compact_and_ms_compile_rows_match() {
        let out = scan(
            "/w/web/app.js: line 1, col 5, Error - 'a' is assigned a value but never used. (no-unused-vars)\n\
             Program.cs(12,9): error CS1002: ; expected\n",
        );
        assert_eq!(out.len(), 2, "{out:?}");
        assert_eq!((out[0].line, out[0].col), (0, 4));
        assert_eq!(out[0].severity, DiagnosticSeverity::Error);
        assert_eq!(out[0].source, "eslint-compact");
        assert_eq!(
            (out[1].file.as_str(), out[1].line, out[1].col),
            ("Program.cs", 11, 8)
        );
        assert_eq!(out[1].message, "CS1002: ; expected");
    }

    /// Negative (#1594): `grep -rn` output, a version banner that looks like
    /// a code and a code line with no arrow after it yield nothing; only the
    /// loose go and lint rows need an existing file.
    #[test]
    fn grep_output_and_loose_text_are_not_problems() {
        let out = scan(
            "./app.py:2:x: int = \"a\"\n\
             ./main.go:4:\tvar unused int\n\
             HTTP2 support enabled\n\
             see above\n\
             E501 line too long\n",
        );
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn display_columns_run_a_tab_to_the_next_stop() {
        // gcc 13: three tabs then `return u;` reports column 32 (1-based).
        let line = "\t\t\treturn u;";
        assert_eq!(char_col(line, 31, ColumnUnit::Display, 8), 10);
        assert_eq!(char_col(line, 0, ColumnUnit::Display, 8), 0);
        // Inside a tab: that tab. Past the end: the character count.
        assert_eq!(char_col(line, 5, ColumnUnit::Display, 8), 0);
        assert_eq!(char_col(line, 200, ColumnUnit::Display, 8), 12);
        // A tab stop of 4.
        assert_eq!(char_col("\tx", 4, ColumnUnit::Display, 4), 1);
        // A wide character takes two cells.
        assert_eq!(char_col("日本x", 4, ColumnUnit::Display, 8), 2);
    }

    #[test]
    fn byte_columns_count_utf8_bytes() {
        // clang: `\t\t\treturn u;` reports 11, `u` being byte 10.
        assert_eq!(char_col("\t\t\treturn u;", 10, ColumnUnit::Byte, 8), 10);
        // `é` is two bytes: byte 3 is the `x` after `aé`.
        assert_eq!(char_col("aéx", 3, ColumnUnit::Byte, 8), 2);
        assert_eq!(char_col("aéx", 2, ColumnUnit::Byte, 8), 1, "inside é");
    }

    #[test]
    fn column_unit_pref_and_tabstop_flag_parse() {
        assert_eq!(ColumnUnit::from_pref(" Byte "), ColumnUnit::Byte);
        assert_eq!(ColumnUnit::from_pref("chars"), ColumnUnit::Char);
        assert_eq!(ColumnUnit::from_pref(""), ColumnUnit::Display);
        assert_eq!(ColumnUnit::from_pref("nonsense"), ColumnUnit::Display);
        assert_eq!(tabstop_from_command("make CFLAGS=-ftabstop=4"), 8);
        assert_eq!(tabstop_from_command("gcc -ftabstop=4 -c x.c"), 4);
        assert_eq!(tabstop_from_command("gcc -ftabstop=0 -c x.c"), 8);
        assert_eq!(tabstop_from_command("make"), 8);
    }

    #[test]
    fn convert_columns_rewrites_only_build_rows_with_tabs_or_non_ascii() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("t.c"),
            "int main(void)\n{\n\t\t\tint u;\n\t\t\treturn u;\n}\nplain u;\n",
        )
        .unwrap();
        let row = |line: u32, col: u32, source: &str| BuildDiag {
            file: "t.c".into(),
            line,
            col,
            severity: DiagnosticSeverity::Warning,
            message: String::new(),
            source: source.into(),
            needs_file: false,
        };
        let mut d = vec![
            row(3, 31, "build"),
            row(5, 6, "build"),
            row(3, 31, "rustc"),
            row(99, 5, "build"),
        ];
        convert_columns(&mut d, dir.path(), ColumnUnit::Display, 8);
        assert_eq!(d[0].col, 10, "tabbed line: display column to character");
        assert_eq!(d[1].col, 6, "plain ASCII line is untouched");
        assert_eq!(d[2].col, 31, "another tool's row is untouched");
        assert_eq!(d[3].col, 5, "a line past the file is untouched");
        let mut same = vec![row(3, 31, "build")];
        convert_columns(&mut same, dir.path(), ColumnUnit::Char, 8);
        assert_eq!(same[0].col, 31, "Char leaves the column alone");
    }

    /// The conversion's reads are bounded per batch: once the budget is
    /// spent, the remaining files' rows keep their columns instead of being
    /// read, and rows of one file are all converted from its single read.
    #[test]
    fn convert_columns_stops_reading_once_the_batch_budget_is_spent() {
        let dir = tempfile::tempdir().unwrap();
        let body = "\tint u;\n";
        for name in ["a.c", "b.c"] {
            std::fs::write(dir.path().join(name), body).unwrap();
        }
        let row = |file: &str| BuildDiag {
            file: file.into(),
            line: 0,
            col: 10,
            severity: DiagnosticSeverity::Warning,
            message: String::new(),
            source: "build".into(),
            needs_file: false,
        };
        let mut d = vec![row("a.c"), row("b.c"), row("a.c")];
        // Room for one file only.
        convert_columns_within(
            &mut d,
            dir.path(),
            ColumnUnit::Display,
            8,
            body.len() as u64,
        );
        assert_eq!(d[0].col, 3, "the first file is read and converted");
        assert_eq!(d[2].col, 3, "its second row comes from the same read");
        assert_eq!(d[1].col, 10, "the file past the budget is not read");
    }

    /// A printed path naming a device is never read: `/dev/zero` reports a
    /// length of 0 and would read forever on the UI thread.
    #[cfg(unix)]
    #[test]
    fn convert_columns_never_reads_a_device_a_build_message_names() {
        let mut d = vec![BuildDiag {
            file: String::from("/dev/zero"),
            line: 0,
            col: 5,
            severity: DiagnosticSeverity::Error,
            message: String::from("boom"),
            source: String::from("build"),
            needs_file: false,
        }];
        convert_columns(&mut d, std::path::Path::new("/"), ColumnUnit::Display, 8);
        assert_eq!(d[0].col, 5, "the row keeps its column");
    }

    /// The one-walk conversion of a line's columns gives exactly what
    /// `char_col` gives for each column on its own, in both units, across
    /// tabs, wide characters, zero-width ones and the end of the line.
    #[test]
    fn char_cols_matches_char_col_column_by_column() {
        let line = "\t a\u{4e2d}e\u{301}b\tc\u{1f600}d";
        let cols: Vec<usize> = (0..48).collect();
        for unit in [ColumnUnit::Byte, ColumnUnit::Display, ColumnUnit::Char] {
            for tabstop in [1, 4, 8] {
                let one: Vec<usize> = cols
                    .iter()
                    .map(|&c| char_col(line, c, unit, tabstop))
                    .collect();
                assert_eq!(
                    char_cols(line, &cols, unit, tabstop),
                    one,
                    "{unit:?} tabstop {tabstop}"
                );
            }
        }
        assert_eq!(char_cols("", &[0, 3], ColumnUnit::Display, 8), [0, 0]);
    }

    /// The read itself never blocks on a FIFO, even one that takes the
    /// path's place after the metadata check: the open does not wait for a
    /// writer, and the opened handle is not a regular file, so nothing is
    /// read.
    #[cfg(unix)]
    #[test]
    fn read_capped_neither_blocks_on_nor_reads_a_fifo() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("pipe");
        let c = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        // SAFETY: a valid NUL-terminated path and a plain mode.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        assert_eq!(read_capped(&fifo, 1024), None);
        let file = dir.path().join("t.c");
        std::fs::write(&file, "int u;\n").unwrap();
        assert_eq!(read_capped(&file, 1024).as_deref(), Some("int u;\n"));
        assert_eq!(
            read_capped(&file, 3),
            None,
            "a file past the cap is not read"
        );
    }
}
