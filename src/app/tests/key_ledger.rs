//! The key ledger (#2062): what every shortcut croft advertises does in each
//! main context of the key router. The router decides between modals, focus
//! targets and the keymap over thousands of lines; `golden/key-ledger.txt`
//! holds its answer for every advertised chord, so a change to which handler
//! answers a key fails here, listing the lines that moved.
//!
//! The chords come from the data that advertises them, spelled for Linux: the
//! F1 sheet, the menus' accelerators and the command palette's hints. A hint
//! that names no keys (a click, a count) is listed at the top with the
//! reason, and a menu shortcut the walked menus miss fails a test of its own.
//! What a chord changed is one line, `context | chord | effects`.
//!
//! Each context is a test of its own, so the harness spreads them over its
//! threads. A context's app is built once and kept for the next chord while
//! the last one changed nothing it shows; after a chord that changed
//! something the next one starts from a new app. `CROFT_LEDGER_FRESH=1`
//! builds a new app for every chord instead: slower, and the ledger must not
//! move, which is how the keeping is checked. `CROFT_BLESS=1` rewrites the
//! ledger after a change that is meant to move a chord.
//!
//! One chord reaches outside the test: re-rooting the Explorer
//! (`ctrl+shift+/`) writes the window title straight to the terminal, so the
//! test's output shows an `ESC]0;` sequence.
//!
//! Linux only: macOS reads `Cmd` and `Ctrl` differently, so its ledger would
//! be another file.

use super::*;
use crate::keymap::Chord;
use std::collections::{BTreeMap, BTreeSet};

/// The event a terminal sends for `chord`: the one `Chord` itself reads as
/// `chord`, so no key name is parsed here a second time. A shifted letter
/// comes as its capital and Shift+Tab as BackTab, as crossterm reports them.
fn event_for(chord: Chord) -> Option<KeyEvent> {
    use KeyCode::{BackTab, Char, Tab};
    let named = [
        KeyCode::Enter,
        Tab,
        KeyCode::Esc,
        KeyCode::Backspace,
        KeyCode::Delete,
        KeyCode::Insert,
        KeyCode::Home,
        KeyCode::End,
        KeyCode::PageUp,
        KeyCode::PageDown,
        KeyCode::Up,
        KeyCode::Down,
        KeyCode::Left,
        KeyCode::Right,
    ];
    let codes = (' '..='~')
        .filter(|c| !c.is_ascii_uppercase())
        .map(Char)
        .chain(named)
        .chain((1..=24).map(KeyCode::F));
    let mods = [
        KeyModifiers::CONTROL,
        KeyModifiers::ALT,
        KeyModifiers::SHIFT,
        KeyModifiers::SUPER,
    ];
    for code in codes {
        for bits in 0..16 {
            let held = (0..4)
                .filter(|i| bits & (1 << i) != 0)
                .fold(KeyModifiers::NONE, |m, i| m | mods[i]);
            if Chord::from_event(key(code, held)) != chord {
                continue;
            }
            let shift = held.contains(KeyModifiers::SHIFT);
            return Some(match code {
                Char(c) if shift => key(Char(c.to_ascii_uppercase()), held),
                Tab if shift => key(BackTab, held),
                _ => key(code, held),
            });
        }
    }
    None
}

/// The keys of a hint spelled for Linux (`Ctrl+K Shift+P`), a step a word,
/// each read by the keymap's own hint reader.
fn keys_of(hint: &str) -> Result<Vec<KeyEvent>, String> {
    hint.split(' ')
        .map(|step| {
            crate::keymap::hint_chord(step)
                .and_then(event_for)
                .ok_or_else(|| format!("`{step}` names no key"))
        })
        .collect()
}

