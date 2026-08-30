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
//! * **Which location wins depends on the runner.** libtest prints
//!   exactly one panic line per failure, before its message, so the FIRST
//!   is the site — taking the last would let a test asserting on panic
//!   TEXT hand us a location out of its own diff. pytest and jest nest
//!   their frames outward-in, so for those the LAST is the assertion.

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
        // A `..` component would resolve outside the workspace; a runner
        // does not print one, so its presence means the path is not what
        // it appears to be.
        if self
            .file
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return None;
        }
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
    // Windows separators first: a `\node_modules\` path is as foreign as
    // its POSIX twin, and a slash-anchored needle would miss it entirely.
    let norm = path.replace('\\', "/");
    // Component-wise, so a directory the user happened to name `rustc` or
    // `site-packages-notes` is still their own code. A bare relative
    // `node_modules/expect/index.js` — what jest prints under --rootDir —
    // has no leading slash, so anchoring on one would miss it.
    const FOREIGN_DIRS: [&str; 5] = [
        "node_modules",
        "site-packages",
        "dist-packages",
        ".rustup",
        "rustc",
    ];
    let mut comps = norm.split('/').peekable();
    while let Some(c) = comps.next() {
        if FOREIGN_DIRS.contains(&c) {
            return false;
        }
        // `.cargo/registry` and `.cargo/git` only as a PAIR: a user
        // directory called `.cargo` holding their own sources is theirs.
        if c == ".cargo" && matches!(comps.peek(), Some(&"registry") | Some(&"git")) {
            return false;
        }
        // The interpreter's own standard library: `lib/python3.11/...`,
        // as a PAIR so a user's `lib/` or a project called `python3-shim`
        // is untouched.
        if c == "lib" && comps.peek().is_some_and(|n| n.starts_with("python3")) {
            return false;
        }
    }
    true
}

