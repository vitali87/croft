//! Keyboard Shortcuts editor (#612): every palette command with its
//! built-in chord and the user's `keybindings.json` bindings, searchable,
//! with conflicts flagged and a rebind written back into the file.
//!
//! Pure: the caller reads and writes `keybindings.json`. The file is JSON
//! with `//` comments; a rebind edits it as text so the comments survive.

use crate::keymap::Chord;
use crate::widgets::command_palette::{ALL_COMMANDS, Command};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// One command as the editor lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub command: Command,
    pub title: &'static str,
    pub id: &'static str,
    /// The built-in chord, as the palette shows it; empty when none.
    pub default: &'static str,
    /// The chords `keybindings.json` binds to this command.
    pub user: Vec<String>,
}

/// The chord string for a key press, as `keybindings.json` spells it
/// ("ctrl+shift+k", "cmd+f2"). `None` for a press the keymap never consults:
/// no real modifier and not a function key.
pub fn chord_string(key: KeyEvent) -> Option<String> {
    let mut mods = key.modifiers;
    let base = match key.code {
        KeyCode::Char(' ') => String::from("space"),
        KeyCode::Char(c) if c.is_ascii_uppercase() => {
            mods |= KeyModifiers::SHIFT;
            c.to_ascii_lowercase().to_string()
        }
        KeyCode::Char(c) => c.to_lowercase().to_string(),
        KeyCode::F(n) => format!("f{n}"),
        KeyCode::Enter => String::from("enter"),
        KeyCode::Tab | KeyCode::BackTab => String::from("tab"),
        KeyCode::Esc => String::from("esc"),
        KeyCode::Up => String::from("up"),
        KeyCode::Down => String::from("down"),
        KeyCode::Left => String::from("left"),
        KeyCode::Right => String::from("right"),
        KeyCode::Backspace => String::from("backspace"),
        KeyCode::Delete => String::from("delete"),
        KeyCode::Home => String::from("home"),
        KeyCode::End => String::from("end"),
        KeyCode::PageUp => String::from("pageup"),
        KeyCode::PageDown => String::from("pagedown"),
        KeyCode::Insert => String::from("insert"),
        _ => return None,
    };
    let real = mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER);
    if !real && !matches!(key.code, KeyCode::F(_)) {
        return None;
    }
    let mut parts = Vec::new();
    for (m, name) in [
        (KeyModifiers::CONTROL, "ctrl"),
        (KeyModifiers::ALT, "alt"),
        (KeyModifiers::SUPER, "cmd"),
        (KeyModifiers::SHIFT, "shift"),
    ] {
        if mods.contains(m) {
            parts.push(name.to_string());
        }
    }
    parts.push(base);
    Some(parts.join("+"))
}

/// The `(key, command)` rows of a `keybindings.json` text; unparsable text
/// reads as none.
pub fn user_bindings(json: &str) -> Vec<(String, String)> {
    let Ok(serde_json::Value::Array(rows)) =
        serde_json::from_str::<serde_json::Value>(&crate::keymap::strip_line_comments(json))
    else {
        return Vec::new();
    };
    rows.iter()
        .filter_map(|r| {
            Some((
                r.get("key")?.as_str()?.to_string(),
                r.get("command")?.as_str()?.to_string(),
            ))
        })
        .collect()
}

/// Every palette command, in palette order, with its user bindings.
pub fn rows(json: &str) -> Vec<Row> {
    let user = user_bindings(json);
    ALL_COMMANDS
        .iter()
        .map(|&c| Row {
            command: c,
            title: c.title(),
            id: c.id(),
            default: c.keybinding_hint(),
            user: user
                .iter()
                .filter(|(_, cmd)| cmd == c.id())
                .map(|(k, _)| k.clone())
                .collect(),
        })
        .collect()
}

/// Whether `row` matches every word of `query` in its title, id or keys,
/// ignoring case.
pub fn matches(row: &Row, query: &str) -> bool {
    let hay = format!(
        "{} {} {} {}",
        row.title,
        row.id,
        row.default,
        row.user.join(" ")
    )
    .to_lowercase();
    query
        .split_whitespace()
        .all(|w| hay.contains(&w.to_lowercase()))
}

