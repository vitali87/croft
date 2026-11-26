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
//! ledger after a change that is meant to move a chord. A published crate's
//! source leaves the ledger out, as it does the screen goldens, and there
//! the comparison is skipped with a note.
//!
//! The ledger also proves that each palette command's chord runs that
//! command. `every_listed_chord_runs_its_command` runs every command with a
//! chord from the palette in the same contexts and compares what it did with
//! what its chord did there, read from the saved ledger. A chord that works
//! only in a given state (a Markdown file, a pinned tab, two terminals) is
//! pressed in a place set up for it, in `PROOF_PLACES`. `UNPROVEN` names the
//! commands no place reaches, with why (a language server, a debug session,
//! this machine), and can only shrink.
//!
//! One chord reaches outside the test: re-rooting the Explorer
//! (`ctrl+shift+/`) writes the window title straight to the terminal, so the
//! test's output shows an `ESC]0;` sequence.
//!
//! Linux only: macOS reads `Cmd` and `Ctrl` differently, so its ledger would
//! be another file.

use super::*;
use crate::command_registry::Command;
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
    use crate::command_registry::hint_for_platform;
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

/// #2648: the palette listed Cmd+Opt+Shift+C for Transform to Title Case,
/// though that chord copies the relative path. Whatever chord it lists must
/// title-case the selection when pressed.
#[test]
fn the_chord_listed_for_title_case_title_cases_the_selection() {
    let copy_relative = KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT;
    assert!(is_copy_relative_path_key(KeyEvent::new(
        KeyCode::Char('C'),
        copy_relative
    )));
    let hint = crate::command_registry::Command::TransformTitle.keybinding_hint();
    let listed = if hint.is_empty() {
        Vec::new()
    } else {
        linux_keys(hint)
    };
    for keys in listed {
        let keys = keys.unwrap_or_else(|why| panic!("{hint}: {why}"));
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("notes.txt");
        std::fs::write(&file, "hello world\n").unwrap();
        let mut app = App::new(tmp.path().to_path_buf()).unwrap();
        app.editor.open_pinned(&file).unwrap();
        app.focus_pane(Pane::Editor);
        select(&mut app, (0, 0), (0, 11));
        for key in keys {
            app.handle_key(key).unwrap();
        }
        assert_eq!(
            app.editor.lines[0], "Hello World",
            "the palette lists {hint} for Transform to Title Case"
        );
    }
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
            if !untaken.is_empty() || !idle.is_empty() {
                wrong.push(format!(
                    "{row} {}: no check takes `{}`, taking none: {idle:?}",
                    entry.handler,
                    spell(&untaken)
                ));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "every key an F1 row lists must be one its handler takes:\n{}",
        wrong.join("\n")
    );
}

