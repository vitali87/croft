//! Localization (#621): a catalog of UI strings keyed by their English text.
//!
//! Menus, command palette titles and status messages pass through [`tr`].
//! With no catalog loaded (English, or no locale set) it returns its input
//! unchanged at the cost of one atomic load, so untranslated croft pays for
//! nothing.
//!
//! A catalog is a JSON object mapping English to the translation. A key
//! with one `{}` is a pattern: `"Saved {}": "Gespeichert: {}"` translates
//! every status that starts with `Saved ` and carries the variable part
//! across. The locale is the `locale` setting, else `LC_ALL`,
//! `LC_MESSAGES` or `LANG` (`de_DE.UTF-8` reads as `de`); the environment
//! only switches to a language whose catalog is near complete, or that the
//! user has a catalog file for (#1629). Built-in
//! catalogs ship in `assets/i18n/`; a file at `<config>/locales/<lang>.json`
//! adds to or overrides one, which is also how a new language starts.
//! `croft locale-template <lang>` prints every translatable string, and
//! `--write` brings `locales/<lang>.json` up to date in place.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::OnceLock;

/// Catalogs compiled in, by language code.
const BUILT_IN: &[(&str, &str)] = &[
    ("de", include_str!("../assets/i18n/de.json")),
    ("es", include_str!("../assets/i18n/es.json")),
];

#[derive(Default)]
pub struct Catalog {
    exact: HashMap<String, String>,
    /// `(prefix, suffix, translation)` for keys holding one `{}`.
    patterns: Vec<(String, String, String)>,
}

impl Catalog {
    /// Merge `json` (an object of English to translation) into this one;
    /// later entries win. Malformed text is ignored.
    pub fn add_json(&mut self, json: &str) {
        let Ok(map) = serde_json::from_str::<HashMap<String, String>>(json) else {
            return;
        };
        for (k, v) in map {
            if v.is_empty() {
                continue;
            }
            match k.split_once("{}") {
                Some((pre, suf)) if !suf.contains("{}") => {
                    self.patterns
                        .retain(|(p, s, _)| (p, s) != (&pre.to_string(), &suf.to_string()));
                    self.patterns.push((pre.to_string(), suf.to_string(), v));
                }
                _ => {
                    self.exact.insert(k, v);
                }
            }
        }
        // Longest fixed text first, so the most specific pattern wins.
        self.patterns
            .sort_by_key(|(p, s, _)| std::cmp::Reverse(p.len() + s.len()));
    }

    pub fn is_empty(&self) -> bool {
        self.exact.is_empty() && self.patterns.is_empty()
    }

    pub fn lookup<'a>(&self, s: &'a str) -> Cow<'a, str> {
        if let Some(t) = self.exact.get(s) {
            return Cow::Owned(t.clone());
        }
        for (pre, suf, t) in &self.patterns {
            if s.len() > pre.len() + suf.len()
                && s.starts_with(pre.as_str())
                && s.ends_with(suf.as_str())
            {
                let middle = &s[pre.len()..s.len() - suf.len()];
                return Cow::Owned(t.replacen("{}", middle, 1));
            }
        }
        Cow::Borrowed(s)
    }
}

static CATALOG: OnceLock<Catalog> = OnceLock::new();

/// The language code for a locale string: `de_DE.UTF-8` -> `de`. `C`,
/// `POSIX` and English read as `None` (no translation).
pub fn language_of(locale: &str) -> Option<String> {
    let lang = locale
        .split(['_', '.', '@', '-'])
        .next()?
        .trim()
        .to_ascii_lowercase();
    if lang.is_empty() || lang == "c" || lang == "posix" || lang == "en" {
        return None;
    }
    lang.chars()
        .all(|c| c.is_ascii_alphabetic())
        .then_some(lang)
}

/// The language croft should use: the setting, else the environment.
pub fn pick_language(
    setting: Option<&str>,
    env: &dyn Fn(&str) -> Option<String>,
) -> Option<String> {
    if let Some(s) = setting.filter(|s| !s.trim().is_empty()) {
        return language_of(s);
    }
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|k| env(k).filter(|v| !v.is_empty()))
        .next()
        .and_then(|v| language_of(&v))
}

