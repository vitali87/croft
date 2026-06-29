//! Ghostty keyboard integration for croft.
//!
//! Ghostty consults its own keybinding layer *before* it encodes a key for the
//! foreground application, so a default bind like `cmd+t=new_tab` swallows the
//! chord and croft never sees it, no matter that croft has already negotiated
//! the kitty keyboard protocol. iTerm2 has the same problem and croft solves it
//! by relocating iTerm2's menu items and installing `GlobalKeyMap` forwarders
//! (see [`crate::iterm2`]); the Ghostty equivalent is a block of `keybind`
//! lines in Ghostty's config that re-emit each chord as the same CSI-u sequence
//! via Ghostty's `csi:` action. The byte payloads are reused verbatim from
//! [`crate::iterm2::payloads`] so croft receives byte-identical input on both
//! terminals.
//!
//! Ghostty's `csi:` action sends a CSI sequence *without* the `ESC [` header,
//! so croft's `ESC [ 116 ; 9 u` (Cmd+T) becomes the keybind `cmd+t=csi:116;9u`.
//! Modifier and key spellings (`cmd`/`shift`/`ctrl`/`alt`, `arrow_left`,
//! `f12`, `enter`, and literal codepoints for printable keys) are taken from
//! Ghostty's trigger parser.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::iterm2::payloads as pl;

/// `~/.config/ghostty/config` (XDG) and the macOS application-support config
/// path Ghostty also reads. The XDG file wins when it already exists so croft
/// edits the file the user is actually maintaining; otherwise the macOS-native
/// location is created.
const XDG_CONFIG_REL: &str = ".config/ghostty/config";
const APP_SUPPORT_CONFIG_REL: &str = "Library/Application Support/com.mitchellh.ghostty/config";

/// Marker lines that fence croft's managed keybind block inside the user's
/// Ghostty config. Re-running setup replaces everything between them, leaving
/// the rest of the file untouched.
const BLOCK_BEGIN: &str = "# >>> croft keybindings (managed by `croft setup-ghostty`) >>>";
const BLOCK_END: &str = "# <<< croft keybindings (managed by `croft setup-ghostty`) <<<";
const KEYBIND_PREFIX: &str = "keybind = ";
const CSI_ACTION: &str = "csi:";

