mod app;
mod cli;
mod clipboard;
mod git;
mod highlight;
mod icons;
mod iterm2;
mod iterm2_inline;
mod remote;
mod widgets;

use anyhow::Result;
use clap::Parser;
use cli::Cli;

fn main() -> Result<()> {
    let cli = Cli::parse();
    cli.run()
}