/// Build the catalog for `lang` from the built-in one plus the user's file.
pub fn catalog_for(lang: &str, user_file: Option<&str>) -> Catalog {
    let mut c = Catalog::default();
    if let Some((_, json)) = BUILT_IN.iter().find(|(l, _)| *l == lang) {
        c.add_json(json);
    }
    if let Some(json) = user_file {
        c.add_json(json);
    }
    c
}

/// The share of [`template`]'s strings a built-in catalog must translate
/// before croft switches to it just because the system locale names its
/// language (#1629). Below it the UI would flip language from one palette
/// row to the next, so it stays English until the user asks.
pub const FOLLOW_ENVIRONMENT_COVERAGE: f64 = 0.95;

/// How many of the strings `croft locale-template` lists the built-in
/// catalog for `lang` translates, and how many it lists.
pub fn built_in_coverage(lang: &str) -> (usize, usize) {
    let catalog = catalog_for(lang, None);
    let keys = template_keys(TRANSLATABLE, None);
    let done = keys
        .iter()
        .filter(|k| catalog.lookup(k) != k.as_str())
        .count();
    (done, keys.len())
}

/// The catalog croft starts with, if any. An explicit `locale` setting
/// always loads its catalog. A language taken from the environment loads
/// only when the user has a catalog file for it (they opted in) or the
/// built-in catalog is near complete (#1629): a starter catalog would mix
/// two languages on every screen of someone who never asked for it.
pub fn startup_catalog(
    setting: Option<&str>,
    env: &dyn Fn(&str) -> Option<String>,
    user_file_for: &dyn Fn(&str) -> Option<String>,
) -> Option<Catalog> {
    let lang = pick_language(setting, env)?;
    let explicit = setting.is_some_and(|s| !s.trim().is_empty());
    let user = user_file_for(&lang);
    if !explicit && user.is_none() {
        let (done, total) = built_in_coverage(&lang);
        if total == 0 || (done as f64) < FOLLOW_ENVIRONMENT_COVERAGE * total as f64 {
            return None;
        }
    }
    Some(catalog_for(&lang, user.as_deref())).filter(|c| !c.is_empty())
}

/// Load the catalog once at startup. Later calls are no-ops.
pub fn init(setting: Option<&str>) {
    let user_file_for = |lang: &str| {
        std::fs::read_to_string(
            crate::prefs::config_dir()
                .join("locales")
                .join(format!("{lang}.json")),
        )
        .ok()
    };
    if let Some(catalog) = startup_catalog(setting, &|k| std::env::var(k).ok(), &user_file_for) {
        let _ = CATALOG.set(catalog);
    }
}

/// `s` in the active language, or `s` itself.
pub fn tr(s: &str) -> Cow<'_, str> {
    match CATALOG.get() {
        Some(c) => c.lookup(s),
        None => Cow::Borrowed(s),
    }
}

/// Whether a catalog is active.
pub fn active() -> bool {
    CATALOG.get().is_some()
}

/// Every string [`tr`] translates outside the command palette, in English:
/// context-menu labels (`ContextMenu::localize` runs each one through `tr`)
/// and the literals handed to `tr` directly. The one list of them:
/// `croft locale-template` passes it as `extra`, so a new language's
/// template offers every one, and a test reads the sources and fails on any
/// such string that is neither here nor a palette title (#849).
pub const TRANSLATABLE: &[&str] = &[
    "Add Folder to Workspace",
    "Ask Navigator",
    "Ask Navigator About Selection",
    "Ask Navigator About This Line",
    "blank",
    "Change All Occurrences",
    "Close",
    "Close All",
    "Close Others",
    "Close Saved",
    "Close to the Right",
    "CodeQL",
    "Color Theme",
    "Command Palette",
    "Compare Selected",
    "Copy",
    "Copy into New Window",
    "Copy Path",
    "Copy Relative Path",
    "Customize Layout",
    "Cut",
    "Editor",
    "Explorer",
    "Extensions",
    "Fix with Navigator",
    "Focus Left Group",
    "Focus Right Group",
    "Go to Symbol",
    "Keep Open",
    "Line",
    "Make root",
    "Menu",
    "Move Above",
    "Move Below",
    "Move into New Window",
    "Move Left",
    "Move Right",
    "New File",
    "New Folder",
    "Open Symbol in Its Own Tab",
    "Output",
    "Panel Alignment",
    "Paste",
    "Pin",
    "Primary Side Bar Position",
    "Quick Input Position",
    "Remote",
    "Remove Folder from Workspace",
    "Rename",
    "Reveal in Explorer View",
    "Reveal in Finder",
    "Run and Debug",
    "Search",
    "Select for Compare",
    "Show Incoming Calls",
    "Show Outgoing Calls",
    "Side",
    "Source Control",
    "Split & Move",
    "Split Down",
    "Split in Group",
    "Split Left",
    "Split Right",
    "Split Up",
    "Terminal",
    "Testing",
    "Unpin",
    "Untitled",
];