/// What the ledger reads off an app: values by name, then what is open or
/// on, `None` while it is not, with a label where one tells it apart. Each
/// is kept twice: as the ledger prints it, with every commit id as `<id>`,
/// and as read, which is what tells whether a key changed it. Two values
/// that print alike can still differ, such as two hex-looking clipboards.
struct Seen {
    values: Vec<(&'static str, String)>,
    flags: Vec<(&'static str, Option<String>)>,
    raw_values: Vec<String>,
    raw_flags: Vec<Option<String>>,
}

/// A fingerprint's values and flags, as `fields` reads them.
type Fields = (
    Vec<(&'static str, String)>,
    Vec<(&'static str, Option<String>)>,
);

/// A group's tabs, the active one starred.
fn tab_list(tabs: &crate::widgets::editor::EditorTabs) -> String {
    let active = tabs.active_index();
    tabs.tab_display_labels()
        .into_iter()
        .enumerate()
        .map(|(i, l)| if i == active { format!("*{l}") } else { l })
        .collect::<Vec<_>>()
        .join(",")
}

/// The editor groups as they tile a 120x40 editor: each one's place and size
/// and its tabs, the active group starred. `1` while there is one group.
fn groups(app: &App) -> String {
    let rects = app
        .editor_layout
        .leaf_rects(ratatui::layout::Rect::new(0, 0, 120, 40), 1);
    if rects.len() < 2 {
        return String::from("1");
    }
    let active = app.editor_layout.active_dfs_index();
    let mut inactive = app.editor_layout.inactive_groups();
    rects
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let (tabs, star) = if i == active {
                (tab_list(&app.editor), "*")
            } else {
                (inactive.next().map_or_else(String::new, tab_list), "")
            };
            format!("{star}{},{}+{}x{}[{tabs}]", r.x, r.y, r.width, r.height)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `rows` as runs, `3-5,9`; `-` when there are none.
fn runs(rows: impl IntoIterator<Item = usize>) -> String {
    let mut out: Vec<(usize, usize)> = Vec::new();
    for row in rows {
        match out.last_mut() {
            Some((_, end)) if *end + 1 == row => *end = row,
            _ => out.push((row, row)),
        }
    }
    if out.is_empty() {
        return String::from("-");
    }
    out.iter()
        .map(|&(a, b)| {
            if a == b {
                a.to_string()
            } else {
                format!("{a}-{b}")
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// A caret as `row:col`, or `anchor-head` when it selects.
fn span(s: &crate::widgets::editor::EditorSelection) -> String {
    let (a, h) = (s.anchor, s.head);
    if a == h {
        format!("{}:{}", h.0, h.1)
    } else {
        format!("{}:{}-{}:{}", a.0, a.1, h.0, h.1)
    }
}

/// The Explorer: every row it lists (the root is `.`, a folder ends in `/`
/// and in `+` when open), the selected one, any marked with it, and how far
/// it is scrolled.
fn explorer(tree: &crate::widgets::file_tree::FileTree, root: &Path) -> String {
    let rel = |p: &Path| match p.strip_prefix(root) {
        Ok(r) if r.as_os_str().is_empty() => String::from("."),
        Ok(r) => r.display().to_string(),
        Err(_) => p.display().to_string(),
    };
    let rows: Vec<String> = tree
        .nodes
        .iter()
        .map(|n| {
            let open = if n.expanded { "+" } else { "" };
            let dir = if n.is_dir { "/" } else { "" };
            format!("{}{dir}{open}", rel(&n.path))
        })
        .collect();
    let mut out = format!(
        "{} > {}",
        rows.join(","),
        tree.selected_path().map_or(String::from("-"), &rel)
    );
    if !tree.marked.is_empty() {
        let marked: Vec<String> = tree.marked.iter().map(|p| rel(p)).collect();
        out.push_str(&format!(" marked={}", marked.join(",")));
    }
    if tree.scroll > 0 {
        out.push_str(&format!(" scroll={}", tree.scroll));
    }
    out
}

/// The Search view: the query, the field with the caret and where the caret
/// is while it is focused, the text selected in the query or another field,
/// the replace and file fields while shown, the options on, and the hits
/// with the one selected.
fn search_view(s: &crate::widgets::search::SearchPanel) -> String {
    let mut out = format!("{:?}", s.query);
    if s.focused {
        out.push_str(&format!(" @{:?}:{}", s.field, s.caret()));
    }
    // A selection is what the next copy or typed key acts on.
    if let Some((a, b)) = s.selection_range() {
        out.push_str(&format!(" sel={a}-{b}"));
    }
    if let Some((a, b)) = s.field_selection {
        out.push_str(&format!(" field_sel={a}-{b}"));
    }
    if s.replace_open {
        out.push_str(&format!(" replace={:?}", s.replace));
    }
    if s.details_open {
        out.push_str(&format!(" include={:?} exclude={:?}", s.include, s.exclude));
    }
    let o = s.opts;
    for (on, name) in [
        (o.case_sensitive, "case"),
        (o.whole_word, "word"),
        (o.use_regex, "regex"),
        (o.preserve_case, "preserve"),
    ] {
        if on {
            out.push_str(&format!(" +{name}"));
        }
    }
    out.push_str(&format!(" hits={}@{}", s.hits.len(), s.selected));
    out
}

/// The Source Control view: the commit message and its caret, what has
/// focus, the selected changes and every change listed.
fn scm_view(sc: &crate::widgets::source_control::SourceControlPanel) -> String {
    let mut out = format!("{:?}@{}", sc.message, sc.message_cursor);
    if sc.focused {
        out.push_str(" +focus");
    }
    match sc.keys {
        crate::widgets::source_control::KeyStop::Message => {}
        crate::widgets::source_control::KeyStop::List => out.push_str(" +list"),
        crate::widgets::source_control::KeyStop::Commits => out.push_str(" +commits"),
    }
    if let Some(i) = sc.selected_change {
        out.push_str(&format!(" sel={i}"));
    }
    if !sc.multi_selection.is_empty() {
        out.push_str(&format!(
            " multi={}",
            runs(sc.multi_selection.iter().copied())
        ));
    }
    for e in &sc.entries {
        out.push_str(&format!(" {:?}:{}", e.kind, e.path));
    }
    out
}

/// The repository at `root` as git reports it: the branch, the stashes and
/// each change's two status letters, `-` outside a repository. The HEAD
/// commit's id is left out: it changes with the clock. Read through
/// [`test_git`], so neither a global ignore file nor a `GIT_DIR` from the
/// developer's setup changes what it reports.
fn git_state(root: &Path) -> String {
    if !root.join(".git").exists() {
        return String::from("-");
    }
    let out = test_git(root)
        .args(["status", "--porcelain=v2", "--branch", "--show-stash"])
        .arg("--untracked-files=all")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let mut parts = Vec::new();
    for line in text.lines() {
        let fields: Vec<&str> = line.split(' ').collect();
        // An entry's path is its last field: after 8 fields for an
        // ordinary change, 9 for a rename and 10 for a conflict.
        let path_after = |n: usize| line.splitn(n + 1, ' ').last().unwrap_or("");
        match fields[0] {
            "#" if fields.get(1) == Some(&"branch.head") => parts.push(fields[2].to_string()),
            "#" if fields.get(1) == Some(&"stash") => parts.push(format!("stash={}", fields[2])),
            "1" => parts.push(format!("{}:{}", fields[1], path_after(8))),
            "2" => parts.push(format!("{}:{}", fields[1], path_after(9))),
            "u" => parts.push(format!("{}:{}", fields[1], path_after(10))),
            "?" | "!" => parts.push(format!("{0}{0}:{1}", fields[0], path_after(1))),
            _ => {}
        }
    }
    parts.join(",")
}

/// `text` with every word that reads as a commit id (7 to 40 lowercase hex
/// digits, letters among them or not) as `<id>`: a commit's id changes with
/// the clock.
fn no_commit_ids(text: &str) -> String {
    let id = |w: &str| {
        (7..=40).contains(&w.len())
            && w.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    };
    let mut out = String::new();
    let mut word = String::new();
    for c in text.chars().map(Some).chain([None]) {
        if let Some(c) = c.filter(char::is_ascii_alphanumeric) {
            word.push(c);
            continue;
        }
        out.push_str(if id(&word) { "<id>" } else { &word });
        word.clear();
        out.extend(c);
    }
    out
}

/// What a key can change that a user sees or acts on next.
fn fingerprint(app: &App, root: &Path) -> Seen {
    // Read from the disk and git once: they are the same for both passes.
    let read = [
        explorer(&app.tree, root),
        search_view(&app.search),
        scm_view(&app.source_control),
        git_state(root),
        files_digest(root),
    ];
    let (values, flags) = fields(app, root, &read, true);
    let (raw_values, raw_flags) = fields(app, root, &read, false);
    Seen {
        values,
        flags,
        raw_values: raw_values.into_iter().map(|(_, v)| v).collect(),
        raw_flags: raw_flags.into_iter().map(|(_, f)| f).collect(),
    }
}

/// The fingerprint's values and flags, with every commit id as `<id>` when
/// `hide_ids`. `read` holds what came from the disk and git: the Explorer,
/// Search and Source Control views, `git status` and the files' digest.
fn fields(app: &App, root: &Path, read: &[String; 5], hide_ids: bool) -> Fields {
    let ed = &app.editor;
    let focus = match app.focus {
        Pane::Tree => "side",
        Pane::Editor => "editor",
        Pane::Terminal => "panel",
    };
    let side = if app.show_tree {
        format!("{:?}", app.sidebar_view)
    } else {
        String::from("hidden")
    };
    let panel = match (app.show_terminal, app.terminal_maximized) {
        (false, _) => String::from("hidden"),
        (true, max) => format!(
            "{:?}{}",
            app.bottom_panel_tab,
            if max { "+max" } else { "" }
        ),
    };
    let sel = ed.selection.as_ref().map_or(String::from("-"), span);
    let carets = if ed.carets.is_empty() {
        String::from("-")
    } else {
        ed.carets.iter().map(span).collect::<Vec<_>>().join(",")
    };
    let folded = runs((0..ed.lines.len()).filter(|&l| ed.is_line_hidden(l)));
    let marks = |m: &std::collections::HashMap<PathBuf, std::collections::BTreeSet<usize>>| {
        let rows: BTreeSet<usize> = m.values().flatten().copied().collect();
        runs(rows)
    };
    let clip = crate::clipboard::test_clip::mock_read().unwrap_or_default();
    // Each app has a workspace of its own, so its path is not news.
    // Nor are the process and pane ids a scrollback dump is named with.
    let real = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut news = vec![
        (real.display().to_string(), String::from("<root>")),
        (root.display().to_string(), String::from("<root>")),
    ];
    news.extend(app.terminals.iter().map(|t| {
        (
            format!("-{}-{}-", std::process::id(), t.uid()),
            String::from("-<pid>-<pane>-"),
        )
    }));
    let scrub = |s: &str| {
        let s = news
            .iter()
            .fold(s.to_string(), |s, (from, to)| s.replace(from, to));
        if hide_ids { no_commit_ids(&s) } else { s }
    };
    let quote = |s: &str| format!("{:?}", scrub(s));
    let on = |on: bool| on.then(String::new);
    let quoted = |s: &str| Some(quote(s));
    // What the app wrote to each terminal pane, by the pane's place in the
    // panel: the same bytes sent to another pane are another effect.
    let typed: Vec<String> = app
        .terminals
        .iter()
        .enumerate()
        .filter_map(|(i, t)| {
            let bytes = t.written_bytes_for_test();
            (!bytes.is_empty()).then(|| format!("{i}:{}", quote(&String::from_utf8_lossy(&bytes))))
        })
        .collect();
    // A picker or menu: what it holds and the row it has selected.
    let picked = |query: &str, cursor: usize, row: &str| {
        Some(format!("{} @{cursor} > {}", quote(query), quote(row)))
    };
    // A picker whose rows come from this machine (its processes, its
    // zoxide database, the command history every test shares): the row it
    // has selected by place, so the ledger reads the same everywhere.
    let picked_at = |query: &str, cursor: usize, selected: usize| {
        Some(format!("{} @{cursor} > #{selected}", quote(query)))
    };
    let menu = app.context_menu.as_ref().map(|m| {
        let label = |items: &[MenuEntry], i: usize| items.get(i).map_or("", menu_label).to_string();
        let mut out = format!(
            "{} > {}",
            quote(&label(&m.items, 0)),
            quote(&label(&m.items, m.selected))
        );
        if let Some(MenuEntry::Submenu { items, .. }) = m.open_submenu.and_then(|i| m.items.get(i))
        {
            out.push_str(&format!(" > {}", quote(&label(items, m.submenu_selected))));
        }
        out
    });
    (
        vec![
            ("focus", String::from(focus)),
            ("side", side),
            ("panel", panel),
            ("tabs", scrub(&tab_list(&app.editor))),
            ("groups", scrub(&groups(app))),
            ("text", digest(&ed.lines.join("\n"))),
            ("caret", format!("{}:{}", ed.cursor_row, ed.cursor_col)),
            ("sel", sel),
            ("carets", carets),
            ("scroll", format!("{}:{}", ed.scroll, ed.scroll_col)),
            ("folded", folded),
            (
                "marks",
                format!("{}/{}", marks(&ed.breakpoints), marks(&ed.bookmarks)),
            ),
            ("status", quote(&app.status)),
            ("clip", quote(&clip)),
            ("typed", typed.join(" ")),
            (
                "terms",
                format!("{}@{}", app.terminals.len(), app.active_terminal),
            ),
            ("tree", read[0].clone()),
            ("search", read[1].clone()),
            ("scm", read[2].clone()),
            ("git", read[3].clone()),
            ("files", read[4].clone()),
        ],
        vec![
            ("quit", on(app.quit)),
            ("drop_to_local", on(app.drop_to_local)),
            ("minimap", on(app.minimap_visible)),
            ("wrap", on(ed.wrap_enabled())),
            ("zen", on(app.zen_mode)),
            ("aux", on(app.secondary_side_bar_visible)),
            ("activity_bar", on(app.activity_bar_visible)),
            ("status_bar", on(app.status_bar_visible)),
            ("centered", on(app.centered_layout)),
            ("vim", on(app.vim.enabled)),
            ("overwrite", on(app.overwrite_mode)),
            ("columns", on(ed.column_selection_mode)),
            ("perf", on(app.perf.status_span().is_some())),
            ("screencast", on(app.screencast.is_on())),
            ("macro", on(app.macro_recording.is_some())),
            (
                "palette",
                app.command_palette.as_ref().and_then(|p| {
                    let row = p.results.get(p.selected).map_or("", |r| r.title());
                    picked(&p.query, p.cursor, row)
                }),
            ),
            (
                "quick_open",
                app.file_finder.as_ref().and_then(|f| {
                    let row = f.results.get(f.selected).map_or("", |r| &r.entry.rel);
                    picked(&f.query, f.cursor, row)
                }),
            ),
            (
                "go_to_symbol",
                app.go_to_symbol.as_ref().and_then(|g| {
                    // A symbol's row is where it jumps to.
                    let row = g
                        .selected_target()
                        .map_or_else(String::new, |(l, c)| format!("{l}:{c}"));
                    picked(&g.query, g.cursor, &row)
                }),
            ),
            (
                "workspace_symbols",
                app.workspace_symbols.as_ref().and_then(|w| {
                    let row = w.results.get(w.selected).map_or("", |r| &r.name);
                    picked(&w.query, w.cursor, row)
                }),
            ),
            (
                "process_picker",
                app.process_picker
                    .as_ref()
                    .map(|p| format!("> #{}", p.selected)),
            ),
            (
                "zoxide",
                app.zoxide_jump
                    .as_ref()
                    .and_then(|z| picked_at(&z.query, z.cursor, z.selected)),
            ),
            (
                "history",
                app.command_history_popup.as_ref().and_then(|h| {
                    picked_at(&format!("{:?} {}", h.scope, h.query), h.cursor, h.selected)
                }),
            ),
            (
                "branch_picker",
                app.branch_picker.as_ref().and_then(|b| {
                    let row = b.branches.get(b.selected).map_or("", |r| &r.display);
                    picked(&b.query, b.cursor, row)
                }),
            ),
            (
                "input",
                app.input_prompt
                    .as_ref()
                    .map(|p| format!("{} {} @{}", quote(&p.title), quote(&p.value), p.cursor)),
            ),
            (
                "list",
                app.list_picker.as_ref().and_then(|l| {
                    let row = l.rows.get(l.selected).map_or("", |r| &r.label);
                    picked(&format!("{:?} {}", l.purpose, l.query), l.cursor, row)
                }),
            ),
            (
                "prompt",
                app.prompt
                    .as_ref()
                    .map(|p| format!("{} {}", quote(&p.label), quote(&p.buffer))),
            ),
            ("menu", menu),
            (
                "commit_feedback",
                app.source_control
                    .commit_feedback
                    .as_deref()
                    .and_then(quoted),
            ),
            ("shortcuts", on(app.shortcuts_modal.is_some())),
            (
                "settings",
                app.settings_editor.as_ref().map(|s| {
                    let row = s.selected_row().map_or("", |r| &r.key);
                    format!("{} > {}", quote(&s.query), quote(row))
                }),
            ),
            ("connect", on(app.connect_dialog.is_some())),
            ("approval", on(app.approval_ui.is_some())),
            ("find", on(app.editor_find.is_some())),
            ("terminal_find", on(app.terminal_find.is_some())),
            ("peek", on(app.peek_popup.is_some())),
            ("peek_target", on(app.peek_target.is_some())),
            (
                "peek_refs",
                app.peek_refs
                    .as_ref()
                    .map(|(refs, i)| format!("{i}/{}", refs.len())),
            ),
            ("hover", on(app.hover_popup.is_some())),
            (
                "completion",
                app.completion_popup.as_ref().map(|c| {
                    let row = c.items.get(c.selected).map_or("", |r| &r.label);
                    format!("{} > {}", quote(&c.prefix), quote(row))
                }),
            ),
            ("signature", on(app.signature_help_popup.is_some())),
            ("comment", on(ed.comment_focus.is_some())),
            ("leader", on(app.cmd_k_leader.is_some())),
            ("recording_shortcut", on(app.recording_shortcut.is_some())),
            ("confirm_shortcut", on(app.confirming_shortcut.is_some())),
            ("scrubber", on(app.scrubber.is_some())),
            ("scrub_view", on(app.scrub_view.is_some())),
            ("tour", on(app.tour.is_some())),
            ("unsaved", on(app.pending_unsaved.is_some())),
            ("lossy_save", on(app.pending_lossy_save.is_some())),
            ("replace_all", on(app.pending_replace_all.is_some())),
            ("unsaved_scm", on(app.pending_unsaved_scm.is_some())),
            ("stash_drop", on(app.pending_stash_drop.is_some())),
            ("discard_all", on(app.pending_discard_all)),
            ("run_block", on(app.pending_run_block.is_some())),
            ("terminal_paste", on(app.pending_terminal_paste.is_some())),
            ("terminal_warning", on(app.pending_terminal_warning)),
            ("broadcast", on(app.pending_broadcast_enable)),
            ("local_open", on(app.pending_local_open.is_some())),
            ("discard", on(app.pending_discard.is_some())),
            ("revert_hunk", on(app.pending_revert_hunk.is_some())),
        ],
    )
}

/// What a key changed: `name=value` for a value, `+name` (with its label) for
/// what opened or came on, `-name` for what closed or went off, and
/// `~name(label)` for an open one whose label changed. `-` when nothing did.
fn effects(before: &Seen, after: &Seen) -> String {
    // Whether something changed is read off the raw values; what is printed
    // is the shown one, so a change a placeholder hides still counts.
    let values = before
        .raw_values
        .iter()
        .zip(&after.raw_values)
        .zip(&after.values);
    let mut changed: Vec<String> = values
        .filter(|((b, a), _)| b != a)
        .map(|(_, (name, shown))| format!("{name}={shown}"))
        .collect();
    let flags =
        (before.raw_flags.iter().zip(&after.raw_flags)).zip(before.flags.iter().zip(&after.flags));
    changed.extend(
        flags
            .filter(|((b, a), _)| b != a)
            .map(|(_, ((name, b), (_, a)))| match (b, a.as_deref()) {
                (_, None) => format!("-{name}"),
                (None, Some("")) => format!("+{name}"),
                (None, Some(label)) => format!("+{name}({label})"),
                (Some(_), Some(label)) => format!("~{name}({label})"),
            }),
    );
    if changed.is_empty() {
        String::from("-")
    } else {
        changed.join(" ")
    }
}

/// A short stable digest (FNV-1a) and the line count: `4L#1a2b3c4d`.
fn digest(text: &str) -> String {
    let hash = text.bytes().fold(0x811c_9dc5u32, |h, b| {
        (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
    });
    format!("{}L#{hash:08x}", text.lines().count())
}

/// Every path under `root` and what each file holds, as one digest: what a
/// key wrote, made or moved on disk. A repository's `.git` is left to
/// [`git_state`]: git rewrites its files with the clock in them.
fn files_digest(root: &Path) -> String {
    let mut all = Vec::new();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path == root.join(".git") {
                continue;
            }
            let body = std::fs::read(&path).unwrap_or_default();
            all.push(format!(
                "{}\n{}",
                path.strip_prefix(root).unwrap_or(&path).display(),
                String::from_utf8_lossy(&body)
            ));
            if path.is_dir() {
                dirs.push(path);
            }
        }
    }
    all.sort();
    digest(&all.join("\n"))
}

#[test]
fn a_keys_effects_name_what_it_changed_and_what_it_opened() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let mut app = app_with_open_file(root, "main.rs", "fn main() {}\n");
    let before = fingerprint(&app, root);
    assert_eq!(effects(&before, &fingerprint(&app, root)), "-");
    type_char(&mut app, 'x');
    app.handle_key(key(
        KeyCode::Char('P'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ))
    .unwrap();
    let fx = effects(&before, &fingerprint(&app, root));
    for part in ["tabs=*● main.rs", "caret=0:1", "+palette(\"\" @0 > \""] {
        assert!(fx.contains(part), "{part} in {fx}");
    }
    assert!(
        !fx.contains("files="),
        "typing changes the buffer, not the disk: {fx}"
    );
    app.handle_key(key(KeyCode::Down, KeyModifiers::NONE))
        .unwrap();
    let down = effects(&before, &fingerprint(&app, root));
    assert_ne!(down, fx, "the palette's selected row is part of it");
    for id in ["1a2b3c4", "4042135", "fedcbae"] {
        let line = format!("[main {id}] Ship it");
        assert_eq!(no_commit_ids(&line), "[main <id>] Ship it");
    }
    assert_eq!(
        no_commit_ids("added 12 lines, cafe"),
        "added 12 lines, cafe"
    );
    assert_eq!(runs([1, 2, 3, 7]), "1-3,7");
}

/// The ledger prints every commit id as `<id>` so it reads the same on every
/// run, but whether a key changed something is read off the real values:
/// a clipboard that goes from one hex-looking word to another is a change.
#[test]
fn a_change_the_commit_id_placeholder_hides_still_counts() {
    let seen = |clip: &str| Seen {
        values: vec![("clip", format!("{:?}", no_commit_ids(clip)))],
        flags: vec![("commit_feedback", Some(no_commit_ids(clip)))],
        raw_values: vec![format!("{clip:?}")],
        raw_flags: vec![Some(clip.to_string())],
    };
    assert_eq!(
        effects(&seen("1234567"), &seen("7654321")),
        "clip=\"<id>\" ~commit_feedback(<id>)"
    );
    assert_eq!(effects(&seen("1234567"), &seen("1234567")), "-");
}

#[test]
fn a_keys_effects_name_the_terminal_pane_it_typed_into() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let mut app = app_with_open_file(root, "main.rs", "fn main() {}\n");
    let quiet = ["-c", "exec sleep 600"].map(String::from);
    app.terminals
        .push(PtyTerminal::new_running("/bin/sh", &quiet, root).unwrap());
    let before = fingerprint(&app, root);
    app.terminals[1].write_input(b"ls");
    let second = effects(&before, &fingerprint(&app, root));
    assert!(second.contains("typed=1:\"ls\""), "{second}");
    // The same bytes in the first pane instead.
    app.terminals[1] = PtyTerminal::new_running("/bin/sh", &quiet, root).unwrap();
    app.terminals[0].write_input(b"ls");
    let first = effects(&before, &fingerprint(&app, root));
    assert!(first.contains("typed=0:\"ls\""), "{first}");
}

#[test]
fn a_keys_effects_name_what_it_did_to_an_open_picker() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let mut app = app_with_open_file(root, "main.rs", "fn main() {}\n");
    let before = fingerprint(&app, root);
    app.handle_key(key(
        KeyCode::Char('O'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ))
    .unwrap();
    let opened = fingerprint(&app, root);
    let fx = effects(&before, &opened);
    assert!(fx.contains("+go_to_symbol(\"\" @0 > "), "{fx}");
    type_char(&mut app, 'x');
    let fx = effects(&opened, &fingerprint(&app, root));
    assert!(fx.contains("~go_to_symbol(\"x\" @1 > "), "its query: {fx}");
}

#[test]
fn a_keys_effects_name_the_text_selected_in_the_search_view() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_with_open_file(tmp.path(), "main.rs", "fn main() {}\n");
    app.search.query = String::from("total");
    assert!(!search_view(&app.search).contains("sel="));
    app.search.select_all_query();
    let view = search_view(&app.search);
    assert!(view.contains(" sel=0-5"), "what a copy takes: {view}");
}

/// Where the ledger is kept.
const LEDGER: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/app/tests/golden/key-ledger.txt"
);

/// The ledger's first lines, before the hints it does not press.
const HEADER: &str = concat!(
    "# What every advertised chord does in each main context: ",
    "src/app/tests/key_ledger.rs.\n",
    "# `context | chord | effects`. ",
    "Rewrite with CROFT_BLESS=1 after a change meant to move one.\n",
);

/// What owns the ledger's lines that list the hints it does not press.
const NOT_PRESSED: &str = "# not pressed";

/// The file every context has open, the caret on `total`: a block comment,
/// a function with a block nested in it, and a region holding another, so
/// each fold and selection command has something of its own to do.
const MAIN_RS: &str = concat!(
    "/*\n",
    " * Adds two numbers and prints the total.\n",
    " */\n",
    "fn main() {\n",
    "    let total = add(1, 2);\n",
    "    if total > 2 {\n",
    "        println!(\"{total}\");\n",
    "    }\n",
    "}\n",
    "\n",
    "// #region helpers\n",
    "fn add(a: i32, b: i32) -> i32 {\n",
    "    a + b\n",
    "}\n",
    "// #endregion\n",
);

/// Where the caret is in [`MAIN_RS`]: on `total`.
const ON_TOTAL: (usize, usize) = (4, 8);

/// Where `total` ends, for the selection context.
const TOTAL_END: (usize, usize) = (4, 13);

/// A launch configuration whose program is asked for: F5 opens that question
/// rather than starting whichever debugger this machine has.
const LAUNCH_JSON: &str = r#"{"configurations": [{"name": "Ledger", "type": "lldb",
  "request": "launch", "program": "${input:program}"}],
  "inputs": [{"id": "program", "type": "promptString", "description": "Program"}]}"#;

/// A workspace of `main.rs`, `docs/notes.md` and `.vscode/launch.json`.
fn workspace(root: &Path) {
    std::fs::create_dir_all(root.join("docs")).unwrap();
    std::fs::create_dir_all(root.join(".vscode")).unwrap();
    std::fs::write(root.join("main.rs"), MAIN_RS).unwrap();
    std::fs::write(root.join("docs/notes.md"), "# Notes\n").unwrap();
    std::fs::write(root.join(".vscode/launch.json"), LAUNCH_JSON).unwrap();
}

/// Make the workspace at `root` a repository: everything committed on
/// `main`, then a change to `docs/notes.md` staged and `todo.txt` new. The
/// fixture's own git commands go through [`test_git`], so the developer's
/// git setup and a `GIT_DIR` a hook exports never reach them. Who commits
/// from croft, and that nothing signs or hooks its commits, is the
/// repository's own config, so a commit from croft goes the same way
/// whatever the developer's git config says.
fn repository(root: &Path) {
    // The app's own git commands are production code and would read a
    // repository a hook names; fail at once rather than ledger it.
    super::golden::refuse_inherited_git_state();
    git_init(root);
    let config = root.join(".git/config");
    let mut text = std::fs::read_to_string(&config).unwrap();
    text.push_str(
        "[user]\n\tname = Ledger\n\temail = ledger@example.com\n\
         [commit]\n\tgpgsign = false\n[core]\n\thooksPath = /dev/null\n",
    );
    std::fs::write(&config, text).unwrap();
    git_ok(root, &["add", "-A"]);
    git_ok(root, &["commit", "-q", "-m", "Start"]);
    std::fs::write(root.join("docs/notes.md"), "# Notes\n\nShip it.\n").unwrap();
    git_ok(root, &["add", "docs/notes.md"]);
    std::fs::write(root.join("todo.txt"), "tests\n").unwrap();
}

/// The contexts where a chord's own handler answers it, with no dialog
/// open to take the key first.
const NO_DIALOG: &[&str] = &[
    "editor",
    "selection",
    "explorer",
    "terminal",
    "search",
    "scm",
];

/// Chords whose answer depends on the machine rather than on croft, with the
/// contexts they are left out of and why. With a dialog open the dialog
/// answers them, and the machine makes no difference.
const MACHINE_BOUND: &[(&[&str], &str, &str)] = &[
    (
        NO_DIALOG,
        "ctrl+f5",
        "lists this machine's running Python processes",
    ),
    (
        NO_DIALOG,
        "ctrl+k shift+g",
        "asks a model when ANTHROPIC_API_KEY is set",
    ),
    (
        NO_DIALOG,
        "ctrl+shift+f9",
        "reads CROFT_NO_UPDATE_CHECK and CROFT_REMOTE_AUTOUPDATE",
    ),
    (
        &["explorer"],
        "ctrl+z",
        "jumps with zoxide, installed or not",
    ),
];

/// Why `chord` is not pressed in `context`, when it is machine-bound there.
fn machine_bound(context: &str, chord: &str) -> Option<&'static str> {
    MACHINE_BOUND
        .iter()
        .find(|(contexts, c, _)| *c == chord && contexts.contains(&context))
        .map(|(_, _, why)| *why)
}

/// One main branch of the key router, as the app is set up to reach it.
struct Context {
    name: &'static str,
    /// Whether the workspace is a git repository.
    repo: bool,
    /// What turns an app with `main.rs` open, the caret on `total`, into
    /// this context.
    setup: fn(&mut App),
}

/// The contexts every chord is pressed in, in the ledger's order.
const CONTEXTS: [Context; 8] = [
    Context {
        name: "editor",
        repo: false,
        setup: |app| app.focus_pane(Pane::Editor),
    },
    Context {
        name: "selection",
        repo: false,
        setup: |app| {
            app.focus_pane(Pane::Editor);
            select(app, ON_TOTAL, TOTAL_END);
        },
    },
    Context {
        name: "explorer",
        repo: false,
        setup: |app| {
            app.set_sidebar_view(SidebarView::Explorer);
            app.focus_pane(Pane::Tree);
            let main_rs = app
                .tree
                .nodes
                .iter()
                .position(|n| n.path.ends_with("main.rs"));
            app.tree.selected = main_rs.expect("main.rs is in the Explorer");
        },
    },
    Context {
        name: "terminal",
        repo: false,
        setup: |app| {
            app.show_terminal = true;
            app.bottom_panel_tab = BottomPanelTab::Terminal;
            app.focus_pane(Pane::Terminal);
        },
    },
    Context {
        name: "search",
        repo: false,
        setup: |app| {
            app.set_sidebar_view(SidebarView::Search);
            app.search.query = String::from("total");
            app.search.run_query();
            assert_eq!(app.search.hits.len(), 4, "`total` 4 times in main.rs");
        },
    },
    Context {
        name: "scm",
        repo: true,
        setup: |app| {
            app.set_sidebar_view(SidebarView::SourceControl);
            crate::test_budget::await_spawned(
                std::time::Duration::from_millis(500),
                "git status for Source Control",
                || {
                    app.drain_git_responses();
                    app.source_control.entries.len() == 2
                },
            );
            app.source_control.message = String::from("Ship it");
            app.source_control.message_cursor = app.source_control.message.len();
        },
    },
    Context {
        name: "palette",
        repo: false,
        setup: |app| app.open_command_palette(),
    },
    Context {
        name: "confirm",
        repo: false,
        setup: |app| {
            app.focus_pane(Pane::Editor);
            type_char(app, 'x');
            press_ctrl_q(app);
            assert!(app.pending_unsaved.is_some(), "the unsaved-changes prompt");
        },
    },
];

/// An app in a context, with what it shows before the next chord. Its fields
/// drop in order, the app first and the folder it all lives in last.
struct Built {
    app: App,
    root: PathBuf,
    seen: Seen,
    /// The shell App::new started, which the quiet child replaced.
    shell: PtyTerminal,
    _cache: crate::session_state::CacheDirForThisTestThread,
    _tmp: tempfile::TempDir,
}

impl Built {
    /// Done with: its terminals end on `enders`, the rest drops here.
    fn retire(mut self, enders: &mut Enders) {
        let mut terminals = std::mem::take(&mut self.app.terminals);
        terminals.push(self.shell);
        enders.end(terminals);
    }
}

/// The app for `ctx` in a workspace of its own, one frame drawn so every
/// pane has its size. A local session with no language server, so none
/// starts and nothing depends on which are installed, and with a terminal
/// child that prints nothing, so no prompt races a key onto the grid. It
/// reads nothing from the developer's config folder, so their own
/// extensions, snippets and macros cannot change the ledger.
fn build(ctx: &Context) -> Built {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("ws");
    workspace(&root);
    if ctx.repo {
        repository(&root);
    }
    // croft's cache (scrollback dumps, sessions) goes beside it, not home.
    let cache = crate::session_state::CacheDirForThisTestThread::set(&tmp.path().join("cache"));
    let mut app = App::new(root.clone()).unwrap();
    super::golden::without_user_files(&mut app, &tmp.path().join("config"));
    app.lsp = None;
    app.is_remote = false;
    app.selected_debug_config = Some(String::from("Ledger"));
    let quiet = PtyTerminal::new_running(
        "/bin/sh",
        &[String::from("-c"), String::from("exec sleep 600")],
        &root,
    )
    .unwrap();
    let shell = std::mem::replace(&mut app.terminals[0], quiet);
    app.clipboard_reader = || Some(String::from("CLIP"));
    crate::clipboard::test_clip::mock_write("CLIP");
    crate::clipboard::test_clip::remember_copy("CLIP");
    app.editor.open_pinned(&root.join("main.rs")).unwrap();
    (app.editor.cursor_row, app.editor.cursor_col) = ON_TOTAL;
    (ctx.setup)(&mut app);
    draw(&mut app, 120, 40);
    let seen = fingerprint(&app, &root);
    Built {
        app,
        root,
        seen,
        shell,
        _cache: cache,
        _tmp: tmp,
    }
}

/// Ends the terminals of retired apps on threads of their own, a few at a
/// time: ending one waits on its shell, most of a press's wall time.
#[derive(Default)]
struct Enders(std::collections::VecDeque<std::thread::JoinHandle<()>>);

impl Enders {
    /// How many retired apps' terminals may be ending at once.
    const AT_ONCE: usize = 4;

