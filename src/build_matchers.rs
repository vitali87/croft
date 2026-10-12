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
//! caller keeps them only for a file that exists ([`needs_existing_file`]).

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
    /// Which matcher produced it: a built-in tag (`rustc`, `tsc`, …) or a
    /// user matcher's name (#252).
    pub source: String,
}

fn severity(word: &str) -> DiagnosticSeverity {
    match word.to_ascii_lowercase().as_str() {
        "error" | "fatal error" => DiagnosticSeverity::Error,
        "warning" => DiagnosticSeverity::Warning,
        _ => DiagnosticSeverity::Information,
    }
}

/// Whether a row from matcher `source` is kept only when its file exists
/// (#1594): `file.go:1:2: text` and `file:1:2: X1 text` are shapes other
/// output can take, where every other built-in row needs a severity word.
pub fn needs_existing_file(source: &str) -> bool {
    matches!(source, "go" | "lint")
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
    Regex::new(r"^(\S+?\.[A-Za-z0-9]+):(\d+): (error|warning|note): (.+)$").unwrap()
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
                source: String::from("build"),
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
                ("app.py", 1, 0, "build"),
                ("Main.java", 0, 0, "build"),
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
        assert!(needs_existing_file("go") && needs_existing_file("lint"));
        assert!(!needs_existing_file("build") && !needs_existing_file("ruff"));
    }
}
