//! User-defined key bindings, loaded from `~/.config/croft/keybindings.json`.
//!
//! croft's default chords are hand-wired in the app key router; this layer sits
//! *in front* of them so a VS Code refugee can carry their muscle memory over.
//! Each binding maps a chord string (`"ctrl+shift+p"`, `"cmd+,"`, `"f2"`) to a
//! palette [`Command`] id (the stable snake_case name from
//! [`Command::id`](crate::widgets::command_palette::Command::id)). A bound chord
//! wins over the built-in handler for the same chord; an unbound chord falls
//! through untouched.
//!
//! JSON shape mirrors VS Code's `keybindings.json`:
//! ```json
//! [
//!   { "key": "cmd+,",        "command": "open_settings" },
//!   { "key": "ctrl+shift+p", "command": "quick_open" }
//! ]
//! ```
//!
//! ponytail: no `when` clauses. The app only consults the keymap when the
//! terminal pane is not focused and the chord carries a real modifier (or is a
//! function key), which stands in for VS Code's `editorTextFocus`-style context
//! and keeps plain typing and raw terminal control keys untouched. Add real
//! `when` contexts only if a user actually needs finer scoping.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::Deserialize;

use crate::widgets::command_palette::Command;

/// A normalized key chord: a base key plus the four modifier flags. Both the
/// parsed config side and the live-event side run through [`Chord::normalize`]
/// so the two always compare on the same footing (letter case folded into an
/// explicit `SHIFT`, only the four real modifiers retained).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Chord {
    code: KeyCode,
    mods: KeyModifiers,
}

impl Chord {
    /// Fold a raw `(code, mods)` into the canonical form used for comparison:
    /// lower-case letters (with the uppercasing re-expressed as `SHIFT`) and
    /// only the CONTROL/ALT/SHIFT/SUPER bits kept.
    fn normalize(code: KeyCode, mods: KeyModifiers) -> Self {
        let mut m = mods
            & (KeyModifiers::CONTROL
                | KeyModifiers::ALT
                | KeyModifiers::SHIFT
                | KeyModifiers::SUPER);
        let code = match code {
            KeyCode::Char(c) if c.is_ascii_uppercase() => {
                m |= KeyModifiers::SHIFT;
                KeyCode::Char(c.to_ascii_lowercase())
            }
            KeyCode::Char(c) => KeyCode::Char(c.to_ascii_lowercase()),
            other => other,
        };
        Self { code, mods: m }
    }

    /// Build the chord an incoming event represents.
    pub fn from_event(key: KeyEvent) -> Self {
        Self::normalize(key.code, key.modifiers)
    }

    /// The iTerm2 `GlobalKeyMap` forwarder for this chord as `(key, csi_u_hex)`,
    /// or `None` when no forwarding is needed or the chord can't be encoded.
    ///
    /// macOS reserves Cmd for its menus, so only Super chords are swallowed
    /// before reaching croft; Ctrl/Alt/function chords already arrive, so those
    /// return `None`. Forwarding re-emits the chord as the same CSI-u sequence
    /// the built-in chords use, which crossterm decodes straight back into the
    /// `KeyEvent` the keymap matches. The encoding reproduces the hand-written
    /// forwarders in `iterm2.rs` byte-for-byte (asserted by a round-trip test).
    ///
    /// ponytail: only `Char` keys, and shift only with letters — a shifted
    /// non-letter's glyph (Shift+/ = ?) is keyboard-layout dependent. Cmd chords
    /// on named/function keys are niche; add them if a user needs one.
    pub fn iterm2_forwarder(self) -> Option<(String, String)> {
        if !self.mods.contains(KeyModifiers::SUPER) {
            return None;
        }
        let KeyCode::Char(c) = self.code else {
            return None;
        };
        let shift = self.mods.contains(KeyModifiers::SHIFT);
        if shift && !c.is_ascii_alphabetic() {
            return None;
        }
        let vk = mac_ansi_keycode(c)?;
        let codepoint = if shift {
            c.to_ascii_uppercase() as u32
        } else {
            c as u32
        };
        let mut mask = 0x10_0000u32; // Cmd
        let mut modbyte = 1u32 + 8; // CSI-u base (1) + Super (8)
        if shift {
            mask |= 0x2_0000;
            modbyte += 1;
        }
        if self.mods.contains(KeyModifiers::ALT) {
            mask |= 0x8_0000;
            modbyte += 2;
        }
        if self.mods.contains(KeyModifiers::CONTROL) {
            mask |= 0x4_0000;
            modbyte += 4;
        }
        Some((
            format!("0x{codepoint:x}-0x{mask:x}-0x{vk:x}"),
            csi_u_hex(codepoint, modbyte),
        ))
    }

