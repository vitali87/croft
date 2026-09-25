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
}

/// The string settings whose values come from a fixed set, with the value
/// an empty string means listed first.
pub fn choices(key: &str) -> Option<Vec<String>> {
    let fixed: &[&str] = match key {
        "problems_scope" => &["whole_project", "open_files"],
        "problems_project_scope" => &["auto", "on", "off"],
        "diff_ignore_whitespace" => &["off", "leading", "all"],
        "render_whitespace" => &["selection", "all", "none"],
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

/// Every top-level setting of `prefs` in key order, with the layer
/// `provenance` says set it.
pub fn rows(prefs: &crate::prefs::Prefs, provenance: &BTreeMap<String, LayerKind>) -> Vec<Row> {
    let Ok(Value::Object(map)) = serde_json::to_value(prefs) else {
        return Vec::new();
    };
    let mut out: Vec<Row> = map
        .into_iter()
        .map(|(key, value)| {
            let kind = match &value {
                Value::Bool(_) => Kind::Bool,
                Value::Number(_) => Kind::Number,
                Value::String(_) => choices(&key).map(Kind::Choice).unwrap_or(Kind::Text),
                _ => Kind::Json,
            };
            let layer = crate::config_layers::layer_of(provenance, &key);
            Row {
                key,
                value,
                kind,
                layer,
            }
        })
        .collect();
    out.sort_by(|a, b| a.key.cmp(&b.key));
    out
}

/// A value as one line: strings bare, `(default)` for an empty one,
/// objects and arrays as compact JSON.
pub fn display(value: &Value) -> String {
    match value {
        Value::String(s) if s.is_empty() => String::from("(default)"),
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
            Some(Value::String(c[(at + 1) % c.len()].clone()))
        }
        _ => None,
    }
}

/// `text` (a layer file's JSON object, or empty) with `key` set to `value`,
/// pretty-printed with keys in sorted order; every other key keeps its
/// value. An error when the text is not a JSON object.
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
    map.insert(key.to_string(), value);
    serde_json::to_string_pretty(&Value::Object(map))
        .map(|s| s + "\n")
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rows_cover_every_setting_with_its_kind_and_layer() {
        let mut prefs = crate::prefs::Prefs::default();
        prefs.auto_save = true;
        prefs.problems_scope = String::from("open_files");
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
    }

    fn row(key: &str, value: Value, kind: Kind) -> Row {
        Row {
            key: key.into(),
            value,
            kind,
            layer: LayerKind::Default,
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
}