    fn end(&mut self, terminals: Vec<PtyTerminal>) {
        if self.0.len() >= Self::AT_ONCE
            && let Some(oldest) = self.0.pop_front()
        {
            oldest.join().unwrap();
        }
        self.0
            .push_back(std::thread::spawn(move || drop(terminals)));
    }
}

impl Drop for Enders {
    fn drop(&mut self) {
        for ender in self.0.drain(..) {
            let _ = ender.join();
        }
    }
}

/// [`advertised`], worked out once for every test that presses them.
fn advertised_once() -> &'static Advertised {
    static ADVERTISED: std::sync::OnceLock<Advertised> = std::sync::OnceLock::new();
    ADVERTISED.get_or_init(advertised)
}

/// The ledger's lines for `ctx`: every advertised chord pressed in it, in
/// order, each on an app in the context and showing nothing any chord
/// before it changed.
fn presses(ctx: &Context) -> Vec<String> {
    let fresh = std::env::var("CROFT_LEDGER_FRESH").is_ok_and(|v| v == "1");
    let mut enders = Enders::default();
    let mut kept: Option<Built> = None;
    let mut lines = Vec::new();
    for (chord, keys) in &advertised_once().0 {
        if machine_bound(ctx.name, chord).is_some() {
            continue;
        }
        let b = kept.get_or_insert_with(|| build(ctx));
        let errors: Vec<String> = keys
            .iter()
            .filter_map(|k| b.app.handle_key(*k).err())
            .map(|e| format!(" error={:?}", e.to_string()))
            .collect();
        // A git operation runs off the UI thread: its end is part of what
        // the chord did, and git must not be busy while it is read.
        if b.app.git_net_job.is_some() {
            wait_for_git_net(&mut b.app);
        }
        if ctx.repo {
            settle_source_control(&mut b.app, &b.root);
        }
        let fx = effects(&b.seen, &fingerprint(&b.app, &b.root)) + &errors.concat();
        let changed = fx != "-";
        lines.push(format!("{} | {chord} | {fx}", ctx.name));
        if fresh || changed {
            kept.take().unwrap().retire(&mut enders);
        }
    }
    if let Some(done) = kept {
        done.retire(&mut enders);
    }
    lines
}