    /// The Ghostty keybind trigger string for this chord (e.g. `"cmd+shift+p"`),
    /// or `None` under the same conditions as [`Self::iterm2_forwarder`]. Ghostty
    /// re-emits the chord via its `csi:` action using that forwarder's payload,
    /// so the two must gate identically.
    pub fn ghostty_trigger(self) -> Option<String> {
        if !self.mods.contains(KeyModifiers::SUPER) {
            return None;
        }
        let KeyCode::Char(c) = self.code else {
            return None;
        };
        if self.mods.contains(KeyModifiers::SHIFT) && !c.is_ascii_alphabetic() {
            return None;
        }
        let mut t = String::from("cmd");
        if self.mods.contains(KeyModifiers::CONTROL) {
            t.push_str("+ctrl");
        }
        if self.mods.contains(KeyModifiers::ALT) {
            t.push_str("+alt");
        }
        if self.mods.contains(KeyModifiers::SHIFT) {
            t.push_str("+shift");
        }
        t.push('+');
        t.push(c.to_ascii_lowercase());
        Some(t)
    }

    /// Parse a config string like `"cmd+shift+p"`, `"ctrl+/"`, `"f2"`. Returns
    /// `None` on an unrecognized token so one bad line is skipped, not fatal.
    pub fn parse(s: &str) -> Option<Chord> {
        if s.trim().is_empty() {
            return None;
        }
        let mut mods = KeyModifiers::empty();
        let mut code: Option<KeyCode> = None;
        for raw in s.split('+') {
            let tok = raw.trim().to_ascii_lowercase();
            if tok.is_empty() {
                // A literal "+" key: the split produced an empty token because
                // the key itself is '+'. Treat it as the base key.
                code = Some(KeyCode::Char('+'));
                continue;
            }
            match tok.as_str() {
                "ctrl" | "control" => mods |= KeyModifiers::CONTROL,
                "alt" | "opt" | "option" => mods |= KeyModifiers::ALT,
                "shift" => mods |= KeyModifiers::SHIFT,
                "cmd" | "command" | "super" | "meta" | "win" => mods |= KeyModifiers::SUPER,
                // "mod" is the portable modifier: Cmd on macOS, Ctrl elsewhere,
                // so one config drives both the local Mac and remote Linux
                // builds (the golden rule: identical behavior on both).
                "mod" => {
                    if cfg!(target_os = "macos") {
                        mods |= KeyModifiers::SUPER;
                    } else {
                        mods |= KeyModifiers::CONTROL;
                    }
                }
                other => code = Some(parse_key(other)?),
            }
        }
        code.map(|c| Chord::normalize(c, mods))
    }
}

/// The CSI-u escape `ESC [ <codepoint> ; <modbyte> u` as the space-separated
/// hex-byte string iTerm2's "Send Hex Code" action wants (each decimal digit
/// becomes its ASCII byte).
fn csi_u_hex(codepoint: u32, modbyte: u32) -> String {
    let mut parts = vec![String::from("0x1b"), String::from("0x5b")];
    parts.extend(codepoint.to_string().bytes().map(|b| format!("0x{b:x}")));
    parts.push(String::from("0x3b"));
    parts.extend(modbyte.to_string().bytes().map(|b| format!("0x{b:x}")));
    parts.push(String::from("0x75"));
    parts.join(" ")
}

