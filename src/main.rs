mod app;
mod cli;
mod clipboard;
mod ghostty;
mod git;
mod gradient;
mod highlight;
mod icons;
mod install_session;
mod iterm2;
mod iterm2_inline;
mod lsp;
mod pdf;
mod prefs;
mod remote;
mod remote_bulk;
mod remote_connect;
mod session_state;
mod sheet;
mod sysmon;
mod termux;
mod theme;
mod update_watch;
mod vim;
mod widgets;
mod zoxide;

use anyhow::Result;
use clap::Parser;
use cli::Cli;

fn main() -> Result<()> {
    let cli = Cli::parse();
    cli.run()
}
