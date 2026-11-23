//! `go test -json` and `codeql test run`: finding `go`, their command lines, and
//! streaming their results.

use super::*;

/// Absolute path to `go`: `PATH`, then where the official installer and
/// Homebrew put it — absolute for the same GUI-stripped-PATH reason as
/// [`cargo_cmd`].
pub(super) fn go_binary() -> PathBuf {
    let path_dirs = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .unwrap_or_default();
    path_dirs
        .into_iter()
        .chain(["/usr/local/go/bin", "/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from))
        .map(|d| d.join("go"))
        .find(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("go"))
}

/// `go test -json <args>` in `root` (#264).
pub(super) fn go_cmd<S: AsRef<std::ffi::OsStr>>(root: &Path, args: &[S]) -> Command {
    let mut cmd = Command::new(go_binary());
    cmd.args(["test", "-json"])
        .args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

/// Run `go test -json` and stream its finished tests, showing the text the
/// tests printed (not the JSON events) in the OUTPUT channel. `./...` is
/// spelled for the root's modules (see [`crate::testing::gotest::Modules::expand`]).
pub(super) fn run_go<S: AsRef<str>>(tx: &EpochTx, root: &Path, args: &[S]) -> Option<bool> {
    let modules = crate::testing::gotest::Modules::of(root);
    let cmd = go_cmd(root, &modules.expand(args));
    run_streaming_shown(tx, cmd, crate::testing::gotest::shown, |line| {
        crate::testing::gotest::parse_event(&modules, line)
            .into_iter()
            .collect()
    })
}

/// Run `codeql <args>` in `root` and stream each test's result (#578). A
/// result is complete once the diff or errors after its line have arrived,
/// so it lands when the next test's line (or the end of output) does; each
/// failure also gets a one-line summary in OUTPUT, above the CLI's own
/// diff-heavy text.
pub(super) fn run_codeql(tx: &EpochTx, root: &Path, args: &[String]) -> Option<bool> {
    let mut cmd = Command::new(tx.codeql);
    cmd.args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let stream = std::cell::RefCell::new(crate::testing::codeqltest::RunStream::new(root));
    let report = |o: crate::testing::codeqltest::Outcome| {
        if let Some(summary) = o.failure_summary() {
            output::push(output::CHANNEL_TESTS, OutputLevel::Error, &summary);
        }
        o.case()
    };
    let ok = run_streaming(tx, cmd, |line| {
        stream
            .borrow_mut()
            .feed(line)
            .map(report)
            .into_iter()
            .collect()
    });
    if let Some(o) = stream.borrow_mut().finish() {
        tx.send(TestResponse::Case(report(o)));
    }
    ok
}

/// The pack files the bundled CodeQL runner looks for. A run needs the test
/// packs again, not just the verdict that there is one.
pub(super) fn codeql_markers() -> Vec<String> {
    vec![String::from("qlpack.yml"), String::from("codeql-pack.yml")]
}
