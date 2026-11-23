//! `croft install-launcher`: the macOS Croft.app that opens a folder or a file in croft
//! inside a new Ghostty window.

use super::*;

pub(super) fn install_launcher(path: Option<PathBuf>, user: bool, yes: bool) -> Result<()> {
    if !cfg!(target_os = "macos") {
        anyhow::bail!("install-launcher is macOS-only");
    }
    // The croft that runs inside Ghostty is this very executable, through
    // the link Homebrew moves on upgrade when Homebrew installed it.
    let croft_bin = crate::launcher::stable_croft_path(
        &std::env::current_exe()
            .context("resolving croft binary path")?
            .canonicalize()
            .context("canonicalizing croft binary path")?,
    );
    let open_dir = match path {
        Some(p) => p,
        None => PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?),
    };
    let open_dir = open_dir
        .canonicalize()
        .with_context(|| format!("resolving {}", open_dir.display()))?;
    if !open_dir.is_dir() {
        anyhow::bail!("{} is not a directory", open_dir.display());
    }
    let app_dir = crate::launcher::applications_dir(user)?;
    println!("This will create a clickable Croft.app launcher:");
    println!("  Location: {}", app_dir.join("Croft.app").display());
    println!(
        "  Opens:    a new Ghostty window running `croft {}`",
        open_dir.display()
    );
    println!("  Reachable from Spotlight (Cmd+Space, type 'Croft'), Launchpad, and the Dock.");
    println!("  Also opens files: drop one on the app, or right-click it in Finder and pick Croft");
    println!("            under Open With (Get Info > Change All makes it the default).");
    println!(
        "  Your normal Ghostty windows are unaffected (this only sets the launcher's window)."
    );
    if !yes {
        print!("Create it? [y/N] ");
        use std::io::Write;
        std::io::stdout().flush()?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
            println!("Aborted.");
            return Ok(());
        }
    }
    let app = crate::launcher::install(
        &app_dir,
        &croft_bin.to_string_lossy(),
        &open_dir.to_string_lossy(),
    )
    .with_context(|| "creating the Croft.app launcher")?;
    // The launcher starts croft with launchd's bare PATH; the shell's is
    // remembered now so even its first launch never waits for the shell.
    crate::gui_path::remember_login_path();
    println!("Created {}.", app.display());
    println!("Find it in Spotlight (Cmd+Space, type 'Croft') or drag it to your Dock.");
    Ok(())
}
