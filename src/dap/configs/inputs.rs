//! launch.json's `inputs`: reading the array, the `${input:id}` ids a configuration
//! uses, and which of them are still to ask for.

use super::*;

impl DebugConfig {
    /// The distinct `${input:id}` ids this configuration uses, in the order
    /// they first appear.
    pub fn input_refs(&self) -> Vec<String> {
        let mut out = Vec::new();
        value_input_refs(&Value::Object(self.raw.clone()), &mut out);
        out
    }
}

/// One entry of a launch.json or tasks.json `inputs` array (#1424).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchInput {
    pub id: String,
    pub description: String,
    pub default: Option<String>,
    pub kind: InputKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputKind {
    /// `promptString`: a text field, masked for `"password": true`.
    Prompt { password: bool },
    /// `pickString`: one of `options`, as (label, value) pairs.
    Pick { options: Vec<(String, String)> },
    /// `command`: the value comes from running a command.
    Command { command: String },
}

/// The `inputs` array of a launch.json or tasks.json body. Entries without
/// an `id` or with a type croft does not know are skipped, as is a
/// `pickString` with no options.
pub fn parse_inputs(v: &Value) -> Vec<LaunchInput> {
    let Some(list) = v.get("inputs").and_then(Value::as_array) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|i| {
            let id = i.get("id")?.as_str()?.to_string();
            let text = |key: &str| i.get(key).and_then(Value::as_str).map(str::to_string);
            let kind = match i.get("type")?.as_str()? {
                "promptString" => InputKind::Prompt {
                    password: i.get("password").and_then(Value::as_bool) == Some(true),
                },
                "pickString" => {
                    let options: Vec<(String, String)> = i
                        .get("options")?
                        .as_array()?
                        .iter()
                        .filter_map(|o| match o {
                            Value::String(s) => Some((s.clone(), s.clone())),
                            Value::Object(o) => {
                                let value = o.get("value")?.as_str()?.to_string();
                                let label = o
                                    .get("label")
                                    .and_then(Value::as_str)
                                    .map_or_else(|| value.clone(), str::to_string);
                                Some((label, value))
                            }
                            _ => None,
                        })
                        .collect();
                    if options.is_empty() {
                        return None;
                    }
                    InputKind::Pick { options }
                }
                "command" => InputKind::Command {
                    command: text("command")?,
                },
                _ => return None,
            };
            Some(LaunchInput {
                description: text("description").unwrap_or_else(|| id.clone()),
                default: text("default"),
                id,
                kind,
            })
        })
        .collect()
}

/// The inputs still to ask for: each id in `refs` with no answer yet, in
/// order, looked up in `defs` (the first entry with that id wins). An id no
/// entry declares is an error naming it.
pub fn inputs_to_ask(
    refs: &[String],
    answers: &BTreeMap<String, String>,
    defs: &[LaunchInput],
) -> Result<Vec<LaunchInput>, String> {
    refs.iter()
        .filter(|id| !answers.contains_key(*id))
        .map(|id| {
            defs.iter()
                .find(|d| d.id == *id)
                .cloned()
                .ok_or_else(|| format!("${{input:{id}}}: no \"inputs\" entry has the id \"{id}\""))
        })
        .collect()
}

/// Add the `${input:id}` ids in `text` to `out`, skipping ones already
/// there.
pub fn text_input_refs(text: &str, out: &mut Vec<String>) {
    let mut rest = text;
    while let Some(start) = rest.find("${input:") {
        let after = &rest[start + "${input:".len()..];
        let Some(end) = after.find('}') else {
            return;
        };
        let id = after[..end].trim();
        if !id.is_empty() && !out.iter().any(|o| o == id) {
            out.push(id.to_string());
        }
        rest = &after[end + 1..];
    }
}

pub(super) fn value_input_refs(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) => text_input_refs(s, out),
        Value::Array(a) => a.iter().for_each(|x| value_input_refs(x, out)),
        Value::Object(o) => o.values().for_each(|x| value_input_refs(x, out)),
        _ => {}
    }
}
