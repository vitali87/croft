mod app;
mod cli;
mod clipboard;
mod dap;
mod file_ref;
mod ghostty;
mod git;
mod gradient;
mod highlight;
mod icons;
mod install_session;
mod iterm2;
mod iterm2_inline;
mod launcher;
mod lsp;
mod mcp;
mod merge;
mod outline_syntax;
mod output;
mod pdf;
mod port_detect;
mod prefs;
mod release_notes;
mod remote;
mod remote_bulk;
mod remote_connect;
mod session_state;
mod sheet;
mod termux;
mod testing;
mod theme;
mod update_watch;
mod vim;
mod voice;
mod widgets;
mod zoxide;

use anyhow::Result;
use clap::Parser;
use cli::Cli;

fn main() -> Result<()> {
    let cli = Cli::parse();
    cli.run()
}