/// Every croft chord forwarded under Ghostty, paired with the byte payload that
/// reaches croft, as `(ghostty_trigger, iterm2_csi_hex)`. This mirrors the set
/// iTerm2 forwards in [`crate::iterm2`] one-for-one. Printable keys use their
/// literal codepoint as the trigger (Ghostty accepts single-codepoint keys);
/// arrows use `arrow_left`/`arrow_right`; function keys use `f12`. Cmd+V is
/// intentionally absent on both terminals so Ghostty's native paste keeps
/// working locally and over SSH via bracketed paste. The digit chords
/// (`cmd+0`..`cmd+9`) are appended separately from [`pl::CMD_DIGIT_CHORDS`].
const CHORDS: &[(&str, &str)] = &[
    // Explorer / search chords.
    ("cmd+shift+f", pl::CMD_SHIFT_F_HEX),
    ("cmd+f", pl::CMD_F_HEX),
    ("cmd+r", pl::CMD_R_HEX),
    ("cmd+alt+r", pl::CMD_OPT_R_HEX),
    ("cmd+alt+c", pl::CMD_OPT_C_HEX),
    ("cmd+alt+m", pl::CMD_OPT_M_HEX),
    ("cmd+/", pl::CMD_SLASH_HEX),
    ("cmd+.", pl::CMD_DOT_HEX),
    ("cmd+shift+/", pl::CMD_SHIFT_SLASH_HEX),
    ("cmd+shift+enter", pl::CMD_SHIFT_ENTER_HEX),
    // Mac-style Cmd+letter editor / terminal / source-control chords.
    ("cmd+a", pl::CMD_A_HEX),
    ("cmd+c", pl::CMD_C_HEX),
    ("cmd+s", pl::CMD_S_HEX),
    ("cmd+x", pl::CMD_X_HEX),
    ("cmd+z", pl::CMD_Z_HEX),
    ("cmd+d", pl::CMD_D_HEX),
    ("cmd+g", pl::CMD_G_HEX),
    ("cmd+k", pl::CMD_K_HEX),
    ("cmd+y", pl::CMD_Y_HEX),
    ("cmd+o", pl::CMD_O_HEX),
    ("cmd+e", pl::CMD_E_HEX),
    ("cmd+p", pl::CMD_P_HEX),
    ("cmd+w", pl::CMD_W_HEX),
    ("cmd+b", pl::CMD_B_HEX),
    // Sidebar / view jumps (Cmd+Shift+letter).
    ("cmd+shift+g", pl::CMD_SHIFT_G_HEX),
    ("cmd+shift+o", pl::CMD_SHIFT_O_HEX),
    ("cmd+shift+e", pl::CMD_SHIFT_E_HEX),
    ("cmd+shift+s", pl::CMD_SHIFT_S_HEX),
    ("cmd+shift+d", pl::CMD_SHIFT_D_HEX),
    ("cmd+shift+r", pl::CMD_SHIFT_R_HEX),
    ("cmd+shift+x", pl::CMD_SHIFT_X_HEX),
    ("cmd+shift+l", pl::CMD_SHIFT_L_HEX),
    ("cmd+shift+n", pl::CMD_SHIFT_N_HEX),
    ("cmd+shift+t", pl::CMD_SHIFT_T_HEX),
    // Terminal: new + cycle.
    ("cmd+t", pl::CMD_T_HEX),
    ("cmd+[", pl::CMD_LBRACKET_HEX),
    ("cmd+]", pl::CMD_RBRACKET_HEX),
    // Maximize terminal.
    ("ctrl+shift+j", pl::CTRL_SHIFT_J_HEX),
    // Editor: split, focus group, LSP navigation.
    ("cmd+\\", pl::CMD_BACKSLASH_HEX),
    // Go to Bracket (VS Code editor.action.jumpToBracket).
    ("cmd+shift+\\", pl::CMD_SHIFT_BACKSLASH_HEX),
    // Select to Bracket (editor.action.selectToBracket).
    ("cmd+alt+\\", pl::CMD_OPT_BACKSLASH_HEX),
    // Convert Indentation to Spaces / Tabs, Trim Final Newlines.
    ("cmd+alt+shift+s", pl::CMD_OPT_SHIFT_S_HEX),
    ("cmd+alt+shift+t", pl::CMD_OPT_SHIFT_T_HEX),
    ("cmd+alt+shift+n", pl::CMD_OPT_SHIFT_N_HEX),
    // Join Lines, Transform Upper/Lower/Title, Sort Asc/Desc, Trim Trailing.
    ("cmd+alt+shift+j", pl::CMD_OPT_SHIFT_J_HEX),
    ("cmd+alt+shift+u", pl::CMD_OPT_SHIFT_U_HEX),
    ("cmd+alt+shift+l", pl::CMD_OPT_SHIFT_L_HEX),
    ("cmd+alt+shift+c", pl::CMD_OPT_SHIFT_C_HEX),
    ("cmd+alt+shift+a", pl::CMD_OPT_SHIFT_A_HEX),
    ("cmd+alt+shift+d", pl::CMD_OPT_SHIFT_D_HEX),
    ("cmd+alt+shift+w", pl::CMD_OPT_SHIFT_W_HEX),
    // Format Document (editor.action.formatDocument).
    ("cmd+alt+shift+f", pl::CMD_OPT_SHIFT_F_HEX),
    ("cmd+alt+arrow_left", pl::CMD_OPT_LEFT_HEX),
    ("cmd+alt+arrow_right", pl::CMD_OPT_RIGHT_HEX),
    // Multi-cursor: add cursor above / below.
    ("cmd+alt+arrow_up", pl::CMD_OPT_UP_HEX),
    ("cmd+alt+arrow_down", pl::CMD_OPT_DOWN_HEX),
    // Command Palette.
    ("cmd+shift+p", pl::CMD_SHIFT_P_HEX),
    ("cmd+f12", pl::CMD_F12_HEX),
    ("ctrl+shift+f12", pl::CTRL_SHIFT_F12_HEX),
];

/// Convert an iTerm2 Send-Hex payload (`"0x1b 0x5b 0x31 0x31 0x36 0x3b 0x39
/// 0x75"`) into a Ghostty `csi:` body (`"116;9u"`) by parsing the space
/// separated bytes and stripping the `ESC [` introducer. The remaining bytes
/// are always printable ASCII (digits, `;`, and a final byte such as `u`, `~`,
/// `D`, or `C`).
fn csi_body(hex: &str) -> String {
    let bytes: Vec<u8> = hex
        .split_whitespace()
        .filter_map(|token| u8::from_str_radix(token.trim_start_matches("0x"), 16).ok())
        .collect();
    let body = bytes.strip_prefix(&[0x1b, 0x5b]).unwrap_or(&bytes);
    body.iter().map(|&b| b as char).collect()
}

/// Render croft's managed keybind block: the begin marker, one `keybind` line
/// per forwarded chord (including the digit chords), and the end marker.
pub(crate) fn render_keybind_block() -> String {
    let mut lines = Vec::with_capacity(CHORDS.len() + pl::CMD_DIGIT_CHORDS.len() + 2);
    lines.push(BLOCK_BEGIN.to_string());
    for (trigger, hex) in CHORDS {
        lines.push(format!(
            "{KEYBIND_PREFIX}{trigger}={CSI_ACTION}{}",
            csi_body(hex)
        ));
    }
    for (digit, (_key, hex)) in pl::CMD_DIGIT_CHORDS.iter().enumerate() {
        lines.push(format!(
            "{KEYBIND_PREFIX}cmd+{digit}={CSI_ACTION}{}",
            csi_body(hex)
        ));
    }
    lines.push(BLOCK_END.to_string());
    lines.join("\n")
}