/// The macOS ANSI virtual key code (`kVK_ANSI_*` from Carbon HIToolbox) for a
/// US-layout character. iTerm2's `GlobalKeyMap` key is `codepoint-mask-vkcode`,
/// so a forwarder can't be built without this. `None` for a key not on the
/// table (skip its forwarder rather than emit a wrong one).
fn mac_ansi_keycode(c: char) -> Option<u32> {
    Some(match c.to_ascii_lowercase() {
        'a' => 0x00,
        'b' => 0x0b,
        'c' => 0x08,
        'd' => 0x02,
        'e' => 0x0e,
        'f' => 0x03,
        'g' => 0x05,
        'h' => 0x04,
        'i' => 0x22,
        'j' => 0x26,
        'k' => 0x28,
        'l' => 0x25,
        'm' => 0x2e,
        'n' => 0x2d,
        'o' => 0x1f,
        'p' => 0x23,
        'q' => 0x0c,
        'r' => 0x0f,
        's' => 0x01,
        't' => 0x11,
        'u' => 0x20,
        'v' => 0x09,
        'w' => 0x0d,
        'x' => 0x07,
        'y' => 0x10,
        'z' => 0x06,
        '0' => 0x1d,
        '1' => 0x12,
        '2' => 0x13,
        '3' => 0x14,
        '4' => 0x15,
        '5' => 0x17,
        '6' => 0x16,
        '7' => 0x1a,
        '8' => 0x1c,
        '9' => 0x19,
        '-' => 0x1b,
        '=' => 0x18,
        '[' => 0x21,
        ']' => 0x1e,
        '\\' => 0x2a,
        ';' => 0x29,
        '\'' => 0x27,
        ',' => 0x2b,
        '.' => 0x2f,
        '/' => 0x2c,
        '`' => 0x32,
        ' ' => 0x31,
        _ => return None,
    })
}

fn parse_key(tok: &str) -> Option<KeyCode> {
    if let Some(n) = tok.strip_prefix('f')
        && let Ok(num) = n.parse::<u8>()
        && (1..=24).contains(&num)
    {
        return Some(KeyCode::F(num));
    }
    Some(match tok {
        "enter" | "return" | "cr" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "esc" | "escape" => KeyCode::Esc,
        "space" => KeyCode::Char(' '),
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "backspace" | "bs" => KeyCode::Backspace,
        "delete" | "del" => KeyCode::Delete,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" | "pgup" => KeyCode::PageUp,
        "pagedown" | "pgdn" => KeyCode::PageDown,
        "insert" | "ins" => KeyCode::Insert,
        s => {
            let mut chars = s.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                return None; // multi-char token that is not a named key
            }
            KeyCode::Char(c)
        }
    })
}

/// Strip `//` line comments so the JSONC config (and the seeded template) parse
/// through `serde_json`, which rejects comments. Tracks string state so a `//`
/// inside a quoted value survives. ponytail: only `//` line comments, no `/* */`
/// blocks — the config has never needed them; add if a user asks.
pub(crate) fn strip_line_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut in_string = false;
    let mut escaped = false;
    let mut chars = src.chars().peekable();
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                for skipped in chars.by_ref() {
                    if skipped == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// A mouse gesture bindable in `keybindings.json` (#259).
///
/// Deliberately NOT expressed as a [`Chord`]: a chord is a `KeyCode` plus
/// modifiers, and a gesture has no key code. Keeping the two apart also keeps
/// gestures out of [`Keymap::chords`], which drives the iTerm2/Ghostty
/// forwarder pass — a terminal forwarder for a mouse gesture is meaningless.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GestureKind {
    Click,
    DoubleClick,
    MiddleClick,
    RightClick,
    WheelUp,
    WheelDown,
}

/// A gesture plus the modifiers held with it.
///
/// `cmd`/`super` parses but can never match: SGR mouse reporting carries no
/// Super bit, which is why croft's own go-to-definition rides Ctrl rather than
/// Cmd. Binding it produces a load warning instead of a binding that silently
/// never fires.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Gesture {
    pub kind: GestureKind,
    pub mods: KeyModifiers,
}