/// Wait until Source Control lists the changes git reports for `root`.
/// A chord that stages or commits asks the git worker for the new state,
/// whose reply lands only when the app drains it, so reading the panel at
/// once would show the changes as they were next to the index as it is.
fn settle_source_control(app: &mut App, root: &Path) {
    crate::test_budget::await_spawned(
        std::time::Duration::from_secs(1),
        "Source Control to list the changes git reports",
        || {
            app.drain_git_responses();
            app.source_control.entries == crate::git::query_changes(root)
        },
    );
}

/// What owns a ledger line: its context, [`NOT_PRESSED`] for a hint it does
/// not press, `None` for the header.
fn owner_of(line: &str) -> Option<&str> {
    if line.starts_with(NOT_PRESSED) {
        Some(NOT_PRESSED)
    } else if line.starts_with('#') || line.is_empty() {
        None
    } else {
        line.split(" | ").next()
    }
}

/// Whether `owner` still owns lines: the hints, or a context there is.
fn known_owner(owner: &str) -> bool {
    owner == NOT_PRESSED || CONTEXTS.iter().any(|c| c.name == owner)
}

/// Where a ledger line goes: the hints first, then each context in
/// [`CONTEXTS`] order, its chords in the order they are pressed.
fn place(line: &str) -> (usize, &str) {
    let owner = owner_of(line).unwrap_or_default();
    let ctx = CONTEXTS.iter().position(|c| c.name == owner);
    let chord = line.split(" | ").nth(1).unwrap_or_default();
    (ctx.map_or(0, |i| i + 1), chord)
}

