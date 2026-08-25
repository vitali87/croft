//! Keyboard macro record/replay (#255): vim's `q`/`@` registers, plus
//! palette commands for users who never touch modal editing.
//!
//! **What a step is.** croft dispatches a keystroke three ways — a palette
//! selection and a user-rebound chord both resolve to a
//! [`Command`](crate::widgets::command_palette::Command), but the built-in
//! default chords (`Cmd+S`, `Alt+Up`, the debugger keys) call their app
//! methods straight from `handle_key` and never construct one. Recording at
//! the command funnel would therefore capture palette work and silently drop
//! most editing, so a step is a KEY, captured at the single keyboard entry
//! point, with consecutive plain typing collapsed into one [`Step::Text`].
//!
//! Replay pushes those keys back through the same entry point, so a macro
//! behaves exactly as the keystrokes did — including auto-close pairs,
//! completion popups, and vim's own state machine, none of which a
//! command-level replay reproduces faithfully.
//!
//! **What it deliberately does not record.** Recording control itself (so a
//! macro cannot contain its own `q`), and anything typed while the focus is
//! not the editor — a macro that replayed terminal input or window management
//! would do something different every time it ran.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// One recorded step. Typing collapses into `Text` runs so a replayed burst
/// is one undo step (consecutive `InsertChar` edits coalesce), while every
/// other key is kept verbatim as the chord that produced it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind")]
pub enum Step {
    /// A run of plain characters typed with no modifiers.
    Text { text: String },
    /// Any other key: a named code plus its modifier bits.
    Key { code: String, mods: u8 },
    /// A step kind this build does not know, from a macros.json written by a
    /// newer croft. Without a catch-all, serde fails the WHOLE file on one
    /// unknown tag, so a single future step would make every register
    /// unloadable and permanently block saves — the store refuses to
    /// overwrite what it cannot parse.
    ///
    /// The payload is kept verbatim rather than discarded, because
    /// `update_json_store` re-serialises the ENTIRE map on every save: a
    /// fieldless catch-all would rewrite a newer croft's steps as
    /// `{"kind":"Unknown"}` the first time any unrelated register was
    /// stored, silently destroying them. Replays as nothing.
    #[serde(untagged)]
    Unknown(serde_json::Value),
}

impl Step {
    /// The key a `Key` step replays, or `None` when the recorded name is not
    /// one this build knows — a macros.json written by a newer croft must
    /// skip the step rather than abort the whole macro.
    fn to_key_event(&self) -> Option<KeyEvent> {
        let Step::Key { code, mods } = self else {
            return None;
        };
        let code = parse_code(code)?;
        Some(KeyEvent::new(code, KeyModifiers::from_bits_truncate(*mods)))
    }
}

/// A named recording. `steps` is replayed in order; an empty macro is legal
/// (recording started and stopped with nothing between) and replays as a
/// no-op rather than an error.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Macro {
    pub steps: Vec<Step>,
}

impl Macro {
    /// Append a key, folding plain typing into the trailing `Text` run.
    /// `Char` with no modifiers is typing; everything else is a chord.
    pub fn push_key(&mut self, key: KeyEvent) {
        let plain = matches!(key.code, KeyCode::Char(_))
            && !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER);
        if let (KeyCode::Char(c), true) = (key.code, plain) {
            if let Some(Step::Text { text }) = self.steps.last_mut() {
                text.push(c);
                return;
            }
            self.steps.push(Step::Text {
                text: c.to_string(),
            });
            return;
        }
        if let Some(code) = code_name(key.code) {
            self.steps.push(Step::Key {
                code,
                mods: key.modifiers.bits(),
            });
        }
    }

    /// The key events this macro replays, in order. Steps naming a key this
    /// build does not know are skipped, not fatal.
    pub fn key_events(&self) -> Vec<KeyEvent> {
        let mut out = Vec::new();
        for step in &self.steps {
            match step {
                Step::Text { text } => {
                    for c in text.chars() {
                        out.push(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
                    }
                }
                Step::Key { .. } => {
                    if let Some(k) = step.to_key_event() {
                        out.push(k);
                    }
                }
                Step::Unknown(_) => {}
            }
        }
        out
    }

    /// Total keys a replay will feed, for the step budget.
    pub fn len(&self) -> usize {
        self.steps
            .iter()
            .map(|s| match s {
                Step::Text { text } => text.chars().count(),
                Step::Key { .. } => 1,
                Step::Unknown(_) => 0,
            })
            .sum()
    }

    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }
}

/// Serialise a key code to the name stored on disk. Returns `None` for codes
/// with no stable spelling, which are dropped at record time rather than
/// written as something a later croft would misread.
fn code_name(code: KeyCode) -> Option<String> {
    Some(match code {
        KeyCode::Char(c) => format!("char:{c}"),
        KeyCode::F(n) => format!("f{n}"),
        KeyCode::Backspace => "backspace".into(),
        KeyCode::Enter => "enter".into(),
        KeyCode::Left => "left".into(),
        KeyCode::Right => "right".into(),
        KeyCode::Up => "up".into(),
        KeyCode::Down => "down".into(),
        KeyCode::Home => "home".into(),
        KeyCode::End => "end".into(),
        KeyCode::PageUp => "pageup".into(),
        KeyCode::PageDown => "pagedown".into(),
        KeyCode::Tab => "tab".into(),
        KeyCode::BackTab => "backtab".into(),
        KeyCode::Delete => "delete".into(),
        KeyCode::Insert => "insert".into(),
        KeyCode::Esc => "esc".into(),
        _ => return None,
    })
}

