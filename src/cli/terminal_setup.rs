//! `croft setup-terminal`, `setup-iterm2` and `setup-ghostty`: the fonts they set, and
//! setting up Terminal.app, iTerm2 and Ghostty for croft on macOS.

use super::*;

pub(super) const SETUP_FONT_PS_NAME: &str = "MesloLGSNFM-Regular";

pub(super) const SETUP_FONT_SIZE: u32 = 13;

pub(super) const ITERM2_FONT_PS_NAME: &str = "MesloLGSNFM-Regular";

pub(super) const ITERM2_NONASCII_PS_NAME: &str = "SymbolsNFM";

pub(super) const ITERM2_FONT_SIZE: u32 = 13;

pub(super) fn setup_terminal(font: &str, size: u32, yes: bool) -> Result<()> {
    if !cfg!(target_os = "macos") {
        anyhow::bail!("setup-terminal is macOS-only");
    }
    println!("This will set Terminal.app's default profile font to '{font}' at {size}pt.");
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

pub(super) fn setup_iterm2(font: &str, nonascii: &str, size: u32, yes: bool) -> Result<()> {
    let plist_path = crate::iterm2::default_plist_path();
    println!(
        "This will configure iTerm2 for Croft:\n  Normal Font: {font} {size}\n  Non-ASCII Font: {nonascii} {size}\n  Use Non-ASCII Font: enabled\n  Global key: Cmd+Shift+F -> Croft Search\n  App menu shortcut: move Find Globally off Cmd+Shift+F\n  App menu shortcut: move Show Help Menu off Cmd+Shift+/ so Croft can reach its 'make root at parent' action\n  Global key: Cmd+Shift+/ -> Croft 'make root at parent'\n  Global key: Cmd+Shift+Return -> Croft Explorer expand/collapse\n  App menu shortcut: relocate Edit > Copy / Cut / Select All / Undo to Cmd+Opt+letter so Croft handles Cmd+C / Cmd+X / Cmd+A / Cmd+Z directly (note: any user-installed profile-level Cmd+letter -> Send Hex Code bindings need to be removed manually in iTerm2 Settings > Profiles > Keys > Key Bindings, since croft cannot tell them apart from intentional user customizations)\n  Global keys: Cmd+A / Cmd+C / Cmd+S / Cmd+X / Cmd+Z -> Croft as CSI-u so the NSResponder default copy:/cut:/selectAll:/undo: bindings cannot consume the chord before croft sees it\n  App menu shortcut: relocate New Tab off Cmd+T (to Cmd+Ctrl+T) so Croft opens a new terminal on Cmd+T\n  App menu shortcut: relocate Previous/Next Pane off Cmd+[ / Cmd+] (to Cmd+Opt+[ / Cmd+Opt+]) so Croft cycles terminals\n  Global keys: Cmd+T -> new terminal; Cmd+[ / Cmd+] -> cycle to previous / next terminal\n  Advanced setting: NoSyncNeverAskAboutMouseReportingFrustration=true to suppress iTerm2's 'Disable mouse reporting?' banner (croft owns the mouse; the banner is noise)\n  Cmd+V: left on iTerm2's native Paste action so it works locally and over SSH via bracketed paste; legacy Cmd+V hex bindings and Paste menu remaps from earlier croft versions are cleared"
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

pub(super) fn setup_ghostty(yes: bool) -> Result<()> {
    // The block forwards Ghostty's Cmd chords; on Linux croft's chords use
    // Ctrl, so there is nothing of this block's to forward (#1439).
    if !cfg!(target_os = "macos") {
        anyhow::bail!(
            "setup-ghostty is macOS-only: it forwards Ghostty's Cmd chords, and croft's chords on this system use Ctrl"
        );
    }
    let config_path = crate::ghostty::default_config_path();
    println!(
        "This will add a managed keybind block to your Ghostty config so Croft, not Ghostty, owns its chords:"
    );
    println!(
        "  Cmd+T -> new terminal; Cmd+[ / Cmd+] -> cycle terminals; Cmd+W -> close editor tab"
    );
    println!(
        "  Cmd+1..Cmd+9 / Cmd+0 -> Croft (Ghostty's goto_tab is overridden) so they work as vim counts"
    );
    println!(
        "  Cmd+C / Cmd+X / Cmd+A / Cmd+Z / Cmd+S and the editor / Explorer / source-control chords"
    );
    println!(
        "  Each chord is re-emitted as the same CSI-u sequence Croft already receives under iTerm2"
    );
    println!("  Cmd+V is left on Ghostty's native paste so it keeps working locally and over SSH");
    println!("Config target: {}", config_path.display());
    println!(
        "Only Croft's marked block is rewritten; the rest of your Ghostty config is preserved."
    );
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
    crate::ghostty::install_keybinds(&config_path)
        .with_context(|| "applying Ghostty Croft keybindings")?;
    // A Ghostty `command = croft` starts croft with launchd's bare PATH; the
    // shell's is remembered now so even its first launch never waits for it.
    crate::gui_path::remember_login_path();
    println!("Wrote Croft keybindings to {}.", config_path.display());
    println!(
        "Reload Ghostty's config (Cmd+Shift+,) or restart Ghostty for the change to take effect."
    );
    Ok(())
}
