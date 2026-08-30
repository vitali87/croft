//! Where a failing test actually failed (#373).
//!
//! The runners all print the assertion's location, in their own shapes, and
//! that location is the only thing standing between "debug this test" and
//! "debug this test and stop where it broke". Parsing it is pure, so it is
//! here rather than tangled into the streaming worker.
//!
//! Two rules shape every matcher below:
//!
//! * **A location that is not the test's own code is worse than none.** A
//!   panic inside the standard library, or inside a dependency under
//!   `~/.cargo/registry`, names a file the user cannot usefully break on —
//!   and a breakpoint there fires on the way in, before the interesting
//!   state exists. Those are rejected, not offered.
//! * **The LAST location wins.** Runners print an outer frame after an
//!   inner one (pytest's traceback ends at the assertion; libtest's panic
//!   line follows the assertion's own output), so a parser that keeps the
//!   first match stops at the wrong frame.

use std::path::{Path, PathBuf};

/// A parsed failure location: where the assertion that failed lives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailureSite {
    /// Path exactly as the runner printed it — relative to the workspace
    /// root for cargo and pytest, absolute for some jest reporters.
    pub file: PathBuf,
    /// 1-based line, as every runner reports it and as a breakpoint wants.
    pub line: u32,
}

impl FailureSite {
    /// The site as an absolute path under `root`, if the file exists. A
    /// location naming a file that is not there (a stale path, a container
    /// path from a remote run) yields `None` rather than a breakpoint the
    /// adapter would silently drop.
    pub fn resolve(&self, root: &Path) -> Option<(PathBuf, u32)> {
        let abs = if self.file.is_absolute() {
            self.file.clone()
        } else {
            root.join(&self.file)
        };
        abs.is_file().then_some((abs, self.line))
    }
}

/// Whether `path` is somewhere a breakpoint would help. A dependency's
/// source or the standard library is not: the user cannot fix it there,
/// and stopping inside it buries the frame they wanted.
fn is_user_code(path: &str) -> bool {
    const FOREIGN: [&str; 8] = [
        "/.cargo/registry/",
        "/.cargo/git/",
        "/rustc/",
        "/site-packages/",
        "/dist-packages/",
        "/node_modules/",
        "/.rustup/",
        "/lib/python3",
    ];
    !FOREIGN.iter().any(|f| path.contains(f))
}

/// Parse a `file:line` pair out of `text`, rejecting foreign code. Accepts
/// a trailing `:column`, which every runner may or may not print.
fn site_from(file: &str, line: &str) -> Option<FailureSite> {
    let file = file.trim();
    if file.is_empty() || !is_user_code(file) {
        return None;
    }
    let line: u32 = line.trim().parse().ok()?;
    // Line 0 is not a line: a runner printing it means it did not know.
    if line == 0 {
        return None;
    }
    Some(FailureSite {
        file: PathBuf::from(file),
        line,
    })
}

/// libtest's panic line: `thread 'x' panicked at src/lib.rs:42:5:`, with
/// the older `thread 'x' panicked at 'msg', src/lib.rs:42:5` form also
/// accepted, since a user's toolchain may predate the 2023 change.
fn parse_rust(line: &str) -> Option<FailureSite> {
    let rest = line.split_once(" panicked at ")?.1;
    // New form: the location runs to the trailing colon. Old form: the
    // location follows the quoted message.
    let loc = match rest.rsplit_once("', ") {
        Some((_, after)) => after,
        None => rest,
    };
    let loc = loc.trim().trim_end_matches(':');
    let mut parts = loc.rsplitn(3, ':');
    let last = parts.next()?;
    let mid = parts.next()?;
    let head = parts.next();
    match head {
        // file:line:col
        Some(file) => site_from(file, mid),
        // file:line
        None => site_from(mid, last),
    }
}

/// pytest's traceback frames: `path/to/test_x.py:42: AssertionError`, and
/// the `E   assert ...` marker's own `file:line: in func` frames.
fn parse_pytest(line: &str) -> Option<FailureSite> {
    let trimmed = line.trim();
    // `tests/test_a.py:12: AssertionError` — a frame line ends with the
    // exception name after the colon.
    let (loc, tail) = trimmed.rsplit_once(": ")?;
    if tail.is_empty() || tail.starts_with(' ') {
        return None;
    }
    let (file, num) = loc.rsplit_once(':')?;
    if !file.ends_with(".py") {
        return None;
    }
    site_from(file, num)
}

/// jest / vitest stack frames: `at Object.<anonymous> (src/a.test.ts:12:9)`
/// and the bare `src/a.test.ts:12:9` form some reporters print.
fn parse_js(line: &str) -> Option<FailureSite> {
    let trimmed = line.trim();
    let inner = match (trimmed.rfind('('), trimmed.rfind(')')) {
        (Some(o), Some(c)) if c > o => &trimmed[o + 1..c],
        _ => trimmed.strip_prefix("at ").unwrap_or(trimmed),
    };
    let mut parts = inner.rsplitn(3, ':');
    let _col = parts.next()?;
    let num = parts.next()?;
    let file = parts.next()?;
    if !file.contains('.') {
        return None;
    }
    site_from(file, num)
}

