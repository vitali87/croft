mod app;
mod cli;
mod clipboard;
mod command_history;
mod dap;
mod file_ref;
mod ghostty;
mod git;
mod gradient;
mod highlight;
mod history;
mod icons;
mod install_session;
mod iterm2;
mod iterm2_inline;
mod keymap;
mod launcher;
mod lsp;
mod markdown;
mod mcp;
mod merge;
mod outline_syntax;
mod output;
mod pdf;
mod port_detect;
mod prefs;
mod quick_select;
mod release_notes;
mod remote;
mod remote_bulk;
mod remote_connect;
mod session;
mod session_state;
mod sheet;
mod shell_integration;
mod snippets;
mod tasks;
mod terminal_session;
mod termux;
mod testing;
mod theme;
mod triggers;
mod update_watch;
mod vim;
mod voice;
mod widgets;
mod zoxide;

use anyhow::Result;
use clap::Parser;
use cli::Cli;

/// Process-wide lock serializing tests that mutate shared environment state
/// (notably `$HOME`). Cargo runs a binary's tests on multiple threads in one
/// process, so any test that calls `set_var`/`remove_var` on `HOME` races every
/// other test reading it (`file_finder`, `zoxide`, the relay tests). Each such
/// test must hold this lock for its whole body.
#[cfg(test)]
pub(crate) static HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn main() -> Result<()> {
    let cli = Cli::parse();
    cli.run()
}