/// The other commands `chord` already triggers: user bindings, and built-in
/// chords no user binding has taken over.
pub fn conflicts(json: &str, chord: &str, except: Command) -> Vec<Command> {
    let Some(target) = Chord::parse(chord) else {
        return Vec::new();
    };
    let user = user_bindings(json);
    let mut out = Vec::new();
    let mut claimed = false;
    for (k, c) in &user {
        if Chord::parse(k) == Some(target) {
            claimed = true;
            if let Some(cmd) = Command::from_id(c)
                && cmd != except
                && !out.contains(&cmd)
            {
                out.push(cmd);
            }
        }
    }
    if !claimed {
        for &c in ALL_COMMANDS {
            if c != except && Chord::parse(c.keybinding_hint()) == Some(target) && !out.contains(&c)
            {
                out.push(c);
            }
        }
    }
    out
}

/// Whether `line` is a single-line row binding command `id`.
fn is_row_for(line: &str, id: &str) -> bool {
    let t = line.trim();
    let t = t.strip_suffix(',').unwrap_or(t).trim_end();
    if !(t.starts_with('{') && t.ends_with('}')) {
        return false;
    }
    serde_json::from_str::<serde_json::Value>(t)
        .ok()
        .and_then(|v| v.get("command")?.as_str().map(|c| c == id))
        .unwrap_or(false)
}

