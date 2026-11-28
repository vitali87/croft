//! vitest's and jest's command lines: the project's own binary, vitest projects, and
//! the arguments that pick one test, a suite or a file.

use super::*;

/// Absolute path to a JS test runner binary for a workspace: the project's own
/// `node_modules/.bin/<name>` first (the installed version, with the project's
/// config resolvable), then `PATH`, then the usual global dirs — absolute for
/// the same GUI-stripped-PATH reason as [`cargo_cmd`].
pub(super) fn js_binary(root: &Path, name: &str) -> PathBuf {
    let path_dirs = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .unwrap_or_default();
    let dirs = std::iter::once(root.join("node_modules").join(".bin"))
        .chain(path_dirs)
        .chain(["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from));
    crate::proc::find::first_in(name, dirs).unwrap_or_else(|| PathBuf::from(name))
}

/// `<vitest|jest> <args>` rooted at the workspace, with piped stdio.
/// `NO_COLOR` strips ANSI from the parsed stream and `CI` keeps vitest out of
/// watch/interactive mode.
pub(super) fn js_cmd<S: AsRef<std::ffi::OsStr>>(root: &Path, runner: &str, args: &[S]) -> Command {
    js_cmd_for(root, runner, args, crate::node::dir().as_deref())
}

/// [`js_cmd`] under the node in `node`, the folder [`crate::node::dir`]
/// found off croft's `PATH`: the runner's script starts with
/// `#!/usr/bin/env node`, so that folder goes first on its `PATH` (#2809).
pub(super) fn js_cmd_for<S: AsRef<std::ffi::OsStr>>(
    root: &Path,
    runner: &str,
    args: &[S],
    node: Option<&Path>,
) -> Command {
    let mut cmd = Command::new(js_binary(root, runner));
    crate::node::put(&mut cmd, node);
    cmd.args(args)
        .current_dir(root)
        .env("NO_COLOR", "1")
        .env("CI", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

/// Split a croft JS node ID (`file::describe...::test`) into its file and an
/// optional title: the first segment is always the test file, the last (when
/// present) the name filter to hand `-t`.
pub(super) fn js_id_parts(id: &str) -> (&str, Option<&str>) {
    match id.split_once("::") {
        Some((file, rest)) => (file, rest.rsplit("::").next()),
        None => (id, None),
    }
}

/// Whether a run-filter pattern is a node-ID (prefix) rooted at a test file,
/// as opposed to a bare title from run-at-cursor.
pub(super) fn is_js_file(pattern: &str) -> bool {
    crate::testing::parse::is_js_test_file(js_id_parts(pattern).0)
}

/// The config file stems and extensions vitest reads its `projects` from.
pub(super) const VITEST_CONFIG_STEMS: [&str; 2] = ["vitest.config", "vite.config"];

pub(super) const VITEST_CONFIG_EXTS: [&str; 6] = ["ts", "mts", "cts", "js", "mjs", "cjs"];

/// Whether `root` runs vitest projects (#1504), formerly workspaces: a
/// `vitest.workspace.*` or `vitest.projects.*` file, or a config declaring
/// `projects` (or the older `workspace`). There tap-flat names a test by its file relative to its
/// PROJECT, without the project, so two packages' `x.test.ts` collapse into
/// one suite and same-named tests overwrite each other. A false positive
/// costs only the live tree updates: the run is read from its JSON report.
pub(super) fn vitest_has_projects(root: &Path) -> bool {
    let ws_file = ["vitest.workspace", "vitest.projects"].iter().any(|stem| {
        VITEST_CONFIG_EXTS
            .iter()
            .chain(&["json"])
            .any(|ext| root.join(format!("{stem}.{ext}")).is_file())
    });
    ws_file
        || VITEST_CONFIG_STEMS.iter().any(|stem| {
            VITEST_CONFIG_EXTS.iter().any(|ext| {
                std::fs::read_to_string(root.join(format!("{stem}.{ext}")))
                    .is_ok_and(|config| declares_vitest_projects(&config))
            })
        })
}

/// Whether config source declares a `projects:` or `workspace:` key, quoted
/// or not. Not a parser: a key-shaped word is enough.
pub(super) fn declares_vitest_projects(config: &str) -> bool {
    ["projects", "workspace"].iter().any(|key| {
        config.match_indices(key).any(|(i, _)| {
            let word_start = config[..i]
                .chars()
                .next_back()
                .is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == '$'));
            let after = &config[i + key.len()..];
            let after = after.strip_prefix(['"', '\'']).unwrap_or(after);
            word_start && after.trim_start().starts_with(':')
        })
    })
}

/// Run vitest with `args`, a `run` reporting through tap-flat. Outside a
/// projects setup the tap-flat lines stream into the tree as they come. In
/// one (#1504) they only reach OUTPUT: the json reporter's report, whose
/// files are absolute, names the cases once the run ends, rooted at the
/// workspace-relative file so each package keeps its own tests.
pub(super) fn run_vitest(tx: &EpochTx, root: &Path, args: &[String]) -> Option<bool> {
    if !vitest_has_projects(root) {
        return run_streaming(tx, js_cmd(root, "vitest", args), one(parse_vitest_tap_line));
    }
    static RUN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = RUN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let report = std::env::temp_dir().join(format!("croft-vitest-{}-{n}.json", std::process::id()));
    let _ = std::fs::remove_file(&report);
    let mut args = args.to_vec();
    args.push(String::from("--reporter=json"));
    args.push(format!("--outputFile.json={}", report.display()));
    let ok = run_streaming(tx, js_cmd(root, "vitest", &args), |_| Vec::new());
    let text = std::fs::read_to_string(&report).unwrap_or_default();
    let _ = std::fs::remove_file(&report);
    for case in parse_jest_json(root, &text) {
        tx.send(TestResponse::Case(case));
    }
    ok
}

/// vitest argv for one exact test: the file scopes the run and `-t` narrows
/// to the test. vitest's `-t/--testNamePattern` is jest-compatible — a
/// REGEX over the full name — so the name is escaped like jest's, and
/// anchored (see [`exact_title_anchor`]).
pub(super) fn vitest_one_args(name: &str) -> Vec<String> {
    let mut args = vec![String::from("run"), js_id_parts(name).0.to_string()];
    if let Some(anchor) = exact_title_anchor(name) {
        args.push(String::from("-t"));
        args.push(anchor);
    }
    args.push(String::from("--reporter=tap-flat"));
    args
}

/// `-t` regex for one test (#1506): its describe chain and title, escaped,
/// joined with [`JS_NAME_JOIN`] the way vitest and jest build the full name
/// they search `-t` in, and anchored at both ends. The bare title is a
/// search too, so `works` also ran `works with negatives` and a `works` in
/// another describe. `None` for an ID that is only a file.
pub(super) fn exact_title_anchor(id: &str) -> Option<String> {
    full_name_regex(id).map(|name| format!("^{name}$"))
}

/// What joins a describe and the next title in the full name `-t` is
/// matched against: a space for jest and vitest before 5, ` > ` from vitest
/// 5 on. A title can itself hold `::` (`connects to ::1`), which the node
/// ID cannot tell from a boundary, so `::` matches at a boundary too.
pub(super) const JS_NAME_JOIN: &str = "(?: | > |::)";

/// The describe and title separator alone, closing a suite's chain.
pub(super) const JS_SUITE_JOIN: &str = "(?: | > )";

/// The describe chain and title of a JS node ID as the escaped full name
/// the runners match `-t` against; `None` for a bare file.
pub(super) fn full_name_regex(id: &str) -> Option<String> {
    let (_, rest) = id.split_once("::")?;
    Some(
        rest.split("::")
            .map(regex_escape)
            .collect::<Vec<_>>()
            .join(JS_NAME_JOIN),
    )
}

/// `-t` regex for a suite click's describe chain: every segment after the
/// file, escaped and joined the way vitest and jest build the full name they
/// match `-t` against (describes + title joined by a space, or ` > ` from
/// vitest 5 — vitest's `getTaskFullName`, jest's `getTestID`), anchored at
/// the start and closed with the separator so describe `auth` cannot sweep
/// an `auth-helper` sibling. `None` when the suite is the whole file (no describe segments),
/// where the file argument alone scopes the run.
pub(super) fn suite_title_anchor(pattern: &str) -> Option<String> {
    full_name_regex(pattern).map(|joined| format!("^{joined}{JS_SUITE_JOIN}"))
}

/// vitest argv for a filter run: a suite click passes a node-ID prefix
/// (`file` or `file::describe`), run-at-cursor a bare title. The file scopes
/// the run when present; a describe chain (anchored, see
/// [`suite_title_anchor`]) or the bare title narrows via `-t`.
pub(super) fn vitest_filter_args(pattern: &str, suite: bool) -> Vec<String> {
    let mut args = vec![String::from("run")];
    let (file, title) = js_id_parts(pattern);
    if is_js_file(pattern) {
        args.push(file.to_string());
        if suite {
            if let Some(anchor) = suite_title_anchor(pattern) {
                args.push(String::from("-t"));
                args.push(anchor);
            }
        } else if let Some(t) = title {
            args.push(String::from("-t"));
            args.push(regex_escape(t));
        }
    } else {
        args.push(String::from("-t"));
        args.push(regex_escape(pattern));
    }
    args.push(String::from("--reporter=tap-flat"));
    args
}

/// jest's own way to name one test file (#1506). A bare positional file is
/// a `testPathPattern`, a regex searched in each absolute path, so
/// `src/a.test.js` also ran `legacy/src/a.test.js`; `--runTestsByPath`
/// takes it as that exact path.
pub(super) fn jest_file_args(file: &str) -> [String; 2] {
    [String::from("--runTestsByPath"), file.to_string()]
}

/// jest argv for one exact test, mirroring [`vitest_one_args`]: the exact
/// file and an anchored `-t` (a regex over the full name) for the test.
pub(super) fn jest_one_args(name: &str) -> Vec<String> {
    let mut args = jest_file_args(js_id_parts(name).0).to_vec();
    if let Some(anchor) = exact_title_anchor(name) {
        args.push(String::from("-t"));
        args.push(anchor);
    }
    args.push(String::from("--json"));
    args
}

/// jest argv for a filter run, mirroring [`vitest_filter_args`]: a node-ID
/// prefix scopes by file (plus an anchored `-t` for a suite's describe
/// chain), a bare title goes through `-t` alone.
pub(super) fn jest_filter_args(pattern: &str, suite: bool) -> Vec<String> {
    let mut args = Vec::new();
    if is_js_file(pattern) {
        let (file, title) = js_id_parts(pattern);
        args.extend(jest_file_args(file));
        if suite {
            if let Some(anchor) = suite_title_anchor(pattern) {
                args.push(String::from("-t"));
                args.push(anchor);
            }
        } else if let Some(t) = title {
            args.push(String::from("-t"));
            args.push(regex_escape(t));
        }
    } else {
        args.push(String::from("-t"));
        args.push(regex_escape(pattern));
    }
    args.push(String::from("--json"));
    args
}