/// Drop any previously-installed croft block (between the markers, inclusive)
/// from existing config content, leaving every other line untouched.
fn strip_managed_block(content: &str) -> String {
    let mut kept: Vec<&str> = Vec::new();
    let mut in_block = false;
    for line in content.lines() {
        if line.trim() == BLOCK_BEGIN {
            in_block = true;
            continue;
        }
        if in_block {
            if line.trim() == BLOCK_END {
                in_block = false;
            }
            continue;
        }
        kept.push(line);
    }
    kept.join("\n")
}

/// Merge croft's managed block into existing config content: strip any prior
/// croft block, then append the fresh block. Idempotent (running twice yields
/// the same result) and order-preserving for the user's own lines.
pub(crate) fn merge_config(existing: &str, block: &str) -> String {
    let user = strip_managed_block(existing);
    let user = user.trim_end_matches('\n');
    if user.is_empty() {
        format!("{block}\n")
    } else {
        format!("{user}\n\n{block}\n")
    }
}

/// Load → merge → save the Ghostty config, creating it (and its parent
/// directory) when absent.
pub fn install_keybinds(config_path: &Path) -> Result<()> {
    let existing = match std::fs::read_to_string(config_path) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(e).with_context(|| format!("reading {}", config_path.display()));
        }
    };
    let merged = merge_config(&existing, &render_keybind_block());
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(config_path, merged)
        .with_context(|| format!("writing {}", config_path.display()))?;
    Ok(())
}

/// Resolve the Ghostty config croft should edit: the XDG file when it already
/// exists, otherwise the macOS application-support location.
pub fn default_config_path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let xdg = home.join(XDG_CONFIG_REL);
    if xdg.exists() {
        return xdg;
    }
    home.join(APP_SUPPORT_CONFIG_REL)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csi_body_strips_the_esc_bracket_introducer() {
        assert_eq!(csi_body(pl::CMD_T_HEX), "116;9u");
        assert_eq!(csi_body(pl::CMD_OPT_LEFT_HEX), "1;11D");
        assert_eq!(csi_body(pl::CMD_OPT_RIGHT_HEX), "1;11C");
        assert_eq!(csi_body(pl::CMD_F12_HEX), "24;9~");
    }

    #[test]
    fn every_chord_body_is_printable_ascii_and_nonempty() {
        for (trigger, hex) in CHORDS {
            let body = csi_body(hex);
            assert!(!body.is_empty(), "empty body for {trigger}");
            assert!(
                body.chars().all(|c| c.is_ascii() && !c.is_control()),
                "non-printable body {body:?} for {trigger}"
            );
        }
    }

    #[test]
    fn rendered_block_contains_cmd_t_and_a_digit_chord() {
        let block = render_keybind_block();
        assert!(block.starts_with(BLOCK_BEGIN));
        assert!(block.ends_with(BLOCK_END));
        assert!(block.contains("keybind = cmd+t=csi:116;9u"));
        // cmd+5 forwards codepoint '5' (53) with Super.
        assert!(block.contains("keybind = cmd+5=csi:53;9u"));
        // The backslash trigger survives literally.
        assert!(block.contains("keybind = cmd+\\=csi:92;9u"));
        // Go to Bracket: same '\' key with Shift (modifier byte 10).
        assert!(block.contains("keybind = cmd+shift+\\=csi:92;10u"));
    }

    #[test]
    fn merge_into_empty_config_is_just_the_block() {
        let merged = merge_config("", &render_keybind_block());
        assert_eq!(merged, format!("{}\n", render_keybind_block()));
    }

    #[test]
    fn merge_preserves_user_lines_and_appends_block() {
        let existing = "font-size = 14\ntheme = dark\n";
        let merged = merge_config(existing, &render_keybind_block());
        assert!(merged.starts_with("font-size = 14\ntheme = dark\n"));
        assert!(merged.contains(BLOCK_BEGIN));
        assert!(merged.contains("keybind = cmd+t=csi:116;9u"));
    }

    #[test]
    fn merge_is_idempotent() {
        let block = render_keybind_block();
        let once = merge_config("font-size = 14\n", &block);
        let twice = merge_config(&once, &block);
        assert_eq!(once, twice);
        // Exactly one managed block survives.
        assert_eq!(twice.matches(BLOCK_BEGIN).count(), 1);
    }

    #[test]
    fn install_round_trips_through_disk_and_replaces_old_block() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config");
        std::fs::write(&cfg, "font-size = 13\n").unwrap();
        install_keybinds(&cfg).unwrap();
        install_keybinds(&cfg).unwrap();
        let content = std::fs::read_to_string(&cfg).unwrap();
        assert_eq!(content.matches(BLOCK_BEGIN).count(), 1);
        assert!(content.starts_with("font-size = 13\n"));
        assert!(content.contains("keybind = cmd+t=csi:116;9u"));
    }

    #[test]
    fn install_creates_missing_config_and_parent_dir() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("nested/ghostty/config");
        install_keybinds(&cfg).unwrap();
        let content = std::fs::read_to_string(&cfg).unwrap();
        assert!(content.contains("keybind = cmd+t=csi:116;9u"));
    }
}