/// The inverse of [`code_name`].
fn parse_code(name: &str) -> Option<KeyCode> {
    if let Some(rest) = name.strip_prefix("char:") {
        let mut it = rest.chars();
        let c = it.next()?;
        if it.next().is_some() {
            return None;
        }
        return Some(KeyCode::Char(c));
    }
    if let Some(n) = name.strip_prefix('f')
        && let Ok(n) = n.parse::<u8>()
    {
        return Some(KeyCode::F(n));
    }
    Some(match name {
        "backspace" => KeyCode::Backspace,
        "enter" => KeyCode::Enter,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        "tab" => KeyCode::Tab,
        "backtab" => KeyCode::BackTab,
        "delete" => KeyCode::Delete,
        "insert" => KeyCode::Insert,
        "esc" => KeyCode::Esc,
        _ => return None,
    })
}

/// Live recording state. `None` on `App` means "not recording"; the register
/// is `None` for the palette recorder and `Some(c)` for vim's `q{a-z}`.
#[derive(Clone, Debug)]
pub struct Recording {
    pub register: Option<char>,
    pub recorded: Macro,
}

/// The store: `~/.config/croft/macros.json`, a register name → macro map,
/// which is exactly the shape [`crate::workspace::update_json_store`] takes.
pub fn macros_path() -> PathBuf {
    crate::prefs::config_dir().join("macros.json")
}

/// Load for READING: any failure reads empty, since a read-only consumer can
/// do nothing better with a corrupt store.
pub fn load(path: &Path) -> HashMap<String, Macro> {
    load_checked(path).unwrap_or_default()
}

/// Load for WRITING: a missing store is `Ok(empty)`, but an unreadable or
/// unparsable one is an error — treating a corrupt store as empty would let
/// one save wipe every other register (the `workspace.rs` precedent).
fn load_checked(path: &Path) -> Result<HashMap<String, Macro>, String> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Default::default()),
        Err(e) => return Err(format!("read {}: {e}", path.display())),
    };
    serde_json::from_str(&raw).map_err(|e| format!("parse {}: {e}", path.display()))
}

/// Store one register, atomically and under the shared store lock. An empty
/// macro removes the key rather than persisting a no-op.
pub fn save_register(path: &Path, name: &str, mac: Macro) -> Result<(), String> {
    crate::workspace::update_json_store::<Macro, _>(path, |map| {
        if mac.is_empty() {
            map.remove(name);
        } else {
            map.insert(name.to_string(), mac);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    #[test]
    fn typing_collapses_into_one_text_run_and_chords_stay_separate() {
        let mut m = Macro::default();
        for c in "abc".chars() {
            m.push_key(ch(c));
        }
        m.push_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        for c in "de".chars() {
            m.push_key(ch(c));
        }
        assert_eq!(
            m.steps,
            vec![
                Step::Text { text: "abc".into() },
                Step::Key {
                    code: "down".into(),
                    mods: 0
                },
                Step::Text { text: "de".into() },
            ],
            "a typing burst is one step so its replay coalesces to one undo"
        );
        assert_eq!(m.len(), 6, "the budget counts keys, not steps");
    }

    #[test]
    fn a_modified_char_is_a_chord_not_typing() {
        let mut m = Macro::default();
        m.push_key(ch('a'));
        m.push_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        m.push_key(ch('b'));
        assert_eq!(
            m.steps,
            vec![
                Step::Text { text: "a".into() },
                Step::Key {
                    code: "char:s".into(),
                    mods: KeyModifiers::CONTROL.bits()
                },
                Step::Text { text: "b".into() },
            ],
            "Ctrl+S must not fold into the surrounding typing"
        );
    }

    #[test]
    fn a_macro_round_trips_through_its_key_events() {
        let mut m = Macro::default();
        for c in "hi".chars() {
            m.push_key(ch(c));
        }
        m.push_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        m.push_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT));
        let keys = m.key_events();
        assert_eq!(keys.len(), 4, "a Text run expands back to one key per char");
        assert_eq!(keys[0], ch('h'));
        assert_eq!(keys[2], KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(
            keys[3],
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT)
        );
    }

    #[test]
    fn an_unknown_step_is_skipped_not_fatal() {
        // A macros.json written by a newer croft naming a key this build has
        // no spelling for must lose that step, not the whole macro.
        let m = Macro {
            steps: vec![
                Step::Text { text: "a".into() },
                Step::Key {
                    code: "keypad_frobnicate".into(),
                    mods: 0,
                },
                Step::Text { text: "b".into() },
            ],
        };
        let keys = m.key_events();
        assert_eq!(keys, vec![ch('a'), ch('b')], "the known steps still run");
    }

    #[test]
    fn a_register_round_trips_through_the_store() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("macros.json");
        let mut m = Macro::default();
        for c in "xy".chars() {
            m.push_key(ch(c));
        }
        save_register(&path, "a", m.clone()).unwrap();
        assert_eq!(load(&path).get("a"), Some(&m));

        // An empty macro prunes its key rather than persisting a no-op.
        save_register(&path, "a", Macro::default()).unwrap();
        assert!(!load(&path).contains_key("a"), "the register was removed");
    }

    #[test]
    fn a_corrupt_store_is_refused_not_silently_replaced() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("macros.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert!(
            save_register(&path, "a", Macro::default()).is_err(),
            "a save must not treat an unparsable store as empty and wipe it"
        );
        assert!(load(&path).is_empty(), "but a read degrades to empty");
    }
}