impl Gesture {
    /// Parse `"ctrl+click"`, `"middle_click"`, `"alt+wheel_up"`. Returns the
    /// gesture and, when the spelling parses but cannot ever match, a warning
    /// for the OUTPUT channel.
    /// `None` — not a gesture spelling; the caller should try it as a key
    /// chord. `Some((None, Some(reason)))` — a gesture croft understands but
    /// refuses to bind, with the reason. `Some((Some(g), warn))` — bind it.
    pub fn parse(s: &str) -> Option<(Option<Gesture>, Option<String>)> {
        let mut mods = KeyModifiers::NONE;
        let mut kind: Option<GestureKind> = None;
        let mut warn = None;
        let mut refused = false;
        for part in s.split('+') {
            let p = part.trim().to_ascii_lowercase();
            match p.as_str() {
                "ctrl" | "control" => mods |= KeyModifiers::CONTROL,
                "alt" | "opt" | "option" => mods |= KeyModifiers::ALT,
                "shift" => mods |= KeyModifiers::SHIFT,
                "cmd" | "command" | "super" | "meta" | "win" => {
                    // Warned AND refused: `gesture_for` masks Super off, so a
                    // bound entry could never be looked up, yet it would make
                    // `has_mouse_bindings()` true and let `cmd+click` dodge
                    // the reserved bare-click check by being a different key.
                    refused = true;
                    warn = Some(format!(
                        "{s}: terminals do not report Cmd/Super for mouse events, so this \
                         binding can never fire - use ctrl or alt instead"
                    ));
                }
                // `mod` is Cmd for KEYS on macOS, but no terminal reports Cmd
                // for mouse, so it resolves to Ctrl on every platform here
                // rather than becoming an unfirable binding.
                "mod" => mods |= KeyModifiers::CONTROL,
                other => {
                    let k = match other {
                        "click" => GestureKind::Click,
                        "double_click" => GestureKind::DoubleClick,
                        // Rejected on purpose, but with its own reason: a
                        // bare `None` falls through to `Chord::parse` and
                        // reports "not a key chord or mouse gesture", which
                        // reads like a typo rather than a deliberate refusal.
                        // Refused with its own reason. A bare `None` would
                        // fall through to `Chord::parse` and report "not a
                        // key chord or mouse gesture", which reads like a
                        // typo rather than a deliberate refusal.
                        "triple_click" => {
                            return Some((
                                None,
                                Some(String::from(
                                    "triple_click: croft's click tracker counts single and \
                                     double only, so a triple binding would fire on the second \
                                     click — refused",
                                )),
                            ));
                        }
                        "middle_click" => GestureKind::MiddleClick,
                        "right_click" => GestureKind::RightClick,
                        "wheel_up" => GestureKind::WheelUp,
                        "wheel_down" => GestureKind::WheelDown,
                        _ => return None,
                    };
                    if kind.replace(k).is_some() {
                        return None;
                    }
                }
            }
        }
        // Checked BEFORE `kind?`: `"cmd"` or `"cmd+bogus"` has no gesture
        // token, so returning early here would drop the Cmd-is-unreportable
        // reason and fall through to a generic "not a key chord or mouse
        // gesture" — losing exactly the explanation this path exists for.
        if refused {
            return Some((None, warn));
        }
        let kind = kind?;
        Some((Some(Gesture { kind, mods }), warn))
    }

    /// True when this gesture would shadow primary selection. Binding these
    /// is refused at load: rebinding a bare click or drag in the editor or
    /// terminal leaves no way to place the caret or select text.
    pub fn is_reserved_in(&self, ctx: MouseContext) -> bool {
        self.mods.is_empty()
            && matches!(self.kind, GestureKind::Click)
            && matches!(ctx, MouseContext::Editor | MouseContext::Terminal)
    }
}

/// Where a mouse binding applies. Omitted means the editor, which is where
/// nearly every gesture is wanted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum MouseContext {
    #[default]
    Editor,
    Terminal,
    FileTree,
    TabStrip,
}

impl MouseContext {
    fn parse(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "editor" => Self::Editor,
            "terminal" => Self::Terminal,
            "file_tree" | "filetree" => Self::FileTree,
            "tab_strip" | "tabstrip" => Self::TabStrip,
            _ => return None,
        })
    }
}

/// One `{ "key": ..., "command": ... }` row as written on disk. `when` scopes
/// a MOUSE binding to a region and is ignored for key chords, which keep the
/// existing focus-based rule (see the module note).
#[derive(Debug, Deserialize)]
struct Binding {
    key: String,
    command: String,
    #[serde(default)]
    when: Option<String>,
}

