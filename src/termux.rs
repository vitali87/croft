//! Termux host integration: install the codicon-capable Nerd Font on its own.
//!
//! Mainline Termux renders no inline-image protocol at all (no OSC 1337, no
//! Kitty graphics, no sixel; its CSI `c` Device Attributes reply carries no
//! `;4`), so the activity bar falls back to codicon glyphs (U+EAF0 ...).
//! Termux draws text with Android's system monospace unless
//! `~/.termux/font.ttf` exists, and no Android system font covers the codicon
//! Private Use Area, so the icons render as blank cells out of the box. The
//! fix croft owns: download MesloLGS Nerd Font Mono (the same Meslo family
//! `setup-iterm2` pins on macOS; Nerd Fonts ships codicons at their upstream
//! U+EA60..U+EBEB codepoints) into `~/.termux/font.ttf`, then apply it live
//! with `termux-reload-settings`. Mirrors the zoxide auto-install: probed
//! once from `app::run`, downloaded on a detached thread, never blocks the
//! first frame, best-effort on failure (retried on the next launch).

use std::path::{Path, PathBuf};

/// Pinned to the Nerd Fonts v3.4.0 release (latest as of 2026-06-10) so the
/// bytes croft installs never drift under it. Raw file fetch, not the GitHub
/// REST API, so shared-egress users (VPN/NAT) don't hit per-IP API limits.
const FONT_URL: &str = "https://raw.githubusercontent.com/ryanoasis/nerd-fonts/v3.4.0/patched-fonts/Meslo/S/Regular/MesloLGSNerdFontMono-Regular.ttf";
const FONT_DIR: &str = ".termux";
const FONT_FILE: &str = "font.ttf";
/// Staging name for the atomic install: write here, rename over `font.ttf`.
const FONT_TMP_FILE: &str = "font.ttf.croft-download";
const RELOAD_CMD: &str = "termux-reload-settings";
/// Hard cap on the downloaded body; the real font is ~2.4 MB.
const MAX_FONT_BYTES: u64 = 16 * 1024 * 1024;

/// Where Termux loads its custom terminal font from.
fn font_path(home: &Path) -> PathBuf {
    home.join(FONT_DIR).join(FONT_FILE)
}

/// The font install runs only inside Termux and never clobbers a font the
/// user already chose; deleting `~/.termux/font.ttf` re-arms it.
fn install_needed(termux: bool, font_exists: bool) -> bool {
    termux && !font_exists
}

/// Sanity check on the downloaded bytes before they replace the terminal
/// font: accept the sfnt magics (TrueType 0x00010000 / `true`, CFF `OTTO`,
/// collection `ttcf`) and reject anything else (HTML error pages, truncated
/// bodies), so a bad download can never brick the Termux font.
fn looks_like_ttf(bytes: &[u8]) -> bool {
    bytes.len() >= 4
        && matches!(
            &bytes[..4],
            [0x00, 0x01, 0x00, 0x00] | b"true" | b"OTTO" | b"ttcf"
        )
}

/// Fetch the pinned font and refuse to hand back anything that is not an
/// sfnt-flavored font file.
fn download_font() -> anyhow::Result<Vec<u8>> {
    use std::io::Read;
    let resp = ureq::get(FONT_URL).call()?;
    let mut bytes = Vec::new();
    resp.into_reader()
        .take(MAX_FONT_BYTES)
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(
        looks_like_ttf(&bytes),
        "downloaded body is not a font file ({} bytes)",
        bytes.len()
    );
    Ok(bytes)
}

/// Write the font next to its final name, then rename into place, so Termux
/// can never observe (or load) a half-written `font.ttf`.
fn install_font(target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = target.parent().expect("font path always has ~/.termux");
    std::fs::create_dir_all(dir)?;
    let staging = dir.join(FONT_TMP_FILE);
    std::fs::write(&staging, bytes)?;
    std::fs::rename(&staging, target)
}