/// Replace `owner`'s lines in the ledger with `now`, under a lock on the
/// file, since each context's test writes its own lines and they run at
/// once. The hints' owner also drops the lines of a context that is gone.
fn bless(owner: &str, now: &[String]) {
    use std::io::{Read, Seek, Write};
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(LEDGER)
        .unwrap();
    file.lock().unwrap();
    let mut saved = String::new();
    file.read_to_string(&mut saved).unwrap();
    let kept = |o: &str| o != owner && (known_owner(o) || owner != NOT_PRESSED);
    let mut lines: Vec<&str> = saved
        .lines()
        .filter(|l| owner_of(l).is_some_and(kept))
        .chain(now.iter().map(String::as_str))
        .collect();
    lines.sort_by(|a, b| place(a).cmp(&place(b)).then(a.cmp(b)));
    file.set_len(0).unwrap();
    file.rewind().unwrap();
    file.write_all(HEADER.as_bytes()).unwrap();
    for line in lines {
        writeln!(file, "{line}").unwrap();
    }
}

/// Fail, listing the lines that moved, unless the ledger's lines for `owner`
/// are `now`; with `CROFT_BLESS=1`, make them `now`.
fn check(owner: &str, now: &[String]) {
    if std::env::var("CROFT_BLESS").is_ok_and(|v| v == "1") {
        bless(owner, now);
        return;
    }
    let saved = match std::fs::read_to_string(LEDGER) {
        Ok(saved) => saved,
        // A published crate's source leaves the ledger out, as it does the
        // screen goldens, so there is nothing to compare with.
        Err(_) if super::golden::packaged_source() => {
            eprintln!("{owner}: skipped, a published crate's source has no saved ledger");
            return;
        }
        Err(err) => panic!("no saved ledger at {LEDGER} ({err}): write it with CROFT_BLESS=1"),
    };
    // The hints' owner also answers for a context the ledger names that is gone.
    let mine = |o: &str| o == owner || (owner == NOT_PRESSED && !known_owner(o));
    let old: BTreeSet<&str> = saved
        .lines()
        .filter(|l| owner_of(l).is_some_and(mine))
        .collect();
    let new: BTreeSet<&str> = now.iter().map(String::as_str).collect();
    let mut moved: Vec<(&str, &str)> = old.difference(&new).map(|l| ("-", *l)).collect();
    moved.extend(new.difference(&old).map(|l| ("+", *l)));
    // A chord's old line just above its new one.
    moved.sort_by_key(|(sign, line)| {
        let context_and_chord: Vec<&str> = line.splitn(3, " | ").take(2).collect();
        (context_and_chord, *sign == "+")
    });
    let moved: Vec<String> = moved
        .iter()
        .map(|(sign, line)| format!("{sign} {line}"))
        .collect();
    assert!(
        moved.is_empty(),
        "{} ledger line(s) moved: a shortcut now does something else. If that \
         is meant, rewrite {LEDGER} with CROFT_BLESS=1.\n{}",
        moved.len(),
        moved.join("\n")
    );
}

