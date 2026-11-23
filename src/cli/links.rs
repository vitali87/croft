//! `croft open-link` and `croft install-link-handler`: checking a link and becoming the
//! `croft remote` or `croft attach` it describes, and making croft the link's handler.

use super::*;

/// `croft open-link` (#359): check the link, then become the `croft remote`
/// or `croft attach` it describes, so the session gets this terminal.
pub(super) fn open_link(url: &str) -> Result<()> {
    let mut link = crate::deep_link::parse(url)?;
    if let Some(host) = link.host.take() {
        let targets: Vec<(String, Option<String>)> = crate::remote::discover_ssh_targets()
            .into_iter()
            .map(|t| (t.alias, t.host_name))
            .collect();
        link.host = crate::deep_link::resolve_host(&host, &local_hostname(), &targets)?;
    }
    let home = std::env::var("HOME").ok();
    if let Some(args) = crate::deep_link::decide_argv(&link, home.as_deref()) {
        // Answer where the edit waits, then report: nothing to attach to.
        let status = match &link.host {
            Some(alias) => std::process::Command::new("ssh")
                .arg("--")
                .arg(alias)
                .arg(crate::deep_link::remote_shell_command(&args))
                .status()?,
            None => std::process::Command::new(std::env::current_exe()?)
                .args(&args)
                .status()?,
        };
        if !status.success() {
            std::process::exit(status.code().unwrap_or(1));
        }
        return Ok(());
    }
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.args(crate::deep_link::argv(&link, home.as_deref()));
    if let Some(focus) = link.focus {
        cmd.env("CROFT_FOCUS", focus.as_str());
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        let err = cmd.exec();
        Err(anyhow::anyhow!("could not start croft: {err}"))
    }
    #[cfg(not(unix))]
    {
        let status = cmd.status()?;
        std::process::exit(status.code().unwrap_or(1));
    }
}

pub(super) fn local_hostname() -> String {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// `croft install-link-handler` (#359).
pub(super) fn install_link_handler() -> Result<()> {
    let exe = std::env::current_exe()?.display().to_string();
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .context("HOME is not set")?;
    if std::env::var_os("TERMUX_VERSION").is_some() {
        let bin = home.join("bin");
        std::fs::create_dir_all(&bin)?;
        let opener = bin.join("termux-url-opener");
        if opener.exists()
            && !std::fs::read_to_string(&opener)
                .unwrap_or_default()
                .contains("croft install-link-handler")
        {
            anyhow::bail!(
                "{} already exists; add a `croft://*) croft open-link \"$1\"` case to it",
                opener.display()
            );
        }
        std::fs::write(&opener, crate::deep_link::termux_url_opener(&exe))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&opener, std::fs::Permissions::from_mode(0o755))?;
        }
        println!("croft:// links now open croft ({})", opener.display());
        return Ok(());
    }
    if cfg!(target_os = "macos") {
        println!("On macOS, `croft install-launcher` registers croft:// links.");
        return Ok(());
    }
    let apps = home.join(".local/share/applications");
    std::fs::create_dir_all(&apps)?;
    let entry = apps.join("croft-link.desktop");
    std::fs::write(&entry, crate::deep_link::desktop_entry(&exe))?;
    let status = std::process::Command::new("xdg-mime")
        .args(["default", "croft-link.desktop", "x-scheme-handler/croft"])
        .status();
    match status {
        Ok(s) if s.success() => println!("croft:// links now open croft ({})", entry.display()),
        _ => println!(
            "Wrote {}; register it with `xdg-mime default croft-link.desktop x-scheme-handler/croft`",
            entry.display()
        ),
    }
    Ok(())
}