/// The resolved user keymap: a chord table into [`Command`], plus a separate
/// mouse-gesture table keyed by gesture AND context, since one gesture can be
/// bound differently per region.
#[derive(Debug, Clone, Default)]
pub struct Keymap {
    bindings: HashMap<Chord, Command>,
    mouse: HashMap<(Gesture, MouseContext), Command>,
    /// Rows that parsed but were refused, for the OUTPUT channel. Silently
    /// dropping these is what makes a mistyped binding feel like croft is
    /// broken; the settings loader surfaces its warnings the same way.
    warnings: Vec<String>,
}

impl Keymap {
    /// Load and resolve `keybindings.json`, ignoring unparsable chords and
    /// unknown command ids (a typo skips that one line, never blocks startup).
    /// A missing file yields an empty keymap.
    pub fn load(path: &Path) -> Self {
        let Ok(json) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        Self::from_json(&json)
    }

    pub fn from_json(json: &str) -> Self {
        let rows: Vec<Binding> =
            serde_json::from_str(&strip_line_comments(json)).unwrap_or_default();
        let mut bindings = HashMap::new();
        let mut mouse = HashMap::new();
        let mut warnings = Vec::new();
        for row in rows {
            let Some(cmd) = Command::from_id(&row.command) else {
                warnings.push(format!(
                    "{}: unknown command id \"{}\"",
                    row.key, row.command
                ));
                continue;
            };
            // A gesture spelling wins over a chord spelling: `Gesture::parse`
            // only accepts the seven gesture names, so nothing a chord could
            // legitimately spell is captured here.
            if let Some((gesture, warn)) = Gesture::parse(&row.key) {
                if let Some(w) = warn {
                    warnings.push(w);
                }
                let Some(gesture) = gesture else {
                    continue;
                };
                let ctx = match row.when.as_deref() {
                    None => MouseContext::default(),
                    Some(w) => match MouseContext::parse(w) {
                        Some(c) => c,
                        None => {
                            warnings.push(format!("{}: unknown \"when\" context \"{w}\"", row.key));
                            continue;
                        }
                    },
                };
                if gesture.is_reserved_in(ctx) {
                    warnings.push(format!(
                        "{}: a bare click is reserved in the {} - rebinding it would leave no \
                         way to place the caret or select text",
                        row.key,
                        match ctx {
                            MouseContext::Terminal => "terminal",
                            _ => "editor",
                        }
                    ));
                    continue;
                }
                mouse.insert((gesture, ctx), cmd);
                continue;
            }
            match Chord::parse(&row.key) {
                Some(chord) => {
                    bindings.insert(chord, cmd);
                }
                None => warnings.push(format!("{}: not a key chord or mouse gesture", row.key)),
            }
        }
        Self {
            bindings,
            mouse,
            warnings,
        }
    }

    /// Rows the loader refused, for the caller to surface. Empty is the
    /// normal case.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// The command a mouse gesture is bound to in `ctx`, if any.
    pub fn command_for_mouse(&self, gesture: Gesture, ctx: MouseContext) -> Option<Command> {
        self.mouse.get(&(gesture, ctx)).copied()
    }

    /// True when no mouse gesture is bound, so `handle_mouse` can skip the
    /// lookup entirely on the common path.
    pub fn has_mouse_bindings(&self) -> bool {
        !self.mouse.is_empty()
    }

    /// No KEY chords bound. Mouse gestures are asked about separately via
    /// [`Self::has_mouse_bindings`], because the key path's caller uses this
    /// to skip its lookup and must not be affected by mouse rows.
    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }

    /// Every bound chord, for the terminal-setup pass that installs forwarders.
    pub fn chords(&self) -> Vec<Chord> {
        self.bindings.keys().copied().collect()
    }

    /// The command a live key event is bound to, if any. Bare keys and
    /// modifier-less letters are never matched here (see the module note): the
    /// caller only asks for chords that carry a modifier or are function keys.
    pub fn command_for(&self, key: KeyEvent) -> Option<Command> {
        self.bindings.get(&Chord::from_event(key)).copied()
    }
}