/// A JSON template for translating into `lang`: every palette title, every
/// key of every built-in catalog, the given extra strings and every key of
/// the user's own file, each mapped to the user's translation, else the
/// built-in one, else `""`.
///
/// The keys come from ALL the built-in catalogs, not only `lang`'s: they are
/// the one list of the strings `tr` translates outside the palette (Close,
/// New File, `Saved {}`, ...), so a language with no catalog of its own got
/// none of them (#849).
pub fn template(lang: &str, user_file: Option<&str>, extra: &[&str]) -> String {
    let catalog = catalog_for(lang, None);
    let user: serde_json::Map<String, serde_json::Value> = user_file
        .and_then(|json| serde_json::from_str(json).ok())
        .unwrap_or_default();
    let keys = template_keys(extra, Some(&user));
    let map: serde_json::Map<String, serde_json::Value> = keys
        .into_iter()
        .map(|k| {
            let own = user
                .get(&k)
                .and_then(|v| v.as_str())
                .filter(|v| !v.is_empty());
            let v = match own {
                Some(v) => v.to_string(),
                None => {
                    let v = catalog.lookup(&k);
                    if v == k {
                        String::new()
                    } else {
                        v.into_owned()
                    }
                }
            };
            (k, serde_json::Value::String(v))
        })
        .collect();
    serde_json::to_string_pretty(&serde_json::Value::Object(map)).unwrap_or_default()
}

/// The strings a template lists, sorted and once each: every palette title,
/// `extra`, every key of every built-in catalog and of the user's file.
fn template_keys(
    extra: &[&str],
    user: Option<&serde_json::Map<String, serde_json::Value>>,
) -> Vec<String> {
    let built_in_keys = BUILT_IN.iter().flat_map(|(_, json)| {
        serde_json::from_str::<HashMap<String, String>>(json)
            .map(|map| map.into_keys().collect::<Vec<_>>())
            .unwrap_or_default()
    });
    let mut keys: Vec<String> = crate::widgets::command_palette::ALL_COMMANDS
        .iter()
        .map(|c| c.title().to_string())
        .chain(extra.iter().map(|s| s.to_string()))
        .chain(built_in_keys)
        .chain(user.into_iter().flat_map(|u| u.keys().cloned()))
        .collect();
    keys.sort();
    keys.dedup();
    keys
}

