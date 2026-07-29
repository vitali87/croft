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

/// The launcher's script: open a fresh Ghostty instance whose first window runs
/// croft. `-n` forces a new instance so a window appears even when Ghostty is
/// already running. Both paths are absolute so the GUI launch environment
/// (which lacks `~/.cargo/bin` on `PATH`) still resolves croft.
///
/// This is AppleScript, not `#!/bin/sh`, because of `on open`. Double-clicking a
/// document hands it to the app as an `odoc` Apple Event, which a shell script
/// cannot receive: a `/bin/sh` bundle executable is launched with *empty* argv
/// and would silently open the default folder instead of the file. An applet
/// built by `osacompile` is a real Cocoa app, so its `on open` handler gets the
/// path. `croft <file>` then roots the workspace at the file's parent
/// (`cli::resolve_workspace`).
///
/// An opened document launches `--zen`: someone double-clicking a file wants to
/// read that file, not land in a full IDE, so the editor fills the window and
/// the Explorer and terminal stay one keystroke away (Cmd+B / Cmd+J). Clicking
/// the launcher itself opens the workspace with the normal layout.
fn launcher_script(croft_bin: &str, open_dir: &str) -> String {
    format!(
        r#"on run
	do shell script "open -na Ghostty.app --args --initial-command=" & quoted form of "{croft_bin} '{open_dir}'"
end run

on open theFiles
	set f to POSIX path of (item 1 of theFiles)
	set inner to "{croft_bin} " & quoted form of f & " --zen"
	do shell script "open -na Ghostty.app --args --initial-command=" & quoted form of inner
end open
"#
    )
}

/// `PlistBuddy` edits applied to the bundle `osacompile` generates. It writes a
/// working applet plist but nothing that identifies the app: no bundle id (so
/// `duti`/Finder cannot name it) and, for the document types its `on open`
/// handler earns it, only the legacy `CFBundleTypeExtensions = *` wildcard.
/// `LSItemContentTypes = public.item` is the modern claim that puts Croft in
/// Finder's "Open With" for every file.
fn plist_edits() -> Vec<String> {
    [
        "Add :CFBundleIdentifier string com.vitali87.croft-launcher",
        "Set :CFBundleName Croft",
        "Add :CFBundleDisplayName string Croft",
        "Add :CFBundleShortVersionString string 1.0",
        // Our icon, not osacompile's applet.icns. CFBundleIconName points at
        // the generated Assets.car and would otherwise win.
        "Set :CFBundleIconFile croft",
        "Delete :CFBundleIconName",
        "Delete :CFBundleDocumentTypes",
        "Add :CFBundleDocumentTypes array",
        "Add :CFBundleDocumentTypes:0:CFBundleTypeName string \"Any File\"",
        "Add :CFBundleDocumentTypes:0:CFBundleTypeRole string Editor",
        "Add :CFBundleDocumentTypes:0:LSHandlerRank string Alternate",
        "Add :CFBundleDocumentTypes:0:LSItemContentTypes array",
        "Add :CFBundleDocumentTypes:0:LSItemContentTypes:0 string public.item",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect()
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
    let app = app_dir.join(format!("{APP_NAME}.app"));
    let _ = std::fs::remove_dir_all(&app);

    let src =
        std::env::temp_dir().join(format!("croft-launcher-{}.applescript", std::process::id()));
    std::fs::write(&src, launcher_script(croft_bin, open_dir))
        .with_context(|| format!("writing {}", src.display()))?;
    let status = Command::new("osacompile")
        .arg("-o")
        .arg(&app)
        .arg(&src)
        .stdout(Stdio::null())
        .status()
        .context("running osacompile")?;
    let _ = std::fs::remove_file(&src);
    if !status.success() {
        anyhow::bail!("osacompile failed building the launcher applet");
    }

    write_icon(&app.join("Contents/Resources/croft.icns")).context("building the launcher icon")?;

    let plist = app.join("Contents/Info.plist");
    let mut buddy = Command::new("/usr/libexec/PlistBuddy");
    for edit in plist_edits() {
        buddy.arg("-c").arg(edit);
    }
    let status = buddy
        .arg(&plist)
        .stdout(Stdio::null())
        .status()
        .context("running PlistBuddy")?;
    if !status.success() {
        anyhow::bail!("PlistBuddy failed writing {}", plist.display());
    }

    // Editing Info.plist invalidates osacompile's signature; without a fresh
    // ad-hoc one macOS refuses to launch the applet.
    let status = Command::new("codesign")
        .args(["--force", "--sign", "-"])
        .arg(&app)
        .stderr(Stdio::null())
        .status()
        .context("running codesign")?;
    if !status.success() {
        anyhow::bail!("codesign failed re-signing the launcher");
    }

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
        assert!(s.contains("on run"));
        assert!(s.contains("open -na Ghostty.app --args"));
        assert!(s.contains("/Users/v/.cargo/bin/croft '/Users/v/Documents'"));
    }

    /// A plain `#!/bin/sh` bundle executable never sees the opened document:
    /// macOS delivers it as an `odoc` Apple Event, so argv arrives empty. Only
    /// an AppleScript applet's `on open` handler receives it.
    #[test]
    fn script_handles_opened_documents_through_an_open_handler() {
        let s = launcher_script("/Users/v/.cargo/bin/croft", "/Users/v/Documents");
        assert!(s.contains("on open theFiles"));
        assert!(s.contains("POSIX path of (item 1 of theFiles)"));
        // The dropped path is passed through as croft's workspace argument;
        // croft itself roots a file at its parent (see cli::resolve_workspace).
        assert!(s.contains("quoted form of f"));
    }

    /// Opening a document means "show me this file", so the window starts with
    /// the Explorer and terminal hidden and the editor filling it. A plain
    /// click on the launcher opens the workspace and keeps the full layout.
    #[test]
    fn opened_documents_start_zen_but_a_plain_click_does_not() {
        let s = launcher_script("/Users/v/.cargo/bin/croft", "/Users/v/Documents");
        let (run, open) = s.split_once("on open theFiles").unwrap();
        assert!(open.contains("--zen"));
        assert!(!run.contains("--zen"));
    }

    #[test]
    fn plist_edits_claim_every_file_type_and_set_the_bundle_id() {
        let e = plist_edits().join(" ");
        assert!(e.contains("CFBundleIdentifier string com.vitali87.croft-launcher"));
        assert!(e.contains("LSItemContentTypes:0 string public.item"));
        assert!(e.contains("CFBundleTypeRole string Editor"));
        assert!(e.contains("Set :CFBundleIconFile croft"));
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