/// `json` with `id`'s single-line user bindings removed and one binding of
/// `chord` to `id` added at the end of the array; comments and every other
/// row stay as written. An error when the text has no array to add to.
pub fn rebind(json: &str, id: &str, chord: &str) -> Result<String, String> {
    let row = format!(
        "{{ \"key\": {}, \"command\": {} }}",
        serde_json::Value::from(chord),
        serde_json::Value::from(id)
    );
    if json.trim().is_empty() {
        return Ok(format!("[\n  {row}\n]\n"));
    }
    let mut kept: String = json
        .split_inclusive('\n')
        .filter(|l| !is_row_for(l, id))
        .collect();
    // The array's closing bracket: the last `]` outside a comment line.
    let mut close = None;
    let mut offset = 0;
    for line in kept.split_inclusive('\n') {
        if !line.trim_start().starts_with("//")
            && let Some(i) = line.rfind(']')
        {
            close = Some(offset + i);
        }
        offset += line.len();
    }
    let close = close.ok_or_else(|| String::from("keybindings.json has no [ ... ] array"))?;
    let at = kept[..close].trim_end().len();
    let last = kept[..at].chars().last();
    let insert = match last {
        Some('[') | Some(',') => format!("\n  {row}"),
        Some(_) => format!(",\n  {row}"),
        None => return Err(String::from("keybindings.json has no [ ... ] array")),
    };
    kept.insert_str(at, &insert);
    // "[]" had nothing between the brackets: put the bracket on its own line.
    if last == Some('[') && !kept[at + insert.len()..].starts_with('\n') {
        kept.insert(at + insert.len(), '\n');
    }
    Ok(kept)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn a_press_spells_its_chord_and_plain_typing_is_not_one() {
        assert_eq!(
            chord_string(press(
                KeyCode::Char('k'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            ))
            .as_deref(),
            Some("ctrl+shift+k")
        );
        assert_eq!(
            chord_string(press(KeyCode::Char('K'), KeyModifiers::SUPER)).as_deref(),
            Some("cmd+shift+k"),
            "an upper-case letter is its shift chord"
        );
        assert_eq!(
            chord_string(press(KeyCode::F(2), KeyModifiers::NONE)).as_deref(),
            Some("f2")
        );
        assert_eq!(
            chord_string(press(KeyCode::Enter, KeyModifiers::ALT)).as_deref(),
            Some("alt+enter")
        );
        assert_eq!(
            chord_string(press(KeyCode::Char('a'), KeyModifiers::NONE)),
            None
        );
        assert_eq!(
            chord_string(press(KeyCode::Char('A'), KeyModifiers::SHIFT)),
            None
        );
        // Every spelling parses back to the chord pressed.
        let k = press(
            KeyCode::Char('p'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        );
        assert_eq!(
            Chord::parse(&chord_string(k).unwrap()),
            Some(Chord::from_event(k))
        );
    }

    const USER: &str = "// mine\n[\n  { \"key\": \"ctrl+alt+o\", \"command\": \"open_settings\" },\n  { \"key\": \"f9\", \"command\": \"quick_open\" }\n]\n";

    #[test]
    fn rows_list_every_command_with_its_default_and_user_keys() {
        assert_eq!(
            user_bindings(USER),
            vec![
                ("ctrl+alt+o".to_string(), "open_settings".to_string()),
                ("f9".to_string(), "quick_open".to_string())
            ]
        );
        assert!(user_bindings("not json").is_empty());
        let all = rows(USER);
        assert_eq!(all.len(), ALL_COMMANDS.len());
        let quick = all
            .iter()
            .find(|r| r.command == Command::QuickOpen)
            .unwrap();
        assert_eq!(quick.default, "Cmd+P");
        assert_eq!(quick.user, vec!["f9"]);
        assert_eq!(quick.id, "quick_open");
    }

    #[test]
    fn search_matches_every_word_across_title_id_and_keys() {
        let all = rows(USER);
        let quick = all
            .iter()
            .find(|r| r.command == Command::QuickOpen)
            .unwrap();
        assert!(matches(quick, ""));
        assert!(matches(quick, "go FILE"));
        assert!(matches(quick, "quick_open"));
        assert!(matches(quick, "f9"));
        assert!(matches(quick, "cmd+p"));
        assert!(!matches(quick, "go settings"));
    }

    #[test]
    fn conflicts_name_user_and_builtin_owners_of_a_chord() {
        assert_eq!(
            conflicts(USER, "F9", Command::OpenSettings),
            vec![Command::QuickOpen]
        );
        assert!(
            conflicts(USER, "f9", Command::QuickOpen).is_empty(),
            "rebinding to itself"
        );
        // Cmd+P is Go to File's built-in chord.
        assert_eq!(
            conflicts("[]", "cmd+p", Command::OpenSettings),
            vec![Command::QuickOpen]
        );
        assert!(conflicts("[]", "ctrl+alt+shift+f12", Command::OpenSettings).is_empty());
    }

    #[test]
    fn rebind_replaces_the_commands_rows_and_keeps_comments() {
        let out = rebind(USER, "open_settings", "cmd+shift+,").unwrap();
        assert_eq!(
            out,
            "// mine\n[\n  { \"key\": \"f9\", \"command\": \"quick_open\" },\n  { \"key\": \"cmd+shift+,\", \"command\": \"open_settings\" }\n]\n"
        );
        assert_eq!(
            user_bindings(&out),
            vec![
                ("f9".to_string(), "quick_open".to_string()),
                ("cmd+shift+,".to_string(), "open_settings".to_string())
            ]
        );
    }

    #[test]
    fn rebind_handles_the_last_row_an_empty_array_and_an_empty_file() {
        let out = rebind(USER, "quick_open", "ctrl+p").unwrap();
        assert_eq!(user_bindings(&out).len(), 2, "{out}");
        assert!(
            out.contains("{ \"key\": \"ctrl+p\", \"command\": \"quick_open\" }"),
            "{out}"
        );
        assert!(!out.contains("\"f9\""), "{out}");
        let empty = rebind("[]", "quick_open", "ctrl+p").unwrap();
        assert_eq!(
            user_bindings(&empty),
            vec![("ctrl+p".to_string(), "quick_open".to_string())]
        );
        let blank = rebind("  \n", "quick_open", "ctrl+p").unwrap();
        assert_eq!(
            user_bindings(&blank),
            vec![("ctrl+p".to_string(), "quick_open".to_string())]
        );
        assert!(rebind("// no array here", "quick_open", "ctrl+p").is_err());
        // The template's example row is for open_settings; rebinding another
        // command leaves it.
        let t = rebind(crate::keymap::TEMPLATE, "quick_open", "ctrl+p").unwrap();
        assert_eq!(user_bindings(&t).len(), 2, "{t}");
        assert!(t.starts_with("// croft keyboard shortcuts."), "{t}");
    }
}