/// Press every advertised chord in the context named `name` and check what
/// each did against the ledger.
fn check_context(name: &str) {
    let ctx = CONTEXTS.iter().find(|c| c.name == name).unwrap();
    check(name, &presses(ctx));
}

#[test]
fn the_ledger_lists_every_hint_it_does_not_press_and_why() {
    let mut lines: Vec<String> = advertised_once().1.clone();
    for (contexts, chord, why) in MACHINE_BOUND {
        lines.push(format!("{} | {chord} | {why}", contexts.join(" ")));
    }
    let lines: Vec<String> = lines
        .iter()
        .map(|l| format!("{NOT_PRESSED} | {l}"))
        .collect();
    check(NOT_PRESSED, &lines);
}

#[test]
fn every_advertised_chord_does_what_the_ledger_says_in_the_editor() {
    check_context("editor");
}

#[test]
fn every_advertised_chord_does_what_the_ledger_says_over_a_selection() {
    check_context("selection");
}

#[test]
fn every_advertised_chord_does_what_the_ledger_says_in_the_explorer() {
    check_context("explorer");
}

#[test]
fn every_advertised_chord_does_what_the_ledger_says_in_the_terminal() {
    check_context("terminal");
}

#[test]
fn every_advertised_chord_does_what_the_ledger_says_in_the_search_view() {
    check_context("search");
}

#[test]
fn every_advertised_chord_does_what_the_ledger_says_in_source_control() {
    check_context("scm");
}

#[test]
fn every_advertised_chord_does_what_the_ledger_says_in_the_command_palette() {
    check_context("palette");
}

#[test]
fn every_advertised_chord_does_what_the_ledger_says_in_a_confirm_dialog() {
    check_context("confirm");
}

/// What each key sequence did in each context, read from the saved ledger,
/// which the tests above keep true: `(context, chord)` to its effects. None
/// where a published crate's source has no saved ledger.
fn saved_presses() -> Option<BTreeMap<(String, String), String>> {
    let saved = match std::fs::read_to_string(LEDGER) {
        Ok(saved) => saved,
        Err(_) if super::golden::packaged_source() => return None,
        Err(err) => panic!("no saved ledger at {LEDGER} ({err}): write it with CROFT_BLESS=1"),
    };
    let presses = saved
        .lines()
        .filter(|line| !line.starts_with('#'))
        .map(|line| {
            let mut parts = line.splitn(3, " | ");
            let mut next = || parts.next().unwrap().to_string();
            let (context, chord, effects) = (next(), next(), next());
            ((context, chord), effects)
        })
        .collect();
    Some(presses)
}

/// `effects` without its status line, which a command run from the palette
/// may set where its chord does not (Toggle Word Wrap says "Word wrap: on").
fn without_status(effects: &str) -> String {
    let Some(at) = effects.find("status=\"") else {
        return effects.to_string();
    };
    // The value is written with `{:?}`, so a quote inside it is escaped.
    let value = &effects[at + "status=\"".len()..];
    let mut end = None;
    let mut escaped = false;
    for (i, c) in value.char_indices() {
        match c {
            '\\' if !escaped => escaped = true,
            '"' if !escaped => {
                end = Some(i);
                break;
            }
            _ => escaped = false,
        }
    }
    let end = at + "status=\"".len() + end.expect("a closed status value") + 1;
    let rest = format!("{}{}", &effects[..at], &effects[end..]);
    rest.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether pressing a command's chord did what running the command did: the
/// same effects, something happened, and at most the status line differs.
fn same_work(pressed: &str, ran: &str) -> bool {
    if ran == "-" {
        return false;
    }
    if pressed == ran {
        return true;
    }
    let (pressed, ran) = (without_status(pressed), without_status(ran));
    !ran.is_empty() && pressed == ran
}

/// What running each palette command with a chord did in `ctx`, on an app
/// set up as the ledger sets it up there. An app is kept for the next
/// command while the last one changed nothing it shows, as the ledger keeps
/// its apps, and `CROFT_LEDGER_FRESH=1` builds a new one for every command.
fn command_runs(ctx: &Context) -> Vec<(Command, String)> {
    let fresh = std::env::var("CROFT_LEDGER_FRESH").is_ok_and(|v| v == "1");
    let mut enders = Enders::default();
    let mut kept: Option<Built> = None;
    let mut runs = Vec::new();
    for cmd in crate::command_registry::ALL_COMMANDS {
        if cmd.keybinding_hint().is_empty() {
            continue;
        }
        let b = kept.get_or_insert_with(|| build(ctx));
        b.app.run_command(*cmd);
        if b.app.git_net_job.is_some() {
            wait_for_git_net(&mut b.app);
        }
        if ctx.repo {
            settle_source_control(&mut b.app, &b.root);
        }
        let fx = effects(&b.seen, &fingerprint(&b.app, &b.root));
        if fresh || fx != "-" {
            kept.take().unwrap().retire(&mut enders);
        }
        runs.push((*cmd, fx));
    }
    if let Some(done) = kept {
        done.retire(&mut enders);
    }
    runs
}

/// Writes `text` to `name` in the app's workspace and opens it, the caret at
/// `caret`.
fn open_new(app: &mut App, name: &str, text: &str, caret: (usize, usize)) {
    let path = app.workspace_root().join(name);
    std::fs::write(&path, text).unwrap();
    app.editor.open_pinned(&path).unwrap();
    (app.editor.cursor_row, app.editor.cursor_col) = caret;
    app.focus_pane(Pane::Editor);
}

/// A file eight blocks deep, so every fold level up to seven has a block.
const DEEP_RS: &str = concat!(
    "fn deep() {\n",
    "    if a {\n",
    "        if b {\n",
    "            if c {\n",
    "                if d {\n",
    "                    if e {\n",
    "                        if f {\n",
    "                            if g {\n",
    "                                done();\n",
    "                            }\n",
    "                        }\n",
    "                    }\n",
    "                }\n",
    "            }\n",
    "        }\n",
    "    }\n",
    "}\n",
);

/// A file with two merge conflicts.
const CONFLICTED: &str = concat!(
    "a\n",
    "<<<<<<< HEAD\n",
    "ours\n",
    "=======\n",
    "theirs\n",
    ">>>>>>> topic\n",
    "b\n",
    "<<<<<<< HEAD\n",
    "ours too\n",
    "=======\n",
    "theirs too\n",
    ">>>>>>> topic\n",
    "c\n",
);

/// A place the ledger's contexts do not set up, where some commands' chords
/// do their work, and the commands it proves.
struct ProofPlace {
    context: Context,
    commands: &'static [Command],
}

/// Source Control with its change list focused on the row for `path`, in
/// the ledger's repository: where the list's own keys act (#871).
fn scm_list_on(app: &mut App, path: &str) {
    app.set_sidebar_view(SidebarView::SourceControl);
    crate::test_budget::await_spawned(
        std::time::Duration::from_millis(500),
        "git status for Source Control",
        || {
            app.drain_git_responses();
            app.source_control.entries.len() == 2
        },
    );
    app.source_control.keys = crate::widgets::source_control::KeyStop::List;
    app.source_control.selected_change = app
        .source_control
        .entries
        .iter()
        .position(|e| e.path == path);
    assert!(
        app.source_control.selected_change.is_some(),
        "{path} has a row"
    );
}

/// The places beyond the ledger's own contexts.
const PROOF_PLACES: &[ProofPlace] = &[
    ProofPlace {
        context: Context {
            name: "trailing whitespace and blank lines",
            repo: false,
            setup: |app| open_new(app, "trim.rs", "fn a() {   \n}  \n\n\n\n", (0, 0)),
        },
        commands: &[Command::TrimTrailingWhitespace, Command::TrimFinalNewlines],
    },
    ProofPlace {
        context: Context {
            name: "a selection grown twice",
            repo: false,
            setup: |app| {
                app.focus_pane(Pane::Editor);
                app.run_command(Command::ExpandSelection);
                app.run_command(Command::ExpandSelection);
            },
        },
        commands: &[Command::ShrinkSelection],
    },
    ProofPlace {
        context: Context {
            name: "Find open over a selection",
            repo: false,
            setup: |app| {
                app.focus_pane(Pane::Editor);
                select(app, ON_TOTAL, TOTAL_END);
                app.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL))
                    .unwrap();
            },
        },
        commands: &[Command::ToggleFindInSelection],
    },
    ProofPlace {
        context: Context {
            name: "a file eight blocks deep",
            repo: false,
            setup: |app| open_new(app, "deep.rs", DEEP_RS, (0, 0)),
        },
        commands: &[
            Command::FoldLevel3,
            Command::FoldLevel4,
            Command::FoldLevel5,
            Command::FoldLevel6,
            Command::FoldLevel7,
        ],
    },
    ProofPlace {
        context: Context {
            name: "everything folded",
            repo: false,
            setup: |app| {
                app.focus_pane(Pane::Editor);
                app.run_command(Command::FoldAll);
            },
        },
        commands: &[Command::UnfoldAll],
    },
    ProofPlace {
        context: Context {
            name: "every region folded",
            repo: false,
            setup: |app| {
                app.focus_pane(Pane::Editor);
                app.run_command(Command::FoldAllRegions);
            },
        },
        commands: &[Command::UnfoldAllRegions],
    },
    ProofPlace {
        context: Context {
            name: "a Markdown file, the caret in a shell fence",
            repo: false,
            setup: |app| open_new(app, "run.md", "# Run\n\n```sh\necho hi\n```\n", (3, 0)),
        },
        commands: &[
            Command::OpenMarkdownPreviewToSide,
            Command::RunCodeBlockAtCursor,
        ],
    },
    ProofPlace {
        context: Context {
            name: "a pinned tab",
            repo: false,
            setup: |app| {
                app.focus_pane(Pane::Editor);
                app.run_command(Command::PinEditor);
            },
        },
        commands: &[Command::UnpinEditor],
    },
    ProofPlace {
        context: Context {
            name: "two editor groups, the right one focused",
            repo: false,
            setup: |app| {
                app.focus_pane(Pane::Editor);
                app.split_editor();
            },
        },
        commands: &[Command::FocusLeftEditorGroup],
    },
    ProofPlace {
        context: Context {
            name: "two editor groups, the left one focused",
            repo: false,
            setup: |app| {
                app.focus_pane(Pane::Editor);
                app.split_editor();
                app.focus_editor_group(true);
            },
        },
        commands: &[Command::FocusRightEditorGroup],
    },
    ProofPlace {
        context: Context {
            name: "two terminals, the panel focused",
            repo: false,
            setup: |app| {
                app.show_terminal = true;
                app.bottom_panel_tab = BottomPanelTab::Terminal;
                app.focus_pane(Pane::Terminal);
                app.split_terminal().unwrap();
            },
        },
        commands: &[Command::FocusNextTerminal, Command::FocusPreviousTerminal],
    },
    ProofPlace {
        context: Context {
            name: "a terminal that reports Shift with Ctrl (kitty keyboard protocol)",
            repo: false,
            setup: |app| {
                app.focus_pane(Pane::Editor);
                app.kitty_keys = true;
            },
        },
        commands: &[Command::ShowSourceControl],
    },
    ProofPlace {
        context: Context {
            name: "a C file beside its header",
            repo: false,
            setup: |app| {
                let header = app.workspace_root().join("pair.h");
                std::fs::write(header, "int pair(void);\n").unwrap();
                open_new(app, "pair.c", "int pair(void) { return 2; }\n", (0, 0));
            },
        },
        commands: &[Command::SwitchHeaderSource],
    },
    ProofPlace {
        context: Context {
            name: "a file with two merge conflicts",
            repo: false,
            setup: |app| open_new(app, "conflicted.txt", CONFLICTED, (0, 0)),
        },
        commands: &[Command::MergeNextConflict, Command::MergePrevConflict],
    },
    ProofPlace {
        context: Context {
            name: "Search with its query selected",
            repo: false,
            setup: |app| {
                app.set_sidebar_view(SidebarView::Search);
                app.search.query = String::from("total");
                app.search.run_query();
                app.set_sidebar_view(SidebarView::Search);
            },
        },
        commands: &[
            Command::SearchToggleMatchCase,
            Command::SearchToggleWholeWord,
            Command::SearchToggleRegex,
            Command::SearchTogglePreserveCase,
            Command::SearchToggleDetails,
        ],
    },
    ProofPlace {
        context: Context {
            name: "Source Control's change list on a new file",
            repo: true,
            setup: |app| scm_list_on(app, "todo.txt"),
        },
        commands: &[
            Command::GitOpenChanges,
            Command::GitStageChanges,
            Command::GitDiscardChanges,
        ],
    },
    ProofPlace {
        context: Context {
            name: "Source Control's change list on a staged file",
            repo: true,
            setup: |app| scm_list_on(app, "docs/notes.md"),
        },
        commands: &[Command::GitUnstageChanges],
    },
    ProofPlace {
        context: Context {
            name: "Source Control's COMMITS with the keys",
            repo: true,
            setup: |app| {
                scm_list_on(app, "todo.txt");
                app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
                    .unwrap();
            },
        },
        commands: &[Command::GitRefresh],
    },
];

