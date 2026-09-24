//! Everything the details pane shows for one result, resolved from the log
//! once when the selection changes: rule text, message with its links,
//! every location, every code flow and thread flow, every stack, and the
//! property bags. The renderer only lays this out.

use super::model::{Location, Message, Run, SarifResult};
use super::semantics::{BaselineState, Kind, Level, Segment, SuppressionState};
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
    /// Every thread flow of every code flow, in order.
    pub threads: Vec<Thread>,
    pub stacks: Vec<StackView>,
}

pub fn details(_run: &Run, _result: &SarifResult, _roots: &[PathBuf]) -> Details {
    Details {
        rule_id: String::new(),
        rule_name: String::new(),
        help_uri: None,
        description: String::new(),
        help: String::new(),
        message: Vec::new(),
        level: Level::None,
        kind: Kind::Fail,
        baseline: BaselineState::Unspecified,
        suppression: SuppressionState::Unknown,
        justification: None,
        locations: Vec::new(),
        related: Vec::new(),
        properties: Vec::new(),
        fingerprints: Vec::new(),
        guid: None,
        rank: None,
        occurrence_count: None,
        threads: Vec::new(),
        stacks: Vec::new(),
    }
}

/// Where `[text](id)` in the message points: a related location with that
/// id, else a primary location with it.
pub fn link_target(_d: &Details, _id: i64) -> Option<&LocRef> {
    None
}

/// The result's JSON exactly as the log has it, pretty-printed. Read from the
/// file on demand, so a large log is not kept twice in memory.
pub fn raw_result_json(_path: &Path, _run: usize, _result: usize) -> Option<String> {
    None
}

#[allow(dead_code)]
fn unused(_: &Location, _: &Message) {}

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
        assert_eq!(link_target(&d, 0).unwrap().label, "src/db.js", "falls back to locations");
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
        let get = |k: &str| d.properties.iter().find(|(a, _)| a == k).map(|(_, v)| v.clone());
        assert_eq!(get("tags").as_deref(), Some("security, cwe-089"));
        assert_eq!(get("security-severity").as_deref(), Some("8.8"));
        assert_eq!(get("github/alertNumber").as_deref(), Some("7"));
        assert_eq!(get("note").as_deref(), Some("—"));
        assert_eq!(get("list").as_deref(), Some("a, b"));
        assert_eq!(get("flag").as_deref(), Some("true"));
        let tags_at = d.properties.iter().position(|(k, _)| k == "tags").unwrap();
        let alert_at = d.properties.iter().position(|(k, _)| k == "github/alertNumber").unwrap();
        assert!(tags_at < alert_at, "rule properties come first");
    }

    #[test]
    fn identity_fields() {
        let d = d();
        assert_eq!(
            d.fingerprints,
            vec![
                ("b/v1".to_string(), "abc".to_string()),
                ("primaryLocationLineHash (partial)".to_string(), "ff00".to_string())
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
        assert_eq!(labels, vec!["Flow 1", "Flow 2 · thread t1", "Flow 2 · thread t2"]);
        let steps = &d.threads[0].steps;
        assert_eq!(steps.len(), 3);
        assert_eq!(d.threads[0].message, "taint path");
        assert_eq!(steps[0].importance, "essential");
        assert_eq!(steps[0].kinds, vec!["acquire".to_string()]);
        assert_eq!(steps[0].location.as_ref().unwrap().line, 12);
        assert_eq!(steps[1].depth, 1);
        assert_eq!(steps[1].importance, "important", "the spec default");
        assert_eq!(steps[1].state, vec![("q".to_string(), "tainted".to_string())]);
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
}