/// Parse a `file:line` pair out of `text`, rejecting foreign code. Accepts
/// a trailing `:column`, which every runner may or may not print.
///
/// A candidate that is prose rather than a path is refused here: a runner
/// prints a bare path, so whitespace or a quote inside it means the line
/// was matched by accident and the "path" is a sentence fragment.
fn site_from(file: &str, line: &str) -> Option<FailureSite> {
    let file = file.trim();
    if file.is_empty()
        || file.contains(char::is_whitespace)
        || file.contains('"')
        || file.contains('\'')
        || !is_user_code(file)
    {
        return None;
    }
    let line = line.trim();
    // `+5` and `007` parse as numbers but no runner prints either: the
    // field is not a line number and the match is spurious.
    if line.is_empty() || !line.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let line: u32 = line.parse().ok()?;
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
    // Every libtest panic line opens `thread '<name>' panicked at`. A line
    // that merely CONTAINS the phrase is quoted data — a test asserting on
    // panic text, printed before or after the real failure — and the two
    // guards below do not catch an unquoted decoy on their own.
    if !line.trim_start().starts_with("thread '") {
        return None;
    }
    let rest = line.split_once(" panicked at ")?.1;
    // New form: the location runs to the trailing colon. Old form: the
    // location follows the quoted message.
    let loc = match rest.rsplit_once("', ") {
        Some((_, after)) => after,
        None => rest,
    };
    // `strip_suffix`, not `trim_end_matches`: libtest prints exactly one
    // trailing colon, and stripping a run of them would let `a.rs:42::::`
    // parse as a location.
    let loc = loc.trim();
    let loc = loc.strip_suffix(':').unwrap_or(loc);
    // Guard the WHOLE location before splitting. Checking only the file
    // field afterwards let a quoted decoy through: `rsplitn` consumed the
    // closing quote as the "column", leaving a quote-free file field that
    // passed every test — while still carrying a colon no real path from
    // this split ever has.
    if loc.contains('"') || loc.contains('\'') {
        return None;
    }
    let mut parts = loc.rsplitn(3, ':');
    let last = parts.next()?;
    let mid = parts.next()?;
    let head = parts.next();
    match head {
        // file:line:col — the file field cannot itself hold a colon here,
        // so one means the split landed inside something that is not a
        // location at all.
        Some(file) if !file.contains(':') => site_from(file, mid),
        Some(_) => None,
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
///
/// Anchored deliberately. An earlier version accepted ANY line ending
/// `:N:M` and treated the prefix as a filename, which made it a catch-all
/// for everything the other parsers rejected — including panic lines
/// naming a dependency, whose foreign path was then laundered into an
/// accepted "file" because it was no longer being read as a path.
fn parse_js(line: &str) -> Option<FailureSite> {
    let trimmed = line.trim();
    let inner = match (trimmed.rfind('('), trimmed.rfind(')')) {
        (Some(o), Some(c)) if c > o => &trimmed[o + 1..c],
        // A bare frame is the whole line, optionally with jest's `at `.
        _ => match trimmed.strip_prefix("at ") {
            Some(rest) => rest,
            // Only a line that IS a frame, not one that ends like one.
            None if !trimmed.contains(char::is_whitespace) => trimmed,
            None => return None,
        },
    };
    let inner = inner.trim();
    // Both a line AND a column: a frame always carries both, and requiring
    // the pair is what stops a bare `file:line` elsewhere being read here.
    let mut parts = inner.rsplitn(3, ':');
    let col = parts.next()?;
    let num = parts.next()?;
    let file = parts.next()?;
    if !col.chars().all(|c| c.is_ascii_digit()) || col.is_empty() {
        return None;
    }
    // A source file has an extension; a `node:internal/...` pseudo-frame
    // and a prose fragment do not.
    let stem = file.rsplit(['/', '\\']).next().unwrap_or(file);
    if !stem.contains('.') {
        return None;
    }
    site_from(file, num)
}

/// Whether `line` is a runner's BANNER opening the failure block for the
/// test `name` (or its leaf).
///
/// Matching on a banner rather than on any mention is what stops the two
/// ways the scan otherwise goes wrong: libtest reprints every failing test
/// name in a trailing `failures:` summary, so "the last line mentioning
/// the name" is the summary entry and the block after it holds no panic
/// line at all; and a name that is a PREFIX of another test's name
/// (`adds` against `adds_two`) matches the wrong block in both
/// directions. The name is compared whole, against the banner's own
/// captured name.
pub fn is_failure_banner(line: &str, name: &str) -> bool {
    let leaf = name.rsplit("::").next().unwrap_or(name);
    let t = line.trim();
    // libtest: `---- tests::adds stdout ----`
    if let Some(rest) = t.strip_prefix("---- ")
        && let Some(named) = rest.split_whitespace().next()
    {
        // Compare whole names, and the banner's own LEAF against a leaf
        // the caller passed: `adds` must match `---- tests::adds ----`
        // but never `---- tests::adds_two ----`.
        let named_leaf = named.rsplit("::").next().unwrap_or(named);
        return named == name || named_leaf == leaf;
    }
    // pytest: `___ test_adds ___` / `=== FAILURES ===` sections name the
    // test between underscore runs.
    let stripped = t.trim_matches(['_', '=', ' ']);
    if (t.starts_with('_') || t.starts_with('=')) && !stripped.is_empty() {
        // pytest prints `TestClass.test_adds` for class-based tests (a
        // DOT), `TestClass::test_adds` in a node id, or a bare function
        // name. Split on both so a `unittest.TestCase` subclass — entirely
        // idiomatic — is not silently unsupported.
        let named = stripped
            .rsplit(['.', ':'])
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or(stripped);
        return stripped == name || named == leaf;
    }
    // jest / vitest: `● suite › test name` or `FAIL src/a.test.ts > name`
    if let Some(rest) = t.strip_prefix('\u{25cf}') {
        // Split on jest's own separator only. Splitting on `>` too broke
        // any test whose TITLE contains one (`handles a > b`), which is
        // ordinary in a comparison suite.
        let named = rest.rsplit('\u{203a}').next().unwrap_or(rest).trim();
        return named == name || named == leaf;
    }
    // jest/vitest also print `FAIL src/a.test.ts > name` and vitest's `x`.
    for prefix in ["FAIL ", "\u{d7} "] {
        if let Some(rest) = t.strip_prefix(prefix) {
            // The real separator only, for the same reason as the arm
            // above: a title containing `>` is ordinary.
            let named = rest.rsplit('\u{203a}').next().unwrap_or(rest).trim();
            return named == name || named == leaf;
        }
    }
    false
}

/// The failure location from a runner's output, or `None` when it named
/// none in the user's own code.
///
/// libtest prints exactly ONE panic line per failure, before its message,
/// so the FIRST panic line is the site — taking the last would let a test
/// that asserts on panic TEXT (an ordinary thing to do: error formatting,
/// log output) hand us a location out of its own assertion diff. pytest
/// and jest genuinely nest their frames outward-in, so for those the last
/// match is the assertion. Hence: a panic line wins outright and early;
/// otherwise the last frame wins.
pub fn failure_site(output: &str) -> Option<FailureSite> {
    let mut frame = None;
    for line in output.lines() {
        if let Some(site) = parse_rust(line) {
            return Some(site);
        }
        if let Some(site) = parse_pytest(line).or_else(|| parse_js(line)) {
            frame = Some(site);
        }
    }
    frame
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

    /// The parsers must reject prose. An earlier version accepted any line
    /// ending `:N:M` as a jest frame, which made it a catch-all for
    /// everything the other parsers rejected — laundering a dependency's
    /// path into an accepted "file" because it was no longer read as one.
    #[test]
    fn prose_is_not_a_stack_frame() {
        for line in [
            // A Windows panic line: `rsplitn` on ':' would split the drive.
            r"thread 'x' panicked at C:\src\lib.rs:42:",
            // The same, naming a dependency — the laundering case.
            r"thread 'x' panicked at C:\Users\me\.cargo\registry\src\s\lib.rs:9:",
            // Overflow and negatives must not survive as part of a path.
            "thread 'x' panicked at src/a.rs:4294967296:1:",
            "thread 'x' panicked at src/a.rs:-5:1:",
            // A compile warning's arrow marker is not a frame.
            "  --> src/helper.rs:77:4",
            // A sentence that happens to end in numbers.
            "note: expected 3 items, found 2:1:1",
        ] {
            let got = failure_site(line);
            assert!(
                got.is_none(),
                "prose must not parse as a frame: {line:?} -> {got:?}"
            );
        }
        // The control: a real frame still parses, so the tightening did
        // not simply reject everything.
        assert_eq!(
            failure_site("    at f (src/a.test.ts:12:9)"),
            site("src/a.test.ts", 12)
        );
    }

    /// A test asserting on panic TEXT (error formatting, log output) prints
    /// locations inside its own diff. libtest prints exactly one panic line
    /// per failure, first, so the site is the first — taking the last hands
    /// the debugger a location out of the assertion's data.
    #[test]
    fn a_location_quoted_in_an_assertion_does_not_win() {
        let out = "\
thread 'tests::formats' panicked at src/render.rs:88:9:
assertion `left == right` failed
  left: \"thread 'w' panicked at /etc/passwd.rs:1:1:\"
 right: \"thread 'w' panicked at src/other.rs:2:2:\"";
        assert_eq!(failure_site(out), site("src/render.rs", 88));
    }

    /// A decoy with NO quote at all — `expected panicked at src/x.rs:5:1`
    /// — is caught by neither the quote guard nor the colon guard, only by
    /// the `thread '` anchor. Pinned separately so the anchor cannot be
    /// removed as redundant.
    #[test]
    fn an_unquoted_decoy_is_rejected_by_the_thread_anchor() {
        assert_eq!(
            failure_site("  expected panicked at src/decoy.rs:5:1"),
            None
        );
        assert_eq!(failure_site("note: it panicked at src/decoy.rs:5:1:"), None);
        // The control: a real panic line still parses, so the anchor did
        // not simply reject everything.
        assert_eq!(
            failure_site("thread 'tests::a' panicked at src/lib.rs:9:1:"),
            site("src/lib.rs", 9)
        );
    }

    /// A decoy printed BEFORE the real panic must not win either. The
    /// first-wins rule fixed the decoy-after case and opened this one:
    /// any test that prints to stdout before failing (a logging test, a
    /// snapshot test) can reach it.
    #[test]
    fn a_quoted_location_loses_from_either_side() {
        // Decoy first, real panic after.
        let out = "\
running 1 test
  saw: \"thread 'w' panicked at src/decoy.rs:1:1:\"
thread 'tests::logs' panicked at src/render.rs:88:9:
assertion failed";
        assert_eq!(failure_site(out), site("src/render.rs", 88));

        // Decoy only, with no real panic at all: nothing is offered.
        let only = "  left: \"thread 'w' panicked at src/decoy.rs:5:1:\"";
        assert_eq!(failure_site(only), None);

        // And the original direction still holds.
        let after = "\
thread 'tests::formats' panicked at src/render.rs:88:9:
  right: \"thread 'w' panicked at src/other.rs:2:2:\"";
        assert_eq!(failure_site(after), site("src/render.rs", 88));
    }

    /// Banners, not mentions: libtest reprints every failing name in its
    /// trailing summary, and a name that is a PREFIX of another test's
    /// must not match its block.
    #[test]
    fn banners_identify_a_block_by_its_whole_name() {
        assert!(is_failure_banner(
            "---- tests::adds stdout ----",
            "tests::adds"
        ));
        assert!(is_failure_banner("---- tests::adds stdout ----", "adds"));
        assert!(
            !is_failure_banner("---- tests::adds_two stdout ----", "tests::adds"),
            "a prefix of another test's name is not this test's banner"
        );
        assert!(
            !is_failure_banner("    tests::adds", "tests::adds"),
            "the trailing summary entry is not a banner"
        );
        assert!(
            !is_failure_banner("test tests::adds ... FAILED", "tests::adds"),
            "nor is the run line"
        );
        // pytest and jest shapes.
        assert!(is_failure_banner("____ test_adds ____", "test_adds"));
        // Class-based pytest prints a DOT, and is entirely idiomatic.
        assert!(is_failure_banner(
            "____ TestMath.test_adds ____",
            "tests/test_a.py::TestMath::test_adds"
        ));
        assert!(is_failure_banner(
            "____ TestMath.test_adds ____",
            "test_adds"
        ));
        assert!(!is_failure_banner(
            "____ TestMath.test_other ____",
            "test_adds"
        ));
        // A vitest title containing `>` is ordinary in a comparison suite.
        assert!(is_failure_banner(
            "\u{25cf} sum \u{203a} handles a > b",
            "handles a > b"
        ));
        assert!(is_failure_banner(
            "FAIL src/sum.test.ts \u{203a} adds",
            "adds"
        ));
        assert!(is_failure_banner(
            "\u{25cf} suite \u{203a} test_adds",
            "test_adds"
        ));
        assert!(!is_failure_banner("____ test_adds_two ____", "test_adds"));
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
