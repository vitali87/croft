use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::process::Command;

mod commands;
mod cross;
mod git_editor;
mod hook;
mod keys;
mod launcher_install;
mod links;
mod pair_provider;
mod plot;
mod run;
mod terminal_setup;
mod theme_import;
mod typos;
mod version;
mod vscode_import;
mod workspace;
use commands::*;
#[cfg(test)]
pub(crate) use cross::CROSS_TARGETS;
use cross::*;
use git_editor::*;
use hook::*;
use keys::*;
use launcher_install::*;
use links::*;
use pair_provider::*;
use plot::*;
use terminal_setup::*;
use theme_import::*;
pub use typos::misspelled_subcommand;
use version::*;
use vscode_import::*;
use workspace::*;

#[derive(Parser, Debug)]
#[command(name = "croft", version = VERSION, about = "Terminal-based VS Code replica")]
pub struct Cli {
    /// Workspace folder to open (defaults to current directory). A file works
    /// too: the workspace roots at its parent and the file opens in the editor.
    #[arg(value_name = "PATH")]
    pub path: Option<PathBuf>,

    // Ignoring a subcommand is left as-is deliberately: clap subcommands are
    // not argument ids, so `conflicts_with = "command"` is a runtime assert
    // failure rather than a guard. A plain comment rather than part of the
    // doc below, because clap prints a field's `///` as its long help and
    // this note is for maintainers, not `croft --help` readers (#853). The
    // flag itself came from #282; the issue number stays here, not in help.
    /// Print the version with build provenance (git hash and build time) and
    /// exit. `--version` prints the plain `x.y.z`; this is the flag to quote
    /// in a bug report, since two builds can share a version.
    ///
    /// It answers before anything else runs, so pairing it with a subcommand
    /// ignores that subcommand, as `--version` does.
    #[arg(long)]
    pub build_info: bool,

    /// Open this file in the editor on launch (the workspace stays rooted at
    /// PATH). Used by "Move/Copy into New Window", and handy on its own:
    /// `croft ~/project --open-file ~/project/src/main.rs`.
    #[arg(long, value_name = "FILE")]
    pub open_file: Option<PathBuf>,

    /// Start focused on the editor: hide the Explorer sidebar and the terminal
    /// so just the file fills the window (toggle them back with Cmd/Ctrl+B and
    /// Cmd/Ctrl+J). "Move/Copy into New Window" launches the new window this way.
    #[arg(long, default_value_t = false)]
    pub zen: bool,

    /// Internal: restore tabs/layout from a session file written just
    /// before a self-update re-exec. Not intended for manual use.
    #[arg(long, hide = true)]
    pub restore_session: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<CliCommand>,
}

#[cfg(test)]
mod tests;