/// A hint written for people, as the hints of the key sequences it names, in
/// the palette's words: `Cmd/Ctrl+C / X / V` is three chords, `Cmd/Ctrl+R or
/// F2` two, `Cmd/Ctrl+A then Cmd/Ctrl+S` one sequence, `F7 / Shift+F7 in a
/// diff` two (where they apply is the context's business). `Cmd/Ctrl` reads
/// both ways, since Linux spells some `Cmd` chords with `Super`. A `Cmd`
/// chord listed beside a `Ctrl` one (`Ctrl+Shift+C / Cmd+C`) is the Mac's
/// key, so it reads `Super`: the hint already names the `Ctrl` chord, and
/// LINUX.md gives such a `Cmd` chord no `Ctrl` form.
fn alternatives(hint: &str) -> Vec<String> {
    let hint = hint.split(" in ").next().unwrap_or(hint);
    let hint = hint.split(" (").next().unwrap_or(hint);
    let hint = hint.replace(" then ", " ").replace(" + ", " ");
    let hint = if hint.contains("Ctrl") && !hint.contains("Cmd/Ctrl") {
        hint.replace("Cmd", "Super")
    } else {
        hint
    };
    let arrows = [("Left", "←"), ("Right", "→"), ("Up", "↑"), ("Down", "↓")];
    let mut out = Vec::new();
    // `Cmd/Ctrl+C / X / V`: a bare key after a `/` takes the modifiers
    // before it, one after an `or` (`Cmd/Ctrl+R or F2`) does not.
    let listed = hint.split(" or ").flat_map(|either| {
        either
            .split(" / ")
            .enumerate()
            .map(|(i, alt)| (i == 0, alt))
    });
    let mut prefix = String::new();
    for (first, alt) in listed {
        if first {
            prefix.clear();
        }
        let alt = if alt.contains(['+', ' ']) {
            alt.to_string()
        } else {
            format!("{prefix}{alt}")
        };
        prefix = alt
            .rfind('+')
            .map_or(String::new(), |i| alt[..=i].to_string());
        // `Shift+PgUp/PgDn`, `Cmd/Ctrl+1..9` and `Shift+arrows` name a key each.
        let (mods, last) = alt
            .rsplit_once('+')
            .map_or(("", alt.as_str()), |(m, k)| (m, k));
        let range = last
            .split_once("..")
            .and_then(|(a, b)| Some((a.parse::<u8>().ok()?, b.parse::<u8>().ok()?)));
        let lasts: Vec<String> = if last.eq_ignore_ascii_case("arrows") {
            arrows.map(|(word, _)| word.to_string()).to_vec()
        } else if let Some((first, end)) = range {
            (first..=end).map(|n| n.to_string()).collect()
        } else if last.len() > 1 && last.contains('/') && !alt.contains(' ') {
            last.split('/').map(str::to_string).collect()
        } else {
            vec![last.to_string()]
        };
        for last in lasts {
            let one = if mods.is_empty() {
                last
            } else {
                format!("{mods}+{last}")
            };
            let mut spellings = vec![one];
            for (either, as_) in [("Cmd/Ctrl", ["Cmd", "Ctrl"]), ("Ctrl/Alt", ["Ctrl", "Alt"])] {
                spellings = spellings
                    .into_iter()
                    .flat_map(|s| {
                        if s.contains(either) {
                            as_.iter().map(|m| s.replace(either, m)).collect()
                        } else {
                            vec![s]
                        }
                    })
                    .collect();
            }
            for s in spellings {
                // The palette names arrows with glyphs, and only so does
                // `hint_for_platform` know `Cmd+Opt+←` is `Super` on Linux.
                let s = s
                    .split(' ')
                    .map(|step| {
                        step.split('+')
                            .map(|t| arrows.iter().find(|(w, _)| *w == t).map_or(t, |(_, g)| g))
                            .collect::<Vec<_>>()
                            .join("+")
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                out.push(s);
            }
        }
    }
    out
}

/// F1 rows that describe a pattern: `N` is a count in them, not the N key.
const COUNTED: &[&str] = &[
    "Cmd+g N g",
    "Cmd+N Cmd+g g",
    "Cmd+N Cmd+d d",
    "Cmd+N Cmd+y y",
];

/// The keys Linux presses for each key sequence `hint` names, or why one
/// names none.
fn linux_keys(hint: &str) -> Vec<Result<Vec<KeyEvent>, String>> {
    use crate::widgets::command_palette::hint_for_platform;
    alternatives(hint)
        .iter()
        .map(|one| keys_of(&hint_for_platform(one, false)))
        .collect()
}

/// Why the ledger does not press an F1 row of `group`, when it does not.
fn unpressed_f1_row(group: &str, hint: &str) -> Option<&'static str> {
    if group.starts_with("Editor: vim mode") {
        Some("vim mode's keys (a capital means Shift there); the ledger has no vim context")
    } else if hint.contains("lick") || hint.contains("Drag") {
        Some("a mouse gesture")
    } else if matches!(hint, "Type" | "Any key") {
        Some("typing, not a chord")
    } else if COUNTED.contains(&hint) {
        Some("N stands for a count")
    } else {
        None
    }
}

/// The actions of the menu rows the ledger walks, over a `main.rs` that exists.
fn walked_menu_actions() -> Vec<MenuAction> {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("main.rs");
    std::fs::write(&file, "").unwrap();
    let app = App::new(tmp.path().to_path_buf()).unwrap();
    menu_row_actions(&app, &file)
}

/// Every key sequence croft advertises, by its spelling, and every hint that
/// names none: where it is advertised, the hint and why.
type Advertised = (BTreeMap<String, Vec<KeyEvent>>, Vec<String>);

/// The [`Advertised`] chords and hints, read from where croft advertises them.
fn advertised() -> Advertised {
    // Where, the hint as written, and the keys of each sequence it names.
    type Keys = Vec<Result<Vec<KeyEvent>, String>>;
    let mut hints: Vec<(String, String, Keys)> = Vec::new();
    for group in crate::widgets::shortcuts::SHORTCUT_GROUPS {
        for entry in group.entries {
            let keys = match unpressed_f1_row(group.title, entry.keys) {
                Some(why) => vec![Err(String::from(why))],
                None => linux_keys(entry.keys),
            };
            hints.push((format!("F1 {}", group.title), entry.keys.into(), keys));
        }
    }
    for cmd in crate::widgets::command_palette::ALL_COMMANDS {
        let hint = cmd.keybinding_hint();
        if !hint.is_empty() {
            let keys = linux_keys(hint);
            hints.push((format!("palette {}", cmd.title()), hint.into(), keys));
        }
    }
    for action in walked_menu_actions() {
        if let Some(hint) = platform_shortcut_for(&action, false) {
            let keys = vec![keys_of(&hint)];
            hints.push((String::from("menu"), hint.into_owned(), keys));
        }
    }
    let mut chords = BTreeMap::new();
    let mut not_keys = Vec::new();
    for (source, hint, all) in hints {
        // One alternative that names no key leaves the others pressed.
        for keys in all {
            match keys {
                Ok(keys) => {
                    chords.insert(spell(&keys), keys);
                }
                Err(why) => not_keys.push(format!("{source} | {hint} | {why}")),
            }
        }
    }
    not_keys.sort();
    not_keys.dedup();
    (chords, not_keys)
}

/// A key sequence as `keybindings.json` spells it, the ledger's chord column.
fn spell(keys: &[KeyEvent]) -> String {
    keys.iter()
        .map(|k| Chord::from_event(*k).to_config_string())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Every hint the menus' shortcut table (`shortcut_for`) can give, read from
/// the table itself in whichever file under `src/app/` holds it: no walk of
/// built menus can tell it has seen them all.
fn shortcut_table_hints() -> BTreeSet<String> {
    let app = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/app");
    let mut dirs = vec![app.clone()];
    let mut tables = Vec::new();
    while let Some(dir) = dirs.pop() {
        for path in std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()) {
            if path.is_dir() {
                if path != app.join("tests") {
                    dirs.push(path);
                }
            } else if path.extension().is_some_and(|x| x == "rs") {
                let text = std::fs::read_to_string(&path).unwrap();
                if let Some(at) = text.find("fn shortcut_for(") {
                    let line = text[..at].rfind('\n').map_or(0, |n| n + 1);
                    let indent: String = text[line..at].chars().take_while(|c| *c == ' ').collect();
                    let body = &text[at..];
                    // The table ends at the brace that closes the function.
                    let end = body
                        .find(&format!("\n{indent}}}"))
                        .expect("shortcut_for ends");
                    tables.push(body[..end].to_string());
                }
            }
        }
    }
    assert_eq!(tables.len(), 1, "one `fn shortcut_for(` under src/app/");
    tables[0]
        .split("Some(\"")
        .skip(1)
        .map(|s| s[..s.find("\")").unwrap()].replace("\\\\", "\\"))
        .collect()
}

/// Menu shortcuts no walked menu row reaches, with why. None today: a new
/// shortcut in the table fails until its menu is walked or it is listed here.
const UNWALKED_MENU_HINTS: &[(&str, &str)] = &[];

#[test]
fn every_shortcut_the_menus_advertise_is_on_a_walked_menu_row() {
    let actions = walked_menu_actions();
    let walked: BTreeSet<&str> = actions.iter().filter_map(shortcut_for).collect();
    let table = shortcut_table_hints();
    assert!(table.contains("⌘K ⇧⌘\\"), "the table is read: {table:?}");
    let listed: BTreeSet<&str> = UNWALKED_MENU_HINTS.iter().map(|(h, _)| *h).collect();
    let missed: Vec<&String> = table
        .iter()
        .filter(|h| !walked.contains(h.as_str()) && !listed.contains(h.as_str()))
        .collect();
    let stale: Vec<&&str> = listed
        .iter()
        .filter(|h| walked.contains(**h) || !table.contains(**h))
        .collect();
    assert!(
        missed.is_empty() && stale.is_empty(),
        "menu_row_actions must reach every shortcut shortcut_for gives, or \
         UNWALKED_MENU_HINTS must say why not.\n  not reached: {missed:?}\n  \
         listed but reached or gone: {stale:?}"
    );
}

#[test]
fn the_ledger_reads_a_hint_as_the_keys_linux_presses() {
    let keys = |hint: &str| -> Vec<String> {
        linux_keys(hint)
            .into_iter()
            .map(|one| spell(&one.unwrap()))
            .collect()
    };
    let ctrl_shift = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
    assert_eq!(
        keys_of("Ctrl+Shift+P").unwrap(),
        [key(KeyCode::Char('P'), ctrl_shift)],
        "a shifted letter comes as its capital"
    );
    assert_eq!(
        keys_of("Shift+Tab").unwrap(),
        [key(KeyCode::BackTab, KeyModifiers::SHIFT)]
    );
    assert_eq!(
        keys("Cmd/Ctrl+C / X / V"),
        ["ctrl+c", "ctrl+c", "ctrl+x", "ctrl+x", "ctrl+v", "ctrl+v"]
    );
    assert_eq!(
        keys("Cmd+Opt+Left / Right"),
        ["alt+cmd+left", "alt+cmd+right"],
        "LINUX.md: no Ctrl form, so Super"
    );
    assert_eq!(keys("Cmd/Ctrl+R or F2"), ["ctrl+r", "ctrl+r", "f2"]);
    assert_eq!(
        keys("Ctrl+Shift+C / Cmd+C"),
        ["ctrl+shift+c", "cmd+c"],
        "LINUX.md: a Cmd chord beside its Ctrl one is Super"
    );
    assert_eq!(keys("Cmd/Ctrl+K Shift+T"), ["ctrl+k shift+t"; 2]);
    assert_eq!(keys("F7 / Shift+F7 in a diff"), ["f7", "shift+f7"]);
    assert_eq!(keys("Shift+PgUp/PgDn"), ["shift+pageup", "shift+pagedown"]);
    assert_eq!(
        keys("Cmd/Ctrl+1..2"),
        ["ctrl+1", "ctrl+1", "ctrl+2", "ctrl+2"]
    );
    assert_eq!(keys("Cmd/Ctrl+A then Cmd/Ctrl+S"), ["ctrl+a ctrl+s"; 2]);
    assert_eq!(
        keys("Shift+arrows"),
        ["shift+left", "shift+right", "shift+up", "shift+down"]
    );
    assert_eq!(
        keys_of("Click").unwrap_err(),
        "`Click` names no key",
        "a hint that names no key says so"
    );
}

/// F1 rows with a key the key check they name does not take, and why: bugs
/// in the sheet, each to be fixed in a change of its own. The ledger presses
/// the keys the row names all the same.
const MISLABELLED_F1_ROWS: &[(&str, &str)] = &[
    (
        "Cmd+Shift+O",
        "opening a line above moved to Cmd/Ctrl+Shift+Enter; the chord the row \
         names opens Go to Symbol",
    ),
    (
        "Cmd/Ctrl+Shift+\\",
        "Go to Bracket has no Ctrl form off macOS (LINUX.md): Linux takes \
         Super+Shift+\\ only, and the row's Ctrl+Shift+\\ does nothing there",
    ),
    (
        "Cmd/Ctrl+Opt+\\",
        "Select to Bracket has no Ctrl form off macOS (LINUX.md): Linux takes \
         Super+Alt+\\ only, and the row's Ctrl+Alt+\\ does nothing there",
    ),
];

/// An F1 row names the key check that answers it in `handler`: every key the
/// row lists must be one a check it names takes, and each check it names must
/// take one of them, or the ledger presses another chord than the one the row
/// means. A row naming two checks (`Cmd+[ / Cmd+]`) splits its keys between
/// them.
#[test]
fn every_f1_row_with_a_key_predicate_names_keys_its_predicate_takes() {
    let (chords, _) = advertised();
    for chord in ["ctrl+shift+p", "ctrl+k ctrl+0", "alt+cmd+left", "f12"] {
        assert!(chords.contains_key(chord), "{chord} is advertised");
    }
    let predicates = key_predicates();
    let mut wrong = Vec::new();
    for group in crate::widgets::shortcuts::SHORTCUT_GROUPS {
        for entry in group.entries.iter().filter(|e| !e.handler.is_empty()) {
            let row = format!("F1 {}: {:?}", group.title, entry.keys);
            let why = unpressed_f1_row(group.title, entry.keys);
            assert_eq!(
                why, None,
                "{row} backs {} but is not pressed",
                entry.handler
            );
            let firsts: Vec<KeyEvent> = linux_keys(entry.keys)
                .into_iter()
                .map(|keys| keys.unwrap_or_else(|why| panic!("{row}: {why}"))[0])
                .collect();
            let mut untaken = firsts.clone();
            let mut idle = Vec::new();
            for name in entry.handler.split_whitespace() {
                let checks: Vec<KeyPredicate> = predicates
                    .iter()
                    .filter(|(n, _)| n.split('@').next() == Some(name))
                    .map(|(_, p)| *p)
                    .collect();
                assert!(!checks.is_empty(), "{row}: {name} is a key check");
                let takes = |k: &KeyEvent| checks.iter().any(|takes| takes(*k));
                if !firsts.iter().any(takes) {
                    idle.push(name);
                }
                untaken.retain(|k| !takes(k));
            }
            let taken = untaken.is_empty() && idle.is_empty();
            let known = MISLABELLED_F1_ROWS
                .iter()
                .any(|(keys, _)| *keys == entry.keys);
            if taken == known {
                wrong.push(format!(
                    "{row} {} listed={known}: no check takes `{}`, taking none: {idle:?}",
                    entry.handler,
                    spell(&untaken)
                ));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "every key an F1 row lists must be one its handler takes; a row with \
         one that is not is in MISLABELLED_F1_ROWS until it is fixed:\n{}",
        wrong.join("\n")
    );
}
