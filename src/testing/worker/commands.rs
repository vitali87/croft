//! cargo's command line: rooted at the workspace, with `--workspace` when it has one,
//! and its build-status lines as progress.

use super::*;

/// `args` for a cargo test or coverage run at `root`, with `--workspace`
/// after the subcommand when the root manifest has a `[workspace]` table
/// (#1508). At the root of a workspace that is also a package, plain `cargo
/// test` builds and runs only that package, so the members' tests were
/// never listed or run; for a virtual manifest the flag changes nothing.
pub(super) fn workspace_args<'a>(root: &Path, args: &[&'a str]) -> Vec<&'a str> {
    let mut out = args.to_vec();
    let subcommand = matches!(args.first(), Some(&"test" | &"llvm-cov"));
    if subcommand && !args.contains(&"--version") && declares_workspace(root) {
        out.insert(1, "--workspace");
    }
    out
}

/// Whether `root/Cargo.toml` has a `[workspace]` table.
pub(super) fn declares_workspace(root: &Path) -> bool {
    std::fs::read_to_string(root.join("Cargo.toml"))
        .ok()
        .and_then(|text| text.parse::<toml::Table>().ok())
        .is_some_and(|manifest| manifest.contains_key("workspace"))
}

/// `cargo <args>` rooted at the workspace, with piped stdio. cargo is resolved
/// by absolute path (GUI-launched croft inherits a stripped PATH) and its own
/// dir is prepended to the child PATH so the rustup shim finds its sibling
/// `rustc` (same fix as the dependencies fetcher).
pub(super) fn cargo_cmd(root: &Path, args: &[&str]) -> Command {
    let cargo = crate::widgets::dependencies::cargo_binary();
    let mut cmd = Command::new(&cargo);
    cmd.args(workspace_args(root, args))
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = cargo.parent()
        && let Some(path_var) = std::env::var_os("PATH")
    {
        let mut paths = vec![dir.to_path_buf()];
        paths.extend(std::env::split_paths(&path_var));
        if let Ok(joined) = std::env::join_paths(paths) {
            cmd.env("PATH", joined);
        }
    }
    cmd
}

/// The cargo build-status verbs printed to stderr (whitespace-indented on a
/// non-TTY). We surface these as live progress; everything else (diagnostics,
/// the `--list` chrome) stays in the OUTPUT channel only.
pub(super) const CARGO_PROGRESS_VERBS: [&str; 6] = [
    "Compiling",
    "Building",
    "Downloading",
    "Updating",
    "Finished",
    "Running",
];

/// Turn a cargo stderr line into a short progress string (trimmed), or `None`
/// if it is not a build-status line.
pub(crate) fn cargo_progress(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let verb = trimmed.split_whitespace().next()?;
    CARGO_PROGRESS_VERBS
        .contains(&verb)
        .then(|| trimmed.to_string())
}
