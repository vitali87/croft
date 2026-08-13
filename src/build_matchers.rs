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

use std::sync::LazyLock;

use regex::Regex;

use crate::lsp::manager::DiagnosticSeverity;

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
    pub source: &'static str,
}

fn severity(word: &str) -> DiagnosticSeverity {
    match word {
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

    let zero = |n: &str| n.parse::<u32>().unwrap_or(1).saturating_sub(1);
    for line in output.lines() {
        if let Some(c) = RUSTC_HEAD.captures(line) {
            rustc_pending = Some((severity(&c[1]), c[3].to_string()));
            continue;
        }
        if let Some(c) = RUSTC_ARROW.captures(line) {
            if let Some((sev, msg)) = rustc_pending.take() {
                out.push(BuildDiag {
                    file: c[1].to_string(),
                    line: zero(&c[2]),
                    col: zero(&c[3]),
                    severity: sev,
                    message: msg,
                    source: "rustc",
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
                source: "tsc",
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
                    source: "python",
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
                    source: "eslint",
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
                source: "build",
            });
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
}