/// The failure location from a runner's output, or `None` when it named
/// none in the user's own code.
///
/// The LAST match wins: every runner prints outward-in, so the final
/// location in the block is the assertion itself rather than the frame
/// that called it.
pub fn failure_site(output: &str) -> Option<FailureSite> {
    let mut found = None;
    for line in output.lines() {
        if let Some(site) = parse_rust(line)
            .or_else(|| parse_pytest(line))
            .or_else(|| parse_js(line))
        {
            found = Some(site);
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site(f: &str, l: u32) -> Option<FailureSite> {
        Some(FailureSite {
            file: PathBuf::from(f),
            line: l,
        })
    }

    /// libtest, both the current and the pre-2023 panic shapes.
    #[test]
    fn rust_panic_lines_yield_the_assertion_site() {
        assert_eq!(
            failure_site("thread 'tests::adds' panicked at src/lib.rs:42:5:\nassertion failed"),
            site("src/lib.rs", 42)
        );
        // The older form, with the message inline before the location.
        assert_eq!(
            failure_site("thread 'main' panicked at 'assertion failed: x', src/a.rs:7:9"),
            site("src/a.rs", 7)
        );
        // No column.
        assert_eq!(
            failure_site("thread 'x' panicked at src/b.rs:3:"),
            site("src/b.rs", 3)
        );
    }

    /// pytest names the frame and the exception; the LAST frame is the
    /// assertion, not the helper that called it.
    #[test]
    fn pytest_tracebacks_take_the_innermost_frame() {
        let out = "\
tests/test_math.py:8: in test_adds
    helper()
tests/helpers.py:3: in helper
    assert 1 == 2
tests/helpers.py:3: AssertionError";
        assert_eq!(failure_site(out), site("tests/helpers.py", 3));
    }

    /// jest and vitest print bracketed stack frames.
    #[test]
    fn js_stack_frames_yield_the_last_user_frame() {
        let out = "\
    at Object.<anonymous> (src/sum.test.ts:12:9)
    at processTicksAndRejections (node:internal/process/task_queues:95:5)";
        // The node: internal frame has no path separator we accept, so the
        // user's own frame is what survives.
        assert_eq!(failure_site(out), site("src/sum.test.ts", 12));
        assert_eq!(failure_site("src/a.test.js:4:1"), site("src/a.test.js", 4));
    }

    /// A location the user cannot act on is worse than none: a breakpoint
    /// in a dependency fires on the way in, before the interesting state.
    #[test]
    fn foreign_code_is_rejected_rather_than_offered() {
        for line in [
            "thread 'x' panicked at /Users/me/.cargo/registry/src/serde-1.0/lib.rs:9:1:",
            "thread 'x' panicked at /rustc/abc123/library/core/src/option.rs:1:1:",
            "  at expect (/w/node_modules/expect/build/index.js:2:3)",
            "/usr/lib/python3.11/unittest/case.py:12: AssertionError",
        ] {
            assert_eq!(failure_site(line), None, "must reject: {line}");
        }
        // A test whose own path merely mentions a foreign-looking name is
        // still the user's code.
        assert_eq!(
            failure_site("thread 'x' panicked at src/node_modules_test.rs:5:1:"),
            site("src/node_modules_test.rs", 5)
        );
    }

    /// Output with no location at all, and shapes that must not be
    /// mistaken for one.
    #[test]
    fn output_without_a_usable_location_yields_none() {
        for text in [
            "",
            "test tests::adds ... FAILED",
            "assertion `left == right` failed",
            "thread 'x' panicked at src/lib.rs:0:0:",
            "note: run with `RUST_BACKTRACE=1`",
            "warning: unused variable: `x`",
        ] {
            assert_eq!(failure_site(text), None, "must not match: {text:?}");
        }
    }

    /// Resolution is against the workspace, and a path that is not there
    /// yields nothing rather than a breakpoint the adapter would drop.
    #[test]
    fn resolving_requires_the_file_to_exist() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        let f = dir.path().join("src/lib.rs");
        std::fs::write(&f, "fn main() {}\n").unwrap();

        let s = FailureSite {
            file: PathBuf::from("src/lib.rs"),
            line: 42,
        };
        assert_eq!(s.resolve(dir.path()), Some((f.clone(), 42)));

        let missing = FailureSite {
            file: PathBuf::from("src/nope.rs"),
            line: 1,
        };
        assert_eq!(missing.resolve(dir.path()), None);

        // An absolute path is taken as-is rather than joined to the root.
        let abs = FailureSite {
            file: f.clone(),
            line: 3,
        };
        assert_eq!(abs.resolve(Path::new("/elsewhere")), Some((f, 3)));
    }
}
