//! macOS launcher app bundle: a clickable `Croft.app` that opens Ghostty with
//! croft already running via `open -na Ghostty.app --args --initial-command=…`.
//!
//! `--initial-command` runs croft only in the first window of that launch (a
//! normal Ghostty stays a plain shell) and, unlike the `-e` flag, does NOT trip
//! macOS's "Allow Ghostty to Execute" prompt. Passing it as a CLI option also
//! leaves the user's Ghostty config (croft's Cmd-chord keybinds included)
//! untouched. macOS-only; the CLI handler bails on other platforms.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

const APP_NAME: &str = "Croft";

/// The launcher's executable: open a fresh Ghostty instance whose first window
/// runs croft at `open_dir`. `-n` forces a new instance so a window appears
/// even when Ghostty is already running. Both paths are absolute so the GUI
/// launch environment (which lacks `~/.cargo/bin` on `PATH`) still resolves
/// croft.
fn launcher_script(croft_bin: &str, open_dir: &str) -> String {
    format!(
        "#!/bin/sh\nexec open -na Ghostty.app --args --initial-command=\"{croft_bin} {open_dir}\"\n"
    )
}

/// Minimal app-bundle metadata. `CFBundleIconFile=icon` points at
/// `Resources/icon.icns`.
fn info_plist() -> String {
    String::from(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Croft</string>
  <key>CFBundleDisplayName</key><string>Croft</string>
  <key>CFBundleIdentifier</key><string>com.vitali87.croft-launcher</string>
  <key>CFBundleExecutable</key><string>Croft</string>
  <key>CFBundleIconFile</key><string>icon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleVersion</key><string>1.0</string>
  <key>CFBundleShortVersionString</key><string>1.0</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
"#,
    )
}

/// Where the bundle lives: system-wide `/Applications` (default) or per-user
/// `~/Applications` (`user = true`, no admin rights needed).
pub fn applications_dir(user: bool) -> Result<PathBuf> {
    if user {
        let home = std::env::var_os("HOME").context("HOME is not set")?;
        Ok(PathBuf::from(home).join("Applications"))
    } else {
        Ok(PathBuf::from("/Applications"))
    }
}

/// Rasterise the embedded square logo into `dest` (an `.icns`) via the macOS
/// `sips` + `iconutil` pipeline.
fn write_icon(dest: &Path) -> Result<()> {
    let tmp = std::env::temp_dir().join(format!("croft-launcher-icon-{}", std::process::id()));
    let iconset = tmp.join("croft.iconset");
    std::fs::create_dir_all(&iconset).with_context(|| format!("creating {}", iconset.display()))?;
    let src_png = tmp.join("logo.png");
    std::fs::write(&src_png, crate::iterm2_inline::APP_ICON_PNG)
        .with_context(|| format!("writing {}", src_png.display()))?;
    for size in [16u32, 32, 128, 256, 512] {
        for (scale, name) in [
            (1u32, format!("icon_{size}x{size}.png")),
            (2, format!("icon_{size}x{size}@2x.png")),
        ] {
            let px = (size * scale).to_string();
            let status = Command::new("sips")
                .args(["-z", &px, &px])
                .arg(&src_png)
                .arg("--out")
                .arg(iconset.join(&name))
                .stdout(Stdio::null())
                .status()
                .context("running sips")?;
            if !status.success() {
                anyhow::bail!("sips failed building {name}");
            }
        }
    }
    let status = Command::new("iconutil")
        .args(["-c", "icns"])
        .arg(&iconset)
        .arg("-o")
        .arg(dest)
        .stdout(Stdio::null())
        .status()
        .context("running iconutil")?;
    let _ = std::fs::remove_dir_all(&tmp);
    if !status.success() {
        anyhow::bail!("iconutil failed building the launcher icon");
    }
    Ok(())
}

/// Build (or rebuild) the `Croft.app` launcher under `app_dir`, returning its
/// path. Any existing bundle of the same name is replaced so re-running keeps
/// the binary path and open directory current.
pub fn install(app_dir: &Path, croft_bin: &str, open_dir: &str) -> Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let app = app_dir.join(format!("{APP_NAME}.app"));
    let _ = std::fs::remove_dir_all(&app);
    let macos = app.join("Contents/MacOS");
    let resources = app.join("Contents/Resources");
    std::fs::create_dir_all(&macos).with_context(|| format!("creating {}", macos.display()))?;
    std::fs::create_dir_all(&resources)
        .with_context(|| format!("creating {}", resources.display()))?;

    write_icon(&resources.join("icon.icns")).context("building the launcher icon")?;
    std::fs::write(app.join("Contents/Info.plist"), info_plist()).context("writing Info.plist")?;

    let exe = macos.join(APP_NAME);
    std::fs::write(&exe, launcher_script(croft_bin, open_dir))
        .context("writing launcher script")?;
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755))
        .context("making the launcher script executable")?;

    // Nudge Launch Services so the icon and Spotlight entry register promptly.
    let _ = Command::new("/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister")
        .arg("-f")
        .arg(&app)
        .status();
    Ok(app)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_opens_ghostty_with_initial_command_and_both_paths() {
        let s = launcher_script("/Users/v/.cargo/bin/croft", "/Users/v/Documents");
        assert!(s.starts_with("#!/bin/sh\n"));
        assert!(s.contains("open -na Ghostty.app --args"));
        assert!(s.contains("--initial-command=\"/Users/v/.cargo/bin/croft /Users/v/Documents\""));
    }

    #[test]
    fn info_plist_declares_executable_and_icon() {
        let p = info_plist();
        assert!(p.contains("<key>CFBundleExecutable</key><string>Croft</string>"));
        assert!(p.contains("<key>CFBundleIconFile</key><string>icon</string>"));
    }

    #[test]
    fn applications_dir_picks_system_or_user() {
        assert_eq!(
            applications_dir(false).unwrap(),
            PathBuf::from("/Applications")
        );
        assert!(applications_dir(true).unwrap().ends_with("Applications"));
    }
}
