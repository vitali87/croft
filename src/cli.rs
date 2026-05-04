use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::Command;

const SETUP_FONT_PS_NAME: &str = "MesloLGSNFM-Regular";
const SETUP_FONT_SIZE: u32 = 13;

const ITERM2_FONT_PS_NAME: &str = "MesloLGSNFM-Regular";
const ITERM2_NONASCII_PS_NAME: &str = "SymbolsNFM";
const ITERM2_FONT_SIZE: u32 = 13;

#[derive(Parser, Debug)]
#[command(name = "croft", version, about = "Terminal-based VS Code replica")]
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
    /// Diagnostic: print every key event the terminal delivers, with modifiers.
    /// Useful for confirming whether cmd / super reaches the app.  Press Ctrl+C to quit.
    Keys,
    /// Configure iTerm2 for Croft: fonts plus Cmd+Shift+F and Search paste.
    SetupIterm2 {
        /// PostScript name of the primary font
        #[arg(long, default_value = ITERM2_FONT_PS_NAME)]
        font: String,
        /// PostScript name of the non-ASCII fallback font
        #[arg(long, default_value = ITERM2_NONASCII_PS_NAME)]
        nonascii: String,
        #[arg(long, default_value_t = ITERM2_FONT_SIZE)]
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
            Some(CliCommand::Keys) => keys_diagnostic(),
            Some(CliCommand::SetupIterm2 { font, nonascii, size, yes }) => {
                setup_iterm2(&font, &nonascii, size, yes)
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_no_args() {
        let cli = Cli::parse_from(["croft"]);
        assert!(cli.path.is_none());
        assert!(cli.command.is_none());
    }

    #[test]
    fn parses_path_argument() {
        let cli = Cli::parse_from(["croft", "/tmp"]);
        assert_eq!(cli.path, Some(PathBuf::from("/tmp")));
        assert!(cli.command.is_none());
    }

    #[test]
    fn parses_setup_terminal_subcommand_with_defaults() {
        let cli = Cli::parse_from(["croft", "setup-terminal"]);
        assert!(cli.path.is_none());
        match cli.command {
            Some(CliCommand::SetupTerminal { font, size, yes }) => {
                assert_eq!(font, SETUP_FONT_PS_NAME);
                assert_eq!(size, SETUP_FONT_SIZE);
                assert!(!yes);
            }
            _ => panic!("expected SetupTerminal"),
        }
    }

    #[test]
    fn parses_setup_terminal_with_overrides() {
        let cli = Cli::parse_from([
            "croft",
            "setup-terminal",
            "--font",
            "FiraCodeNFM-Regular",
            "--size",
            "14",
            "--yes",
        ]);
        match cli.command {
            Some(CliCommand::SetupTerminal { font, size, yes }) => {
                assert_eq!(font, "FiraCodeNFM-Regular");
                assert_eq!(size, 14);
                assert!(yes);
            }
            _ => panic!("expected SetupTerminal"),
        }
    }

    #[test]
    fn parses_setup_terminal_with_short_yes() {
        let cli = Cli::parse_from(["croft", "setup-terminal", "-y"]);
        match cli.command {
            Some(CliCommand::SetupTerminal { yes, .. }) => assert!(yes),
            _ => panic!("expected SetupTerminal"),
        }
    }

    #[test]
    fn parses_keys_subcommand() {
        let cli = Cli::parse_from(["croft", "keys"]);
        assert!(matches!(cli.command, Some(CliCommand::Keys)));
    }

    #[test]
    fn parses_setup_iterm2_with_defaults() {
        let cli = Cli::parse_from(["croft", "setup-iterm2"]);
        match cli.command {
            Some(CliCommand::SetupIterm2 { font, nonascii, size, yes }) => {
                assert_eq!(font, ITERM2_FONT_PS_NAME);
                assert_eq!(nonascii, ITERM2_NONASCII_PS_NAME);
                assert_eq!(size, ITERM2_FONT_SIZE);
                assert!(!yes);
            }
            _ => panic!("expected SetupIterm2"),
        }
    }

    #[test]
    fn parses_setup_iterm2_with_overrides() {
        let cli = Cli::parse_from([
            "croft",
            "setup-iterm2",
            "--font",
            "FiraCodeNFM-Reg",
            "--nonascii",
            "SymbolsNFM",
            "--size",
            "15",
            "-y",
        ]);
        match cli.command {
            Some(CliCommand::SetupIterm2 { font, nonascii, size, yes }) => {
                assert_eq!(font, "FiraCodeNFM-Reg");
                assert_eq!(nonascii, "SymbolsNFM");
                assert_eq!(size, 15);
                assert!(yes);
            }
            _ => panic!("expected SetupIterm2"),
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

fn keys_diagnostic() -> Result<()> {
    use crossterm::event::{
        self, Event, KeyCode, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    };
    use crossterm::{execute, terminal};
    use std::io::stdout;
    use std::time::Duration;

    println!("croft keys: press any key to inspect; Ctrl+C to quit.");
    println!("If the kitty keyboard protocol is negotiated, modifier keys");
    println!("(including Cmd/Super on macOS) will appear in the modifier list.");
    println!();

    terminal::enable_raw_mode().context("enable raw mode")?;
    let mut out = stdout();
    let kbd_enhanced = execute!(
        out,
        PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
        )
    )
    .is_ok();

    let result = (|| -> Result<()> {
        loop {
            if event::poll(Duration::from_millis(500))? {
                match event::read()? {
                    Event::Key(k) => {
                        if k.kind != KeyEventKind::Press && k.kind != KeyEventKind::Repeat {
                            continue;
                        }
                        let mut mods: Vec<&str> = Vec::new();
                        if k.modifiers.contains(KeyModifiers::CONTROL) {
                            mods.push("CONTROL");
                        }
                        if k.modifiers.contains(KeyModifiers::ALT) {
                            mods.push("ALT");
                        }
                        if k.modifiers.contains(KeyModifiers::SHIFT) {
                            mods.push("SHIFT");
                        }
                        if k.modifiers.contains(KeyModifiers::SUPER) {
                            mods.push("SUPER (Cmd)");
                        }
                        if k.modifiers.contains(KeyModifiers::HYPER) {
                            mods.push("HYPER");
                        }
                        if k.modifiers.contains(KeyModifiers::META) {
                            mods.push("META");
                        }
                        let mods_s = if mods.is_empty() {
                            String::from("none")
                        } else {
                            mods.join(" + ")
                        };
                        // Quit on Ctrl+C.
                        if matches!(k.code, KeyCode::Char('c'))
                            && k.modifiers.contains(KeyModifiers::CONTROL)
                        {
                            print!("\r\nQuitting (Ctrl+C).\r\n");
                            break;
                        }
                        let clipboard_probe = if is_paste_probe_key(k.code, k.modifiers) {
                            match crate::clipboard::read_string() {
                                Some(s) if s.is_empty() => {
                                    String::from("  clipboard=empty")
                                }
                                Some(s) => format!(
                                    "  clipboard=ok chars={} bytes={}",
                                    s.chars().count(),
                                    s.len()
                                ),
                                None => String::from("  clipboard=read_failed"),
                            }
                        } else {
                            String::new()
                        };
                        print!(
                            "\r  key={:?}  code={:?}  modifiers=[{}]  kitty={}{}\r\n",
                            k, k.code, mods_s, kbd_enhanced, clipboard_probe
                        );
                        use std::io::Write;
                        std::io::stdout().flush().ok();
                    }
                    Event::Paste(s) => {
                        print!(
                            "\r  paste_event chars={} bytes={} kitty={}\r\n",
                            s.chars().count(),
                            s.len(),
                            kbd_enhanced
                        );
                        use std::io::Write;
                        std::io::stdout().flush().ok();
                    }
                    other => {
                        print!("\r  event={other:?} kitty={kbd_enhanced}\r\n");
                        use std::io::Write;
                        std::io::stdout().flush().ok();
                    }
                }
            }
        }
        Ok(())
    })();

    if kbd_enhanced {
        execute!(stdout(), PopKeyboardEnhancementFlags).ok();
    }
    terminal::disable_raw_mode().ok();
    result
}

fn is_paste_probe_key(
    code: crossterm::event::KeyCode,
    modifiers: crossterm::event::KeyModifiers,
) -> bool {
    let crossterm::event::KeyCode::Char(c) = code else {
        return false;
    };
    c == '\u{16}'
        || (c.eq_ignore_ascii_case(&'v')
            && (modifiers.contains(crossterm::event::KeyModifiers::CONTROL)
                || modifiers.contains(crossterm::event::KeyModifiers::SUPER)))
}

fn setup_iterm2(font: &str, nonascii: &str, size: u32, yes: bool) -> Result<()> {
    let plist_path = crate::iterm2::default_plist_path();
    println!(
        "This will configure iTerm2 for Croft:\n  Normal Font: {font} {size}\n  Non-ASCII Font: {nonascii} {size}\n  Use Non-ASCII Font: enabled\n  Global key: Cmd+Shift+F -> Croft Search\n  Global/profile key: Cmd+V -> CSI-u Cmd+V, handled by Croft Search\n  App menu shortcuts: move Find Globally and Paste off Cmd+Shift+F/Cmd+V"
    );
    println!("Plist target: {}", plist_path.display());
    println!("Existing custom profile fonts are not modified; global key mappings are updated.");
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
    crate::iterm2::install_croft_settings(&plist_path, font, nonascii, size)
        .with_context(|| "applying iTerm2 Croft settings")?;
    println!("Wrote Croft settings to {}.", plist_path.display());
    println!(
        "Quit iTerm2 entirely (cmd+Q) and reopen it. macOS caches plists; iTerm2 must be relaunched to pick up the change."
    );
    Ok(())
}