/// What `run` did on an app freshly set up as `ctx`.
fn effects_in(ctx: &Context, enders: &mut Enders, run: impl FnOnce(&mut App)) -> String {
    let mut b = build(ctx);
    run(&mut b.app);
    if b.app.git_net_job.is_some() {
        wait_for_git_net(&mut b.app);
    }
    if ctx.repo {
        settle_source_control(&mut b.app, &b.root);
    }
    let fx = effects(&b.seen, &fingerprint(&b.app, &b.root));
    b.retire(enders);
    fx
}

/// What each [`PROOF_PLACES`] entry showed for each chord of each command
/// it names: the command, the chord, and `None` where pressing the chord did
/// what running the command did there, else the line saying what each did.
fn proved_in_places() -> Vec<(Command, String, Option<String>)> {
    let mut enders = Enders::default();
    let mut out = Vec::new();
    for place in PROOF_PLACES {
        for cmd in place.commands {
            let ran = effects_in(&place.context, &mut enders, |app| app.run_command(*cmd));
            for keys in linux_keys(cmd.keybinding_hint()) {
                let Ok(keys) = keys else { continue };
                let pressed = effects_in(&place.context, &mut enders, |app| {
                    for key in &keys {
                        let _ = app.handle_key(*key);
                    }
                });
                let chord = spell(&keys);
                let miss = (!same_work(&pressed, &ran)).then(|| {
                    format!(
                        "{} | {chord} | ran: {ran} | pressed: {pressed}",
                        place.context.name
                    )
                });
                out.push((*cmd, chord, miss));
            }
        }
    }
    out
}

/// Why a command's chord needs a language server to do anything.
const NEEDS_A_LANGUAGE_SERVER: &str = "works through a language server; the tests start none";
/// Why a command's chord needs a debug session.
const NEEDS_A_DEBUG_SESSION: &str = "steers a debug session; the tests start no debugger";
/// Why a merge-conflict command's chord is not the command itself.
const OPENS_THE_MERGE_PICKER: &str = "Cmd+. in a merge conflict opens the Accept Current / \
     Incoming / Both picker, whose rows run these; the chord is the way there";
/// Why a Source Control diff command's chord is not proven.
const A_KEY_IN_THE_DIFF_VIEW: &str =
    "a single key in Source Control's diff view, which no proof place opens";

/// Palette commands whose chord [`every_listed_chord_runs_its_command`]
/// cannot press where it does its work, by id, with why. One that becomes
/// proven fails the test until it is taken off, so the list only shrinks.
const UNPROVEN: &[(&str, &str)] = &[
    ("merge_accept_current", OPENS_THE_MERGE_PICKER),
    ("merge_accept_incoming", OPENS_THE_MERGE_PICKER),
    ("merge_accept_both", OPENS_THE_MERGE_PICKER),
    ("peek_references", NEEDS_A_LANGUAGE_SERVER),
    ("go_to_definition", NEEDS_A_LANGUAGE_SERVER),
    ("go_to_references", NEEDS_A_LANGUAGE_SERVER),
    ("go_to_declaration", NEEDS_A_LANGUAGE_SERVER),
    ("go_to_type_definition", NEEDS_A_LANGUAGE_SERVER),
    ("go_to_implementations", NEEDS_A_LANGUAGE_SERVER),
    ("stage_hunk", A_KEY_IN_THE_DIFF_VIEW),
    ("unstage_hunk", A_KEY_IN_THE_DIFF_VIEW),
    ("revert_hunk", A_KEY_IN_THE_DIFF_VIEW),
    ("pause_debugging", NEEDS_A_DEBUG_SESSION),
    ("continue", NEEDS_A_DEBUG_SESSION),
    ("step_over", NEEDS_A_DEBUG_SESSION),
    ("step_into", NEEDS_A_DEBUG_SESSION),
    ("step_out", NEEDS_A_DEBUG_SESSION),
    ("switch_debug_session", NEEDS_A_DEBUG_SESSION),
    (
        "toggle_inline_suggestions",
        "asks a model when ANTHROPIC_API_KEY is set (machine-bound, see MACHINE_BOUND)",
    ),
    (
        "attach_python_process",
        "lists this machine's running Python processes (machine-bound)",
    ),
    (
        "update_croft",
        "checks for a croft update over the network (machine-bound)",
    ),
    (
        "zoxide_jump",
        "jumps with zoxide, installed or not (machine-bound)",
    ),
    ("http_send_request", "sends a real HTTP request"),
    (
        "hex_find_next",
        "F3 in the hex view with its find open, which no proof place opens",
    ),
];

