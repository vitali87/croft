//! UI strings in other languages (#621). Every string keeps its English
//! text in the code; a catalog in `~/.config/croft/locale/<lang>.json`
//! (a flat `{"key": "text"}` object) replaces it where it has a key. The
//! palette's command titles are keyed `command.<id>`.
//!
//! The active catalog is per thread: the UI runs on one thread, and tests
//! running in parallel each see their own.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::Path;

/// Translated strings by key.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Catalog {
    pub strings: HashMap<String, String>,
}

/// The language to show: `pref` when set, else the `LANG` value's language
/// (`de_DE.UTF-8` -> `de_DE`), else English. `C` and `POSIX` are English.
pub fn language(pref: &str, env_lang: Option<&str>) -> String {
    if !pref.trim().is_empty() {
        return pref.trim().to_string();
    }
    let lang = env_lang
        .unwrap_or("")
        .split(['.', '@'])
        .next()
        .unwrap_or("")
        .trim();
    match lang {
        "" | "C" | "POSIX" => String::from("en"),
        other => other.to_string(),
    }
}

fn read_catalog(path: &Path) -> Option<Catalog> {
    let text = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let strings = v
        .as_object()?
        .iter()
        .filter_map(|(k, v)| v.as_str().map(|t| (k.clone(), t.to_string())))
        .collect();
    Some(Catalog { strings })
}

/// The catalog for `lang` under `config_dir/locale/`: `<lang>.json`, else
/// the base language's file (`de_DE` -> `de.json`), else empty. Keys whose
/// value is not a string are skipped.
pub fn load(config_dir: &Path, lang: &str) -> Catalog {
    let dir = config_dir.join("locale");
    let base = lang.split(['_', '-']).next().unwrap_or(lang);
    // Never a path out of the locale folder, whatever the setting says.
    let safe = |l: &str| {
        !l.is_empty()
            && l.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    };
    [lang, base]
        .into_iter()
        .filter(|l| safe(l))
        .find_map(|l| read_catalog(&dir.join(format!("{l}.json"))))
        .unwrap_or_default()
}

thread_local! {
    static ACTIVE: std::cell::RefCell<Catalog> = std::cell::RefCell::new(Catalog::default());
}

/// Make `catalog` the one this thread's UI reads.
pub fn set(catalog: Catalog) {
    ACTIVE.with(|a| *a.borrow_mut() = catalog);
}

/// `key`'s text in the active catalog, else `english`.
pub fn tr<'a>(key: &str, english: &'a str) -> Cow<'a, str> {
    ACTIVE.with(|a| match a.borrow().strings.get(key) {
        Some(t) => Cow::Owned(t.clone()),
        None => Cow::Borrowed(english),
    })
}

/// A catalog for translators: every palette command's key and English
/// title, pretty-printed in key order.
pub fn template() -> String {
    let map: std::collections::BTreeMap<String, &str> =
        crate::widgets::command_palette::ALL_COMMANDS
            .iter()
            .map(|c| (format!("command.{}", c.id()), c.title()))
            .collect();
    serde_json::to_string_pretty(&map).unwrap_or_default() + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_language_comes_from_the_setting_then_lang() {
        assert_eq!(language("fr", Some("de_DE.UTF-8")), "fr");
        assert_eq!(language("", Some("de_DE.UTF-8")), "de_DE");
        assert_eq!(language("", Some("C")), "en");
        assert_eq!(language("", Some("POSIX")), "en");
        assert_eq!(language("", None), "en");
        assert_eq!(language("  ", Some("")), "en");
    }

    #[test]
    fn a_catalog_loads_for_the_language_or_its_base() {
        let cfg = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(cfg.path().join("locale")).unwrap();
        std::fs::write(
            cfg.path().join("locale/de.json"),
            r#"{"command.quick_open": "Gehe zu Datei", "bad": 3}"#,
        )
        .unwrap();
        let c = load(cfg.path(), "de_DE");
        assert_eq!(
            c.strings.get("command.quick_open").map(String::as_str),
            Some("Gehe zu Datei")
        );
        assert!(!c.strings.contains_key("bad"));
        assert_eq!(load(cfg.path(), "fr"), Catalog::default());
        std::fs::write(cfg.path().join("locale/es.json"), "{ broken").unwrap();
        assert_eq!(
            load(cfg.path(), "es"),
            Catalog::default(),
            "an unreadable file is ignored"
        );
    }

    #[test]
    fn translations_replace_english_only_where_they_have_a_key() {
        set(Catalog {
            strings: HashMap::from([("greeting".into(), "Hallo".into())]),
        });
        assert_eq!(tr("greeting", "Hello"), "Hallo");
        assert_eq!(tr("other", "Other"), "Other");
        set(Catalog::default());
        assert_eq!(tr("greeting", "Hello"), "Hello");
    }

    #[test]
    fn the_template_lists_every_palette_command_in_english() {
        let t: serde_json::Value = serde_json::from_str(&template()).unwrap();
        let obj = t.as_object().unwrap();
        assert_eq!(
            obj.len(),
            crate::widgets::command_palette::ALL_COMMANDS.len()
        );
        assert_eq!(obj["command.quick_open"], serde_json::json!("Go to File"));
    }
}