/// Probe for a missing `~/.termux/font.ttf` and, only inside Termux, install
/// the codicon-capable Meslo Nerd Font on a detached thread, then apply it
/// live via `termux-reload-settings` (in Termux's bootstrap; if it is ever
/// absent the font still applies on the next app restart). Best-effort:
/// every failure is logged and retried on the next launch. Invoked once from
/// `app::run`; a no-op everywhere but Termux, so local macOS and remote
/// Linux behavior is untouched.
pub fn ensure_codicon_font_in_background() {
    use crate::lsp::log_file;
    if !crate::iterm2_inline::detect_termux() {
        return;
    }
    let Some(home) = std::env::var_os("HOME") else {
        log_file::log("termux font: $HOME unset, skipping font install");
        return;
    };
    let target = font_path(Path::new(&home));
    if !install_needed(true, target.exists()) {
        return;
    }
    std::thread::spawn(move || {
        match download_font() {
            Ok(bytes) => {
                if let Err(e) = install_font(&target, &bytes) {
                    log_file::log(&format!("termux font: install to {target:?} failed: {e}"));
                    return;
                }
                log_file::log(&format!(
                    "termux font: installed Meslo Nerd Font Mono to {target:?}"
                ));
            }
            Err(e) => {
                log_file::log(&format!("termux font: download failed: {e}"));
                return;
            }
        }
        match std::process::Command::new(RELOAD_CMD).output() {
            Ok(out) if out.status.success() => {
                log_file::log("termux font: reloaded Termux settings, icons live")
            }
            Ok(out) => log_file::log(&format!(
                "termux font: {RELOAD_CMD} exited {}; font applies on next Termux restart",
                out.status
            )),
            Err(e) => log_file::log(&format!(
                "termux font: {RELOAD_CMD} not runnable ({e}); font applies on next Termux restart"
            )),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_path_is_termux_font_ttf_under_home() {
        assert_eq!(
            font_path(Path::new("/data/data/com.termux/files/home")),
            PathBuf::from("/data/data/com.termux/files/home/.termux/font.ttf"),
            "Termux loads exactly ~/.termux/font.ttf; any other path is ignored"
        );
    }

    #[test]
    fn install_runs_only_on_termux_and_only_when_no_font_is_present() {
        assert!(
            install_needed(true, false),
            "Termux with no custom font is the broken-icons case the install fixes"
        );
        assert!(
            !install_needed(true, true),
            "an existing font.ttf is the user's choice; never clobber it"
        );
        assert!(
            !install_needed(false, false),
            "non-Termux hosts pick fonts via setup-iterm2/setup-ghostty, not font.ttf"
        );
        assert!(!install_needed(false, true));
    }

    #[test]
    fn ttf_magic_accepts_sfnt_flavors_and_rejects_garbage() {
        assert!(looks_like_ttf(&[0x00, 0x01, 0x00, 0x00, 0xab]));
        assert!(looks_like_ttf(b"trueXYZ"));
        assert!(looks_like_ttf(b"OTTOrest"));
        assert!(looks_like_ttf(b"ttcfrest"));
        assert!(
            !looks_like_ttf(b"<!DOCTYPE html>"),
            "a 404/error page must never be installed as the terminal font"
        );
        assert!(!looks_like_ttf(b"\x00\x01"), "truncated header rejected");
        assert!(!looks_like_ttf(b""));
    }

    #[test]
    fn font_url_pins_release_tag_and_mono_variant() {
        assert!(
            FONT_URL.contains("/v3.4.0/"),
            "URL must pin a release tag so installed bytes never drift"
        );
        assert!(
            FONT_URL.ends_with("MesloLGSNerdFontMono-Regular.ttf"),
            "must be the Mono variant of the same Meslo family setup-iterm2 uses"
        );
        assert!(
            FONT_URL.starts_with("https://raw.githubusercontent.com/ryanoasis/nerd-fonts/"),
            "raw file fetch, not the rate-limited GitHub REST API"
        );
    }
}