pub fn keybindings_path() -> PathBuf {
    crate::prefs::config_dir().join("keybindings.json")
}

/// The starter file written on first "Open Keyboard Shortcuts (JSON)" so the
/// user lands on a working example rather than a blank buffer. Uses `mod` so the
/// same file behaves the same on macOS and Linux.
pub const TEMPLATE: &str = r#"// croft keyboard shortcuts. Rebind any palette command by its id.
// "mod" is Cmd on macOS, Ctrl on Linux. Modifiers: ctrl, alt/opt, shift, cmd/super, mod.
// Run "Preferences: Open Settings" to see command ids, or the command palette.
//
// In iTerm2, reserved Cmd chords must be forwarded first: run `croft setup-iterm2`.
// Ctrl / Alt / function-key bindings always reach croft.
[
  { "key": "mod+,", "command": "open_settings" }
]
"#;

#[cfg(test)]
mod tests {

    #[test]
    fn gestures_parse_with_modifiers_and_reject_nonsense() {
        use super::{Gesture, GestureKind};
        let (g, warn) = Gesture::parse("ctrl+click").expect("ctrl+click parses");
        let g = g.expect("and binds");
        assert_eq!(g.kind, GestureKind::Click);
        assert_eq!(g.mods, KeyModifiers::CONTROL);
        assert!(warn.is_none());

        let (g, _) = Gesture::parse("middle_click").expect("bare gesture parses");
        let g = g.expect("and binds");
        assert_eq!(g.kind, GestureKind::MiddleClick);
        assert_eq!(g.mods, KeyModifiers::NONE);

        let (g, _) = Gesture::parse("alt+shift+wheel_up").expect("two modifiers parse");
        let g = g.expect("and binds");
        assert_eq!(g.kind, GestureKind::WheelUp);
        assert_eq!(g.mods, KeyModifiers::ALT | KeyModifiers::SHIFT);

        assert!(
            Gesture::parse("ctrl+f").is_none(),
            "a key chord is not a gesture"
        );
        assert!(
            Gesture::parse("click+wheel_up").is_none(),
            "two gestures in one binding is nonsense"
        );
        let (g, warn) = Gesture::parse("triple_click").expect("it is recognised");
        assert!(g.is_none(), "but refused rather than bound");
        assert!(
            warn.expect("with a reason").contains("second click"),
            "the reason names the real problem, not a typo"
        );
    }

    #[test]
    fn a_cmd_gesture_warns_because_terminals_never_report_super() {
        use super::Gesture;
        let (g, warn) = Gesture::parse("cmd+click").expect("it still parses");
        assert!(g.is_none(), "and is refused, not bound to a dead entry");
        let warn = warn.expect("but it warns");
        assert!(
            warn.contains("can never fire"),
            "the warning says why, not just that: {warn}"
        );
        // `mod` resolves to Ctrl for mouse on every platform rather than
        // becoming an unfirable Cmd binding.
        let (g, warn) = Gesture::parse("mod+click").expect("mod parses");
        assert_eq!(g.expect("and binds").mods, KeyModifiers::CONTROL);
        assert!(warn.is_none(), "and needs no warning");
    }

