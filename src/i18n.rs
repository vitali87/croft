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
//! `LC_MESSAGES` or `LANG` (`de_DE.UTF-8` reads as `de`). Built-in
//! catalogs ship in `assets/i18n/`; a file at `<config>/locales/<lang>.json`
//! adds to or overrides one, which is also how a new language starts.
//! `croft locale-template <lang>` prints every translatable string.

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

/// Load the catalog once at startup. Later calls are no-ops.
pub fn init(setting: Option<&str>) {
    let Some(lang) = pick_language(setting, &|k| std::env::var(k).ok()) else {
        return;
    };
    let user = std::fs::read_to_string(
        crate::prefs::config_dir()
            .join("locales")
            .join(format!("{lang}.json")),
    )
    .ok();
    let catalog = catalog_for(&lang, user.as_deref());
    if !catalog.is_empty() {
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

/// A JSON template for translating into `lang`: every palette title plus the
/// given extra strings, each mapped to its existing translation or `""`.
pub fn template(lang: &str, extra: &[&str]) -> String {
    let catalog = catalog_for(lang, None);
    let mut keys: Vec<String> = crate::widgets::command_palette::ALL_COMMANDS
        .iter()
        .map(|c| c.title().to_string())
        .chain(extra.iter().map(|s| s.to_string()))
        .chain(catalog.exact.keys().cloned())
        .chain(
            catalog
                .patterns
                .iter()
                .map(|(p, s, _)| format!("{p}{{}}{s}")),
        )
        .collect();
    keys.sort();
    keys.dedup();
    let map: serde_json::Map<String, serde_json::Value> = keys
        .into_iter()
        .map(|k| {
            let v = catalog.lookup(&k);
            let v = if v == k {
                String::new()
            } else {
                v.into_owned()
            };
            (k, serde_json::Value::String(v))
        })
        .collect();
    serde_json::to_string_pretty(&serde_json::Value::Object(map)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let t = template("de", &["New File"]);
        let map: HashMap<String, String> = serde_json::from_str(&t).unwrap();
        assert_eq!(map.get("New File").map(String::as_str), Some("Neue Datei"));
        assert!(map.contains_key("File: Save"));
    }
}