/// Every chord croft lists for a palette command runs that command: pressed
/// where the command does its work, it does what the palette's run of the
/// command does there (the status line may differ), and something happens.
/// The places are the key ledger's contexts, whose saved presses this reads,
/// and [`PROOF_PLACES`]. The palette and confirm contexts are left out: there
/// a chord goes to the open palette or dialog. [`UNPROVEN`] names the
/// commands no place reaches, with why.
#[test]
fn every_listed_chord_runs_its_command() {
    use crate::command_registry::ALL_COMMANDS;
    // Blessing rewrites the saved ledger while this reads it: check after.
    if std::env::var("CROFT_BLESS").is_ok_and(|v| v == "1") {
        eprintln!("skipped while CROFT_BLESS=1 rewrites the ledger: run again without it");
        return;
    }
    let Some(saved) = saved_presses() else {
        eprintln!("skipped, a published crate's source has no saved ledger");
        return;
    };
    let contexts: Vec<&Context> = CONTEXTS
        .iter()
        .filter(|c| !matches!(c.name, "palette" | "confirm"))
        .collect();
    // One context after another, as the ledger's own tests build one app at
    // a time: apps built side by side here are load the suite's wait budgets
    // do not count, and under CI's load Source Control's git status outran
    // its budget.
    let runs: Vec<(&str, Vec<(Command, String)>)> = contexts
        .iter()
        .map(|ctx| (ctx.name, command_runs(ctx)))
        .collect();
    let places = proved_in_places();
    let mut unproven: Vec<String> = Vec::new();
    let mut proven: Vec<&str> = Vec::new();
    for cmd in ALL_COMMANDS {
        let hint = cmd.keybinding_hint();
        if hint.is_empty() {
            continue;
        }
        // Every chord the hint lists must run the command somewhere.
        let mut lines = Vec::new();
        let mut done = true;
        let mut pressed_any = false;
        for keys in linux_keys(hint) {
            let Ok(keys) = keys else { continue };
            pressed_any = true;
            let chord = spell(&keys);
            let mut this_one = false;
            for (context, list) in &runs {
                let ran = &list.iter().find(|(c, _)| c == cmd).unwrap().1;
                let pressed = saved.get(&(context.to_string(), chord.clone()));
                let pressed = pressed.map_or("(not pressed there)", String::as_str);
                this_one |= same_work(pressed, ran);
                lines.push(format!(
                    "  {context} | {chord} | ran: {ran} | pressed: {pressed}"
                ));
            }
            for (_, _, miss) in places.iter().filter(|(c, k, _)| c == cmd && *k == chord) {
                match miss {
                    None => this_one = true,
                    Some(line) => lines.push(format!("  {line}")),
                }
            }
            done &= this_one;
        }
        if done && pressed_any {
            proven.push(cmd.id());
        } else if !UNPROVEN.iter().any(|(id, _)| *id == cmd.id()) {
            unproven.push(format!("{} [{hint}]\n{}", cmd.id(), lines.join("\n")));
        }
    }
    let stale: Vec<&str> = UNPROVEN
        .iter()
        .map(|(id, _)| *id)
        .filter(|id| {
            proven.contains(id)
                || !ALL_COMMANDS
                    .iter()
                    .any(|c| c.id() == *id && !c.keybinding_hint().is_empty())
        })
        .collect();
    assert!(
        stale.is_empty(),
        "UNPROVEN lists commands that are proven now or list no chord; take them off: {stale:?}"
    );
    assert!(
        unproven.is_empty(),
        "{} palette command(s) list a chord that does not run them where they work. \
         Fix the chord or the hint, add a PROOF_PLACES entry that sets up where it \
         works, or list it in UNPROVEN with why:\n{}",
        unproven.len(),
        unproven.join("\n")
    );
}

/// Every key sequence the F1 sheet lists, spelled as the ledger spells it.
fn f1_chords() -> BTreeSet<String> {
    let mut chords = BTreeSet::new();
    for group in crate::widgets::shortcuts::SHORTCUT_GROUPS {
        for entry in group.entries {
            if unpressed_f1_row(group.title, entry.keys).is_none() {
                for keys in linux_keys(entry.keys).into_iter().flatten() {
                    chords.insert(spell(&keys));
                }
            }
        }
    }
    chords
}

/// The chords written in backticks in `text`, one per run of backticked
/// keys: `` `Cmd`+`Opt`+`S` ``, `` `Cmd+K` `L` `` and `` `Enter`/`F3` `` are
/// one run each. `` `Cmd+K` `Cmd+1` … `Cmd+K` `Cmd+7` `` is the range
/// `Cmd+K Cmd+1..7`.
fn backticked_chords(text: &str) -> Vec<String> {
    // Each run of backticked keys, with the text before it.
    let mut runs: Vec<(&str, String)> = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('`') {
        let Some(len) = rest[open + 1..].find('`') else {
            break;
        };
        let between = &rest[..open];
        let span = &rest[open + 1..open + 1 + len];
        match runs.last_mut() {
            // `Cmd`+`Opt`+`Shift`+`U` / `L`: the bare key takes the modifiers.
            Some((_, run)) if matches!(between, "+" | "/" | " " | " / ") => {
                run.push_str(between);
                run.push_str(span);
            }
            // `Ctrl+p` / `Cmd+p`, then `#`: each chord, then the key.
            Some((_, run)) if matches!(between, ", then " | " then ") => {
                *run = run
                    .split(" / ")
                    .map(|chord| format!("{chord} then {span}"))
                    .collect::<Vec<_>>()
                    .join(" / ");
            }
            _ => runs.push((between, span.to_string())),
        }
        rest = &rest[open + len + 2..];
    }
    let mut out: Vec<String> = Vec::new();
    for (between, run) in runs {
        // `X1 … X7`, the same keys but the last digit, is the range X1..7.
        if between == " … "
            && let Some(prev) = out.last_mut()
            && let (Some(a), Some(b)) = (prev.chars().last(), run.chars().last())
            && a.is_ascii_digit()
            && b.is_ascii_digit()
            && prev[..prev.len() - 1] == run[..run.len() - 1]
        {
            prev.push_str(&format!("..{b}"));
        } else {
            out.push(run);
        }
    }
    out
}

/// Every key sequence docs/KEYBINDINGS.md writes in backticks, spelled as the
/// ledger spells it. Spans that name no keys (a setting, a file) are skipped.
fn documented_chords() -> BTreeSet<String> {
    let doc =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/KEYBINDINGS.md"))
            .unwrap();
    let mut chords = BTreeSet::new();
    for line in doc.lines() {
        for run in backticked_chords(line) {
            for alt in alternatives(&run) {
                // The docs write some letters small (`Cmd+Shift+s`); hints
                // and the Linux spelling rules write them capital.
                let alt: String = alt
                    .split(' ')
                    .map(|step| match step.rsplit_once('+') {
                        Some((mods, key)) if key.len() == 1 => {
                            format!("{mods}+{}", key.to_ascii_uppercase())
                        }
                        _ => step.to_string(),
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                let linux = crate::command_registry::hint_for_platform(&alt, false);
                if let Ok(keys) = keys_of(&linux) {
                    chords.insert(spell(&keys));
                }
            }
        }
    }
    chords
}

/// A chord in a palette hint is where people look for it: the F1 sheet lists
/// it, and so does docs/KEYBINDINGS.md.
#[test]
fn every_palette_chord_is_on_the_f1_sheet_and_in_the_docs() {
    let f1 = f1_chords();
    let docs = documented_chords();
    let mut missing = Vec::new();
    for cmd in crate::command_registry::ALL_COMMANDS {
        let hint = cmd.keybinding_hint();
        for keys in linux_keys(hint).into_iter().flatten() {
            let chord = spell(&keys);
            let gone: Vec<&str> = [("the F1 sheet", &f1), ("docs/KEYBINDINGS.md", &docs)]
                .into_iter()
                .filter(|(_, listed)| !listed.contains(&chord))
                .map(|(place, _)| place)
                .collect();
            if !gone.is_empty() {
                missing.push(format!(
                    "{} [{hint}] {chord}: not in {}",
                    cmd.id(),
                    gone.join(" or ")
                ));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "{} palette chord(s) missing where people look for them:\n{}",
        missing.len(),
        missing.join("\n")
    );
}

#[test]
fn backticked_keys_read_as_the_chords_they_spell() {
    assert_eq!(
        backticked_chords("| `Cmd`+`Opt`+`Shift`+`S` | x |"),
        ["Cmd+Opt+Shift+S"]
    );
    assert_eq!(
        backticked_chords("`Cmd+K` `L` / `Ctrl+K` `L`"),
        ["Cmd+K L / Ctrl+K L"]
    );
    assert_eq!(backticked_chords("`Enter`/`F3` forward"), ["Enter/F3"]);
    assert_eq!(
        backticked_chords("| `Cmd`+`Opt`+`Shift`+`U` / `L` |"),
        ["Cmd+Opt+Shift+U / L"]
    );
    assert_eq!(
        backticked_chords("`Ctrl+p` / `Cmd+p`, then `#`"),
        ["Ctrl+p then # / Cmd+p then #"]
    );
    assert_eq!(
        backticked_chords("| `Cmd+K` `Cmd+1` … `Cmd+K` `Cmd+7` | Fold |"),
        ["Cmd+K Cmd+1..7"]
    );
    assert_eq!(
        backticked_chords("set `tab_size`, and `Esc`"),
        ["tab_size", "Esc"]
    );
}
