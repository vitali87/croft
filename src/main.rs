mod app;
mod cli;
mod clipboard;
mod git;
mod highlight;
mod icons;
mod install_session;
mod iterm2;
mod iterm2_inline;
mod lsp;
mod pdf;
mod remote;
mod remote_connect;
mod session_state;
mod sheet;
mod sysmon;
mod update_watch;
mod widgets;
mod zoxide;

use anyhow::Result;
use clap::Parser;
use cli::Cli;

fn main() -> Result<()> {
    let cli = Cli::parse();
    cli.run()
}