    #[test]
    fn a_bare_click_is_refused_in_the_editor_and_terminal() {
        let km = Keymap::from_json(
            r#"[{"key": "click", "command": "save_file"},
                {"key": "click", "command": "save_file", "when": "terminal"},
                {"key": "click", "command": "save_file", "when": "file_tree"}]"#,
        );
        // Editor and terminal refuse; the file tree is fair game, since a
        // click there is not how text gets selected.
        assert_eq!(
            km.warnings().len(),
            2,
            "both reserved contexts warn: {:?}",
            km.warnings()
        );
        assert!(
            km.warnings().iter().all(|w| w.contains("reserved")),
            "and say why: {:?}",
            km.warnings()
        );
        assert!(
            km.command_for_mouse(
                Gesture::parse("click").unwrap().0.unwrap(),
                MouseContext::FileTree
            )
            .is_some(),
            "the file-tree binding survived"
        );
    }

    #[test]
    fn one_gesture_binds_differently_per_context() {
        let km = Keymap::from_json(
            r#"[{"key": "ctrl+click", "command": "save_file", "when": "editor"},
                {"key": "ctrl+click", "command": "quick_open", "when": "file_tree"}]"#,
        );
        let g = Gesture::parse("ctrl+click").unwrap().0.unwrap();
        assert_eq!(
            km.command_for_mouse(g, MouseContext::Editor)
                .map(|c| c.id()),
            Some("save_file")
        );
        assert_eq!(
            km.command_for_mouse(g, MouseContext::FileTree)
                .map(|c| c.id()),
            Some("quick_open"),
            "the same gesture means something else in another region"
        );
        assert!(
            km.command_for_mouse(g, MouseContext::Terminal).is_none(),
            "and nothing at all where it was not bound"
        );
    }

    #[test]
    fn a_bad_row_warns_instead_of_vanishing() {
        let km = Keymap::from_json(
            r#"[{"key": "ctrl+click", "command": "no_such_command"},
                {"key": "ctrl+click", "command": "save_file", "when": "nowhere"},
                {"key": "not-a-thing", "command": "save_file"}]"#,
        );
        assert_eq!(km.warnings().len(), 3, "{:?}", km.warnings());
        assert!(km.warnings()[0].contains("unknown command id"));
        assert!(km.warnings()[1].contains("when"));
        assert!(km.warnings()[2].contains("not a key chord or mouse gesture"));
    }

    #[test]
    fn mouse_rows_do_not_make_the_key_map_look_non_empty() {
        // `is_empty` gates the KEY lookup in handle_key; a mouse-only file
        // must not make it start asking.
        let km = Keymap::from_json(r#"[{"key": "ctrl+click", "command": "save_file"}]"#);
        assert!(km.is_empty(), "no key chords bound");
        assert!(km.has_mouse_bindings(), "but a gesture is");
    }
    use super::*;

    fn ev(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn parses_modifier_stack_and_letter() {
        let c = Chord::parse("ctrl+shift+p").unwrap();
        assert_eq!(
            c,
            Chord::from_event(ev(
                KeyCode::Char('p'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            ))
        );
    }

    #[test]
    fn uppercase_letter_event_folds_to_shift_so_it_matches_a_shift_binding() {
        // Terminals deliver Cmd+Shift+P as an uppercase char; the binding is
        // written lowercase with an explicit shift. They must still match.
        let bound = Chord::parse("cmd+shift+p").unwrap();
        let live = Chord::from_event(ev(KeyCode::Char('P'), KeyModifiers::SUPER));
        assert_eq!(bound, live);
    }

    #[test]
    fn function_key_and_named_keys_parse() {
        assert_eq!(
            Chord::parse("f2"),
            Some(Chord::normalize(KeyCode::F(2), KeyModifiers::empty()))
        );
        assert_eq!(
            Chord::parse("cmd+enter").unwrap(),
            Chord::from_event(ev(KeyCode::Enter, KeyModifiers::SUPER))
        );
    }

    #[test]
    fn literal_plus_key_parses() {
        assert_eq!(
            Chord::parse("ctrl++").unwrap(),
            Chord::from_event(ev(KeyCode::Char('+'), KeyModifiers::CONTROL))
        );
    }

    #[test]
    fn mod_resolves_per_platform() {
        let c = Chord::parse("mod+s").unwrap();
        let expected = if cfg!(target_os = "macos") {
            KeyModifiers::SUPER
        } else {
            KeyModifiers::CONTROL
        };
        assert_eq!(c, Chord::from_event(ev(KeyCode::Char('s'), expected)));
    }

    #[test]
    fn iterm2_forwarder_reproduces_the_hand_written_consts() {
        // These are copied from src/iterm2.rs; the generated forwarder for a
        // user binding of the same chord must match byte-for-byte, or iTerm2
        // would install a subtly different (broken) entry.
        let (k, h) = Chord::parse("cmd+f").unwrap().iterm2_forwarder().unwrap();
        assert_eq!(k, "0x66-0x100000-0x3");
        assert_eq!(h, "0x1b 0x5b 0x31 0x30 0x32 0x3b 0x39 0x75");

        let (k, h) = Chord::parse("cmd+shift+p")
            .unwrap()
            .iterm2_forwarder()
            .unwrap();
        assert_eq!(k, "0x50-0x120000-0x23");
        assert_eq!(h, "0x1b 0x5b 0x38 0x30 0x3b 0x31 0x30 0x75");

        let (k, h) = Chord::parse("cmd+.").unwrap().iterm2_forwarder().unwrap();
        assert_eq!(k, "0x2e-0x100000-0x2f");
        assert_eq!(h, "0x1b 0x5b 0x34 0x36 0x3b 0x39 0x75");

        // Matches the hand-written CMD_SHIFT_Z consts in iterm2.rs (editor redo).
        let (k, h) = Chord::parse("cmd+shift+z")
            .unwrap()
            .iterm2_forwarder()
            .unwrap();
        assert_eq!(k, "0x5a-0x120000-0x6");
        assert_eq!(h, "0x1b 0x5b 0x39 0x30 0x3b 0x31 0x30 0x75");

        let (k, _) = Chord::parse("cmd+0").unwrap().iterm2_forwarder().unwrap();
        assert_eq!(k, "0x30-0x100000-0x1d");
    }

    #[test]
    fn iterm2_forwarder_skips_chords_that_need_no_forwarding_or_cant_encode() {
        // No Super: the terminal already delivers Ctrl/Alt/function chords.
        assert_eq!(Chord::parse("ctrl+alt+g").unwrap().iterm2_forwarder(), None);
        assert_eq!(Chord::parse("f2").unwrap().iterm2_forwarder(), None);
        // Shifted punctuation is layout-dependent, so it's skipped.
        assert_eq!(
            Chord::parse("cmd+shift+/").unwrap().iterm2_forwarder(),
            None
        );
        // A key not on the ANSI table has no virtual keycode.
        assert_eq!(Chord::parse("cmd+enter").unwrap().iterm2_forwarder(), None);
    }

    #[test]
    fn junk_tokens_yield_none() {
        assert_eq!(Chord::parse("ctrl+notakey"), None);
        assert_eq!(Chord::parse(""), None);
    }

    #[test]
    fn keymap_resolves_command_ids_and_skips_bad_rows() {
        let json = r#"[
            { "key": "cmd+,", "command": "open_settings" },
            { "key": "ctrl+shift+p", "command": "quick_open" },
            { "key": "garbage++key", "command": "save_file" },
            { "key": "cmd+j", "command": "no_such_command" }
        ]"#;
        let km = Keymap::from_json(json);
        assert_eq!(
            km.command_for(ev(KeyCode::Char(','), KeyModifiers::SUPER)),
            Some(Command::OpenSettings)
        );
        assert_eq!(
            km.command_for(ev(
                KeyCode::Char('p'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            )),
            Some(Command::QuickOpen)
        );
        // Unknown command id is dropped, not fatal.
        assert_eq!(
            km.command_for(ev(KeyCode::Char('j'), KeyModifiers::SUPER)),
            None
        );
    }

    #[test]
    fn strips_line_comments_but_keeps_slashes_in_strings() {
        let stripped =
            strip_line_comments("// lead\n[{ \"key\": \"a//b\", \"command\": \"x\" }] // trailing");
        assert!(!stripped.contains("lead"));
        assert!(!stripped.contains("trailing"));
        assert!(
            stripped.contains("a//b"),
            "// inside a quoted value must survive"
        );
    }

    #[test]
    fn empty_or_missing_json_is_empty_keymap() {
        assert!(Keymap::from_json("").is_empty());
        assert!(Keymap::from_json("not json").is_empty());
        assert!(Keymap::load(Path::new("/no/such/keybindings.json")).is_empty());
    }

    #[test]
    fn template_parses_as_a_valid_keymap() {
        // The seeded starter must itself resolve, or first-run users hit a
        // dead example. Strip the // comments the way the loader tolerates.
        let km = Keymap::from_json(TEMPLATE);
        assert_eq!(
            km.command_for(ev(
                KeyCode::Char(','),
                if cfg!(target_os = "macos") {
                    KeyModifiers::SUPER
                } else {
                    KeyModifiers::CONTROL
                }
            )),
            Some(Command::OpenSettings)
        );
    }
}
