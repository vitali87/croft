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

/// What the ledger reads off an app: values by name, then what is open or
/// on, `None` while it is not, with a label where one tells it apart.
struct Seen {
    values: Vec<(&'static str, String)>,
    flags: Vec<(&'static str, Option<String>)>,
}

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
    let mut inactive = app.editor_layout.inactive_groups().into_iter();
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
/// is while it is focused, the replace and file fields while shown, the
/// options on, and the hits with the one selected.
fn search_view(s: &crate::widgets::search::SearchPanel) -> String {
    let mut out = format!("{:?}", s.query);
    if s.focused {
        out.push_str(&format!(" @{:?}:{}", s.field, s.caret()));
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
    if sc.list_focused {
        out.push_str(" +list");
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
/// commit's id is left out: it changes with the clock.
fn git_state(root: &Path) -> String {
    if !root.join(".git").exists() {
        return String::from("-");
    }
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
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
        no_commit_ids(
            &news
                .iter()
                .fold(s.to_string(), |s, (from, to)| s.replace(from, to)),
        )
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
    Seen {
        values: vec![
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
            ("tree", explorer(&app.tree, root)),
            ("search", search_view(&app.search)),
            ("scm", scm_view(&app.source_control)),
            ("git", git_state(root)),
            ("files", files_digest(root)),
        ],
        flags: vec![
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
            ("scm_menu", on(app.scm_menu.open)),
            ("commit_menu", on(app.commit_menu_open)),
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
    }
}

/// What a key changed: `name=value` for a value, `+name` (with its label) for
/// what opened or came on, `-name` for what closed or went off, and
/// `~name(label)` for an open one whose label changed. `-` when nothing did.
fn effects(before: &Seen, after: &Seen) -> String {
    let values = before.values.iter().zip(&after.values);
    let mut changed: Vec<String> = values
        .filter(|((_, b), (_, a))| b != a)
        .map(|((name, _), (_, a))| format!("{name}={a}"))
        .collect();
    let flags = before.flags.iter().zip(&after.flags);
    changed.extend(
        flags
            .filter(|((_, b), (_, a))| b != a)
            .map(|((name, b), (_, a))| match (b, a.as_deref()) {
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
