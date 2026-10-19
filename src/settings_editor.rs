//! Settings editor model (#612): every setting croft has, its effective
//! value, which layer set it, and how it is edited in place.
//!
//! The rows come from the merged view [`crate::config_layers`] builds, so a
//! workspace override shows as the value in force with its layer named.
//! Writing goes to one layer file at a time with [`set_key`]; the caller
//! re-merges afterwards.

use crate::config_layers::LayerKind;
use serde_json::Value;
use std::collections::BTreeMap;

/// How a setting is edited in place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// Enter flips it.
    Bool,
    /// Enter steps through these values.
    Choice(Vec<String>),
    /// Enter asks for a number.
    Number,
    /// Enter asks for text.
    Text,
    /// Objects and arrays: edited in the JSON file.
    Json,
}

/// One setting as the editor lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub key: String,
    pub value: Value,
    pub kind: Kind,
    pub layer: LayerKind,
    /// The value differs from croft's built-in default (#1655), whichever
    /// layer set it: what "what have I changed?" lists.
    pub modified: bool,
}

/// The string settings whose values come from a fixed set. An empty
/// (default) value counts as the first choice, so Enter moves it to the
/// second. An optional setting's first choice is the empty one, its
/// default, which Enter writes as no value at all (#1555).
pub fn choices(key: &str) -> Option<Vec<String>> {
    let fixed: &[&str] = match key {
        "problems_scope" => &["whole_project", "open_files"],
        // An enum: free text such as `enabled` was saved and then ignored.
        "code_scanning" => &["off", "on", "prompt"],
        "locale" => {
            let langs = crate::i18n::languages(&crate::prefs::config_dir());
            return Some(std::iter::once(String::new()).chain(langs).collect());
        }
        "problems_project_scope" => &["auto", "on", "off"],
        "diff_ignore_whitespace" => &["off", "leading", "all"],
        "render_whitespace" => &["selection", "all", "none"],
        "line_numbers" => &["on", "relative", "interval", "off"],
        "word_based_suggestions" => &[
            "matchingDocuments",
            "currentDocument",
            "allDocuments",
            "off",
        ],
        "theme" => {
            return Some(
                crate::theme::Theme::all()
                    .iter()
                    .map(|t| t.id().to_string())
                    .collect(),
            );
        }
        _ => return None,
    };
    Some(fixed.iter().map(|c| c.to_string()).collect())
}

/// The optional settings (#1555): unset, their value is `null`, which says
/// nothing of their type, so it is declared here. Text and Number rows of
/// these take an empty entry as unset.
const OPTIONAL_TEXT: &[&str] = &[
    "codeql_cli_path",
    "lane_agent",
    "review_ai_prefix",
    "screen_reader_command",
];
const OPTIONAL_NUMBER: &[&str] = &["terminal_rewind_mb"];

fn is_optional(key: &str) -> bool {
    OPTIONAL_TEXT.contains(&key) || OPTIONAL_NUMBER.contains(&key) || key == "locale"
}

/// How an unset (`null`) setting is edited: its declared kind, or, for a
/// setting with no declaration, the JSON file.
fn unset_kind(key: &str) -> Kind {
    if OPTIONAL_TEXT.contains(&key) {
        Kind::Text
    } else if OPTIONAL_NUMBER.contains(&key) {
        Kind::Number
    } else {
        choices(key).map_or(Kind::Json, Kind::Choice)
    }
}

/// Every top-level setting of `prefs` in key order, with the layer
/// `provenance` says set it.
pub fn rows(prefs: &crate::prefs::Prefs, provenance: &BTreeMap<String, LayerKind>) -> Vec<Row> {
    let Ok(Value::Object(map)) = serde_json::to_value(prefs) else {
        return Vec::new();
    };
    let defaults = match serde_json::to_value(crate::prefs::Prefs::default()) {
        Ok(Value::Object(d)) => d,
        _ => serde_json::Map::new(),
    };
    let mut out: Vec<Row> = map
        .into_iter()
        .map(|(key, value)| {
            let kind = match &value {
                Value::Bool(_) => Kind::Bool,
                Value::Number(_) => Kind::Number,
                Value::String(_) => choices(&key).map(Kind::Choice).unwrap_or(Kind::Text),
                Value::Null => unset_kind(&key),
                _ => Kind::Json,
            };
            let layer = crate::config_layers::layer_of(provenance, &key);
            let modified = defaults.get(&key) != Some(&value);
            Row {
                key,
                value,
                kind,
                layer,
                modified,
            }
        })
        .collect();
    out.sort_by(|a, b| a.key.cmp(&b.key));
    out
}