/// `croft locale-template <lang> --write` (#1148): bring the translation at
/// `path` up to date in place, keeping every value already in it and adding
/// the strings it lacks as `""`. A file that isn't a JSON object of strings
/// is refused, never replaced. The new file goes to a sibling of its own,
/// created with the old file's permissions, and is renamed over the old
/// one, so a failed write leaves the old file whole. Returns how many
/// strings the file holds and how many are still untranslated.
pub fn write_template(path: &std::path::Path, lang: &str) -> std::io::Result<(usize, usize)> {
    use std::io::{Error, ErrorKind};
    let existing = match std::fs::read_to_string(path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    let existing = existing.filter(|t| !t.trim().is_empty());
    if let Some(text) = &existing
        && serde_json::from_str::<HashMap<String, String>>(text).is_err()
    {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!(
                "{} isn't a JSON object of strings; fix it or move it aside, then run this again",
                path.display()
            ),
        ));
    }
    let text = template(lang, existing.as_deref(), TRANSLATABLE);
    let map: HashMap<String, String> = serde_json::from_str(&text).unwrap_or_default();
    let todo = map.values().filter(|v| v.is_empty()).count();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Each run writes its own sibling, created fresh: a shared name let an
    // overlapping run rename this one's half-written file into place.
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".{}.croft-tmp", std::process::id()));
    let tmp = std::path::PathBuf::from(tmp);
    let written = crate::prefs::write_keeping_mode(&tmp, path, format!("{text}\n").as_bytes())
        .and_then(|()| std::fs::rename(&tmp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written.map(|()| (map.len(), todo))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_write_leaves_another_runs_temp_file_alone() {
        // Two overlapping `--write` runs once shared `fr.json.croft-tmp`:
        // one renamed it over the translation while the other was still
        // writing into it.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fr.json");
        std::fs::write(&path, "{\"File: Save\": \"Enregistrer\"}").unwrap();
        let other = dir.path().join("fr.json.croft-tmp");
        std::fs::write(&other, "half written by another run").unwrap();
        write_template(&path, "fr").unwrap();
        assert_eq!(
            std::fs::read_to_string(&other).unwrap(),
            "half written by another run"
        );
        let map: HashMap<String, String> =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(map["File: Save"], "Enregistrer");
    }

    #[cfg(unix)]
    #[test]
    fn a_write_never_follows_a_link_planted_at_its_temp_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fr.json");
        let target = dir.path().join("elsewhere");
        std::fs::write(&target, "untouched").unwrap();
        for name in [
            "fr.json.croft-tmp".to_string(),
            format!("fr.json.{}.croft-tmp", std::process::id()),
        ] {
            std::os::unix::fs::symlink(&target, dir.path().join(name)).unwrap();
        }
        write_template(&path, "fr").unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "untouched");
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("File: Save")
        );
    }

    #[test]
    fn a_write_leaves_no_temp_file_behind() {
        // Negative: the temp file is the one renamed into place.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fr.json");
        write_template(&path, "fr").unwrap();
        write_template(&path, "fr").unwrap();
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["fr.json"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_write_keeps_the_files_permissions() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fr.json");
        std::fs::write(&path, "{}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        write_template(&path, "fr").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn locales_reduce_to_a_language_and_english_means_none() {
        assert_eq!(language_of("de_DE.UTF-8").as_deref(), Some("de"));
        assert_eq!(language_of("es").as_deref(), Some("es"));
        assert_eq!(language_of("pt-BR").as_deref(), Some("pt"));
        assert_eq!(language_of("en_US.UTF-8"), None);
        assert_eq!(language_of("C.UTF-8"), None);
        assert_eq!(language_of("POSIX"), None);
    }

    #[test]
    fn the_setting_wins_over_the_environment() {
        let env = |k: &str| match k {
            "LANG" => Some("es_ES.UTF-8".to_string()),
            "LC_ALL" => Some(String::new()),
            _ => None,
        };
        assert_eq!(pick_language(None, &env).as_deref(), Some("es"));
        assert_eq!(pick_language(Some("de"), &env).as_deref(), Some("de"));
        assert_eq!(pick_language(Some("en"), &env), None);
    }

    /// A German desktop with no `locale` setting.
    fn german_env(k: &str) -> Option<String> {
        (k == "LANG").then(|| String::from("de_DE.UTF-8"))
    }

    fn no_user_file(_: &str) -> Option<String> {
        None
    }

    fn translated(c: &Option<Catalog>, s: &str) -> String {
        c.as_ref()
            .map_or(s.to_string(), |c| c.lookup(s).into_owned())
    }

    /// #1629: the system locale alone no longer switches on a starter
    /// catalog, so a German desktop gets one language, English, not a palette
    /// that flips between "Datei: Speichern" and "View: Pin Editor".
    #[test]
    fn the_environment_does_not_switch_on_a_partial_catalog() {
        let c = startup_catalog(None, &german_env, &no_user_file);
        assert!(c.is_none(), "an 18% catalog must not load from LANG alone");
        assert_eq!(translated(&c, "File: Save"), "File: Save");
        assert_eq!(translated(&c, "View: Pin Editor"), "View: Pin Editor");
        let spanish = |k: &str| (k == "LC_ALL").then(|| String::from("es_ES.UTF-8"));
        assert!(startup_catalog(Some(""), &spanish, &no_user_file).is_none());
    }

    /// Asking for the language by name still loads the starter catalog.
    #[test]
    fn the_locale_setting_still_loads_a_partial_catalog() {
        let c = startup_catalog(Some("de"), &german_env, &no_user_file);
        assert_eq!(translated(&c, "File: Save"), "Datei: Speichern");
        let c = startup_catalog(Some("es"), &|_| None, &no_user_file);
        assert!(c.is_some());
    }

    /// A catalog file of the user's own for the environment's language is
    /// opting in, so it loads, over the built-in entries.
    #[test]
    fn a_user_catalog_for_the_environments_language_is_honoured() {
        let user = |lang: &str| {
            (lang == "de")
                .then(|| String::from(r#"{"View: Pin Editor": "Ansicht: Editor anheften"}"#))
        };
        let c = startup_catalog(None, &german_env, &user);
        assert_eq!(
            translated(&c, "View: Pin Editor"),
            "Ansicht: Editor anheften"
        );
        assert_eq!(translated(&c, "File: Save"), "Datei: Speichern");
    }

    /// The gate is coverage of what the template lists, and the starter
    /// catalogs are well under it; a complete catalog would pass.
    #[test]
    fn coverage_is_measured_against_the_template() {
        for lang in ["de", "es"] {
            let (done, total) = built_in_coverage(lang);
            assert!(done > 0 && total > done, "{lang}: {done}/{total}");
            assert!(
                (done as f64) < FOLLOW_ENVIRONMENT_COVERAGE * total as f64,
                "{lang} is a starter catalog: {done}/{total}"
            );
        }
        assert_eq!(built_in_coverage("fr").0, 0, "no French catalog ships");
        let total = built_in_coverage("de").1;
        let template: HashMap<String, String> =
            serde_json::from_str(&template("de", None, TRANSLATABLE)).unwrap();
        assert_eq!(
            template.len(),
            total,
            "the same strings locale-template lists"
        );
    }

    /// Negative: English, C and no locale at all load nothing either way.
    #[test]
    fn english_and_unset_locales_load_nothing() {
        for value in ["en_US.UTF-8", "C", "POSIX"] {
            let env = |k: &str| (k == "LANG").then(|| value.to_string());
            assert!(
                startup_catalog(None, &env, &no_user_file).is_none(),
                "{value}"
            );
        }
        assert!(startup_catalog(None, &|_| None, &no_user_file).is_none());
        assert!(startup_catalog(Some("en"), &german_env, &no_user_file).is_none());
    }

    #[test]
    fn exact_entries_and_patterns_translate_and_the_rest_passes_through() {
        let mut c = Catalog::default();
        c.add_json(r#"{"New File": "Neue Datei", "Saved {}": "Gespeichert: {}", "Empty": ""}"#);
        assert_eq!(c.lookup("New File"), "Neue Datei");
        assert_eq!(c.lookup("Saved main.rs"), "Gespeichert: main.rs");
        assert_eq!(
            c.lookup("Saved "),
            "Saved ",
            "a pattern needs its variable part"
        );
        assert_eq!(
            c.lookup("Empty"),
            "Empty",
            "an empty translation is untranslated"
        );
        assert_eq!(c.lookup("Something else"), "Something else");
    }

    #[test]
    fn a_user_file_overrides_the_built_in_catalog() {
        let c = catalog_for("de", Some(r#"{"New File": "Datei anlegen"}"#));
        assert_eq!(c.lookup("New File"), "Datei anlegen");
        assert_ne!(
            c.lookup("New Folder"),
            "New Folder",
            "built-in entries remain"
        );
    }

    #[test]
    fn built_in_catalogs_parse_and_have_matching_placeholders() {
        for (lang, json) in BUILT_IN {
            let map: HashMap<String, String> =
                serde_json::from_str(json).unwrap_or_else(|e| panic!("{lang}: {e}"));
            assert!(!map.is_empty(), "{lang} is empty");
            for (k, v) in &map {
                assert_eq!(
                    k.matches("{}").count(),
                    v.matches("{}").count(),
                    "{lang}: {k} -> {v}"
                );
            }
        }
    }

    #[test]
    fn the_template_lists_palette_titles_with_known_translations_filled() {
        let t = template("de", None, &["New File"]);
        let map: HashMap<String, String> = serde_json::from_str(&t).unwrap();
        assert_eq!(map.get("New File").map(String::as_str), Some("Neue Datei"));
        assert!(map.contains_key("File: Save"));
    }

    /// #849: a language with no built-in catalog got the palette titles
    /// only, missing every other string `tr` translates (Close, New File,
    /// `Saved {}`, ...), since those keys came from `lang`'s own catalog.
    #[test]
    fn a_new_languages_template_lists_every_built_in_key() {
        let fresh: HashMap<String, String> =
            serde_json::from_str(&template("xx", None, &[])).unwrap();
        for (lang, json) in BUILT_IN {
            let map: HashMap<String, String> = serde_json::from_str(json).unwrap();
            for key in map.keys() {
                assert!(
                    fresh.contains_key(key),
                    "{lang}'s {key:?} is missing from a new language's template"
                );
            }
        }
        assert!(
            fresh.values().all(String::is_empty),
            "a language with no catalog has nothing to fill in"
        );
        let de: HashMap<String, String> = serde_json::from_str(&template("de", None, &[])).unwrap();
        assert_eq!(
            de.len(),
            fresh.len(),
            "every language's template lists the same strings"
        );
    }

    /// #849 guard: `TRANSLATABLE` holds what the palette does not. A palette
    /// title listed again, or an entry listed twice, is a second copy to keep
    /// in step, so neither is allowed; nor is a blank or padded entry.
    #[test]
    fn translatable_repeats_no_palette_title_and_no_entry() {
        let palette: std::collections::HashSet<&str> =
            crate::widgets::command_palette::ALL_COMMANDS
                .iter()
                .map(|c| c.title())
                .collect();
        let mut seen = std::collections::HashSet::new();
        for s in TRANSLATABLE {
            assert!(seen.insert(*s), "{s:?} is listed twice");
            assert!(
                !palette.contains(s),
                "{s:?} is a palette title, already in every template"
            );
            assert!(!s.is_empty() && s.trim() == *s, "{s:?} is blank or padded");
        }
    }

    /// #849 guard: offering every translatable string prefills nothing it
    /// should not. A new language's template is all blanks, and a built-in
    /// language prefills an entry only from its own exact translation, never
    /// from a pattern: `Opened {}` would otherwise fill in a label that
    /// merely starts with `Opened `.
    #[test]
    fn translatable_strings_are_offered_blank_unless_the_catalog_has_them() {
        let fresh: HashMap<String, String> =
            serde_json::from_str(&template("xx", None, TRANSLATABLE)).unwrap();
        for s in TRANSLATABLE {
            assert_eq!(fresh.get(*s).map(String::as_str), Some(""), "{s:?}");
        }
        assert!(fresh.values().all(String::is_empty));
        for (lang, json) in BUILT_IN {
            let own: HashMap<String, String> = serde_json::from_str(json).unwrap();
            let t: HashMap<String, String> =
                serde_json::from_str(&template(lang, None, TRANSLATABLE)).unwrap();
            for s in TRANSLATABLE {
                let want = own
                    .get(*s)
                    .filter(|v| v.as_str() != *s)
                    .map_or("", String::as_str);
                assert_eq!(t.get(*s).map(String::as_str), Some(want), "{lang}: {s:?}");
            }
        }
    }

    /// #849 guard: the keys come from every built-in catalog, the values
    /// only from `lang`'s own. German's template carries German, never
    /// another catalog's translation of the same string, and a string only
    /// another language translates is offered blank.
    #[test]
    fn a_template_fills_in_only_its_own_languages_translations() {
        for (lang, json) in BUILT_IN {
            let own: HashMap<String, String> = serde_json::from_str(json).unwrap();
            let t: HashMap<String, String> =
                serde_json::from_str(&template(lang, None, &[])).unwrap();
            for (key, translation) in &own {
                // A translation spelled like the English is offered blank.
                let want = if translation == key { "" } else { translation };
                assert_eq!(
                    t.get(key).map(String::as_str),
                    Some(want),
                    "{lang}'s own {key:?}"
                );
            }
            for (other, json) in BUILT_IN.iter().filter(|(l, _)| l != lang) {
                let theirs: HashMap<String, String> = serde_json::from_str(json).unwrap();
                for key in theirs.keys().filter(|k| !own.contains_key(*k)) {
                    assert_eq!(
                        t.get(key).map(String::as_str),
                        Some(""),
                        "{lang}'s template must offer {other}'s {key:?} blank"
                    );
                }
            }
        }
    }
}
