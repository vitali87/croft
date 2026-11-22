//! Finding and installing debug adapters: lldb-dap under each name LLVM ships it as, what
//! to say when it is missing, whether a file is a runnable binary, and Debug: Install Go
//! Debugger (delve).

use super::*;

/// Rank an executable basename as an lldb-dap candidate, or `None` if it is not
/// one. LLVM ships the adapter under several names: the bare `lldb-dap`, the
/// versioned `lldb-dap-18` that Debian/Ubuntu install (often with no
/// unversioned symlink), and the legacy `lldb-vscode[-NN]` name from older
/// releases. Higher rank wins: an unversioned binary (tracks the system
/// default) beats any versioned one, a newer LLVM release beats an older one,
/// and at equal version the modern `lldb-dap` name beats legacy `lldb-vscode`.
pub(in crate::app) fn lldb_dap_rank(name: &str) -> Option<i64> {
    if name == "lldb-dap" {
        return Some(i64::MAX);
    }
    if name == "lldb-vscode" {
        return Some(i64::MAX - 1);
    }
    // `family` breaks ties at equal version: lldb-dap (1) over lldb-vscode (0).
    let (version, family) = name
        .strip_prefix("lldb-dap-")
        .map(|v| (v, 1))
        .or_else(|| name.strip_prefix("lldb-vscode-").map(|v| (v, 0)))?;
    let version: i64 = version.parse().ok()?;
    Some(version * 2 + family)
}

/// Resolve the `lldb-dap` adapter binary: the Xcode Command Line Tools copy
/// first (always present on a dev Mac), then the best-ranked match found by
/// scanning `PATH` for `lldb-dap`/`lldb-dap-NN`/`lldb-vscode[-NN]`. `None` if
/// none is installed.
pub(in crate::app) fn lldb_dap_path() -> Option<String> {
    let xcode = "/Library/Developer/CommandLineTools/usr/bin/lldb-dap";
    if std::path::Path::new(xcode).is_file() {
        return Some(xcode.to_string());
    }
    let path_var = std::env::var_os("PATH")?;
    let mut best: Option<(i64, String)> = None;
    for dir in std::env::split_paths(&path_var) {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Some(rank) = lldb_dap_rank(&name) else {
                continue;
            };
            let full = entry.path();
            if is_executable_file(&full) && best.as_ref().is_none_or(|(r, _)| rank > *r) {
                best = Some((rank, full.to_string_lossy().into_owned()));
            }
        }
    }
    best.map(|(_, p)| p)
}

/// The error shown when no lldb-dap adapter is installed, with a host-correct
/// install hint: Xcode Command Line Tools on macOS, the LLVM package elsewhere.
pub(in crate::app) fn lldb_dap_missing_message() -> String {
    let hint = if cfg!(target_os = "macos") {
        "install Xcode Command Line Tools"
    } else {
        "install LLVM, e.g. apt install lldb"
    };
    format!("lldb-dap not found ({hint})")
}

/// Whether `path` is a regular file with the executable bit set — i.e. a
/// compiled binary or script croft can run directly (`./file`) and debug via
/// lldb-dap without a build step. The honest signal for "directly executable",
/// since such files usually have no extension.
pub(in crate::app) fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

impl App {
    /// Debug: Install Go Debugger (delve) (#264). Runs `go install` in a
    /// terminal pane, where the user sees the command and its output: that
    /// visible run is the consent, as for a coverage tool. Without Go there
    /// is nothing to run it with, so it says that instead.
    pub(in crate::app) fn install_delve(&mut self) {
        if !crate::dap::install::go_on_path() {
            self.status = crate::dap::install::dlv_missing_message(false);
            return;
        }
        match crate::widgets::terminal::PtyTerminal::new(self.roots.primary()) {
            Ok(mut term) => {
                term.set_manual_name(Some(String::from("delve install")));
                term.write_input(format!("{}\r", crate::dap::install::DLV_INSTALL).as_bytes());
                self.insert_terminal(term);
                self.show_terminal = true;
                self.focus_pane(Pane::Terminal);
                self.status = String::from(
                    "Installing delve into ~/.croft/servers/go; start debugging again once it finishes",
                );
            }
            Err(e) => self.status = format!("Could not open a terminal: {e}"),
        }
    }
}
