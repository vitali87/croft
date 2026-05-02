use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::Command;

const SETUP_FONT_PS_NAME: &str = "MesloLGSNFM-Regular";
const SETUP_FONT_SIZE: u32 = 13;

#[derive(Parser, Debug)]
#[command(name = "tcode", version, about = "Terminal-based VS Code replica")]
pub struct Cli {
    /// Workspace folder to open (defaults to current directory)
    #[arg(value_name = "PATH")]
    pub path: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<CliCommand>,
}

#[derive(Subcommand, Debug)]
pub enum CliCommand {
    /// Set macOS Terminal.app's default profile font to a Nerd Font.
    SetupTerminal {
        /// PostScript name of the font (read from the .ttf with `fontTools` or `fc-scan`)
        #[arg(long, default_value = SETUP_FONT_PS_NAME)]
        font: String,
        #[arg(long, default_value_t = SETUP_FONT_SIZE)]
        size: u32,
        /// Skip confirmation prompt
        #[arg(short, long, default_value_t = false)]
        yes: bool,
    },
}

impl Cli {
    pub fn run(self) -> Result<()> {
        match self.command {
            Some(CliCommand::SetupTerminal { font, size, yes }) => {
                setup_terminal(&font, size, yes)
            }
            None => {
                let path = self
                    .path
                    .unwrap_or_else(|| std::env::current_dir().expect("cwd"))
                    .canonicalize()
                    .context("resolving workspace path")?;
                if !path.is_dir() {
                    anyhow::bail!("{} is not a directory", path.display());
                }
                crate::app::run(path)
            }
        }
    }
}

fn setup_terminal(font: &str, size: u32, yes: bool) -> Result<()> {
    if !cfg!(target_os = "macos") {
        anyhow::bail!("setup-terminal is macOS-only");
    }
    println!(
        "This will set Terminal.app's default profile font to '{font}' at {size}pt."
    );
    println!("Existing custom profiles are not modified.");
    if !yes {
        print!("Apply this change? [y/N] ");
        use std::io::Write;
        std::io::stdout().flush()?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
            println!("Aborted.");
            return Ok(());
        }
    }
    let script = format!(
        r#"tell application "Terminal"
    set the font name of the default settings to "{font}"
    set the font size of the default settings to {size}
    set the font name of the startup settings to "{font}"
    set the font size of the startup settings to {size}
end tell"#
    );
    let output = Command::new("osascript")
        .args(["-e", &script])
        .output()
        .context("running osascript")?;
    if !output.status.success() {
        anyhow::bail!(
            "AppleScript failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    println!("Set Terminal.app default profile font to {font} at {size}pt.");
    println!("Quit Terminal.app entirely (cmd+Q) and reopen it for the change to take effect.");
    Ok(())
}