/// What a row shows for a setting at its default: an empty string, or an
/// optional setting that is unset.
pub const DEFAULT_LABEL: &str = "(default)";

/// A value as one line: strings bare, `(default)` for an empty one or an
/// unset optional one, objects and arrays as compact JSON.
pub fn display(value: &Value) -> String {
    match value {
        Value::String(s) if s.is_empty() => String::from(DEFAULT_LABEL),
        Value::Null => String::from(DEFAULT_LABEL),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Whether `row` matches every word of `query` in its key (underscores read
/// as spaces) or its displayed value, ignoring case.
pub fn matches(row: &Row, query: &str) -> bool {
    let hay = format!(
        "{} {} {}",
        row.key,
        row.key.replace('_', " "),
        display(&row.value)
    )
    .to_lowercase();
    query
        .split_whitespace()
        .all(|w| hay.contains(&w.to_lowercase()))
}

/// The value Enter moves a Bool or Choice row to; `None` for other kinds.
pub fn next_value(row: &Row) -> Option<Value> {
    match (&row.kind, &row.value) {
        (Kind::Bool, Value::Bool(b)) => Some(Value::Bool(!b)),
        (Kind::Choice(c), v) if !c.is_empty() => {
            let now = v.as_str().unwrap_or_default();
            let at = c.iter().position(|x| x == now).unwrap_or(0);
            let next = &c[(at + 1) % c.len()];
            Some(if next.is_empty() && is_optional(&row.key) {
                Value::Null
            } else {
                Value::String(next.clone())
            })
        }
        _ => None,
    }
}

/// The value typed into a Number or Text row's prompt (#1555). A Number
/// must be a whole number. An optional setting left empty, or given back
/// `(default)`, is unset, except `review_ai_prefix`, whose empty value is
/// itself a setting (no marker); any other text setting's `(default)` is
/// its empty default.
pub fn typed_value(row: &Row, text: &str) -> Result<Value, String> {
    let t = text.trim();
    let optional = is_optional(&row.key);
    if t == DEFAULT_LABEL {
        return Ok(if optional {
            Value::Null
        } else {
            Value::from("")
        });
    }
    match row.kind {
        Kind::Number if t.is_empty() && optional => Ok(Value::Null),
        Kind::Number => t
            .parse::<u64>()
            .map(Value::from)
            .map_err(|_| format!("{} needs a whole number, not \"{t}\"", row.key)),
        _ if t.is_empty() && optional && row.key != "review_ai_prefix" => Ok(Value::Null),
        _ => Ok(Value::from(t)),
    }
}

/// Whether `key` may be set to `value` (#1555): the settings, with it set,
/// still read back as croft's settings. A value of the wrong type, or a
/// word an enum setting doesn't know, would otherwise be saved, reported as
/// saved, and then ignored.
pub fn check(prefs: &crate::prefs::Prefs, key: &str, value: &Value) -> Result<(), String> {
    let Ok(Value::Object(mut map)) = serde_json::to_value(prefs) else {
        return Ok(());
    };
    map.insert(key.to_string(), value.clone());
    serde_json::from_value::<crate::prefs::Prefs>(Value::Object(map))
        .map(|_| ())
        .map_err(|_| format!("{key} can't be {}", display(value)))
}

/// The 0-based line of top-level `key` in a layer file's `text`, for
/// opening a JSON-only setting where it is written (#1555).
pub fn key_line(text: &str, key: &str) -> Option<usize> {
    let quoted = format!("\"{key}\"");
    text.lines().position(|l| {
        l.trim_start()
            .strip_prefix(&quoted)
            .is_some_and(|rest| rest.trim_start().starts_with(':'))
    })
}

/// `text` (a layer file's JSON object, or empty) with `key` set to `value`,
/// pretty-printed; every other key keeps its value and its place (croft's
/// serde_json preserves order), and a new key goes last. A `null` value
/// takes the key out instead, so the setting falls back to its default
/// (#1555). An error when the text is not a JSON object.
pub fn set_key(text: &str, key: &str, value: Value) -> Result<String, String> {
    let mut map = if text.trim().is_empty() {
        serde_json::Map::new()
    } else {
        match serde_json::from_str::<Value>(text) {
            Ok(Value::Object(m)) => m,
            Ok(_) => return Err(String::from("the settings file is not a JSON object")),
            Err(e) => return Err(format!("the settings file does not parse: {e}")),
        }
    };
    if value.is_null() {
        map.remove(key);
    } else {
        map.insert(key.to_string(), value);
    }
    serde_json::to_string_pretty(&Value::Object(map))
        .map(|s| s + "\n")
        .map_err(|e| e.to_string())
}

/// `text` (a layer file's JSON object) without `key` (#1655), pretty-printed
/// like [`set_key`]; `Ok(None)` when the file does not set it, so nothing
/// is written. Reads JSONC like the layer loader, and also drops `key`
/// from the active `platform` scope block (`"linux"` / `"macos"` /
/// `"android"`), which the loader merges over the flat keys. An error when
/// the text is not a JSON object.
pub fn remove_key(text: &str, key: &str, platform: &str) -> Result<Option<String>, String> {
    if text.trim().is_empty() {
        return Ok(None);
    }
    let mut map = match serde_json::from_str::<Value>(&crate::tasks::strip_jsonc(text)) {
        Ok(Value::Object(m)) => m,
        Ok(_) => return Err(String::from("the settings file is not a JSON object")),
        Err(e) => return Err(format!("the settings file does not parse: {e}")),
    };
    let mut removed = map.remove(key).is_some();
    if let Some(Value::Object(block)) = map.get_mut(platform) {
        removed |= block.remove(key).is_some();
    }
    if !removed {
        return Ok(None);
    }
    serde_json::to_string_pretty(&Value::Object(map))
        .map(|s| Some(s + "\n"))
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rows_cover_every_setting_with_its_kind_and_layer() {
        let prefs = crate::prefs::Prefs {
            auto_save: true,
            problems_scope: String::from("open_files"),
            ..Default::default()
        };
        let provenance = BTreeMap::from([(String::from("auto_save"), LayerKind::Workspace)]);
        let all = rows(&prefs, &provenance);
        let expected = serde_json::to_value(&prefs)
            .unwrap()
            .as_object()
            .unwrap()
            .len();
        assert_eq!(all.len(), expected, "one row per setting");
        let keys: Vec<&str> = all.iter().map(|r| r.key.as_str()).collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted, "in key order");
        let get = |k: &str| all.iter().find(|r| r.key == k).unwrap();
        assert_eq!(get("auto_save").kind, Kind::Bool);
        assert_eq!(get("auto_save").value, json!(true));
        assert_eq!(get("auto_save").layer, LayerKind::Workspace);
        assert_eq!(get("copy_on_select").layer, LayerKind::Default);
        assert_eq!(
            get("problems_scope").kind,
            Kind::Choice(vec!["whole_project".into(), "open_files".into()])
        );
        assert!(matches!(get("theme").kind, Kind::Choice(ref c) if c.len() > 1));
        assert_eq!(get("explorer_views").kind, Kind::Json);
    }

    #[test]
    fn values_display_on_one_line() {
        assert_eq!(display(&json!("open_files")), "open_files");
        assert_eq!(display(&json!("")), "(default)");
        assert_eq!(display(&json!(true)), "true");
        assert_eq!(display(&json!(12)), "12");
        assert_eq!(display(&json!({"a": 1})), "{\"a\":1}");
        assert_eq!(display(&Value::Null), "(default)", "#1555: not `null`");
    }

    /// #1555: an unset optional setting is edited as what it is, not sent
    /// to the JSON file because its value, `null`, says nothing of its type.
    #[test]
    fn unset_optional_settings_get_their_real_kind() {
        let all = rows(&crate::prefs::Prefs::default(), &BTreeMap::new());
        let get = |k: &str| all.iter().find(|r| r.key == k).unwrap().kind.clone();
        let Kind::Choice(langs) = get("locale") else {
            panic!("locale is a choice: {:?}", get("locale"));
        };
        assert_eq!(langs[0], "", "the first choice is the system's language");
        for lang in ["en", "de", "es"] {
            assert!(langs.iter().any(|l| l == lang), "{lang} in {langs:?}");
        }
        assert_eq!(get("terminal_rewind_mb"), Kind::Number);
        for key in [
            "codeql_cli_path",
            "lane_agent",
            "review_ai_prefix",
            "screen_reader_command",
        ] {
            assert_eq!(get(key), Kind::Text, "{key}");
        }
        assert_eq!(
            get("code_scanning"),
            Kind::Choice(vec!["off".into(), "on".into(), "prompt".into()])
        );
    }

    /// #1555 negative: objects and lists are still edited in the JSON file.
    #[test]
    fn objects_and_lists_stay_json_rows() {
        let all = rows(&crate::prefs::Prefs::default(), &BTreeMap::new());
        let get = |k: &str| all.iter().find(|r| r.key == k).unwrap().kind.clone();
        assert_eq!(get("files_exclude"), Kind::Json);
        assert_eq!(get("explorer_views"), Kind::Json);
    }

    /// #1555: stepping an optional choice past its last value unsets it.
    #[test]
    fn an_optional_choice_steps_back_to_unset() {
        let langs = Kind::Choice(vec!["".into(), "de".into(), "es".into()]);
        let locale = |v: Value| row("locale", v, langs.clone());
        assert_eq!(next_value(&locale(Value::Null)), Some(json!("de")));
        assert_eq!(next_value(&locale(json!("es"))), Some(Value::Null));
    }

    /// #1555: a typed value is a number for a number row, and an optional
    /// setting left empty, or given `(default)`, is unset.
    #[test]
    fn a_typed_value_parses_or_unsets_an_optional_setting() {
        let rewind = row("terminal_rewind_mb", Value::Null, Kind::Number);
        assert_eq!(typed_value(&rewind, " 500 "), Ok(json!(500)));
        assert_eq!(typed_value(&rewind, ""), Ok(Value::Null));
        assert_eq!(typed_value(&rewind, "(default)"), Ok(Value::Null));
        assert!(typed_value(&rewind, "lots").unwrap_err().contains("number"));
        let codeql = row("codeql_cli_path", Value::Null, Kind::Text);
        assert_eq!(
            typed_value(&codeql, "/opt/codeql"),
            Ok(json!("/opt/codeql"))
        );
        assert_eq!(typed_value(&codeql, "  "), Ok(Value::Null));
    }

    /// #1555 negative: a required number still needs one, an empty AI
    /// review prefix is its own setting (no marker), and a plain text
    /// setting's `(default)` is its empty default, not unset.
    #[test]
    fn empty_entries_unset_only_what_may_be_unset() {
        let scrollback = row("terminal_scrollback", json!(1000), Kind::Number);
        assert!(typed_value(&scrollback, "").is_err());
        let prefix = row("review_ai_prefix", Value::Null, Kind::Text);
        assert_eq!(typed_value(&prefix, ""), Ok(json!("")));
        assert_eq!(typed_value(&prefix, "(default)"), Ok(Value::Null));
        let font = row("a_text_setting", json!(""), Kind::Text);
        assert_eq!(typed_value(&font, "(default)"), Ok(json!("")));
    }

    /// #1555: a value the setting can't hold is refused; one it can, and
    /// unsetting an optional setting, are not.
    #[test]
    fn check_refuses_a_value_the_setting_cannot_hold() {
        let prefs = crate::prefs::Prefs::default();
        assert!(check(&prefs, "code_scanning", &json!("enabled")).is_err());
        assert!(check(&prefs, "auto_save", &json!("yes")).is_err());
        assert_eq!(check(&prefs, "code_scanning", &json!("prompt")), Ok(()));
        assert_eq!(check(&prefs, "terminal_rewind_mb", &json!(500)), Ok(()));
        assert_eq!(check(&prefs, "locale", &Value::Null), Ok(()));
    }

    /// #1555: a `null` value takes the key out of the layer file.
    #[test]
    fn set_key_with_null_removes_the_key() {
        let out = set_key(
            "{\"locale\": \"de\", \"auto_save\": true}",
            "locale",
            Value::Null,
        )
        .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v, json!({"auto_save": true}));
    }

    #[test]
    fn key_line_finds_a_top_level_key() {
        let text = "{\n  \"files\": 1,\n  \"files_exclude\" : [],\n  \"x\": \"files_exclude\"\n}";
        assert_eq!(key_line(text, "files_exclude"), Some(2));
        assert_eq!(key_line(text, "files"), Some(1));
        assert_eq!(key_line(text, "search_exclude"), None);
    }

    fn row(key: &str, value: Value, kind: Kind) -> Row {
        Row {
            key: key.into(),
            value,
            kind,
            layer: LayerKind::Default,
            modified: false,
        }
    }

    #[test]
    fn search_reads_underscores_as_spaces_and_matches_values() {
        let r = row("format_on_save", json!(true), Kind::Bool);
        assert!(matches(&r, "format save"));
        assert!(matches(&r, "FORMAT_ON"));
        assert!(matches(&r, "true"));
        assert!(!matches(&r, "format type"));
    }

    #[test]
    fn enter_flips_a_bool_and_steps_a_choice_wrapping_round() {
        assert_eq!(
            next_value(&row("a", json!(false), Kind::Bool)),
            Some(json!(true))
        );
        let c = Kind::Choice(vec!["auto".into(), "on".into(), "off".into()]);
        assert_eq!(
            next_value(&row("p", json!("on"), c.clone())),
            Some(json!("off"))
        );
        assert_eq!(
            next_value(&row("p", json!("off"), c.clone())),
            Some(json!("auto"))
        );
        // An empty or unknown value is the first choice, so Enter moves on.
        assert_eq!(next_value(&row("p", json!(""), c)), Some(json!("on")));
        assert_eq!(next_value(&row("n", json!(3), Kind::Number)), None);
    }

    #[test]
    fn set_key_writes_one_key_and_keeps_the_rest() {
        let out = set_key(
            "{\n  \"theme\": \"dark\",\n  \"auto_save\": false\n}",
            "auto_save",
            json!(true),
        )
        .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v, json!({"theme": "dark", "auto_save": true}));
        assert_eq!(
            serde_json::from_str::<Value>(&set_key("", "k", json!(1)).unwrap()).unwrap(),
            json!({"k": 1})
        );
        assert!(set_key("[1, 2]", "k", json!(1)).is_err());
        assert!(set_key("{ broken", "k", json!(1)).is_err());
    }

    #[test]
    fn remove_key_reads_jsonc_and_clears_the_active_platform_block() {
        let jsonc = "{\n  // mine\n  \"theme\": \"dark\",\n  \"auto_save\": true,\n}";
        let out = remove_key(jsonc, "auto_save", "linux").unwrap().unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v, json!({"theme": "dark"}));

        let scoped = r#"{"theme":"dark","linux":{"theme":"nord"},"macos":{"theme":"x"}}"#;
        let out = remove_key(scoped, "theme", "linux").unwrap().unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v, json!({"linux": {}, "macos": {"theme": "x"}}));

        let only_scoped = r#"{"linux":{"auto_save":true}}"#;
        let out = remove_key(only_scoped, "auto_save", "linux")
            .unwrap()
            .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v, json!({"linux": {}}));

        assert_eq!(remove_key(only_scoped, "auto_save", "macos").unwrap(), None);
        assert_eq!(remove_key("", "k", "linux").unwrap(), None);
        assert!(remove_key("[1, 2]", "k", "linux").is_err());
        assert!(remove_key("{ broken", "k", "linux").is_err());
    }
}
