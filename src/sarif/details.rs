//! Everything the details pane shows for one result, resolved from the log
//! once when the selection changes: rule text, message with its links,
//! every location, every code flow and thread flow, every stack, and the
//! property bags. The renderer only lays this out.

use super::model::{Location, Message, Run, SarifResult};
use super::resolve::expand;
use super::semantics::{self as sem, BaselineState, Kind, Level, Segment, SuppressionState};
use super::view::display_file;
use std::path::{Path, PathBuf};

/// A location as the pane shows it and as navigation needs it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LocRef {
    /// `id` within the result (§3.28.2), the target of `[text](id)` links.
    pub id: Option<i64>,
    /// Display path (workspace-relative when possible), or a logical name.
    pub label: String,
    /// Expanded URI, empty for a purely logical location.
    pub uri: String,
    pub line: i64,
    pub column: i64,
    /// The location's own message, if it has one.
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Step {
    /// `nestingLevel`, 0 when absent.
    pub depth: usize,
    pub message: String,
    pub location: Option<LocRef>,
    /// `essential`, `important` (the default) or `unimportant` (§3.38.13).
    pub importance: String,
    pub kinds: Vec<String>,
    /// `state` as `name = value` pairs, in key order.
    pub state: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Thread {
    /// "Flow 1", or "Flow 2 · thread t7" when a code flow has several.
    pub label: String,
    pub message: String,
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Frame {
    /// The frame's message, else its fully qualified logical name.
    pub text: String,
    pub location: Option<LocRef>,
    pub module: String,
    pub thread_id: Option<i64>,
    pub parameters: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StackView {
    pub message: String,
    pub frames: Vec<Frame>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Details {
    pub rule_id: String,
    pub rule_name: String,
    pub help_uri: Option<String>,
    /// `fullDescription`, else `shortDescription` (markdown when present).
    pub description: String,
    /// `help` (markdown when present).
    pub help: String,
    /// The message split into text and links.
    pub message: Vec<Segment>,
    pub level: Level,
    pub kind: Kind,
    pub baseline: BaselineState,
    pub suppression: SuppressionState,
    /// The first non-empty justification among the suppressions.
    pub justification: Option<String>,
    pub locations: Vec<LocRef>,
    pub related: Vec<LocRef>,
    /// Rule then result property bags, flattened to `key = value`.
    pub properties: Vec<(String, String)>,
    pub fingerprints: Vec<(String, String)>,
    pub guid: Option<String>,
    pub rank: Option<f64>,
    pub occurrence_count: Option<i64>,
    /// Taxonomy entries (CWE, OWASP, ...) the result or its rule names,
    /// resolved through `run.taxonomies` (§3.27.26, §3.49.15).
    pub taxa: Vec<String>,
    /// Every thread flow of every code flow, in order.
    pub threads: Vec<Thread>,
    pub stacks: Vec<StackView>,
}

/// The label of one taxon reference: "<taxonomy> <id>: <description>".
/// The taxonomy is found by `toolComponent` index, name or guid; the taxon
/// by index, id or guid. What does not resolve keeps the reference's own id.
fn taxon_label(run: &Run, r: &crate::sarif::model::ReportingDescriptorReference) -> Option<String> {
    let tc = r.tool_component.as_ref()?;
    let taxonomy = tc
        .index
        .and_then(|i| usize::try_from(i).ok())
        .and_then(|i| run.taxonomies.get(i))
        .or_else(|| {
            run.taxonomies.iter().find(|t| {
                tc.name.as_deref().is_some_and(|n| n == t.name)
                    || (tc.guid.is_some() && tc.guid == t.guid)
            })
        });
    let taxon = taxonomy.and_then(|t| {
        r.index
            .and_then(|i| usize::try_from(i).ok())
            .and_then(|i| t.taxa.get(i))
            .or_else(|| {
                t.taxa.iter().find(|x| {
                    r.id.as_deref().is_some_and(|id| id == x.id)
                        || (r.guid.is_some() && r.guid == x.guid)
                })
            })
    });
    let name = taxonomy
        .map(|t| t.name.clone())
        .or_else(|| tc.name.clone())
        .unwrap_or_default();
    let id = taxon
        .map(|x| x.id.clone())
        .or_else(|| r.id.clone())
        .or_else(|| r.guid.clone())?;
    let what = taxon.and_then(|x| {
        x.short_description
            .as_ref()
            .map(|d| d.text.clone())
            .or_else(|| x.name.clone())
    });
    let head = format!("{name} {id}").trim().to_string();
    Some(match what {
        Some(w) if !w.is_empty() => format!("{head}: {w}"),
        _ => head,
    })
}

/// The labels of every taxon the result or its rule refers to: the
/// result's `taxa` first, then the rule's relationships that point into a
/// taxonomy, each once.
fn taxa(
    run: &Run,
    result: &SarifResult,
    rule: Option<&crate::sarif::model::ReportingDescriptor>,
) -> Vec<String> {
    let refs = result.taxa.iter().chain(
        rule.into_iter()
            .flat_map(|r| r.relationships.iter().map(|rel| &rel.target)),
    );
    let mut out: Vec<String> = Vec::new();
    for label in refs.filter_map(|r| taxon_label(run, r)) {
        if !out.contains(&label) {
            out.push(label);
        }
    }
    out
}

pub fn details(run: &Run, result: &SarifResult, roots: &[PathBuf]) -> Details {
    let rule = sem::rule_for(run, result);
    let component = sem::rule_component(run, result);
    let text = sem::message_text(&result.message, rule, Some(component));
    let pick = |m: &Option<crate::sarif::model::MultiformatMessageString>| {
        m.as_ref()
            .map(|m| m.markdown.clone().unwrap_or_else(|| m.text.clone()))
    };
    let description = rule
        .and_then(|r| pick(&r.full_description).or_else(|| pick(&r.short_description)))
        .unwrap_or_default();
    let help = rule.and_then(|r| pick(&r.help)).unwrap_or_default();
    let justification = result.suppressions.iter().flatten().find_map(|s| {
        s.justification
            .as_deref()
            .filter(|j| !j.trim().is_empty())
            .map(str::to_string)
    });
    let msg = |m: Option<&Message>| {
        m.map(|m| sem::message_text(m, rule, Some(component)))
            .unwrap_or_default()
    };
    let loc_ref = |l: &Location| loc_ref(run, l, roots, msg(l.message.as_ref()));
    let mut properties = Vec::new();
    if let Some(r) = rule {
        push_props(&mut properties, &r.properties);
    }
    push_props(&mut properties, &result.properties);
    let mut fingerprints: Vec<(String, String)> = result
        .fingerprints
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    fingerprints.extend(
        result
            .partial_fingerprints
            .iter()
            .map(|(k, v)| (format!("{k} (partial)"), v.clone())),
    );
    let mut threads = Vec::new();
    for (fi, flow) in result.code_flows.iter().enumerate() {
        let several = flow.thread_flows.len() > 1;
        for (ti, tf) in flow.thread_flows.iter().enumerate() {
            let label = if several {
                let id = tf.id.clone().unwrap_or_else(|| (ti + 1).to_string());
                format!("Flow {} · thread {id}", fi + 1)
            } else {
                format!("Flow {}", fi + 1)
            };
            let message = msg(tf.message.as_ref().or(flow.message.as_ref()));
            let steps = tf
                .locations
                .iter()
                .map(|s| Step {
                    depth: s
                        .nesting_level
                        .and_then(|n| usize::try_from(n).ok())
                        .unwrap_or(0),
                    message: msg(s.location.as_ref().and_then(|l| l.message.as_ref())),
                    location: s.location.as_ref().map(&loc_ref),
                    importance: s
                        .importance
                        .clone()
                        .unwrap_or_else(|| "important".to_string()),
                    kinds: s.kinds.clone(),
                    state: s
                        .state
                        .iter()
                        .map(|(k, v)| (k.clone(), v.text.clone()))
                        .collect(),
                })
                .collect();
            threads.push(Thread {
                label,
                message,
                steps,
            });
        }
    }
    let stacks = result
        .stacks
        .iter()
        .map(|s| StackView {
            message: msg(s.message.as_ref()),
            frames: s
                .frames
                .iter()
                .map(|f| {
                    let own = msg(f.location.as_ref().and_then(|l| l.message.as_ref()));
                    let fqn = f
                        .location
                        .as_ref()
                        .and_then(|l| l.logical_locations.first())
                        .and_then(|ll| ll.fully_qualified_name.clone().or_else(|| ll.name.clone()))
                        .unwrap_or_default();
                    Frame {
                        text: if own.is_empty() { fqn } else { own },
                        location: f.location.as_ref().map(&loc_ref),
                        module: f.module.clone().unwrap_or_default(),
                        thread_id: f.thread_id,
                        parameters: f.parameters.clone(),
                    }
                })
                .collect(),
        })
        .collect();
    Details {
        rule_id: sem::rule_id(run, result).unwrap_or_default(),
        rule_name: rule.and_then(|r| r.name.clone()).unwrap_or_default(),
        help_uri: rule.and_then(|r| r.help_uri.clone()),
        description,
        help,
        message: sem::segments(&text),
        level: sem::effective_level(result, rule),
        kind: sem::result_kind(result),
        baseline: sem::baseline_state(result),
        suppression: sem::suppression_state(result),
        justification,
        locations: result.locations.iter().map(&loc_ref).collect(),
        related: result.related_locations.iter().map(&loc_ref).collect(),
        properties,
        fingerprints,
        guid: result.guid.clone(),
        rank: result.rank,
        occurrence_count: result.occurrence_count,
        taxa: taxa(run, result, rule),
        threads,
        stacks,
    }
}

fn loc_ref(run: &Run, l: &Location, roots: &[PathBuf], message: String) -> LocRef {
    let physical = l.physical_location.as_ref();
    let uri = physical
        .and_then(|p| p.artifact_location.as_ref())
        .and_then(|a| expand(run, a))
        .map(|e| e.uri)
        .unwrap_or_default();
    let region = physical.and_then(|p| p.region.as_ref());
    let label = if uri.is_empty() {
        l.logical_locations
            .first()
            .and_then(|ll| ll.fully_qualified_name.clone().or_else(|| ll.name.clone()))
            .unwrap_or_default()
    } else {
        display_file(&uri, roots)
    };
    LocRef {
        id: l.id,
        label,
        uri,
        line: region.and_then(|r| r.start_line).unwrap_or(0),
        column: region.and_then(|r| r.start_column).unwrap_or(0),
        message,
    }
}

/// Flatten a property bag to `key = value` text: arrays join with commas,
/// null shows as an em dash, nested objects as compact JSON.
fn push_props(out: &mut Vec<(String, String)>, bag: &crate::sarif::model::PropertyBag) {
    for (k, v) in bag {
        let text = match v {
            serde_json::Value::Null => "—".to_string(),
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Array(a) => a
                .iter()
                .map(|x| match x {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .collect::<Vec<_>>()
                .join(", "),
            other => other.to_string(),
        };
        out.push((k.clone(), text));
    }
}

/// Where `[text](id)` in the message points: a related location with that
/// id, else a primary location with it.
pub fn link_target(d: &Details, id: i64) -> Option<&LocRef> {
    d.related
        .iter()
        .find(|l| l.id == Some(id))
        .or_else(|| d.locations.iter().find(|l| l.id == Some(id)))
}

/// The result's JSON exactly as the log has it (key order included),
/// re-indented. Read from the file on demand, so a large log is not kept
/// twice in memory.
pub fn raw_result_json(path: &Path, run: usize, result: usize) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct RawLog<'a> {
        #[serde(borrow, default)]
        runs: Vec<RawRun<'a>>,
    }
    #[derive(serde::Deserialize)]
    struct RawRun<'a> {
        #[serde(borrow, default)]
        results: Option<Vec<&'a serde_json::value::RawValue>>,
    }
    let bytes = std::fs::read(path).ok()?;
    let text = std::str::from_utf8(&bytes).ok()?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let log: RawLog = serde_json::from_str(text).ok()?;
    let raw = log.runs.get(run)?.results.as_ref()?.get(result)?;
    Some(pretty_preserving(raw.get()))
}

/// Re-indent JSON text two spaces per level without reordering anything:
/// `serde_json::Value` sorts object keys, which would show a result in an
/// order its log never had. String contents, escapes included, pass through.
fn pretty_preserving(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() * 2);
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut chars = raw.chars().peekable();
    let newline = |out: &mut String, depth: usize| {
        out.push('\n');
        out.push_str(&"  ".repeat(depth));
    };
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '{' | '[' => {
                out.push(c);
                // An empty container stays on one line: `{}`, `[]`.
                while chars.peek().is_some_and(|n| n.is_whitespace()) {
                    chars.next();
                }
                if matches!(chars.peek(), Some('}') | Some(']')) {
                    out.push(chars.next().unwrap());
                } else {
                    depth += 1;
                    newline(&mut out, depth);
                }
            }
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                newline(&mut out, depth);
                out.push(c);
            }
            ',' => {
                out.push(c);
                newline(&mut out, depth);
            }
            ':' => out.push_str(": "),
            c if c.is_whitespace() => {}
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sarif::load::parse_log;

    const LOG: &str = r#"{"version":"2.1.0","runs":[{
      "tool":{"driver":{"name":"CodeQL","rules":[{
        "id":"js/sql-injection","name":"SqlInjection",
        "shortDescription":{"text":"short"},
        "fullDescription":{"text":"Building a query from user input.","markdown":"Building a query from **user input**."},
        "help":{"text":"Use parameters.","markdown":"Use `?` parameters."},
        "helpUri":"https://codeql.github.com/sql",
        "properties":{"tags":["security","cwe-089"],"security-severity":"8.8","precision":"high"}}]}},
      "originalUriBaseIds":{"SRC":{"uri":"file:///ws/"}},
      "results":[{
        "ruleId":"js/sql-injection","ruleIndex":0,
        "message":{"text":"Query built from [user input](1) and [this](2)."},
        "locations":[{"id":0,"physicalLocation":{"artifactLocation":{"uri":"src/db.js","uriBaseId":"SRC"},"region":{"startLine":42,"startColumn":7}}}],
        "relatedLocations":[
          {"id":1,"message":{"text":"user input"},"physicalLocation":{"artifactLocation":{"uri":"src/api.js","uriBaseId":"SRC"},"region":{"startLine":12,"startColumn":9}}},
          {"id":2,"physicalLocation":{"artifactLocation":{"uri":"src/x.js","uriBaseId":"SRC"},"region":{"startLine":3}}}],
        "suppressions":[{"kind":"external","status":"rejected","justification":""},{"kind":"inSource","justification":"reviewed"}],
        "baselineState":"updated",
        "rank":55.5,"occurrenceCount":3,"guid":"G-1",
        "fingerprints":{"b/v1":"abc"},"partialFingerprints":{"primaryLocationLineHash":"ff00"},
        "properties":{"github/alertNumber":7,"note":null,"list":["a","b"],"flag":true},
        "codeFlows":[
          {"message":{"text":"taint path"},"threadFlows":[{"locations":[
            {"nestingLevel":0,"importance":"essential","kinds":["acquire"],"location":{"message":{"text":"source"},"physicalLocation":{"artifactLocation":{"uri":"src/api.js","uriBaseId":"SRC"},"region":{"startLine":12,"startColumn":9}}}},
            {"nestingLevel":1,"state":{"q":{"text":"tainted"}},"location":{"message":{"text":"step"},"physicalLocation":{"artifactLocation":{"uri":"src/db.js","uriBaseId":"SRC"},"region":{"startLine":40}}}},
            {"nestingLevel":0,"importance":"unimportant","location":{"message":{"text":"sink"}}}]}]},
          {"threadFlows":[{"id":"t1","locations":[{"location":{"message":{"text":"a"}}}]},{"id":"t2","locations":[]}]}],
        "stacks":[
          {"frames":[{"module":"app","threadId":4,"parameters":["x"],"location":{"logicalLocations":[{"fullyQualifiedName":"App.run"}],"physicalLocation":{"artifactLocation":{"uri":"src/db.js","uriBaseId":"SRC"},"region":{"startLine":42,"startColumn":7}}}}]},
          {"message":{"text":"second"},"frames":[{"location":{"message":{"text":"top"}}}]}]
      }]}]}"#;

    fn d() -> Details {
        let log = parse_log(LOG).unwrap();
        let run = &log.runs[0];
        let result = &run.results.as_ref().unwrap()[0];
        details(run, result, &[PathBuf::from("/ws")])
    }

    #[test]
    fn rule_text_prefers_full_description_and_markdown() {
        let d = d();
        assert_eq!(d.rule_id, "js/sql-injection");
        assert_eq!(d.rule_name, "SqlInjection");
        assert_eq!(d.description, "Building a query from **user input**.");
        assert_eq!(d.help, "Use `?` parameters.");
        assert_eq!(d.help_uri.as_deref(), Some("https://codeql.github.com/sql"));
    }

    #[test]
    fn message_links_resolve_to_related_locations() {
        let d = d();
        assert_eq!(
            d.message[1],
            Segment::LocationLink {
                text: "user input".into(),
                id: 1
            }
        );
        let t = link_target(&d, 1).unwrap();
        assert_eq!((t.label.as_str(), t.line, t.column), ("src/api.js", 12, 9));
        assert_eq!(link_target(&d, 2).unwrap().label, "src/x.js");
        assert_eq!(
            link_target(&d, 0).unwrap().label,
            "src/db.js",
            "falls back to locations"
        );
        assert!(link_target(&d, 9).is_none());
    }

    #[test]
    fn states_and_justification() {
        let d = d();
        // No `level` and no rule default: §3.27.10 makes a `fail` warning.
        assert_eq!(d.level, Level::Warning);
        assert_eq!(d.kind, Kind::Fail);
        assert_eq!(d.baseline, BaselineState::Updated);
        assert_eq!(d.suppression, SuppressionState::Suppressed);
        assert_eq!(d.justification.as_deref(), Some("reviewed"));
    }

    #[test]
    fn locations_every_one_not_just_the_first() {
        let d = d();
        assert_eq!(d.locations.len(), 1);
        assert_eq!(d.locations[0].label, "src/db.js");
        assert_eq!(d.related.len(), 2);
        assert_eq!(d.related[0].message, "user input");
        assert_eq!(d.related[0].id, Some(1));
    }

    #[test]
    fn properties_rule_then_result_rendered_as_text() {
        let d = d();
        let get = |k: &str| {
            d.properties
                .iter()
                .find(|(a, _)| a == k)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(get("tags").as_deref(), Some("security, cwe-089"));
        assert_eq!(get("security-severity").as_deref(), Some("8.8"));
        assert_eq!(get("github/alertNumber").as_deref(), Some("7"));
        assert_eq!(get("note").as_deref(), Some("—"));
        assert_eq!(get("list").as_deref(), Some("a, b"));
        assert_eq!(get("flag").as_deref(), Some("true"));
        let tags_at = d.properties.iter().position(|(k, _)| k == "tags").unwrap();
        let alert_at = d
            .properties
            .iter()
            .position(|(k, _)| k == "github/alertNumber")
            .unwrap();
        assert!(tags_at < alert_at, "rule properties come first");
    }

    #[test]
    fn identity_fields() {
        let d = d();
        assert_eq!(
            d.fingerprints,
            vec![
                ("b/v1".to_string(), "abc".to_string()),
                (
                    "primaryLocationLineHash (partial)".to_string(),
                    "ff00".to_string()
                )
            ]
        );
        assert_eq!(d.guid.as_deref(), Some("G-1"));
        assert_eq!(d.rank, Some(55.5));
        assert_eq!(d.occurrence_count, Some(3));
    }

    #[test]
    fn every_code_flow_and_thread_flow() {
        let d = d();
        let labels: Vec<_> = d.threads.iter().map(|t| t.label.as_str()).collect();
        assert_eq!(
            labels,
            vec!["Flow 1", "Flow 2 · thread t1", "Flow 2 · thread t2"]
        );
        let steps = &d.threads[0].steps;
        assert_eq!(steps.len(), 3);
        assert_eq!(d.threads[0].message, "taint path");
        assert_eq!(steps[0].importance, "essential");
        assert_eq!(steps[0].kinds, vec!["acquire".to_string()]);
        assert_eq!(steps[0].location.as_ref().unwrap().line, 12);
        assert_eq!(steps[1].depth, 1);
        assert_eq!(steps[1].importance, "important", "the spec default");
        assert_eq!(
            steps[1].state,
            vec![("q".to_string(), "tainted".to_string())]
        );
        assert_eq!(steps[1].message, "step");
        assert_eq!(steps[2].importance, "unimportant");
        assert!(steps[2].location.as_ref().unwrap().uri.is_empty());
    }

    #[test]
    fn stacks_without_a_message_still_show_and_keep_columns() {
        let d = d();
        assert_eq!(d.stacks.len(), 2);
        let f = &d.stacks[0].frames[0];
        assert_eq!(f.text, "App.run");
        assert_eq!(f.module, "app");
        assert_eq!(f.thread_id, Some(4));
        assert_eq!(f.parameters, vec!["x".to_string()]);
        let loc = f.location.as_ref().unwrap();
        assert_eq!((loc.line, loc.column), (42, 7));
        assert_eq!(d.stacks[1].message, "second");
        assert_eq!(d.stacks[1].frames[0].text, "top");
    }

    #[test]
    fn pretty_printing_keeps_key_order_and_string_contents() {
        let raw = r#"{"z":1,"a":[true,{"k":"a, {b} [c]\"q\""}],"e":{}}"#;
        assert_eq!(
            pretty_preserving(raw),
            "{\n  \"z\": 1,\n  \"a\": [\n    true,\n    {\n      \"k\": \"a, {b} [c]\\\"q\\\"\"\n    }\n  ],\n  \"e\": {}\n}"
        );
    }

    #[test]
    fn raw_json_keeps_the_logs_key_order() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.sarif");
        std::fs::write(&p, LOG).unwrap();
        let raw = raw_result_json(&p, 0, 0).unwrap();
        let rule = raw.find("\"ruleId\"").unwrap();
        let flows = raw.find("\"codeFlows\"").unwrap();
        assert!(
            rule < flows,
            "ruleId is written before codeFlows in the log:\n{raw}"
        );
    }

    #[test]
    fn raw_json_is_the_results_own_text() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.sarif");
        std::fs::write(&p, LOG).unwrap();
        let raw = raw_result_json(&p, 0, 0).unwrap();
        assert!(raw.contains("\"ruleId\": \"js/sql-injection\""), "{raw}");
        assert!(raw.contains("\"occurrenceCount\": 3"));
        assert!(raw_result_json(&p, 0, 5).is_none());
        assert!(raw_result_json(&dir.path().join("missing.sarif"), 0, 0).is_none());
    }

    #[test]
    fn taxa_come_from_the_result_and_the_rules_relationships() {
        let log = parse_log(
            r#"{"version":"2.1.0","runs":[{
              "tool":{"driver":{"name":"T","rules":[{"id":"R1","relationships":[
                {"target":{"id":"CWE-89","toolComponent":{"name":"CWE"}},"kinds":["superset"]},
                {"target":{"index":1,"toolComponent":{"index":0}}}]}]}},
              "taxonomies":[{"name":"CWE","taxa":[
                {"id":"CWE-89","name":"SqlInjection","shortDescription":{"text":"Improper Neutralization of SQL"}},
                {"id":"CWE-20","shortDescription":{"text":"Improper Input Validation"}}]},
                {"name":"OWASP","taxa":[{"id":"A03","name":"Injection"}]}],
              "results":[{"ruleId":"R1","ruleIndex":0,"message":{"text":"m"},
                "taxa":[{"id":"A03","toolComponent":{"name":"OWASP"}},
                        {"id":"CWE-89","toolComponent":{"name":"CWE"}},
                        {"id":"X-1","toolComponent":{"name":"Unknown"}}]}]}]}"#,
        )
        .unwrap();
        let run = &log.runs[0];
        let d = details(run, &run.results.as_ref().unwrap()[0], &[]);
        assert_eq!(
            d.taxa,
            vec![
                "OWASP A03: Injection",
                "CWE CWE-89: Improper Neutralization of SQL",
                "Unknown X-1",
                "CWE CWE-20: Improper Input Validation",
            ],
            "result taxa first, then the rule's, each once; an unresolved one keeps its id"
        );
    }
}
